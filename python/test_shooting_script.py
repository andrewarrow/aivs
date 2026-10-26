"""Offline contract tests for arbitrary screenplay translation."""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

from shooting_script import create_shooting_script, load_shooting_script, parse_request, validate_shooting_script


PROFILES = [
    {"id": "rhea", "name": "Rhea", "description": "A woman with copper hair and a navy coat",
     "voice_description": "A low, careful voice"},
    {"id": "omar", "name": "Omar", "description": "A man with dark curls and a gray jacket",
     "voice_description": "A warm, quick voice"},
]
REQUEST = {"version": 1, "cast": [{"role": "MARA", "character": "rhea"},
                                  {"role": "TOMAS", "character": "omar"}],
           "script": """INT. GLASSHOUSE - NIGHT

Mara lifts a cracked lantern and crosses the damp floor.

MARA (quietly): “The seedlings are alive.”

Tomas takes the lantern from Mara. They open the greenhouse door.

TOMAS: "Then we have time."

EXT. GARDEN - NIGHT

They carry the lantern into the rain and walk to the gate.

FADE OUT.
"""}


def candidate():
    return {"shots": [
        {"seconds": 6, "beat_ids": [1, 2],
         "prompt_template": "In the glasshouse Mara lifts a cracked lantern and crosses the damp floor. {{line_2}}",
         "start_state": "Mara stands by the lantern; Tomas is near the door; lantern on bench.",
         "end_state": "Mara holds the lantern across the floor; Tomas remains near the door.",
         "continue_previous": False},
        {"seconds": 7, "beat_ids": [3, 4],
         "prompt_template": "Tomas takes the lantern from Mara and opens the door with her. {{line_4}}",
         "start_state": "Mara holds the lantern across the floor; Tomas is beside her.",
         "end_state": "Tomas holds the lantern; greenhouse door is open; both stand at it.",
         "continue_previous": True},
        {"seconds": 5, "beat_ids": [5],
         "prompt_template": "In the garden Tomas carries the lantern through rain. Mara walks with him to the gate.",
         "start_state": "Tomas holds the lantern at the open greenhouse door.",
         "end_state": "Both reach the garden gate; Tomas still holds the lantern.",
         "continue_previous": False},
    ]}


class ParseTests(unittest.TestCase):
    def test_arbitrary_scene_beats_and_fade(self):
        source = parse_request(REQUEST, PROFILES)
        self.assertEqual([b["kind"] for b in source["beats"]],
                         ["action", "dialogue", "action", "dialogue", "action"])
        self.assertEqual(source["beats"][1]["text"], "The seedlings are alive.")
        self.assertEqual(source["beats"][1]["speaker"], "MARA")
        self.assertEqual(source["beats"][1]["cue"], "quietly")
        self.assertEqual(source["beats"][4]["scene"], "EXT. GARDEN - NIGHT")
        self.assertTrue(source["fade_out"])
        self.assertEqual(source["script"], REQUEST["script"].strip())

    def test_fountain_dialogue_and_silent_script(self):
        request = {"version": 1, "cast": REQUEST["cast"],
                   "script": """INT. OBSERVATORY - DAY

MARA
(under her breath)
It moved again.

TOMAS
I saw it too.
"""}
        beats = parse_request(request, PROFILES)["beats"]
        self.assertEqual([(b["speaker"], b["text"]) for b in beats],
                         [("MARA", "It moved again."), ("TOMAS", "I saw it too.")])
        self.assertEqual(beats[0]["cue"], "under her breath")
        silent = {"version": 1, "cast": [], "script": "EXT. ROOFTOP - DAWN\n\nA kite catches wind.\n\nIt rises over the city.\n"}
        self.assertEqual([b["kind"] for b in parse_request(silent, [])["beats"]], ["action", "action"])

    def test_cast_errors(self):
        for cast in ([{"role": "MARA", "character": "missing"}],
                     [{"role": "MARA", "character": "rhea"}, {"role": "mara", "character": "omar"}],
                     [{"role": "MARA", "character": "rhea"}, {"role": "TOMAS", "character": "rhea"}]):
            with self.subTest(cast=cast), self.assertRaises(ValueError):
                parse_request({**REQUEST, "cast": cast}, PROFILES)


class ValidationTests(unittest.TestCase):
    def test_filled_prompts_and_source_metadata(self):
        artifact = validate_shooting_script(candidate(), REQUEST, PROFILES, token_counter=lambda s: len(s.split()))
        self.assertEqual(artifact["seconds"], 18)
        self.assertEqual(artifact["source"]["beats"][3]["text"], "Then we have time.")
        self.assertIn('MARA, quietly, says, "The seedlings are alive."', artifact["shots"][0]["prompt"])
        self.assertIn("MARA: A woman with copper hair", artifact["shots"][0]["prompt"])
        self.assertIn("TOMAS voice: A warm, quick voice", artifact["shots"][2]["prompt"])
        self.assertTrue(artifact["fade_out"])

    def test_missing_reordered_duplicate_beats(self):
        for ids in ([2], [1, 3], [1, 1]):
            bad = candidate()
            bad["shots"][0]["beat_ids"] = ids
            with self.subTest(ids=ids), self.assertRaisesRegex(ValueError, "skips|duplicates|reorders"):
                validate_shooting_script(bad, REQUEST, PROFILES)

    def test_dialogue_placeholder_integrity(self):
        for template in ("Mara crosses the room.",
                         "{{line_4}} {{line_2}}",
                         "{{line_2}} {{line_2}}",
                         'Mara says "The seedlings are alive." {{line_2}}',
                         "Mara says new words. {{line_2}}"):
            bad = candidate()
            bad["shots"][0]["prompt_template"] = template
            with self.subTest(template=template), self.assertRaises(ValueError):
                validate_shooting_script(bad, REQUEST, PROFILES)

    def test_timing_tokens_and_scene_boundary(self):
        bad = candidate()
        bad["shots"][0]["seconds"] = 3
        with self.assertRaisesRegex(ValueError, "4 to 12"):
            validate_shooting_script(bad, REQUEST, PROFILES)
        bad = candidate()
        bad["shots"][0]["beat_ids"] = [1, 2, 3, 4]
        bad["shots"][0]["prompt_template"] += " Tomas takes the lantern and opens the door. {{line_4}}"
        bad["shots"][0]["seconds"] = 4
        bad["shots"].pop(1)
        with self.assertRaisesRegex(ValueError, "needs at least"):
            validate_shooting_script(bad, REQUEST, PROFILES)
        bad = candidate()
        bad["shots"][2]["continue_previous"] = True
        with self.assertRaisesRegex(ValueError, "across scenes"):
            validate_shooting_script(bad, REQUEST, PROFILES)
        with self.assertRaisesRegex(ValueError, "1024 Gemma tokens"):
            validate_shooting_script(candidate(), REQUEST, PROFILES, token_counter=lambda s: 1025)

    def test_stale_script_or_profile_rejected_on_load(self):
        artifact = validate_shooting_script(candidate(), REQUEST, PROFILES)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "shooting-script.json"
            path.write_text(json.dumps(artifact), encoding="utf-8")
            self.assertEqual(load_shooting_script(path, REQUEST, PROFILES)["seconds"], 18)
            with self.assertRaisesRegex(ValueError, "different screenplay"):
                load_shooting_script(path, {**REQUEST, "script": REQUEST["script"] + "\nA bell rings."}, PROFILES)
            changed = [dict(PROFILES[0], description="A woman in a red coat"), PROFILES[1]]
            with self.assertRaisesRegex(ValueError, "different screenplay"):
                load_shooting_script(path, REQUEST, changed)


class CodexTests(unittest.TestCase):
    def _fake(self, directory: Path, *, fail: bool = False) -> Path:
        path = directory / "fake-codex"
        path.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv
assert args[1] == 'exec'
assert '--output-schema' in args and '--ephemeral' in args and '--json' in args
assert '--sandbox' in args and args[args.index('--sandbox')+1] == 'read-only'
assert 'approval_policy=never' in args
prompt = sys.stdin.read()
assert 'Create a shooting plan' in prompt
if os.environ.get('FAKE_CODEX_FAIL') == '1':
    print('login required', file=sys.stderr)
    sys.exit(7)
output = pathlib.Path(args[args.index('--output-last-message')+1])
response = json.loads(pathlib.Path(os.environ['FAKE_CODEX_RESPONSE']).read_text())
output.write_text(json.dumps(response))
print(json.dumps({'type': 'turn.completed'}))
""", encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def test_create_and_logs(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            response = base / "response.json"
            response.write_text(json.dumps(candidate()), encoding="utf-8")
            fake = self._fake(base)
            run = base / "run"
            with patch.dict(os.environ, {"FAKE_CODEX_RESPONSE": str(response)}):
                result = create_shooting_script(REQUEST, PROFILES, run, executable=fake,
                                                token_counter=lambda s: len(s.split()))
            self.assertEqual(result["seconds"], 18)
            self.assertTrue((run / "shooting-script.json").is_file())
            self.assertTrue((run / "shooting-script.txt").is_file())
            self.assertTrue((run / "shooting-script-schema.json").is_file())
            self.assertTrue((run / "shooting-script-request-1.txt").is_file())
            self.assertTrue((run / "shooting-script-response-1.json").is_file())

    def test_codex_failure_is_explicit(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            response = base / "response.json"
            response.write_text(json.dumps(candidate()), encoding="utf-8")
            fake = self._fake(base)
            with patch.dict(os.environ, {"FAKE_CODEX_RESPONSE": str(response), "FAKE_CODEX_FAIL": "1"}):
                with self.assertRaisesRegex(RuntimeError, "login required"):
                    create_shooting_script(REQUEST, PROFILES, base / "run", executable=fake)
            with self.assertRaisesRegex(RuntimeError, "executable is unavailable"):
                create_shooting_script(REQUEST, PROFILES, base / "run", executable=base / "missing")


if __name__ == "__main__":
    unittest.main()
