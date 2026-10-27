"""Real manual storyboard bindings with mocked B/C inference and video checks."""

import copy
from contextlib import nullcontext
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from PIL import Image

import premium_backend
import production_comparison as comparison
import quality_backend
import shot_renderer
import storyboard
import video


class ProductionComparisonTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run = self.root / "experiment"
        (self.root / "screenplay.txt").write_text("A light flickers.", encoding="utf-8")
        Image.new("RGB", (32, 32), "navy").save(self.root / "frame.png")
        plan = {"version": 1, "seconds": 6, "fade_out": False, "render_format": "landscape",
                "shots": [{"seconds": 6, "prompt": "A light flickers in an empty room.",
                           "continue_previous": False, "start_state": "An illuminated room.",
                           "end_state": "A flickering light.", "beat_ids": [1]}]}
        self.write(self.root / "plan.json", plan)
        self.write(self.root / "request.json", {
            "version": 1, "screenplay": "screenplay.txt", "baseline_plan": "plan.json",
            "controlled_plan": "plan.json", "requirements": ["The light flickers."],
            "candidate_seeds": [83], "local_profile": "hq", "premium_model": premium_backend.MODEL,
            "storyboards": [{"shot": 1, "image": "frame.png", "description": "An empty room."}],
        })
        self.mock(comparison, "report")
        self.mock(video, "project_lock", side_effect=lambda name: nullcontext())
        self.mock(quality_backend, "doctor")
        self.mock(quality_backend, "model_path")
        self.local = self.mock(quality_backend, "render", side_effect=self.render_video)
        self.cloud = self.mock(premium_backend, "render", side_effect=self.render_video)
        self.mock(shot_renderer, "validate_video", return_value={"valid": True})
        self.mock(shot_renderer, "_assemble", side_effect=self.assemble)
        comparison.prepare(self.root / "request.json", self.run)

    def mock(self, target, name, **options):
        mocked = patch.object(target, name, **options)
        self.addCleanup(mocked.stop)
        return mocked.start()

    @staticmethod
    def write(path, value):
        path.write_text(json.dumps(value), encoding="utf-8")

    @staticmethod
    def render_video(prompt, seed, frames, output, **options):
        if not Path(options["image_file"]).is_file():
            raise AssertionError("Manual storyboard was not passed to inference.")
        Path(output).write_bytes(f"mock-video:{seed}:{prompt}".encode())
        return {"backend": "mock-video"}

    @staticmethod
    def assemble(clips, output, seconds, fade_out, **options):
        Path(output).write_bytes(b"".join(Path(clip).read_bytes() for clip in clips))
        return {"valid": True}

    def test_manual_storyboards_render_in_both_comparison_arms(self):
        with patch.object(storyboard, "require_accepted", side_effect=AssertionError("Experimental frames need no production receipt")) as gate:
            for arm in ("B", "C"):
                with self.subTest(arm=arm):
                    comparison.render(self.run, arm, 1)
                    directory = self.run / "candidates" / arm / "001"
                    candidate = json.loads((directory / "candidate.json").read_text())
                    take = json.loads((directory / "shots/001/render.json").read_text())
                    self.assertEqual(candidate["status"], "complete")
                    self.assertEqual(take["continuity"]["conditioning"], "storyboard-first-frame")
                    self.assertNotIn("storyboard_acceptance", take)
            gate.assert_not_called()
        self.local.assert_called_once()
        self.cloud.assert_called_once()

    def test_manual_storyboards_still_require_matching_plan_and_image_bytes(self):
        plan = json.loads((self.run / "controlled-plan.json").read_text())
        boards = json.loads((self.run / "storyboards.json").read_text())
        bad = copy.deepcopy(boards)
        bad["shots"][0]["planned_shot_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "different planned shot"):
            shot_renderer.render_shooting_script(plan, 83, self.run / "video.mp4",
                                                 storyboard=bad, review_enabled=False)
        frame = self.run / boards["shots"][0]["image"]["file"]
        frame.write_bytes(b"changed image bytes")
        with self.assertRaisesRegex(ValueError, "Storyboard image changed"):
            shot_renderer.render_shooting_script(plan, 83, self.run / "video.mp4",
                                                 storyboard=boards, review_enabled=False)
        self.local.assert_not_called()
        self.cloud.assert_not_called()

    def test_experiment_flags_cannot_bypass_declared_production_policy(self):
        original = json.loads((self.run / "controlled-plan.json").read_text())
        for placement in ("plan", "source"):
            with self.subTest(placement=placement):
                plan = copy.deepcopy(original)
                plan["source"] = {"cast": [], "beats": []}
                owner = plan if placement == "plan" else plan["source"]
                owner["storyboard_policy"] = "per-shot-first-frame-v1"
                boards = json.loads((self.run / "storyboards.json").read_text())
                boards["shots"][0]["planned_shot_sha256"] = storyboard.planned_shot_sha256(plan, plan["shots"][0])
                # The real production gate rejects this unapproved manual input.
                with self.assertRaisesRegex(ValueError, "canonical props"):
                    shot_renderer.render_shooting_script(plan, 83, self.run / "video.mp4",
                                                         storyboard=boards, review_enabled=False, resume=True)
        self.local.assert_not_called()
        self.cloud.assert_not_called()


if __name__ == "__main__":
    unittest.main()
