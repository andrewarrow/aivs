import hashlib
import io
import importlib.util
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

    def test_selected_prompt_reaches_renderer_timing_and_report_through_cli(self):
        library = Path(video.__file__).resolve().parents[1]
        for profile_path in sorted((library / "characters").glob("*/character.json")):
            char_id = profile_path.parent.name
            with self.subTest(character=char_id):
                profile = video.characters.load_character(char_id, root=library)
                name = profile["name"]
                prompt = f'A cinematic shot of {name}. {name} says "hello my name is {name}".'
                expanded = video.characters.expand_prompt(prompt, [profile], image_conditioned=True)
                relative_image = Path("characters") / char_id / "scene-reference.png"

                def render(actual_prompt, seed, frames, output, *, on_progress, image_file, hold_image_framing):
                    self.assertEqual(actual_prompt, expanded)
                    self.assertEqual(seed, 42)
                    self.assertEqual(frames, duration_estimate(prompt)["seconds"] * backend.FPS)
                    self.assertEqual(image_file, output.parent / relative_image)
                    self.assertEqual(image_file.read_bytes(), (library / relative_image).read_bytes())
                    self.assertTrue(hold_image_framing)
                    output.write_bytes(b"validated video")
                    return {}

                with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=render) as renderer, patch("sys.argv", ["video.py", "render", "--seed", "42", "--prompt", prompt]), patch("sys.stdout", new_callable=io.StringIO):
                    self.assertEqual(video.main(), 0)
                    renderer.assert_called_once()
                    report = json.loads(next(Path(directory).glob("outputs/*/render.json")).read_text())
                    self.assertEqual(report["original_prompt"], prompt)
                    self.assertEqual(report["prompt"], expanded)
                    self.assertEqual(report["characters"], [profile])
                    self.assertEqual(report["dialogue_timing"], duration_estimate(prompt))
                    self.assertEqual(report["image_source"]["file"], str(relative_image))
                    self.assertEqual(report["image_source"]["conditioning"], "whole-first-and-last-frame")
                    self.assertEqual(
                        report["image_source"]["anchor_frames"],
                        [0, duration_estimate(prompt)["seconds"] * backend.FPS - 1],
                    )
                    video.characters.verify_snapshots([profile], next(Path(directory).glob("outputs/*")))

    def test_workspace_character_library_overrides_checkout_and_explicit_selector_works(self):
        profile = {"schema_version": 1, "id": "eli", "name": "Eli",
                   "description": "A person in a bright red coat", "voice_description": "A soft voice"}
        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=RuntimeError("captured")) as renderer, patch("sys.stdout", new_callable=io.StringIO):
            folder = Path(directory) / "characters/eli"
            folder.mkdir(parents=True)
            (folder / "character.json").write_text(json.dumps(profile))
            with self.assertRaisesRegex(RuntimeError, "captured"):
                video.generate(42, 'A person says "Hello."', character=["ELI"])
            self.assertIn(profile["description"], renderer.call_args.args[0])
            self.assertIn(profile["voice_description"], renderer.call_args.args[0])
            self.assertNotIn("image_file", renderer.call_args.kwargs)

    def test_multiple_character_images_require_text_only_and_keep_dialogue_timing(self):
        prompt = 'Eli and Ava say "hello there".'
        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=RuntimeError("captured")) as renderer, patch("sys.stdout", new_callable=io.StringIO):
            with self.assertRaisesRegex(ValueError, "Multiple saved characters"):
                video.generate(42, prompt)
            renderer.assert_not_called()
            self.assertFalse((Path(directory) / "outputs").exists())
            with self.assertRaisesRegex(RuntimeError, "captured"):
                video.generate(42, prompt, character_text_only=True)
            self.assertNotIn("image_file", renderer.call_args.kwargs)
            self.assertIn("Character continuity:", renderer.call_args.args[0])
            self.assertEqual(renderer.call_args.args[2], duration_estimate(prompt)["seconds"] * backend.FPS)
            report = json.loads(next(Path(directory).glob("outputs/*/render.json")).read_text())
            self.assertTrue(report["character_text_only"])
            self.assertEqual([profile["id"] for profile in report["characters"]], ["eli", "ava"])

    def test_scene_only_and_portrait_characters_are_both_counted_as_image_references(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=RuntimeError("captured")) as renderer, patch("sys.stdout", new_callable=io.StringIO):
            folder = Path(directory) / "characters/eli"
            folder.mkdir(parents=True)
            scene = b"scene image"
            (folder / "scene-reference.png").write_bytes(scene)
            (folder / "character.json").write_text(json.dumps({
                "schema_version": 1, "id": "eli", "name": "Eli", "description": "An adult",
                "voice_description": "", "scene_image": {"file": "scene-reference.png", "sha256": hashlib.sha256(scene).hexdigest()},
            }))
            ava_folder = Path(directory) / "characters/ava"
            ava_folder.mkdir(parents=True)
            portrait = b"portrait image"
            (ava_folder / "reference.png").write_bytes(portrait)
            (ava_folder / "character.json").write_text(json.dumps({
                "schema_version": 1, "id": "ava", "name": "Ava", "description": "An adult",
                "voice_description": "", "image": {"file": "reference.png", "sha256": hashlib.sha256(portrait).hexdigest()},
            }))
            with self.assertRaisesRegex(ValueError, "Multiple saved characters"):
                video.generate(42, "Eli and Ava wave.")
            renderer.assert_not_called()

    @unittest.skipUnless(importlib.util.find_spec("PIL"), "Pillow is installed in the renderer's .venv")
    def test_reference_image_is_checked_before_loading_the_pipeline(self):
        source = Path(video.__file__).resolve().parents[1] / "characters/eli/reference.png"
        info = backend.prepare_source_image(source)
        self.assertEqual(info["source_file"], str(source))
        self.assertEqual(info["conditioning"], "whole-first-frame")
        self.assertGreater(info["width"], 0)
        self.assertGreater(info["height"], 0)
        with tempfile.TemporaryDirectory() as directory:
            broken = Path(directory) / "broken.png"
            broken.write_bytes(b"not an image")
            with self.assertRaisesRegex(ValueError, "Cannot decode reference image"):
                backend.render("A scene", 42, 24, Path(directory) / "video.mp4", image_file=broken)

    @unittest.skipUnless(importlib.util.find_spec("ltx_pipelines_mlx"), "LTX MLX runtime is installed in the renderer's .venv")
    def test_scene_reference_uses_first_and_last_pipeline_guides_while_portrait_mode_stays_legacy(self):
        source = "/workspace/scene-reference.png"
        self.assertEqual(backend._image_generation_options(source, 192, False), {"image": source})
        held = backend._image_generation_options(source, 192, True)["images"]
        self.assertEqual([image.frame_idx for image in held], [0, 191])
        self.assertEqual([image.path for image in held], [source, source])
        self.assertTrue(held[0].replaces)
        self.assertFalse(held[1].replaces)
        self.assertEqual([image.strength for image in held], [1.0, 1.0])
        single = backend._image_generation_options(source, 1, True)["images"]
        self.assertEqual([image.frame_idx for image in single], [0])

    def test_success_forwards_render_estimates_and_saves_phase_timings(self):
        def render(prompt, seed, frames, output, *, on_progress):
            self.assertEqual(prompt, video.DEFAULT_PROMPT)
            on_progress(label="Stage 2/2 denoising", fraction=0.7, remaining_seconds=120)
            output.write_bytes(b"validated video")
            return {"elapsed_seconds": 400, "phase_seconds": {"stage1": 100, "stage2": 200}}

        with tempfile.TemporaryDirectory() as directory, patch.object(video, "ROOT", Path(directory)), patch.object(video, "check_environment"), patch.object(video, "render", side_effect=render), patch("sys.stdout", new_callable=io.StringIO) as out:
            video.generate(42)
            report = json.loads(next(Path(directory).glob("outputs/*/render.json")).read_text())
            events = [json.loads(line.removeprefix("AIVS_EVENT ")) for line in out.getvalue().splitlines()]
            self.assertEqual(report["status"], "complete")
            self.assertEqual(report["prompt"], video.DEFAULT_PROMPT)
            self.assertEqual(report["characters"], [])
            self.assertNotIn("image_source", report)
            self.assertEqual(report["runtime"]["phase_seconds"], {"stage1": 100, "stage2": 200})
            self.assertEqual(events[-2]["event"], "render_progress")
            self.assertEqual(events[-2]["remaining_seconds"], 120)
            self.assertEqual(events[-1]["event"], "complete")

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
