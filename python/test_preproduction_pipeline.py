"""Mocked pipeline integration: real checkpoints/hashes, no models or subprocesses.

Run explicitly with: cd python && python -m unittest test_preproduction_pipeline
"""

import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
import wave
from unittest.mock import patch

from PIL import Image

import character_conditioning
import characters
import preproduction
import quality_backend
import shooting_script
import shot_renderer
import storyboard
import storyboard_backend
import video


class PreproductionPipelineTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run = self.root / "run"
        self.run.mkdir()
        portrait = self.root / "characters/mara/reference.png"
        portrait.parent.mkdir(parents=True)
        Image.new("RGB", (32, 32), "navy").save(portrait)
        profile = {"schema_version": 1, "id": "mara", "name": "Mara",
                   "description": "A woman with copper hair", "voice_description": "A low clear voice",
                   "image": {"file": "reference.png", "sha256": self.digest(portrait)}}
        self.write(portrait.parent / "character.json", profile)
        self.profiles = characters.snapshot_characters([profile], self.run, root=self.root)
        self.request = {
            "version": 1, "cast": [{"role": "MARA", "character": "mara"}],
            "character_conditioning": character_conditioning.POLICY,
            "storyboard_policy": shooting_script.STORYBOARD_POLICY, "voice_mode": "native",
            "script": ('INT. ROOM - DAY\n\nMara lifts a brass key from the table.\n\n'
                       'MARA: "Found it."\n\nMara places the key inside an open box.\n\n'
                       'MARA: "Keep it safe."'),
        }
        self.actions = ["Mara lifts a brass key from the table.", "Mara places the key inside an open box."]
        self.templates = [action + " {{line_" + str(2 * number) + "}}"
                          for number, action in enumerate(self.actions, 1)]
        self.director = {
            "planning_policy": "single-render-v1", "storyboard_policy": shooting_script.STORYBOARD_POLICY,
            "cast_identity": [{"role": "MARA", "description": profile["description"],
                               "voice_description": profile["voice_description"]}],
            "revision_notes": ["Keep the key transfer visible."],
            "shots": [{"seconds": 5, "beat_ids": [2 * number - 1, 2 * number],
                       "visible_roles": ["MARA"], "coverage": "continuous", "continue_previous": False,
                       "prompt_template": self.templates[number - 1],
                       "start_state": start, "storyboard_prompt": "Static medium shot. " + start,
                       "end_state": end, "dramatic_purpose": action,
                       "visual_contract": [action],
                       "action_coverage": [{"beat_id": 2 * number - 1, "part": 1, "parts": 1,
                                            "action": action}]}
                      for number, (action, start, end) in enumerate(zip(self.actions,
                          ["Mara stands beside a key on the table.", "Mara holds the key above an open box."],
                          ["Mara holds the lifted key.", "The key rests inside the box."]), 1)],
        }
        self.codex_calls, self.image_calls, self.render_calls = [], [], []
        self.interrupt_image = self.interrupt_render = False
        self.mock(quality_backend, "ROOT", self.root)
        self.mock(video, "ROOT", self.root)
        self.mock(shooting_script, "_executable", return_value="mock-codex")
        self.mock(shooting_script, "_stream_codex", side_effect=self.codex)
        self.mock(storyboard_backend, "check", return_value={"backend": "mock-still"})
        self.mock(storyboard_backend, "generate", side_effect=self.generate_image)
        self.mock(shot_renderer, "validate_video", return_value={"valid": True})
        self.mock(shot_renderer, "_assemble", side_effect=self.assemble)
        self.mock(character_conditioning, "_worker", side_effect=AssertionError("Native mode called Seed-VC"))
        self.mock(character_conditioning, "apply_voice", side_effect=AssertionError("Native mode converted audio"))

    def mock(self, target, name, *args, **kwargs):
        mocked = patch.object(target, name, *args, **kwargs)
        self.addCleanup(mocked.stop)
        return mocked.start()

    @staticmethod
    def digest(path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()

    @staticmethod
    def write(path, value):
        Path(path).write_text(json.dumps(value), encoding="utf-8")

    def codex(self, command, instruction, **options):
        properties = options["output_schema"]["properties"]
        if "storyboard_policy" in properties:
            self.codex_calls.append("director")
            result = self.director
        elif "checks" in properties:
            self.codex_calls.append("fidelity")
            data = json.loads(instruction.split("\nDATA\n", 1)[1])
            prompts = {shot["shot"]: shot["prompt"] for shot in data["shots"]}
            result = {"checks": [{"id": item["id"], "preserved": True,
                                   "evidence": [{"shot": item["shot_numbers"][0],
                                                 "quote": prompts[item["shot_numbers"][0]]}],
                                   "reason": "The assigned action and dialogue remain explicit."}
                                  for item in data["requirements"]]}
        else:
            self.codex_calls.append("motion")
            data = json.loads(instruction.rsplit("\n\n", 1)[1])
            self.assertEqual(len(options["image_paths"]), len(data["shots_to_direct"]))
            self.assertTrue(all(Path(path).is_file() for path in options["image_paths"]))
            result = {"shots": [{"shot": number, "prompt_template": "A steady camera. " + self.templates[number - 1],
                                 "image_observations": "The performer, key and destination are visible.",
                                 "blocking_issue": "", "storyboard_prompt": ""}
                                for number in data["shots_to_direct"]]}
        raw = json.dumps(result)
        options["response_path"].write_text(raw, encoding="utf-8")
        return raw

    def generate_image(self, prompt, references, output, seed, *, width, height, on_activity):
        self.image_calls.append(Path(output).name)
        self.assertTrue(references, "Every storyboard should receive the saved portrait")
        if self.interrupt_image and Path(output).name == "002.partial.png":
            self.interrupt_image = False
            raise KeyboardInterrupt()
        Image.new("RGB", (width, height), (seed % 255, 40, 90)).save(output)
        return {"sha256": self.digest(output)}

    def render_video(self, prompt, seed, frames, output, **options):
        self.render_calls.append({"shot": Path(output).parent.name, "prompt": prompt,
                                  "seed": seed, "image": Path(options["image_file"]),
                                  "image_sha256": self.digest(options["image_file"])})
        if self.interrupt_render and Path(output).parent.name == "002":
            self.interrupt_render = False
            raise KeyboardInterrupt()
        Path(output).write_bytes(f"mock-video:{seed}:{prompt}".encode())
        return {"backend": "mock-video"}

    @staticmethod
    def assemble(clips, output, seconds, fade_out, **options):
        Path(output).write_bytes(b"".join(Path(clip).read_bytes() for clip in clips))
        return {"valid": True}

    def prepare(self):
        return preproduction.prepare(self.request, self.profiles, self.run, 17,
                                     token_counter=lambda prompt: len(prompt.split()), fresh=True)

    def render(self, plan, boards, *, resume=False):
        return shot_renderer.render_shooting_script(
            plan, 17, self.run / "video.mp4", storyboard=boards, character_profiles=self.profiles,
            render_fn=self.render_video, review_enabled=False, resume=resume)

    def test_interruption_resumes_storyboards_and_takes_without_repeating_completed_work(self):
        self.interrupt_image = True
        with self.assertRaises(KeyboardInterrupt):
            self.prepare()
        first_frame = self.digest(self.run / "storyboards/001.png")
        self.assertEqual(self.codex_calls, ["director"])
        self.assertFalse((self.run / "shooting-script.json").exists())

        plan, boards = self.prepare()
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertEqual(self.image_calls, ["001.partial.png", "002.partial.png", "002.partial.png"])
        self.assertEqual(self.digest(self.run / "storyboards/001.png"), first_frame)
        self.assertEqual(plan["planning_stage"], "final")
        self.assertEqual(plan["source_fidelity"]["plan_sha256"], shooting_script._plan_fingerprint(plan))
        self.assertIn('"Found it."', plan["shots"][0]["prompt"])
        self.assertIn('"Keep it safe."', plan["shots"][1]["prompt"])
        self.assertNotIn("{{line_", plan["shots"][0]["prompt"])

        self.interrupt_render = True
        with self.assertRaises(KeyboardInterrupt):
            self.render(plan, boards)
        first_take = self.digest(self.run / "shots/001/video.mp4")
        resumed_plan, resumed_boards = self.prepare()
        result = self.render(resumed_plan, resumed_boards, resume=True)
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertEqual([call["shot"] for call in self.render_calls], ["001", "002", "002"])
        self.assertEqual(self.render_calls[1]["seed"], self.render_calls[2]["seed"])
        self.assertTrue(result["shots"][0]["reused"])
        self.assertEqual(self.digest(self.run / "shots/001/video.mp4"), first_take)
        for call in self.render_calls:
            number = int(call["shot"])
            self.assertEqual(call["prompt"], plan["shots"][number - 1]["prompt"])
            self.assertEqual(call["image_sha256"], boards["shots"][number - 1]["image"]["sha256"])

    def test_changed_storyboard_invalidates_motion_receipt_and_saved_take(self):
        plan, boards = self.prepare()
        self.render(plan, boards)
        replacement = self.run / "storyboards/001-revised.png"
        Image.new("RGB", (quality_backend.WIDTH, quality_backend.HEIGHT), "gold").save(replacement)
        changed = copy.deepcopy(boards)
        changed["shots"][0]["image"].update(file="storyboards/001-revised.png", sha256=self.digest(replacement))
        self.write(self.run / "storyboards.json", changed)
        with self.assertRaisesRegex(ValueError, "images differ"):
            self.prepare()
        self.assertEqual(len(self.render_calls), 2)

        # Exercise the renderer guard independently with an explicitly supplied
        # replacement binding; an old first-frame take must never be reused.
        self.render_calls.clear()
        result = self.render(plan, changed, resume=True)
        self.assertEqual([call["shot"] for call in self.render_calls], ["001"])
        self.assertEqual(self.render_calls[0]["image"], replacement)
        self.assertFalse(result["shots"][0].get("reused", False))
        self.assertTrue(result["shots"][1]["reused"])
        self.assertTrue((self.run / "takes/001/001/video.mp4").is_file())

    def test_storyboards_only_produces_final_motion_plan_without_video_setup_or_inference(self):
        events = self.mock(video, "emit")
        self.mock(video, "check_environment")
        self.mock(video, "prompt_token_counter", return_value=lambda prompt: len(prompt.split()))
        model = self.mock(video, "model_path", side_effect=AssertionError("Debug mode requested LTX weights"))
        renderer = self.mock(video, "render_shooting_script", side_effect=AssertionError("Debug mode rendered video"))
        with patch.dict(os.environ):
            video.generate_screenplay(self.request, seed=17, storyboards_only=True, fresh_preproduction=True)
        run = next((self.root / "outputs").iterdir())
        saved = json.loads((run / "render.json").read_text())
        self.assertEqual(saved["status"], "storyboards-ready")
        self.assertTrue((run / "shooting-script.json").is_file())
        self.assertTrue((run / "storyboards.json").is_file())
        self.assertFalse((run / "video.mp4").exists())
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertTrue(any(call.args[0] == "storyboards_ready" for call in events.call_args_list))
        model.assert_not_called()
        renderer.assert_not_called()

    def test_missing_portrait_stops_before_codex_or_model_checks(self):
        profile_path = self.root / "characters/mara/character.json"
        profile = json.loads(profile_path.read_text())
        profile.pop("image")
        self.write(profile_path, profile)
        self.mock(video, "check_environment")
        model = self.mock(video, "model_path")
        tokenizer = self.mock(video, "prompt_token_counter")
        with patch.dict(os.environ), self.assertRaisesRegex(ValueError, "saved image"):
            video.generate_screenplay(self.request, seed=17)
        self.assertEqual(self.codex_calls, [])
        self.assertEqual(self.image_calls, [])
        model.assert_not_called()
        tokenizer.assert_not_called()

    def test_unavailable_saved_voice_converter_stops_before_codex(self):
        profile_path = self.root / "characters/mara/character.json"
        sample = profile_path.parent / "voice.wav"
        with wave.open(str(sample), "wb") as audio:
            audio.setparams((1, 2, 24000, 0, "NONE", "not compressed"))
            audio.writeframes(b"\x01\x00" * 2400)
        profile = json.loads(profile_path.read_text())
        profile["voice_sample"] = {"file": "voice.wav", "sha256": self.digest(sample)}
        self.write(profile_path, profile)
        self.mock(video, "check_environment")
        model = self.mock(video, "model_path")
        converter = self.mock(character_conditioning, "_worker", side_effect=RuntimeError("Seed-VC unavailable"))
        with patch.dict(os.environ), self.assertRaisesRegex(RuntimeError, "Seed-VC unavailable"):
            video.generate_screenplay(self.request, seed=17, voice_mode="saved")
        converter.assert_called_once_with("--check")
        self.assertEqual(self.codex_calls, [])
        self.assertEqual(self.image_calls, [])
        model.assert_not_called()


if __name__ == "__main__":
    unittest.main()
