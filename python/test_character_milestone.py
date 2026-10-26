"""Character conditioning contracts using real WAV files and no GPU inference."""

from array import array
from contextlib import nullcontext
import importlib.util
import io
import json
import math
from pathlib import Path
import shutil
import subprocess
import tempfile
import types
import unittest
from unittest.mock import Mock, patch
import wave

import character_milestone as milestone
import characters
import quality_backend as backend
import quality_progress
import video


def write_audio(path, seconds=1, frequency=440, rate=24000):
    pcm = array("h", (round(5000 * math.sin(2 * math.pi * frequency * i / rate))
                      for i in range(round(rate * seconds))))
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(rate)
        stream.writeframes(pcm.tobytes())


class MilestoneTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        for name, voice in [("lena", "af_sarah"), ("owen", "am_michael")]:
            directory = self.root / "characters" / name
            directory.mkdir(parents=True)
            portrait = directory / "reference.png"
            portrait.write_bytes(name.encode())
            (directory / "character.json").write_text(json.dumps({
                "schema_version": 1, "id": name, "name": name.title(),
                "description": "An adult performer", "voice_id": voice,
                "image": {"file": "reference.png", "sha256": characters._digest(portrait)},
            }))
        image = self.root / "scene.png"
        image.write_bytes(b"staged scene image")
        self.request = {"version": 1, "cast": [{"role": "SUE", "character": "lena"},
                                               {"role": "JOE", "character": "owen"}],
                        "lines": [{"id": "sue", "role": "SUE", "text": "There is time."},
                                  {"id": "joe", "role": "JOE", "text": "Let us go."}], "shots": []}
        sources = {name: characters.load_character(name, self.root)["image"]["sha256"]
                   for name in ["lena", "owen"]}
        for scene, line in [("desert", "sue"), ("desert", "joe"), ("cafe", "sue"), ("cafe", "joe")]:
            self.request["shots"].append({"line": line, "scene": scene, "prompt": "Deliver the supplied line.",
                                           "image": {"file": "scene.png", "sha256": characters._digest(image),
                                                     "source_images": sources}})
        self.request_file = self.root / "request.json"
        self.run = self.root / "run"
        self.calls = []

    def speech(self, profile, text, output):
        self.calls.append((profile["id"], text))
        write_audio(output)
        return {"sha256": characters._digest(output), "text": text, "duration_seconds": 1.0,
                "voice": {"voice_id": profile["voice_id"], "engine": "test"}}

    def prepare(self):
        self.request_file.write_text(json.dumps(self.request))
        with patch("sys.stdout", new_callable=io.StringIO):
            return milestone.prepare(self.request_file, self.run, character_root=self.root, speech_fn=self.speech)

    def test_preparation_reuses_exact_performer_audio_across_scenes_with_measured_pauses(self):
        plan = self.prepare()
        self.assertEqual([c[0] for c in self.calls], ["lena", "owen"])
        self.assertEqual(plan, milestone.verify(self.run))
        first, _, third, _ = plan["shots"]
        self.assertEqual(first["audio"]["sha256"], third["audio"]["sha256"])
        self.assertEqual(first["speaker"], {"role": "SUE", "character": "lena", "start_seconds": 0.35, "end_seconds": 1.35})
        self.assertEqual(first["frames"], 96)
        self.assertIn('says, "There is time."', first["prompt"])
        with wave.open(str(self.run / first["audio"]["file"]), "rb") as stream:
            self.assertEqual(stream.getnframes() / stream.getframerate(), 4)
            self.assertEqual(stream.readframes(8400), b"\0\0" * 8400)
            raw = stream.readframes(24000)
        with wave.open(str(self.run / plan["lines"]["sue"]["file"]), "rb") as stream:
            self.assertEqual(raw, stream.readframes(24000))

    def test_wrong_portrait_provenance_fails_before_synthesis_or_creating_run(self):
        self.request["shots"][0]["image"]["source_images"] = {"lena": "wrong"}
        with self.assertRaisesRegex(ValueError, "different character portraits"):
            self.prepare()
        self.assertEqual(self.calls, [])
        self.assertFalse(self.run.exists())

    def test_wrong_voice_is_recorded_as_failed_without_falling_back(self):
        original = self.speech
        def wrong_voice(*args):
            result = original(*args)
            result["voice"]["voice_id"] = "am_adam"
            return result
        self.speech = wrong_voice
        with self.assertRaisesRegex(ValueError, "selected character"):
            self.prepare()
        self.assertEqual(json.loads((self.run / "render.json").read_text())["status"], "failed")
        self.assertFalse((self.run / "manifest.json").exists())

    def test_prepared_assets_and_manifest_cannot_change_before_render(self):
        plan = self.prepare()
        image = self.run / plan["shots"][0]["image"]["file"]
        image.write_bytes(b"a different face")
        with self.assertRaisesRegex(ValueError, "Asset changed"):
            milestone.verify(self.run)
        plan["shots"][0]["prompt"] = "A different scene."
        (self.run / "manifest.json").write_text(json.dumps(plan))
        with self.assertRaisesRegex(ValueError, "manifest has changed"):
            milestone.verify(self.run)

    def test_existing_gui_lock_prevents_inference_without_altering_prepared_report(self):
        self.prepare()
        before = (self.run / "render.json").read_bytes()
        renderer = Mock()
        with patch.object(video, "ROOT", self.root), video.project_lock(".render.lock"):
            with self.assertRaisesRegex(RuntimeError, "Another AIVS"):
                milestone.render(self.run, render_fn=renderer)
        renderer.assert_not_called()
        self.assertEqual(before, (self.run / "render.json").read_bytes())

    def test_render_forwards_both_assets_and_resumes_only_matching_takes(self):
        plan = self.prepare()
        calls = []
        def renderer(prompt, seed, frames, output, **kwargs):
            calls.append((prompt, seed, frames, kwargs))
            output.write_bytes(b"video")
            return {"audio": "supplied"}
        with patch.object(video, "ROOT", self.root):
            report = milestone.render(self.run, seed=83, shot_number=1, render_fn=renderer)
            milestone.render(self.run, seed=83, shot_number=1, render_fn=renderer)
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][1:3], (83, 96))
            self.assertEqual(calls[0][3]["audio_file"], self.run / plan["shots"][0]["audio"]["file"])
            self.assertEqual(calls[0][3]["image_file"], self.run / plan["shots"][0]["image"]["file"])
            self.assertEqual((calls[0][3]["width"], calls[0][3]["height"]), (768, 448))
            self.assertEqual(report["review"]["status"], "pending")
            self.assertEqual(report["status"], "partially-rendered")
            with self.assertRaisesRegex(ValueError, "Existing take differs"):
                milestone.render(self.run, seed=84, shot_number=1, render_fn=renderer)

    def test_later_failure_preserves_finished_take_and_failure_reports(self):
        self.prepare()
        def renderer(prompt, seed, frames, output, **kwargs):
            if output.parent.name == "002":
                raise RuntimeError("GPU error")
            output.write_bytes(b"video")
            return {}
        with patch.object(video, "ROOT", self.root):
            with self.assertRaisesRegex(RuntimeError, "GPU error"):
                milestone.render(self.run, render_fn=renderer)
        self.assertEqual(json.loads((self.run / "shots/001/render.json").read_text())["status"], "rendered-needs-review")
        self.assertEqual(json.loads((self.run / "shots/002/render.json").read_text())["status"], "failed")
        self.assertEqual(json.loads((self.run / "render.json").read_text())["status"], "failed")
        self.assertFalse((self.run / "video.mp4").exists())

    def test_asset_paths_cannot_escape_the_request_directory(self):
        self.request["shots"][0]["image"]["file"] = "../outside.png"
        with self.assertRaisesRegex(ValueError, "outside its directory"):
            self.prepare()


class FixedAudioBackendTests(unittest.TestCase):
    def test_short_or_broken_audio_is_rejected_before_loading_the_video_model(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(backend, "model_path") as model:
            root = Path(directory)
            path = root / "speech.wav"
            write_audio(path, seconds=1)
            with self.assertRaisesRegex(ValueError, "shorter"):
                backend.render("Scene", 1, 96, root / "video.mp4", audio_file=path)
            path.write_bytes(b"broken WAV")
            with self.assertRaisesRegex(ValueError, "Cannot read fixed audio"):
                backend.render("Scene", 1, 96, root / "video.mp4", audio_file=path)
            model.assert_not_called()

    @unittest.skipUnless(shutil.which("ffmpeg") and importlib.util.find_spec("PIL"), "FFmpeg and Pillow are required")
    def test_real_mux_keeps_supplied_voice_and_native_mode_still_uses_generated_audio(self):
        # Fake only GPU generation. Real FFmpeg handles causal-grid padding,
        # final audio selection, frame trim, AAC encoding and media validation.
        class Progress:
            def __init__(self, **kwargs): pass
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def phase(self, *args): return nullcontext()
            def log(self, *args, **kwargs): pass
        calls = []
        class Pipeline:
            def __init__(self, **kwargs): pass
            def generate_and_save(self, **kwargs):
                calls.append((type(self).__name__, self.generate_audio, kwargs))
                subprocess.run([
                    "ffmpeg", "-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i",
                    f"color=blue:s={kwargs['width']}x{kwargs['height']}:r=24:d=5",
                    "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=5",
                    "-frames:v", str(kwargs["num_frames"]), "-c:v", "libx264", "-c:a", "aac",
                    kwargs["output_path"],
                ], check=True, capture_output=True)
        class AudioPipeline(Pipeline): pass
        mx = types.SimpleNamespace(reset_peak_memory=lambda: None, get_peak_memory=lambda: 0)
        modules = {"mlx.core": mx,
                   "ltx_pipelines_mlx.distilled": types.SimpleNamespace(DistilledPipeline=Pipeline),
                   "ltx_pipelines_mlx.a2vid_distilled": types.SimpleNamespace(A2VidDistilledPipeline=AudioPipeline)}
        with tempfile.TemporaryDirectory() as directory, patch.dict("sys.modules", modules), \
                patch.object(quality_progress, "RenderProgress", Progress), \
                patch.object(quality_progress, "pipeline_activity", lambda *args: nullcontext()), \
                patch.object(backend, "model_path", return_value=Path(directory)), \
                patch("importlib.metadata.version", return_value="test"):
            root = Path(directory)
            source = root / "speech.wav"
            write_audio(source, seconds=4)
            for supplied, frequency in [(True, 440), (False, 880)]:
                with self.subTest(supplied=supplied):
                    output = root / f"{supplied}.mp4"
                    result = backend.render("A person speaks.", 83, 96, output, width=128, height=64,
                                            audio_file=source if supplied else None)
                    self.assertEqual(result["audio"], "supplied" if supplied else "generated")
                    self.assertEqual(calls[-1][0], "AudioPipeline" if supplied else "Pipeline")
                    self.assertEqual(calls[-1][1], not supplied)
                    self.assertEqual(result["video"]["nb_read_frames"], "96")
                    if supplied:
                        with wave.open(calls[-1][2]["audio_path"], "rb") as stream:
                            self.assertGreaterEqual(stream.getnframes() / stream.getframerate(), 97 / 24)
                    decoded = subprocess.run([
                        "ffmpeg", "-v", "error", "-i", str(output), "-ss", "0.5", "-t", "1",
                        "-f", "s16le", "-ac", "1", "-ar", "24000", "pipe:1",
                    ], check=True, capture_output=True).stdout
                    samples = array("h", decoded)
                    crossings = sum(a < 0 <= b for a, b in zip(samples, samples[1:]))
                    self.assertAlmostEqual(crossings, frequency, delta=2)


if __name__ == "__main__":
    unittest.main()
