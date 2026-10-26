"""Offline contracts for accepted character voice-reference preparation."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch
import wave


REPO = Path(__file__).resolve().parents[1]
HELPER_PATH = REPO / "experiments/character-consistency/prepare_voice_reference.py"
spec = importlib.util.spec_from_file_location("prepare_voice_reference", HELPER_PATH)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def write_wav(path, samples):
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(1000)
        stream.writeframes(b"".join(int(sample).to_bytes(2, "little", signed=True)
                                      for sample in samples))


class VoiceReferencePreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = (Path(self.temp.name) / "repo").resolve()
        self.run_rel = "outputs/synthetic-accepted-run"
        self.source = self.root / self.run_rel
        self.out = self.root / "outputs/prepared"
        self.acceptance_path = self.root / "experiments/character-consistency/acceptance.json"
        self._create_fixture()
        self.root_patch = patch.object(helper, "ROOT", self.root)
        self.sources_patch = patch.dict(helper.SOURCE_FILES, {
            name: self._source_code(name) for name in helper.SOURCE_FILES
        }, clear=True)
        self.root_patch.start()
        self.sources_patch.start()
        self.addCleanup(self.root_patch.stop)
        self.addCleanup(self.sources_patch.stop)

    def _source_code(self, name):
        path = self.root / "fixture-source" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"synthetic source: {name}\n", encoding="utf-8")
        return path

    def _create_fixture(self, character="lena", voice_id=None):
        assets = self.root / "characters" / character
        assets.mkdir(parents=True, exist_ok=True)
        (assets / "reference.png").write_bytes(b"synthetic portrait asset")
        (assets / "scene-reference.png").write_bytes(b"synthetic scene asset")
        write_wav(assets / "voice.wav", [1000, -1000, 2000, -2000] * 500)
        self.profile = {
            "schema_version": 1,
            "id": character,
            "name": character.title(),
            "description": f"Synthetic {character.title()} profile for an offline test.",
            "voice_description": "Clear, even voice.",
            "voice_id": voice_id or {"lena": "af_sarah", "owen": "am_michael"}[character],
            "image": {"file": "reference.png", "sha256": digest(assets / "reference.png")},
            "scene_image": {"file": "scene-reference.png", "sha256": digest(assets / "scene-reference.png")},
            "voice_sample": {"file": "voice.wav", "sha256": digest(assets / "voice.wav")},
        }
        write_json(assets / "character.json", self.profile)

        run_chars = self.source / "characters" / character
        run_chars.mkdir(parents=True, exist_ok=True)
        for asset in ("reference.png", "scene-reference.png", "voice.wav"):
            (run_chars / asset).write_bytes((assets / asset).read_bytes())
        write_json(run_chars / "character.json", self.profile)

        (self.source / "video.mp4").write_bytes(b"synthetic accepted final video bytes")
        (self.source / "video.native.mp4").write_bytes(b"synthetic native video bytes")
        write_wav(self.source / "native-audio.wav", [1500, -1500, 2400, -2400] * 500)
        manifest = {"character_id": character, "character_snapshot": self.profile,
                    "fixed_voice_acceptance": False}
        write_json(self.source / "manifest.json", manifest)
        render = {
            "fixed_voice_acceptance": False,
            "video": {"file": "video.mp4", "sha256": digest(self.source / "video.mp4")},
            "native_video": {"file": "video.native.mp4", "sha256": digest(self.source / "video.native.mp4")},
            "native_audio": {"file": "native-audio.wav", "sha256": digest(self.source / "native-audio.wav")},
        }
        write_json(self.source / "render.json", render)
        review = self.source / "review"
        review.mkdir(exist_ok=True)
        evidence = {"inputs": {
            name: {"file": filename, "sha256": digest(self.source / filename)}
            for name, filename in {
                "manifest": "manifest.json", "render": "render.json", "video": "video.mp4",
                "native_video": "video.native.mp4", "native_audio": "native-audio.wav",
            }.items()
        }}
        write_json(review / "evidence.json", evidence)
        outputs = {
            key: {"file": f"{self.run_rel}/{filename}", "sha256": digest(self.source / filename)}
            for key, filename in {
                "video": "video.mp4", "original_native_video": "video.native.mp4",
                "native_audio": "native-audio.wav",
            }.items()
        }
        acceptance = {
            "status": "accepted-diagnostic-native-take",
            "fixed_voice_acceptance": False,
            "run": self.run_rel,
            "source": {
                "manifest": {"file": f"{self.run_rel}/manifest.json", "sha256": digest(self.source / "manifest.json")},
                "render_report": {"file": f"{self.run_rel}/render.json", "sha256": digest(self.source / "render.json")},
                "git_head": "synthetic",
            },
            "outputs": outputs,
            "automated_observations": {
                "evidence": {"file": f"{self.run_rel}/review/evidence.json",
                             "sha256": digest(review / "evidence.json")},
            },
            "human_review": {"status": "accepted", "lip_sync": "accepted",
                             "face_identity": "accepted", "spoken_words": "accepted"},
            "visual_review": {"sheets": {}},
        }
        if character == "lena":
            acceptance["milestone_a_passed"] = True
        else:
            acceptance.update(character_id=character, native_take_accepted=True)
        write_json(self.acceptance_path, acceptance)

    def _acceptance(self):
        return json.loads(self.acceptance_path.read_text(encoding="utf-8"))

    def _save_acceptance(self, value):
        write_json(self.acceptance_path, value)

    def test_copies_hash_bound_accepted_assets_and_snapshots_voice(self):
        manifest = helper.prepare_voice_reference(self.acceptance_path, self.out)
        for source_name, output_name in (("video.mp4", "source-video.mp4"),
                                         ("video.native.mp4", "source-native-video.mp4"),
                                         ("native-audio.wav", "source-audio.wav")):
            self.assertEqual((self.out / output_name).read_bytes(), (self.source / source_name).read_bytes())
        self.assertEqual((self.out / "acceptance.json").read_bytes(), self.acceptance_path.read_bytes())
        self.assertEqual(manifest["status"], "reference-prepared-needs-quality-review")
        self.assertFalse(manifest["fixed_voice_acceptance"])
        self.assertEqual(manifest["character"]["voice_id"], "af_sarah")
        self.assertEqual(manifest["character"]["voice_reference_sha256"],
                         digest(self.out / "characters/lena/voice.wav"))
        self.assertEqual(manifest["inputs"]["source-native-video.mp4"]["sha256"],
                         digest(self.out / "source-native-video.mp4"))
        self.assertTrue((self.out / "source/prepare_voice_reference.py").is_file())
        (self.source / "video.mp4").write_bytes(b"later source mutation")
        self.assertEqual((self.out / "source-video.mp4").read_bytes(), b"synthetic accepted final video bytes")

    def test_owen_uses_accepted_performer_and_canonical_voice_without_milestone_a(self):
        self._create_fixture("owen")
        self.assertNotIn("milestone_a_passed", self._acceptance())
        manifest = helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertEqual(manifest["character"]["id"], "owen")
        self.assertEqual(manifest["character"]["voice_id"], "am_michael")
        self.assertEqual(manifest["canonical_reference_path"], "characters/owen/voice.wav")
        self.assertEqual(manifest["character"]["voice_reference_sha256"],
                         digest(self.out / "characters/owen/voice.wav"))
        self.assertEqual((self.out / "characters/owen/voice.wav").read_bytes(),
                         (self.source / "characters/owen/voice.wav").read_bytes())
        self.assertFalse((self.out / "characters/lena").exists())
        self.assertFalse(manifest["fixed_voice_acceptance"])

    def test_rejects_owen_wrong_acceptance_performer_or_missing_native_approval(self):
        self._create_fixture("owen")
        original = self._acceptance()
        mutations = {
            "wrong performer": lambda value: value.update(character_id="lena"),
            "pending take": lambda value: value.update(status="rendered-needs-review"),
            "unaccepted take": lambda value: value.update(native_take_accepted=False),
            "legacy approval for Owen": lambda value: value.update(native_take_accepted=False, milestone_a_passed=True),
            "pending lip sync": lambda value: value["human_review"].update(lip_sync="pending"),
            "pending human review": lambda value: value["human_review"].update(status="pending"),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label):
                acceptance = copy.deepcopy(original)
                mutate(acceptance)
                self._save_acceptance(acceptance)
                with self.assertRaises(ValueError):
                    helper.prepare_voice_reference(self.acceptance_path, self.out)
                self.assertFalse(self.out.exists())

    def test_rejects_mismatched_owen_manifest_snapshot_with_updated_acceptance_hash(self):
        self._create_fixture("owen")
        manifest_path = self.source / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["character_snapshot"]["id"] = "lena"
        write_json(manifest_path, manifest)
        acceptance = self._acceptance()
        acceptance["source"]["manifest"]["sha256"] = digest(manifest_path)
        self._save_acceptance(acceptance)
        with self.assertRaisesRegex(ValueError, "same supported performer"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

    def test_rejects_owen_wrong_voice_even_when_current_profile_matches_snapshot(self):
        self._create_fixture("owen", voice_id="af_sarah")
        with self.assertRaisesRegex(ValueError, "expected voice ID am_michael"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

    def test_rejects_replaced_evidence_and_acceptance_output_mismatch(self):
        evidence = self.source / "review/evidence.json"
        evidence.write_text('{"inputs": {}}\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "hash does not match"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

        shutil.rmtree(self.root)
        self._create_fixture()
        evidence_path = self.source / "review/evidence.json"
        evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
        evidence["inputs"]["video"]["sha256"] = digest(self.source / "native-audio.wav")
        write_json(evidence_path, evidence)
        acceptance = self._acceptance()
        acceptance["automated_observations"]["evidence"]["sha256"] = digest(evidence_path)
        self._save_acceptance(acceptance)
        with self.assertRaisesRegex(ValueError, "source video hash does not match"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

        shutil.rmtree(self.root)
        self._create_fixture()
        acceptance = self._acceptance()
        acceptance["outputs"]["video"]["sha256"] = "0" * 64
        self._save_acceptance(acceptance)
        with self.assertRaisesRegex(ValueError, "hash does not match"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

    def test_rejects_missing_original_native_video(self):
        (self.source / "video.native.mp4").unlink()
        with self.assertRaisesRegex(ValueError, "Missing or unsafe source native_video|Missing or unsafe accepted native_video"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

    def test_rejects_changed_current_profile_and_source_snapshot_reference(self):
        load = helper.characters.load_character

        def changed_profile(name, root=None):
            profile = load(name, root)
            profile["voice_id"] = "af_nicole"
            return profile

        with patch.object(helper.characters, "load_character", side_effect=changed_profile):
            with self.assertRaisesRegex(ValueError, "Current Lena profile differs"):
                helper.prepare_voice_reference(self.acceptance_path, self.out)

        source_voice = self.source / "characters/lena/voice.wav"
        source_voice.write_bytes(b"changed reference")
        with self.assertRaisesRegex(ValueError, "Source voice reference does not match|Voice sample asset is missing or has changed"):
            helper.prepare_voice_reference(self.acceptance_path, self.out)
        self.assertFalse(self.out.exists())

    def test_pcm_stats_flag_silence_and_full_scale_without_mutating_wav(self):
        path = self.root / "edge.wav"
        write_wav(path, [0] * 20 + [32767, -32768] * 10 + [0] * 20)
        original = path.read_bytes()
        stats = helper._wav_stats(path)
        self.assertEqual(stats["clipped_sample_count"], 20)
        self.assertIn("pcm16-full-scale-samples", stats["quality_issues"])
        self.assertGreater(stats["activity"]["active_windows"], 0)
        self.assertEqual(path.read_bytes(), original)
        silent = self.root / "silent.wav"
        write_wav(silent, [0] * 10)
        self.assertIn("silent", helper._wav_stats(silent)["quality_issues"])

    def test_pcm_stats_reject_non_pcm16_and_truncated_data(self):
        malformed = self.root / "malformed.wav"
        malformed.write_bytes(b"not a wav")
        with self.assertRaisesRegex(ValueError, "Malformed PCM16 WAV"):
            helper._wav_stats(malformed)


if __name__ == "__main__":
    unittest.main()
