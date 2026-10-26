import json
from contextlib import nullcontext
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch
import wave

import native_reference as native


ROOT = Path(__file__).resolve().parents[1]
REQUEST = ROOT / "experiments/character-consistency/request.json"


class NativeReferenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.request_file = self._inputs()

    def _inputs(self):
        input_dir = self.root / "input"
        input_dir.mkdir(exist_ok=True)
        request = json.loads(REQUEST.read_text(encoding="utf-8"))
        for record in (request["shots"][0]["image"], request["reference_provenance"]):
            source = REQUEST.parent / record["file"]
            target = input_dir / record["file"]
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        request_file = input_dir / "request.json"
        request_file.write_text(json.dumps(request), encoding="utf-8")
        return request_file

    def _success_mocks(self):
        calls = {}

        def render(prompt, seed, frames, output, **kwargs):
            calls.update(prompt=prompt, seed=seed, frames=frames, output=Path(output), kwargs=kwargs)
            Path(output).write_bytes(b"final-video")
            Path(output).with_name("video.native.mp4").write_bytes(b"native-video")
            return {"pipeline": "distilled-two-stage"}

        def audio(video_path, wav_path):
            with wave.open(str(wav_path), "wb") as stream:
                stream.setnchannels(2)
                stream.setsampwidth(2)
                stream.setframerate(48000)
                stream.writeframes(b"\0" * (48000 * 4 * 2 * 2))
            return {"file": wav_path.name, "sha256": native._sha256(wav_path),
                    "sample_rate": 48000, "channels": 2, "encoding": "PCM_S16LE",
                    "duration_seconds": 4.0,
                    "derived_from": {"file": video_path.name, "sha256": native._sha256(video_path),
                                     "stream": "0:a:0", "trim_seconds": 4}}

        return calls, render, audio

    def _run(self, *, render_fn=None, run_dir=None, request_file=None):
        calls, render, audio = self._success_mocks()
        with patch.object(native.quality_backend, "doctor"), \
                patch.object(native, "_extract_native_audio", audio), \
                patch.object(native, "_final_video_info", return_value={"codec_name": "h264"}):
            report = native.run(run_dir or self.root / "run", request_file=request_file or self.request_file,
                                character_root=ROOT, render_fn=render_fn or render,
                                lock_factory=lambda _name: nullcontext())
        return report, run_dir or self.root / "run", calls

    def test_native_configuration_prompt_snapshot_and_provenance(self):
        report, run_dir, calls = self._run()

        self.assertEqual(report["status"], "rendered-needs-review")
        self.assertEqual(report["label"], "diagnostic-native-voice")
        self.assertFalse(report["fixed_voice_acceptance"])
        self.assertEqual(report["review"]["spoken_words"], "pending")
        self.assertEqual((calls["seed"], calls["frames"]), (83, 96))
        self.assertEqual(calls["kwargs"], {
            "image_file": (run_dir / "start.png").resolve(), "hold_image_framing": False,
            "width": 1280, "height": 704, "keep_native": True,
        })
        self.assertNotIn("audio_file", calls["kwargs"])
        prompt = (run_dir / "prompt.txt").read_text()
        self.assertIn("We still have some time.", prompt)
        self.assertIn("quiet moment at the beginning and end", prompt)
        self.assertEqual((run_dir / "video.native.mp4").read_bytes(), b"native-video")
        self.assertTrue((run_dir / "characters/lena/reference.png").is_file())
        self.assertTrue((run_dir / "canonical-portrait.png").is_file())
        manifest = json.loads((run_dir / "manifest.json").read_text())
        provenance = manifest["source"]["reference_provenance"]
        self.assertEqual(provenance["sha256"], native._sha256(run_dir / provenance["file"]))
        self.assertEqual(manifest["source"]["starting_image"]["sha256"],
                         native._sha256(run_dir / "start.png"))
        self.assertTrue(manifest["source"]["git_head"])
        self.assertEqual(report["native_audio"]["derived_from"]["file"], "video.mp4")

    def test_source_portrait_mismatch_fails_before_creating_run(self):
        request = json.loads(self.request_file.read_text())
        request["shots"][0]["image"]["source_images"]["lena"] = "0" * 64
        self.request_file.write_text(json.dumps(request))
        run_dir = self.root / "mismatch-run"
        with self.assertRaisesRegex(ValueError, "different Lena portrait"):
            native.run(run_dir, request_file=self.request_file, character_root=ROOT,
                       render_fn=lambda *args, **kwargs: self.fail("render must not run"),
                       lock_factory=lambda _name: nullcontext())
        self.assertFalse(run_dir.exists())

    def test_existing_run_is_never_overwritten(self):
        run_dir = self.root / "existing"
        run_dir.mkdir()
        marker = run_dir / "keep.txt"
        marker.write_text("original")
        with self.assertRaises(FileExistsError):
            native.run(run_dir, request_file=self.request_file, character_root=ROOT,
                       render_fn=lambda *args, **kwargs: self.fail("render must not run"),
                       lock_factory=lambda _name: nullcontext())
        self.assertEqual(marker.read_text(), "original")

    def test_lock_contention_does_not_create_run_or_render(self):
        class Contended:
            def __enter__(inner_self):
                raise RuntimeError("lock busy")

            def __exit__(inner_self, *args):
                return False

        run_dir = self.root / "locked-run"
        with self.assertRaisesRegex(RuntimeError, "lock busy"):
            native.run(run_dir, request_file=self.request_file, character_root=ROOT,
                       render_fn=lambda *args, **kwargs: self.fail("render must not run"),
                       lock_factory=lambda _name: Contended())
        self.assertFalse(run_dir.exists())

    def test_render_failures_are_recorded(self):
        for exception, status in ((RuntimeError("bad render"), "failed"),
                                  (KeyboardInterrupt(), "interrupted")):
            with self.subTest(status=status), patch.object(native.quality_backend, "doctor"):
                run_dir = self.root / status

                def render(*args, _exception=exception, **kwargs):
                    raise _exception

                with self.assertRaises(type(exception)):
                    native.run(run_dir, request_file=self.request_file, character_root=ROOT,
                               render_fn=render, lock_factory=lambda _name: nullcontext())
                report = json.loads((run_dir / "render.json").read_text())
                self.assertEqual(report["status"], status)
                self.assertEqual(report["error"], str(exception) or type(exception).__name__)


if __name__ == "__main__":
    unittest.main()
