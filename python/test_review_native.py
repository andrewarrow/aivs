import hashlib
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "experiments/character-consistency/review_native.py"
spec = importlib.util.spec_from_file_location("review_native", SCRIPT)
review = importlib.util.module_from_spec(spec)
spec.loader.exec_module(review)


def _fingerprint(manifest):
    source = {key: value for key, value in manifest.items() if key != "source_fingerprint"}
    encoded = json.dumps(source, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _valid_run(character):
    role, line_id, dialogue = review.BINDINGS[character]
    manifest = {
        "label": "diagnostic-native-voice",
        "fixed_voice_acceptance": False,
        "seed": 83,
        "frames": 96,
        "fps": 24,
        "width": 1280,
        "height": 704,
        "character_id": character,
        "role": role,
        "line_id": line_id,
        "dialogue": dialogue,
        "character_snapshot": {"id": character},
    }
    manifest["source_fingerprint"] = _fingerprint(manifest)
    render = {
        "status": "rendered-needs-review",
        "fixed_voice_acceptance": False,
        "character": {"id": character},
    }
    return manifest, render


class ReviewNativeManifestTests(unittest.TestCase):
    def test_supported_character_bindings_return_the_exact_line(self):
        for character in ("lena", "owen"):
            with self.subTest(character=character):
                manifest, render = _valid_run(character)
                self.assertEqual(review.validate_manifest(manifest, render),
                                 (character, review.BINDINGS[character][2]))

    def test_rejects_mixed_role_line_id_and_arbitrary_dialogue_even_with_valid_fingerprint(self):
        for field, value in (("role", "JOE"), ("line_id", "joe-line"), ("dialogue", "Anything goes.")):
            with self.subTest(field=field):
                manifest, render = _valid_run("lena")
                manifest[field] = value
                manifest["source_fingerprint"] = _fingerprint(manifest)
                with self.assertRaisesRegex(ValueError, "binding"):
                    review.validate_manifest(manifest, render)

    def test_rejects_manifest_and_render_snapshot_id_mismatch_with_valid_fingerprint(self):
        manifest, render = _valid_run("owen")
        manifest["character_snapshot"]["id"] = "lena"
        manifest["source_fingerprint"] = _fingerprint(manifest)
        with self.assertRaisesRegex(ValueError, "binding"):
            review.validate_manifest(manifest, render)

        manifest, render = _valid_run("owen")
        render["character"]["id"] = "lena"
        manifest["source_fingerprint"] = _fingerprint(manifest)
        with self.assertRaisesRegex(ValueError, "binding"):
            review.validate_manifest(manifest, render)

    def test_rejects_non_diagnostic_status_and_fixed_voice_acceptance(self):
        manifest, render = _valid_run("lena")
        render["status"] = "failed"
        with self.assertRaisesRegex(ValueError, "completed .* diagnostic"):
            review.validate_manifest(manifest, render)

        for target in ("manifest", "render"):
            with self.subTest(target=target):
                manifest, render = _valid_run("lena")
                if target == "manifest":
                    manifest["fixed_voice_acceptance"] = True
                    manifest["source_fingerprint"] = _fingerprint(manifest)
                else:
                    render["fixed_voice_acceptance"] = True
                with self.assertRaisesRegex(ValueError, "completed .* diagnostic"):
                    review.validate_manifest(manifest, render)

    def test_rejects_altered_fingerprint(self):
        manifest, render = _valid_run("owen")
        manifest["post_preparation_change"] = True
        with self.assertRaisesRegex(ValueError, "changed after preparation"):
            review.validate_manifest(manifest, render)


if __name__ == "__main__":
    unittest.main()
