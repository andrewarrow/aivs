"""Offline runner contracts; inference and model loading are injected in tests."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import nullcontext
from unittest.mock import patch
import wave
try:
    import numpy as np
    import soundfile as sf
except ImportError:  # Optional conversion stack is absent from the main MLX environment.
    np = None
    sf = None

ROOT = Path(__file__).resolve().parents[1]
RUNNER_PATH = ROOT / "experiments/character-consistency/run_voice_conversion.py"
spec = importlib.util.spec_from_file_location("run_voice_conversion", RUNNER_PATH)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

def write_wav(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(24000)
        stream.writeframes(b"\x10\x00\xf0\xff" * 2400)


class VoiceConversionRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.run_dir = base / "prepared"
        self._make_prepared_run()
        self.models = base / "models"
        self.models.mkdir()
        self.pinned_config = self.models / "seed/config_dit_mel_seed_uvit_whisper_small_wavenet.yml"
        self.pinned_config.parent.mkdir(parents=True)
        self.pinned_config.write_text("pinned config")
        self.models_manifest = {"version": 1, "code_revision": runner.EXPECTED_REVISION, "models": []}
        (self.models / "models.json").write_text(json.dumps(self.models_manifest))
        self.code = base / "code"
        self.code.mkdir()
        config = self.code / runner.CONFIG_RELATIVE
        config.parent.mkdir(parents=True)
        config.write_text("pinned config")

    def _make_prepared_run(self):
        run = self.run_dir
        run.mkdir()
        chars = run / "characters/lena"
        chars.mkdir(parents=True)
        voice = chars / "voice.wav"
        voice.write_bytes(b"canonical sample")
        profile = {"schema_version": 1, "id": "lena", "name": "Lena", "description": "A performer.",
                   "voice_description": "", "voice_id": "af_sarah",
                   "voice_sample": {"file": "voice.wav", "sha256": runner._hash(voice)}}
        (chars / "character.json").write_text(json.dumps(profile))
        for filename, content in (("source-video.mp4", b"video bytes"), ("source-native-video.mp4", b"native video"),
                                  ("source-audio.wav", b"native audio"),
                                  ("source-render.json", b"{}"), ("source-review-evidence.json", b"{}")):
            (run / filename).write_bytes(content)
        source_manifest_obj = {"character_snapshot": profile}
        source_manifest = run / "source-manifest.json"
        source_manifest.write_text(json.dumps(source_manifest_obj))
        source_hashes = {name: runner._hash(run / name) for name in (
            "source-video.mp4", "source-native-video.mp4", "source-audio.wav", "source-manifest.json", "source-render.json",
            "source-review-evidence.json")}
        # Acceptance anchors the immutable source manifest and render; evidence
        # anchors all four copied source inputs. Write it before the prep manifest
        # so its own digest can be recorded without a cycle.
        evidence = {"inputs": {
            "manifest": {"file": "manifest.json", "sha256": source_hashes["source-manifest.json"]},
            "render": {"file": "render.json", "sha256": source_hashes["source-render.json"]},
            "video": {"file": "video.mp4", "sha256": source_hashes["source-video.mp4"]},
            "native_video": {"file": "video.native.mp4", "sha256": source_hashes["source-native-video.mp4"]},
            "native_audio": {"file": "native-audio.wav", "sha256": source_hashes["source-audio.wav"]}}}
        (run / "source-review-evidence.json").write_text(json.dumps(evidence))
        source_hashes["source-review-evidence.json"] = runner._hash(run / "source-review-evidence.json")
        source_run = "fixture-native-run"
        acceptance = {"status": "accepted-diagnostic-native-take", "milestone_a_passed": True,
                      "fixed_voice_acceptance": False,
                      "human_review": {"status": "accepted", "lip_sync": "accepted",
                                       "face_identity": "accepted", "spoken_words": "accepted"},
                      "source": {"manifest": {"sha256": source_hashes["source-manifest.json"]},
                                 "render_report": {"sha256": source_hashes["source-render.json"]}},
                      "outputs": {
                          "video": {"file": f"{source_run}/video.mp4", "sha256": source_hashes["source-video.mp4"]},
                          "original_native_video": {"file": f"{source_run}/video.native.mp4",
                                                     "sha256": source_hashes["source-native-video.mp4"]},
                          "native_audio": {"file": f"{source_run}/native-audio.wav",
                                           "sha256": source_hashes["source-audio.wav"]}},
                      "automated_observations": {"evidence": {
                          "file": f"{source_run}/review/evidence.json",
                          "sha256": source_hashes["source-review-evidence.json"]}},
                      "visual_review": {"sheets": {}}}
        acceptance_path = run / "acceptance.json"
        acceptance_path.write_text(json.dumps(acceptance))
        input_names = {"source-video.mp4": "source_video_sha256",
                       "source-native-video.mp4": "source_native_video_sha256",
                       "source-audio.wav": "source_audio_sha256",
                       "source-manifest.json": "source_manifest_sha256", "source-render.json": "source_render_sha256",
                       "source-review-evidence.json": "source_evidence_sha256"}
        prepared = {"version": 1, "status": "reference-prepared-needs-quality-review",
                    "fixed_voice_acceptance": False, "source_acceptance_sha256": runner._hash(acceptance_path),
                    "source_run": source_run,
                    "character": {"id": "lena", "voice_id": "af_sarah",
                                  "voice_reference_sha256": runner._hash(voice)},
                    "source_character_voice_sha256": runner._hash(voice),
                    "canonical_reference_path": "characters/lena/voice.wav",
                    "inputs": {name: {"sha256": source_hashes[name]} for name in input_names}}
        prepared.update({key: source_hashes[name] for name, key in input_names.items()})
        (run / "manifest.json").write_text(json.dumps(prepared))
        (run / "source-quality.json").write_text(json.dumps({
            "version": 1, "status": "clean-speech-confirmed", "reviewer": "user",
            "source_video_sha256": source_hashes["source-video.mp4"],
            "source_audio_sha256": source_hashes["source-audio.wav"],
            "question": "Is the native speech clean with quiet around it?",
            "response": "Clean speech with quiet around it", "timestamp": "2026-10-26T12:00:00Z"}))

    def _patch_validation(self):
        return (
            patch.object(runner, "validate_code", return_value={"revision": runner.EXPECTED_REVISION, "clean": True}),
            patch.object(runner, "validate_models", return_value={"seed/checkpoint": {"sha256": "a" * 64}}),
            patch.object(runner, "_capture_code_archive", return_value={"file": "seed-vc-source.tar", "sha256": "b" * 64}),
        )

    def test_prepared_run_rejects_acceptance_source_and_reference_tampering(self):
        acceptance_path = self.run_dir / "acceptance.json"
        acceptance_original = acceptance_path.read_bytes()
        acceptance = json.loads(acceptance_original)
        acceptance["status"] = "rejected"
        acceptance_path.write_text(json.dumps(acceptance))
        with self.assertRaisesRegex(ValueError, "acceptance copy"):
            runner.validate_prepared(self.run_dir)
        acceptance_path.write_bytes(acceptance_original)
        original = (self.run_dir / "source-audio.wav").read_bytes()
        (self.run_dir / "source-audio.wav").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "source file hash"):
            runner.validate_prepared(self.run_dir)
        (self.run_dir / "source-audio.wav").write_bytes(original)
        (self.run_dir / "characters/lena/voice.wav").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "snapshot|reference hash|asset is missing"):
            runner.validate_prepared(self.run_dir)

    def test_source_quality_confirmation_is_required_and_hash_bound(self):
        source_quality = self.run_dir / "source-quality.json"
        original = source_quality.read_bytes()
        source_quality.unlink()
        with self.assertRaisesRegex(ValueError, "source-quality confirmation"):
            runner.validate_prepared(self.run_dir)
        source_quality.write_bytes(original)
        record = json.loads(original)
        record["source_audio_sha256"] = "0" * 64
        source_quality.write_text(json.dumps(record))
        with self.assertRaisesRegex(ValueError, "bound to different source media"):
            runner.validate_prepared(self.run_dir)

    def test_rejects_self_consistent_prep_manifest_with_unanchored_accepted_video(self):
        acceptance_path = self.run_dir / "acceptance.json"
        acceptance = json.loads(acceptance_path.read_text())
        acceptance["outputs"]["video"]["sha256"] = "0" * 64
        acceptance_path.write_text(json.dumps(acceptance))
        manifest_path = self.run_dir / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["source_acceptance_sha256"] = runner._hash(acceptance_path)
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "directly anchor copied video"):
            runner.validate_prepared(self.run_dir)

    @unittest.skipIf(sf is None or np is None, "requires optional Seed-VC soundfile/numpy conversion dependencies")
    def test_conversion_output_is_immutable_and_keeps_review_pending(self):
        with (self.run_dir / "source-video.mp4").open("rb") as stream:
            before_video = stream.read()
        contexts = self._patch_validation()
        with contexts[0], contexts[1], contexts[2], patch.object(runner, "_mps_info", return_value={"device": "mps"}), \
                patch.object(runner, "_run_inference") as fake_inference:
            def convert(code, models, conversion, inputs, **kwargs):
                raw = conversion / "raw" / "generated.wav"
                write_wav(raw)
                config = conversion / "config.local.yml"
                config.write_text("test config")
                return {"raw_output": raw, "elapsed_seconds": 3.0, "config_file": config,
                        "pinned_config": self.pinned_config,
                        "settings": {"diffusion_steps": 30}}
            fake_inference.side_effect = convert
            result = runner.run(self.run_dir, code_dir=self.code, models_dir=self.models,
                                lock_factory=nullcontext)
        self.assertEqual(result["status"], "converted-needs-review")
        self.assertFalse(result["fixed_voice_acceptance"])
        self.assertEqual(result["converted_output"]["sha256"],
                         runner._hash(self.run_dir / "conversion/converted.wav"))
        self.assertEqual((self.run_dir / "source-video.mp4").read_bytes(), before_video)
        self.assertEqual(json.loads((self.run_dir / "conversion-status.json").read_text())["status"],
                         "converted-needs-review")
        with self.assertRaises(FileExistsError):
            runner.run(self.run_dir, code_dir=self.code, models_dir=self.models)

    def test_converter_failure_leaves_terminal_failed_status(self):
        contexts = self._patch_validation()
        with contexts[0], contexts[1], contexts[2], patch.object(runner, "_mps_info", return_value={"device": "mps"}), \
                patch.object(runner, "_run_inference", side_effect=RuntimeError("injected inference failure")):
            with self.assertRaisesRegex(RuntimeError, "injected inference"):
                runner.run(self.run_dir, code_dir=self.code, models_dir=self.models,
                           lock_factory=nullcontext)
        status = json.loads((self.run_dir / "conversion-status.json").read_text())
        self.assertEqual(status["status"], "failed")
        self.assertFalse(status["fixed_voice_acceptance"])
        self.assertIn("injected inference failure", status["error"])

    def test_source_audio_mutation_during_inference_fails_and_records_status(self):
        contexts = self._patch_validation()
        with contexts[0], contexts[1], contexts[2], patch.object(runner, "_mps_info", return_value={"device": "mps"}), \
                patch.object(runner, "_run_inference") as fake_inference:
            def mutate(*args, **kwargs):
                (self.run_dir / "source-audio.wav").write_bytes(b"mutated during converter")
                raw = self.run_dir / "conversion/raw/result.wav"
                write_wav(raw)
                config = self.run_dir / "conversion/config.local.yml"
                config.write_text("test config")
                return {"raw_output": raw, "elapsed_seconds": 1, "config_file": config,
                        "pinned_config": self.pinned_config, "settings": {}}
            fake_inference.side_effect = mutate
            with self.assertRaisesRegex(ValueError, "changed during inference"):
                runner.run(self.run_dir, code_dir=self.code, models_dir=self.models,
                           lock_factory=nullcontext)
        self.assertEqual(json.loads((self.run_dir / "conversion-status.json").read_text())["status"], "failed")

    def test_models_manifest_mutation_during_inference_is_rejected(self):
        contexts = self._patch_validation()
        with contexts[0], contexts[1], contexts[2], patch.object(runner, "_mps_info", return_value={"device": "mps"}), \
                patch.object(runner, "_run_inference") as fake_inference:
            def mutate(*args, **kwargs):
                (self.models / "models.json").write_text('{"changed": true}')
                raw = self.run_dir / "conversion/raw/result.wav"
                write_wav(raw)
                config = self.run_dir / "conversion/config.local.yml"
                config.write_text("test config")
                return {"raw_output": raw, "elapsed_seconds": 1, "config_file": config,
                        "pinned_config": self.pinned_config, "settings": {}}
            fake_inference.side_effect = mutate
            with self.assertRaisesRegex(ValueError, "models.json changed during inference"):
                runner.run(self.run_dir, code_dir=self.code, models_dir=self.models,
                           lock_factory=nullcontext)
        self.assertEqual(json.loads((self.run_dir / "conversion-status.json").read_text())["status"], "failed")

    def test_rejects_dirty_or_wrong_revision_and_model_hash_mismatch(self):
        with patch.object(runner.subprocess, "check_output", side_effect=[runner.EXPECTED_REVISION + "\n", " M dirty\n"]):
            with self.assertRaisesRegex(ValueError, "clean at"):
                runner.validate_code(self.code, self.models_manifest)
        with patch.object(runner.subprocess, "check_output", side_effect=["bad-revision\n", ""]):
            with self.assertRaisesRegex(ValueError, "clean at"):
                runner.validate_code(self.code, self.models_manifest)
        for name, (repo_id, filenames) in runner.REQUIRED_MODELS.items():
            records = {filename: "0" * 64 for filename in filenames}
            self.models_manifest["models"].append({"name": name, "repo_id": repo_id,
                                                   "revision": "pinned", "files": records})
            for filename in filenames:
                path = self.models / name / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"model")
        with self.assertRaisesRegex(ValueError, "matching pinned hash"):
            runner.validate_models(self.models, self.models_manifest)

    @unittest.skipIf(sf is None or np is None, "requires optional Seed-VC soundfile/numpy conversion dependencies")
    def test_float_output_gets_pcm16_derivative_without_normalization(self):
        raw = Path(self.temp.name) / "float.wav"
        converted = Path(self.temp.name) / "converted.wav"
        values = np.linspace(-0.5, 0.5, 128, dtype=np.float32)
        sf.write(raw, values, 24000, subtype="FLOAT")
        before = runner._hash(raw)
        provenance = runner._materialize_pcm16(raw, converted)
        self.assertEqual(provenance["source_subtype"], "FLOAT")
        self.assertEqual(provenance["prequantization_peak"], 0.5)
        self.assertEqual(sf.info(converted).subtype, "PCM_16")
        converted_values, _ = sf.read(converted, dtype="float64")
        self.assertAlmostEqual(float(np.max(np.abs(converted_values))), 0.5, delta=1 / 32768)
        self.assertEqual(runner._hash(raw), before)

    @unittest.skipIf(sf is None or np is None, "requires optional Seed-VC soundfile/numpy conversion dependencies")
    def test_float_output_rejects_nonfinite_silent_and_out_of_range_samples(self):
        for name, values, expected in (
            ("nan", np.array([0.1, np.nan], dtype=np.float32), "non-finite"),
            ("silent", np.zeros(16, dtype=np.float32), "silent"),
            ("hot", np.array([0.2, 1.01], dtype=np.float32), "exceeds PCM full scale"),
        ):
            with self.subTest(name=name):
                raw = Path(self.temp.name) / f"{name}.wav"
                converted = Path(self.temp.name) / f"{name}-converted.wav"
                sf.write(raw, values, 24000, subtype="FLOAT")
                with self.assertRaisesRegex(ValueError, expected):
                    runner._materialize_pcm16(raw, converted)
                self.assertFalse(converted.exists())


if __name__ == "__main__":
    unittest.main()
