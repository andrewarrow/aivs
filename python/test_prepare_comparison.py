"""Prompt-only comparison preparation contracts without GPU inference."""

from array import array
import importlib.util
import json
import math
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import wave

import characters
import character_milestone as milestone

ROOT = Path(__file__).resolve().parents[1]
HELPER_PATH = ROOT / "experiments/character-consistency/prepare_comparison.py"
spec = importlib.util.spec_from_file_location("prepare_comparison", HELPER_PATH)
comparison_helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison_helper)


def write_audio(path):
    rate = 24000
    pcm = array("h", (round(5000 * math.sin(2 * math.pi * 440 * i / rate))
                      for i in range(rate)))
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(rate)
        stream.writeframes(pcm.tobytes())


class ComparisonPreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        for name, voice in (("lena", "af_sarah"), ("owen", "am_michael")):
            directory = self.root / "characters" / name
            directory.mkdir(parents=True)
            portrait = directory / "reference.png"
            portrait.write_bytes(f"{name} portrait".encode())
            (directory / "character.json").write_text(json.dumps({
                "schema_version": 1, "id": name, "name": name.title(),
                "description": "An adult performer", "voice_id": voice,
                "image": {"file": "reference.png", "sha256": characters._digest(portrait)},
            }))
        scene = self.root / "scene.png"
        scene.write_bytes(b"same staged scene image")
        portraits = {name: characters.load_character(name, self.root)["image"]["sha256"]
                     for name in ("lena", "owen")}
        self.request = {
            "version": 1,
            "cast": [{"role": "SUE", "character": "lena"}, {"role": "JOE", "character": "owen"}],
            "lines": [{"id": "sue", "role": "SUE", "text": "There is time."},
                      {"id": "joe", "role": "JOE", "text": "Let us go."}],
            "shots": [
                {"line": "sue", "scene": "desert", "prompt": "Original shot prompt.",
                 "image": {"file": "scene.png", "sha256": characters._digest(scene), "source_images": portraits}},
                {"line": "joe", "scene": "cafe", "prompt": "Original second prompt.",
                 "image": {"file": "scene.png", "sha256": characters._digest(scene), "source_images": portraits}},
            ],
        }
        self.request_file = self.root / "request.json"
        self.request_file.write_text(json.dumps(self.request))
        self.source = self.root / "source"
        self.comparison = self.root / "comparison"
        self.real_prepare = milestone.prepare
        milestone.prepare(self.request_file, self.source, character_root=self.root, speech_fn=self.speech)

    def speech(self, profile, text, output):
        write_audio(output)
        return {"sha256": characters._digest(output), "text": text,
                "duration_seconds": 1.0,
                "voice": {"voice_id": profile["voice_id"], "engine": "test"}}

    def _comparison_prepare(self, request_file, run_dir, *, speech_fn):
        return self.real_prepare(request_file, run_dir, character_root=self.root, speech_fn=speech_fn)

    def test_custom_prompt_reuses_saved_pcm_and_image_assets(self):
        request = json.loads(self.request_file.read_text())
        request["shots"][0]["prompt"] = "Use a restrained close-up, with a quiet background."
        custom = self.root / "custom-request.json"
        custom.write_text(json.dumps(request))
        source_manifest = milestone.verify(self.source)
        with patch.object(comparison_helper.milestone, "prepare", side_effect=self._comparison_prepare):
            plan = comparison_helper.prepare_comparison(self.source, self.comparison, request_file=custom)
        self.assertIn("restrained close-up", plan["shots"][0]["prompt"])
        for source_shot, compared_shot in zip(source_manifest["shots"], plan["shots"], strict=True):
            self.assertEqual(source_shot["image"]["sha256"], compared_shot["image"]["sha256"])
            self.assertEqual(source_shot["audio"]["sha256"], compared_shot["audio"]["sha256"])
        for line_id in source_manifest["lines"]:
            self.assertEqual(source_manifest["lines"][line_id]["sha256"], plan["lines"][line_id]["sha256"])
            self.assertEqual(plan["lines"][line_id]["reused_from"]["source_sha256"],
                             source_manifest["source_sha256"])
        self.assertEqual(plan, milestone.verify(self.comparison))

    def test_cast_mismatch_is_rejected_before_creating_a_comparison(self):
        request = json.loads(self.request_file.read_text())
        request["cast"][0]["character"] = "owen"
        custom = self.root / "wrong-cast.json"
        custom.write_text(json.dumps(request))
        with self.assertRaisesRegex(ValueError, "cast does not match"):
            comparison_helper.prepare_comparison(self.source, self.comparison, request_file=custom)
        self.assertFalse(self.comparison.exists())

    def test_source_voice_mismatch_fails_preparation(self):
        manifest_path = self.source / "manifest.json"
        source_manifest = json.loads(manifest_path.read_text())
        source_manifest["lines"]["sue"]["voice"]["voice_id"] = "am_adam"
        payload = {key: value for key, value in source_manifest.items() if key != "source_sha256"}
        source_manifest["source_sha256"] = milestone._fingerprint(payload)
        manifest_path.write_text(json.dumps(source_manifest))
        with patch.object(comparison_helper.milestone, "prepare", side_effect=self._comparison_prepare):
            with self.assertRaisesRegex(ValueError, "different fixed voice"):
                comparison_helper.prepare_comparison(self.source, self.comparison, request_file=self.request_file)
        self.assertEqual(json.loads((self.comparison / "render.json").read_text())["status"], "failed")
        self.assertFalse((self.comparison / "manifest.json").exists())

    def test_late_asset_mismatch_invalidates_manifest_and_marks_report_failed(self):
        def altered_return(request_file, run_dir, *, speech_fn):
            plan = self.real_prepare(request_file, run_dir, character_root=self.root, speech_fn=speech_fn)
            plan["shots"][0]["image"]["sha256"] = "late-mismatch"
            return plan

        with patch.object(comparison_helper.milestone, "prepare", side_effect=altered_return):
            with self.assertRaisesRegex(ValueError, "image hash differs"):
                comparison_helper.prepare_comparison(self.source, self.comparison, request_file=self.request_file)
        self.assertFalse((self.comparison / "manifest.json").exists())
        self.assertTrue((self.comparison / "manifest.failed.json").exists())
        self.assertEqual(json.loads((self.comparison / "render.json").read_text())["status"], "failed")
        with self.assertRaises(FileNotFoundError):
            milestone.verify(self.comparison)


if __name__ == "__main__":
    unittest.main()
