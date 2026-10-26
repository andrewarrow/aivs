import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import shot_renderer
from render_formats import resolve_format


def plan_for(*, fade_out=False, continuations=(False, True, False), scene_ids=None):
    shots = [
        {
            "seconds": 4,
            "prompt": f"Directed shot {index + 1}.",
            "continue_previous": continuations[index],
            "start_state": f"state {index} start",
            "end_state": f"state {index} end",
            "beat_ids": [index + 1],
        }
        for index in range(len(continuations))
    ]
    if scene_ids is not None:
        for shot, scene_id in zip(shots, scene_ids):
            shot.update(scene_id=scene_id, scene="INT. SAME ROOM - DAY")
    return {
        "version": 1,
        "seconds": sum(shot["seconds"] for shot in shots),
        "fade_out": fade_out,
        "shots": shots,
        "source": {"beats": [{"id": index + 1, "kind": "action", "text": "A light flickers."}
                             for index in range(len(shots))]},
    }


class ContinuityPlanTests(unittest.TestCase):
    def test_scene_ids_force_handoff_even_when_director_requested_a_cut(self):
        plan = plan_for(continuations=(False, False, False), scene_ids=(1, 1, 2))
        shots, _, _ = shot_renderer._validate_plan(plan)
        self.assertEqual(
            [shot_renderer._uses_previous_frame(shots, index) for index in range(3)],
            [False, True, False],
        )
        # Identical scene headings do not erase a distinct source scene boundary.
        self.assertEqual(shots[1]["scene"], shots[2]["scene"])
        self.assertFalse(shots[1]["continue_previous"])

    def test_legacy_plans_use_explicit_continuation(self):
        shots, _, _ = shot_renderer._validate_plan(plan_for())
        self.assertEqual(
            [shot_renderer._uses_previous_frame(shots, index) for index in range(3)],
            [False, True, False],
        )

    def test_rejects_first_shot_and_cross_scene_continuation(self):
        for plan, error in (
            (plan_for(continuations=(True,)), "first shot"),
            (plan_for(continuations=(False, True), scene_ids=(1, 2)), "across scenes"),
        ):
            with self.subTest(error=error), self.assertRaisesRegex(ValueError, error):
                shot_renderer._validate_plan(plan)

    def test_scene_ids_cannot_be_partial_or_invalid(self):
        for value in (None, True, -1, "1"):
            plan = plan_for(continuations=(False, False), scene_ids=(1, value))
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "scene_id"):
                shot_renderer._validate_plan(plan)
        del plan["shots"][1]["scene_id"]
        with self.assertRaisesRegex(ValueError, "scene_id"):
            shot_renderer._validate_plan(plan)


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg is required")
class ShotRendererTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def render_small_clip(self, prompt, seed, frames, output, *, on_progress,
                          image_file, hold_image_framing, width=None, height=None):
        self.calls.append((prompt, seed, frames, image_file, hold_image_framing))
        width = shot_renderer.WIDTH if width is None else width
        height = shot_renderer.HEIGHT if height is None else height
        seconds = frames / shot_renderer.FPS
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
            "-f", "lavfi", "-i",
            f"testsrc2=size={width}x{height}:rate={shot_renderer.FPS}:duration={seconds}",
            "-f", "lavfi", "-i", f"sine=frequency={440 + seed % 100}:sample_rate=48000:duration={seconds}",
            "-t", str(seconds), "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-ar", "48000", "-ac", "2", str(output),
        ], check=True, capture_output=True, text=True)
        on_progress(label="test render", fraction=0.0, remaining_seconds=3)
        on_progress(label="test render", fraction=0.5, remaining_seconds=2)
        on_progress(label="test render", fraction=1.0, remaining_seconds=0)
        return {"backend": "fake", "output_frames": frames, "seed": seed}

    def test_exact_delivery_sizes_preserve_native_frame_handoff_and_resume_takes(self):
        for name in ("landscape", "portrait"):
            with self.subTest(format=name), tempfile.TemporaryDirectory() as directory:
                self.calls = []
                spec = resolve_format(name)
                plan = dict(plan_for(continuations=(False, True), scene_ids=(1, 1)), render_format=name)
                output = Path(directory) / "video.mp4"

                def render(*args, **kwargs):
                    self.assertEqual((kwargs["width"], kwargs["height"]),
                                     (spec["model_width"], spec["model_height"]))
                    return self.render_small_clip(*args, **kwargs)

                result = shot_renderer.render_shooting_script(plan, 19, output, render_fn=render)
                self.assertEqual((result["video"]["width"], result["video"]["height"]),
                                 (spec["width"], spec["height"]))
                self.assertEqual(result["video"]["sample_aspect_ratio"], "1:1")
                self.assertEqual(len(self.calls), 2)
                native = subprocess.check_output([
                    "ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                    "stream=width,height", "-of", "json", str(self.calls[1][3]),
                ], text=True)
                frame = json.loads(native)["streams"][0]
                self.assertEqual((frame["width"], frame["height"]),
                                 (spec["model_width"], spec["model_height"]))
                with patch.object(shot_renderer.quality_backend, "render") as renderer:
                    resumed = shot_renderer.render_shooting_script(plan, 19, output, resume=True)
                    renderer.assert_not_called()
                self.assertTrue(all(shot["reused"] for shot in resumed["shots"]))
                self.assertEqual((resumed["video"]["width"], resumed["video"]["height"]),
                                 (spec["width"], spec["height"]))

    def test_continuity_order_seeds_progress_assembly_and_validation(self):
        self.calls = []
        progress = []
        shot_updates = []
        plan = plan_for(fade_out=True, continuations=(False, False, False), scene_ids=(1, 1, 2))
        with patch.object(shot_renderer, "WIDTH", 128), patch.object(shot_renderer, "HEIGHT", 64):
            runtime = shot_renderer.render_shooting_script(
                plan, 2**31 - 2, self.root / "video.mp4",
                on_progress=lambda **event: progress.append(event),
                on_shot=lambda index, report: shot_updates.append((index, report["status"])),
                render_fn=self.render_small_clip,
            )

        self.assertEqual([call[0] for call in self.calls], [shot["prompt"] for shot in plan["shots"]])
        self.assertEqual([call[1] for call in self.calls], [2**31 - 2, 2**31 - 1, 0])
        self.assertEqual([call[2] for call in self.calls], [96, 96, 96])
        self.assertEqual([call[3] for call in self.calls], [None, self.root / "shots/002/start-frame.png", None])
        self.assertTrue(all(call[4] is False for call in self.calls))
        self.assertTrue((self.root / "shots/002/start-frame.png").read_bytes().startswith(b"\x89PNG"))
        continuity = runtime["shots"][1]["continuity"]
        self.assertEqual(continuity["conditioning"], "previous-shot-last-frame")
        self.assertEqual(continuity["source_shot"], 1)
        self.assertEqual(continuity["source_frame"], 95)
        self.assertEqual(continuity["source_image"], str(self.root / "shots/002/start-frame.png"))
        self.assertEqual(
            json.loads((self.root / "shots/002/render.json").read_text())["continuity"],
            continuity,
        )
        # Verify the saved anchor is the final decoded frame, not an arbitrary
        # earlier frame or a portrait from the cast library.
        decoded = []
        for media in (self.root / "shots/001/video.mp4", self.root / "shots/002/start-frame.png"):
            decoded.append(subprocess.run([
                "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-i", str(media),
                "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
            ], check=True, capture_output=True).stdout)
        self.assertEqual(decoded[0][-128 * 64 * 3:], decoded[1])
        fractions = [event["fraction"] for event in progress]
        self.assertEqual(fractions, sorted(fractions))
        self.assertLessEqual(max(fractions), 0.99)
        self.assertEqual(runtime["pipeline"], "directed-shots")
        self.assertEqual(runtime["output_frames"], 12 * shot_renderer.FPS)
        self.assertEqual(runtime["video"]["nb_read_frames"], str(12 * shot_renderer.FPS))
        self.assertEqual(runtime["video"]["r_frame_rate"], "24/1")
        self.assertEqual(len(runtime["shots"]), 3)
        self.assertEqual(
            [item for item in shot_updates if item[1] == "complete"],
            [(0, "complete"), (1, "complete"), (2, "complete")],
        )
        self.assertTrue((self.root / "video.mp4").is_file())
        self.assertEqual(json.loads((self.root / "shots/003/render.json").read_text())["status"], "complete")

    def test_missing_anchor_fails_without_rendering_unconditioned_shot(self):
        calls = []

        def fake_renderer(prompt, seed, frames, output, **kwargs):
            calls.append(prompt)
            output.write_bytes(b"unusable completed clip")
            return {"output_frames": frames}

        with patch.object(shot_renderer, "validate_video", return_value={}), \
                patch.object(shot_renderer, "_extract_last_frame", side_effect=RuntimeError("No final frame")):
            with self.assertRaisesRegex(RuntimeError, "No final frame"):
                shot_renderer.render_shooting_script(
                    plan_for(continuations=(False, False), scene_ids=(1, 1)),
                    41, self.root / "video.mp4", render_fn=fake_renderer,
                )
        self.assertEqual(len(calls), 1)
        self.assertFalse((self.root / "video.mp4").exists())
        report = json.loads((self.root / "shots/002/render.json").read_text())
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["continuity"]["source_shot"], 1)

    def test_failure_and_interruption_keep_shot_reports_without_publishing(self):
        for exception, status in ((RuntimeError("render failed"), "failed"),
                                  (KeyboardInterrupt(), "interrupted")):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)

                def fake_renderer(prompt, seed, frames, output, **kwargs):
                    if "2." in prompt:
                        raise exception
                    output.write_bytes(b"first completed clip")
                    return {"output_frames": frames}

                with patch.object(shot_renderer, "validate_video", return_value={}), \
                        self.assertRaises(type(exception)):
                    shot_renderer.render_shooting_script(
                        plan_for(continuations=(False, False)), 41, root / "video.mp4",
                        render_fn=fake_renderer,
                    )
                self.assertFalse((root / "video.mp4").exists())
                self.assertTrue((root / "shots/001/video.mp4").is_file())
                self.assertEqual(
                    json.loads((root / "shots/001/render.json").read_text())["status"], "complete"
                )
                failed = json.loads((root / "shots/002/render.json").read_text())
                self.assertEqual(failed["status"], status)
                if status == "failed":
                    self.assertEqual(failed["error"], "render failed")

    def test_invalid_plan_is_rejected_before_creating_shot_reports(self):
        plan = plan_for()
        plan["seconds"] -= 1
        output = self.root / "video.mp4"
        with self.assertRaisesRegex(ValueError, "sum of its shot durations"):
            shot_renderer.render_shooting_script(plan, 1, output, render_fn=self.render_small_clip)
        self.assertFalse((self.root / "shots").exists())

    def test_generated_audio_is_trusted_without_transcription(self):
        plan = plan_for(continuations=(False, True), scene_ids=(1, 1))
        plan["source"]["beats"] = [
            {"id": 1, "kind": "dialogue", "text": "Good news. It's definitely the car."},
            {"id": 2, "kind": "dialogue", "text": "He was an optimistic man."},
        ]
        self.calls = []
        # The fixture contains a tone, not the screenplay words. Only playable
        # media and timing matter to the production audio path.
        with patch.object(shot_renderer, "WIDTH", 128), patch.object(shot_renderer, "HEIGHT", 64):
            result = shot_renderer.render_shooting_script(
                plan, 1, self.root / "video.mp4", render_fn=self.render_small_clip,
            )
        self.assertEqual(len(self.calls), 2)
        self.assertTrue((self.root / "video.mp4").is_file())
        self.assertNotIn("speech_validation", result["video"])
        self.assertTrue(all(shot["status"] == "complete" for shot in result["shots"]))
        self.assertEqual(list(self.root.rglob("speech-*")), [])

    def test_resume_reuses_take_previously_failed_only_for_speech(self):
        plan = plan_for(continuations=(False, True), scene_ids=(1, 1))
        self.calls = []
        output = self.root / "video.mp4"
        with patch.object(shot_renderer, "WIDTH", 128), patch.object(shot_renderer, "HEIGHT", 64):
            shot_renderer.render_shooting_script(plan, 1, output, render_fn=self.render_small_clip)
            report_file = self.root / "shots/001/render.json"
            report = json.loads(report_file.read_text())
            mismatch = {"status": "mismatch", "transcript": "chimo", "expected_text": "chemo"}
            report.update(status="failed", error="Generated speech needs review", speech_validation=mismatch)
            report_file.write_text(json.dumps(report))
            historical = report_file.with_name("speech-validation.json")
            historical.write_text(json.dumps(mismatch))
            original_video = report_file.with_name("video.mp4").read_bytes()
            output.unlink()
            with patch.object(shot_renderer.quality_backend, "render") as renderer:
                result = shot_renderer.render_shooting_script(plan, 1, output, resume=True)
                renderer.assert_not_called()
        self.assertTrue(output.is_file())
        self.assertTrue(all(shot["reused"] for shot in result["shots"]))
        self.assertEqual(report_file.with_name("video.mp4").read_bytes(), original_video)
        restored = json.loads(report_file.read_text())
        self.assertEqual(restored["status"], "complete")
        self.assertNotIn("error", restored)
        self.assertNotIn("speech_validation", restored)
        self.assertEqual(json.loads(historical.read_text()), mismatch)

    def test_failed_assembled_media_check_keeps_final_video_unpublished(self):
        plan = plan_for(continuations=(False,), fade_out=True)
        self.calls = []
        with patch.object(shot_renderer, "WIDTH", 128), patch.object(shot_renderer, "HEIGHT", 64), \
                patch.object(shot_renderer, "validate_video", side_effect=[{}, RuntimeError("Invalid assembled video")]):
            with self.assertRaisesRegex(RuntimeError, "Invalid assembled video"):
                shot_renderer.render_shooting_script(plan, 1, self.root / "video.mp4", render_fn=self.render_small_clip)
        self.assertFalse((self.root / "video.mp4").exists())
        self.assertTrue((self.root / "video.partial.mp4").exists())


if __name__ == "__main__":
    unittest.main()
