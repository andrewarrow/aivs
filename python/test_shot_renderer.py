import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import shot_renderer


def plan_for(*, fade_out=False, continuations=(False, True, False)):
    shots = [
        {
            "seconds": 4,
            "prompt": f"Directed shot {index + 1}.",
            "continue_previous": continuations[index],
            "start_state": f"state {index} start",
            "end_state": f"state {index} end",
            "beat_ids": [f"beat-{index + 1}"],
        }
        for index in range(len(continuations))
    ]
    return {
        "version": 1,
        "seconds": sum(shot["seconds"] for shot in shots),
        "fade_out": fade_out,
        "shots": shots,
    }


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg is required")
class ShotRendererTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def render_small_clip(self, prompt, seed, frames, output, *, on_progress,
                          image_file, hold_image_framing):
        self.calls.append((prompt, seed, frames, image_file, hold_image_framing))
        seconds = frames / shot_renderer.FPS
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
            "-f", "lavfi", "-i",
            f"testsrc2=size={shot_renderer.WIDTH}x{shot_renderer.HEIGHT}:rate={shot_renderer.FPS}:duration={seconds}",
            "-f", "lavfi", "-i", f"sine=frequency={440 + seed % 100}:sample_rate=48000:duration={seconds}",
            "-t", str(seconds), "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-ar", "48000", "-ac", "2", str(output),
        ], check=True, capture_output=True, text=True)
        on_progress(label="test render", fraction=0.0, remaining_seconds=3)
        on_progress(label="test render", fraction=0.5, remaining_seconds=2)
        on_progress(label="test render", fraction=1.0, remaining_seconds=0)
        return {"backend": "fake", "output_frames": frames, "seed": seed}

    def test_continuity_order_seeds_progress_assembly_and_validation(self):
        self.calls = []
        progress = []
        shot_updates = []
        plan = plan_for(fade_out=True)
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

                with self.assertRaises(type(exception)):
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


if __name__ == "__main__":
    unittest.main()
