"""Prompt budgeting contracts; no tokenizer downloads, model loads or inference."""

import unittest

import storyboard_backend


class WordTokenizer:
    def apply_chat_template(self, messages, **options):
        return "user " + messages[0]["content"] + " assistant"

    def encode(self, value, **options):
        return value.split()


class StoryboardPromptBudgetTests(unittest.TestCase):
    def test_short_composition_is_preserved_in_full(self):
        prompt, tokens, shortened = storyboard_backend._fit_prompt(
            WordTokenizer(), "Required visible state.", "Medium shot beside the table.")
        self.assertIn("Required visible state.", prompt)
        self.assertTrue(prompt.endswith("Medium shot beside the table."))
        self.assertLessEqual(tokens, 512)
        self.assertFalse(shortened)

    def test_long_composition_cannot_truncate_required_physical_state(self):
        required = "owner A; corded-landline; receiver connected-to base=yes; opaque coat."
        prompt, tokens, shortened = storyboard_backend._fit_prompt(
            WordTokenizer(), required, "Medium shot. " + "descriptive prose " * 400)
        self.assertTrue(prompt.startswith(required))
        self.assertIn("Composition: Medium shot.", prompt)
        self.assertLessEqual(tokens, 512)
        self.assertTrue(shortened)

    def test_oversized_essential_state_is_rejected_instead_of_truncated(self):
        with self.assertRaisesRegex(ValueError, "Essential storyboard state"):
            storyboard_backend._fit_prompt(WordTokenizer(), "required " * 513, "Medium shot.")

    def test_reference_prompt_has_no_optional_truncation(self):
        original = "Canonical reference with its components and connections."
        prompt, tokens, shortened = storyboard_backend._fit_prompt(WordTokenizer(), original)
        self.assertEqual(prompt, original)
        self.assertLessEqual(tokens, 512)
        self.assertFalse(shortened)


if __name__ == "__main__":
    unittest.main()
