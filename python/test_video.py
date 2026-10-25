import io
import json
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch

import quality_backend as backend
import video
from dialogue_helpers import duration_estimate
from validation import validate_video


class SetupTests(unittest.TestCase):
    def test_cache_requires_every_weight_including_audio_and_valid_lengths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            header = json.dumps({"x": {"data_offsets": [0, 4], "dtype": "F32", "shape": [1]}}).encode()
            for name in backend.WEIGHTS:
                (root / name).write_bytes(len(header).to_bytes(8, "little") + header + b"\0" * 4)
            for name in backend.CONFIGS:
                (root / name).write_text("{}")
            hub = types.SimpleNamespace(snapshot_download=lambda *args, **kwargs: root)
            with patch.dict("sys.modules", {"huggingface_hub": hub}):
                self.assertEqual(backend.model_path(), root)
                (root / "vocoder.safetensors").unlink()
                with self.assertRaisesRegex(RuntimeError, "incomplete.*vocoder"):
                    backend.model_path()
                (root / "vocoder.safetensors").write_bytes(len(header).to_bytes(8, "little") + header)
                with self.assertRaisesRegex(RuntimeError, "Truncated"):
                    backend.model_path()

    def test_missing_env_emits_instructions_and_does_not_download(self):
        fake_dotenv = types.SimpleNamespace(dotenv_values=lambda path: {})
        with patch.dict("sys.modules", {"dotenv": fake_dotenv}), patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaisesRegex(ValueError, "Hugging Face"):
                video.check_environment()
            self.assertIn('"event": "needs_token"', out.getvalue())

    def test_events_redact_token(self):
        with patch.object(video, "_token", "hf_secret"), patch("sys.stdout", new_callable=io.StringIO) as out:
            video.emit("error", message="Authorization failed: hf_secret")
            self.assertNotIn("hf_secret", out.getvalue())
            self.assertIn("[redacted]", out.getvalue())

    def test_setup_repairs_a_damaged_blob_instead_of_trusting_its_cache_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repairs = []
            hub = types.SimpleNamespace(
                snapshot_download=lambda *args, **kwargs: root,
                hf_hub_download=lambda repo, filename, **kwargs: repairs.append((filename, kwargs)),
            )
            dotenv = types.SimpleNamespace(dotenv_values=lambda path: {"HF_API": "hf_test"})
            damaged = backend.InvalidCache("vocoder.safetensors")
            with patch.dict("sys.modules", {"huggingface_hub": hub, "dotenv": dotenv}), patch.object(backend, "validate_snapshot", side_effect=[damaged, damaged, None]), patch.object(backend.shutil, "disk_usage", return_value=types.SimpleNamespace(free=100 * 1024**3)):
                self.assertEqual(backend.model_path(download=True), root)
            self.assertEqual(len(repairs), 1)
            self.assertEqual(repairs[0][0], "vocoder.safetensors")
            self.assertTrue(repairs[0][1]["force_download"])
            self.assertEqual(repairs[0][1]["revision"], backend.MODEL_REVISION)


class RenderTests(unittest.TestCase):
    def test_default_matches_option_eight_and_its_auto_duration(self):
        self.assertIn('"hello everyone my name is nancy and I am so excited to be here talking with you"', video.DEFAULT_PROMPT)
        timing = duration_estimate(video.DEFAULT_PROMPT)
        self.assertEqual(timing["seconds"], 8)
        self.assertEqual(timing["turn_count"], 1)
        self.assertEqual((backend.WIDTH, backend.HEIGHT, backend.FPS), (1280, 704, 24))

    def test_failed_render_keeps_report_and_never_publishes_complete(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=RuntimeError("GPU unavailable")), patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaisesRegex(RuntimeError, "GPU unavailable"):
                video.generate(42)
            report = json.loads(next(Path(directory).glob("outputs/*/render.json")).read_text())
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["seed"], 42)
            self.assertNotIn('"event": "complete"', out.getvalue())

    def test_workspace_lock_prevents_a_second_render(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)):
            with video.project_lock(".render.lock"):
                with self.assertRaisesRegex(RuntimeError, "Another AIVS"):
                    with video.project_lock(".render.lock"):
                        pass

    def test_real_ffmpeg_validates_video_and_rejects_missing_audio(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "video.mp4"
            subprocess.run([
                "ffmpeg", "-v", "error", "-f", "lavfi", "-i", "color=c=blue:s=128x128:r=24:d=1",
                "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", str(path),
            ], check=True)
            info = validate_video(path, 24, 128, 128, expect_audio=True)
            self.assertIn("audio", info)
            with self.assertRaises(RuntimeError):
                validate_video(path, 23, 128, 128, expect_audio=True)
            silent = Path(directory) / "silent.mp4"
            subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-c:v", "copy", "-an", str(silent)], check=True)
            with self.assertRaisesRegex(RuntimeError, "expected one audio"):
                validate_video(silent, 24, 128, 128, expect_audio=True)


if __name__ == "__main__":
    unittest.main()
