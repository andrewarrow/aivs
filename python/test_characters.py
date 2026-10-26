"""Hunter-compatible name resolution, continuity, and reference snapshots."""

import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import characters


class CharacterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def profile(self, name, *, image=False, scene_image=False):
        folder = self.root / "characters" / name.lower()
        folder.mkdir(parents=True)
        profile = {
            "schema_version": 1, "id": name.lower(), "name": name,
            "description": "An adult with short dark hair", "voice_description": "Low, steady voice",
        }
        if image:
            data = b"reference asset"
            (folder / "reference.png").write_bytes(data)
            profile["image"] = {"file": "reference.png", "sha256": hashlib.sha256(data).hexdigest()}
        if scene_image:
            data = b"wide scene asset"
            (folder / "scene-reference.png").write_bytes(data)
            profile["scene_image"] = {"file": "scene-reference.png", "sha256": hashlib.sha256(data).hexdigest()}
        (folder / "character.json").write_text(json.dumps(profile))
        return profile

    def test_names_and_markers_are_case_insensitive_deduplicated_and_in_mention_order(self):
        eli = self.profile("Eli")
        ava = self.profile("Ava")
        self.assertEqual(characters.resolve_characters("@ELI meets Ava. Eli waves.", root=self.root), [eli, ava])
        self.assertEqual(characters.resolve_characters("AVA meets Eli.", root=self.root), [ava, eli])
        self.assertEqual(characters.resolve_characters("@ELI waits.", explicit=["ava", "ELI"], root=self.root), [ava, eli])
        self.assertEqual(characters.resolve_characters("Elias walks.", root=self.root), [])

    def test_hyphenated_names_do_not_select_the_prefix_character(self):
        self.profile("Eli")
        longer = self.profile("Eli-Smith")
        for prompt in ("Eli-Smith walks.", "@ELI-SMITH walks."):
            self.assertEqual(characters.resolve_characters(prompt, root=self.root), [longer])

    def test_explicit_cast_does_not_resolve_script_role_names_as_performers(self):
        ava = self.profile("Ava")
        eli = self.profile("Eli")
        self.profile("Sue")
        self.profile("Joe")
        prompt = 'Cast: @eli plays JOE; @ava plays SUE. Sue turns to Joe. JOE: "Sue, contact @unknown."'
        self.assertEqual(characters.resolve_characters(prompt, root=self.root, references_only=True), [eli, ava])
        self.assertEqual(characters.resolve_characters('SUE: "Hello."', explicit="ava",
                                                      root=self.root, references_only=True), [ava])

    def test_quoted_references_and_apostrophes_are_preserved(self):
        eli = self.profile("Eli")
        for prompt in (
            'Someone says "Eli and @eli."',
            "Someone says ‘Eli’s friend should contact @unknown’",
            "Someone says 'Eli's friend should contact @unknown'",
        ):
            with self.subTest(prompt=prompt):
                self.assertEqual(characters.resolve_characters(prompt, root=self.root), [])
                self.assertEqual(characters.resolve_characters(prompt, explicit="ELI", root=self.root), [eli])
                self.assertEqual(characters.replace_references(prompt, [eli]), prompt)
        self.assertEqual(characters.resolve_characters("Eli's friend walks.", root=self.root), [eli])

    def test_outer_quotes_do_not_hide_scene_references(self):
        eli = self.profile("Eli")
        prompt = '“@eli says “Contact @unknown.””'
        self.assertEqual(characters.resolve_characters(prompt, root=self.root), [eli])
        self.assertEqual(characters.replace_references(prompt, [eli]), '“Eli says “Contact @unknown.””')

    def test_unknown_markers_and_explicit_selections_fail_before_rendering(self):
        with self.assertRaisesRegex(ValueError, "Unknown character reference: @missing"):
            characters.resolve_characters("@missing walks.", root=self.root)
        with self.assertRaisesRegex(ValueError, "Unknown character: missing"):
            characters.resolve_characters("A person walks.", explicit=["missing"], root=self.root)

    def test_expansion_adds_appearance_and_voice_without_changing_dialogue(self):
        eli = self.profile("Eli")
        prompt = '@eli says "Ava, contact @eli."'
        expanded = characters.expand_prompt(prompt, [eli])
        self.assertTrue(expanded.startswith("Character continuity: Eli has a consistent appearance:"))
        self.assertIn(eli["description"], expanded)
        self.assertIn("Voice quality: " + eli["voice_description"], expanded)
        self.assertIn("Visual style: live-action footage", expanded)
        self.assertTrue(expanded.endswith('Eli says "Ava, contact @eli."'))
        self.assertNotIn("Visual style:", characters.expand_prompt(prompt, [eli], image_conditioned=True))
        self.assertEqual(characters.expand_prompt(prompt, []), prompt)

    def test_expansion_respects_explicit_visual_style(self):
        eli = self.profile("Eli")
        self.assertNotIn("Visual style:", characters.expand_prompt("A watercolor of Eli.", [eli]))
        self.assertIn("Visual style:", characters.expand_prompt('Eli says "I like anime."', [eli]))
        self.assertIn("Visual style:", characters.expand_prompt("Eli walks, no cartoon style.", [eli]))

    def test_selected_profile_is_validated_but_an_unused_corrupt_profile_is_ignored(self):
        self.profile("Eli")
        (self.root / "characters/eli/character.json").write_text("broken")
        self.assertEqual(characters.resolve_characters("A field of flowers.", root=self.root), [])
        with self.assertRaisesRegex(ValueError, "Cannot read character profile"):
            characters.resolve_characters("Eli walks.", root=self.root)

    def test_snapshots_survive_live_edits_and_validate_reference_hashes(self):
        eli = self.profile("Eli", image=True)
        run = self.root / "run"
        self.assertEqual(characters.snapshot_characters([eli], run, self.root), [eli])
        (self.root / "characters/eli/character.json").write_text("changed later")
        characters.verify_snapshots([eli], run)
        image = characters.reference_image([eli], run)
        self.assertEqual(image, run / "characters/eli/reference.png")
        image.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "asset is missing or has changed"):
            characters.reference_image([eli], run)

    def test_scene_image_takes_precedence_and_is_snapshotted_and_hash_checked(self):
        eli = self.profile("Eli", image=True, scene_image=True)
        run = self.root / "run"
        characters.snapshot_characters([eli], run, self.root)
        self.assertEqual(characters.reference_image([eli], run), run / "characters/eli/scene-reference.png")
        self.assertEqual((run / "characters/eli/scene-reference.png").read_bytes(), b"wide scene asset")
        (run / "characters/eli/scene-reference.png").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "asset is missing or has changed"):
            characters.reference_image([eli], run)

    def test_scene_only_profile_uses_scene_image_and_counts_as_a_reference(self):
        eli = self.profile("Eli", scene_image=True)
        run = self.root / "run"
        characters.snapshot_characters([eli], run, self.root)
        self.assertEqual(characters.reference_image([eli], run), run / "characters/eli/scene-reference.png")
        ava = self.profile("Ava", image=True)
        characters.snapshot_characters([ava], run, self.root)
        with self.assertRaisesRegex(ValueError, "Multiple character reference images"):
            characters.reference_image([eli, ava], run)

    def test_live_assets_and_profile_changes_are_rejected_before_snapshot(self):
        eli = self.profile("Eli", image=True)
        (self.root / "characters/eli/reference.png").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "asset is missing or has changed"):
            characters.load_character("eli", self.root)
        ava = self.profile("Ava")
        changed = dict(ava, description="A changed appearance")
        (self.root / "characters/ava/character.json").write_text(json.dumps(changed))
        with self.assertRaisesRegex(ValueError, "changed before snapshot"):
            characters.snapshot_characters([ava], self.root / "run", self.root)

    def test_multiple_reference_images_require_an_explicit_mode_choice(self):
        profiles = [self.profile("Eli", image=True), self.profile("Ava", image=True)]
        run = self.root / "run"
        characters.snapshot_characters(profiles, run, self.root)
        with self.assertRaisesRegex(ValueError, "Multiple character reference images"):
            characters.reference_image(profiles, run)

    def test_all_imported_names_resolve_to_valid_profiles_and_assets(self):
        root = Path(__file__).resolve().parents[1]
        for path in sorted((root / "characters").glob("*/character.json")):
            profile = json.loads(path.read_text())
            with self.subTest(name=profile["name"]):
                self.assertEqual(characters.resolve_characters(f"@{profile['id']} walks.", root=root), [profile])


if __name__ == "__main__":
    unittest.main()
