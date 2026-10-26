"""Offline contract tests for arbitrary screenplay translation."""

from __future__ import annotations

import json
import copy
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

import shooting_script
from shooting_script import create_shooting_script, load_shooting_script, parse_request, save_shooting_script, validate_shooting_script


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
        self.assertEqual(source["script"], REQUEST["script"])

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
        missing = {**request, "script": "INT. OBSERVATORY - DAY\n\nLENA\nThe lens is cracked.\n"}
        with self.assertRaisesRegex(ValueError, "no cast binding"):
            parse_request(missing, PROFILES)

    def test_cast_errors(self):
        for cast in ([{"role": "MARA", "character": "missing"}],
                     [{"role": "MARA", "character": "rhea"}, {"role": "mara", "character": "omar"}]):
            with self.subTest(cast=cast), self.assertRaises(ValueError):
                parse_request({**REQUEST, "cast": cast}, PROFILES)
        shared = {**REQUEST, "cast": [{"role": "MARA", "character": "rhea"},
                                      {"role": "TOMAS", "character": "rhea"}]}
        self.assertEqual(len(parse_request(shared, [PROFILES[0]])["cast"]), 2)

    def test_repeated_heading_and_transition_make_new_scene_context(self):
        request = {"version": 1, "cast": [], "script": """INT./EXT. HALL - DAY

A door opens.

CUT TO:

A shadow crosses the wall.

EXT./INT. HALL - DAY

The door closes.
"""}
        beats = parse_request(request, [])["beats"]
        self.assertEqual([beat["scene_id"] for beat in beats], [1, 2, 3])
        self.assertEqual(beats[1]["transition"], "CUT TO:")
        self.assertEqual(beats[0]["scene"], "INT./EXT. HALL - DAY")
        self.assertEqual(beats[2]["scene"], "EXT./INT. HALL - DAY")


class ValidationTests(unittest.TestCase):
    def test_reference_asset_changes_invalidate_cached_plans_without_changing_description(self):
        profiles = copy.deepcopy(PROFILES)
        for field in ("image", "scene_image", "voice_sample"):
            with self.subTest(field=field):
                profiles[0][field] = {"file": f"{field}.bin", "sha256": "a" * 64}
                saved = validate_shooting_script(candidate(), REQUEST, profiles)
                profiles[0][field]["sha256"] = "b" * 64
                with self.assertRaisesRegex(ValueError, "different screenplay or cast"):
                    validate_shooting_script(saved, REQUEST, profiles)

    def test_filled_prompts_and_source_metadata(self):
        artifact = validate_shooting_script(candidate(), REQUEST, PROFILES, token_counter=lambda s: len(s.split()))
        self.assertEqual(artifact["seconds"], 18)
        self.assertEqual(artifact["source"]["beats"][3]["text"], "Then we have time.")
        self.assertIn('MARA (quietly) says, "The seedlings are alive."', artifact["shots"][0]["prompt"])
        self.assertTrue(artifact["shots"][0]["prompt"].startswith("In the glasshouse Mara lifts"))
        self.assertIn("MARA looks like a woman with copper hair", artifact["shots"][0]["prompt"])
        self.assertIn("TOMAS speaks with a warm, quick voice", artifact["shots"][2]["prompt"])
        self.assertNotIn("MARA:", artifact["shots"][0]["prompt"])
        self.assertIn("delivered exactly once in order", artifact["shots"][0]["prompt"])
        self.assertIn("no spoken dialogue", artifact["shots"][2]["prompt"])
        self.assertNotIn("current speaker", artifact["shots"][2]["prompt"])
        self.assertTrue(artifact["fade_out"])

    def test_same_scene_cannot_restart_without_a_visual_handoff(self):
        bad = candidate()
        bad["shots"][1]["continue_previous"] = False
        with self.assertRaisesRegex(ValueError, "must continue.*same scene"):
            validate_shooting_script(bad, REQUEST, PROFILES)

    def test_final_fade_does_not_force_three_seconds_for_a_brief_action(self):
        request = {"version": 1, "cast": REQUEST["cast"],
                   "script": 'INT. GLASSHOUSE - NIGHT\n\nMARA: "Then we have time. Ready?"\n\nMara nods.\n'}
        proposed = {"shots": [{
            "seconds": 4, "beat_ids": [1, 2], "continue_previous": False,
            "prompt_template": "MARA faces TOMAS. {{line_1}} MARA nods once, lips at rest.",
            "start_state": "MARA and TOMAS face each other.",
            "end_state": "MARA has nodded and remains facing TOMAS.",
        }]}
        for ending in ("", "\nFADE OUT.\n"):
            with self.subTest(ending=ending):
                artifact = validate_shooting_script(
                    proposed, {**request, "script": request["script"] + ending}, PROFILES)
                self.assertEqual(artifact["seconds"], 4)
                self.assertEqual(artifact["fade_out"], bool(ending))

    def test_source_delivery_can_still_require_a_long_pause(self):
        request = {"version": 1, "cast": REQUEST["cast"],
                   "script": 'INT. GLASSHOUSE - NIGHT\n\nMARA (after a long pause): "Ready?"\n'}
        proposed = {"shots": [{
            "seconds": 12, "beat_ids": [1], "continue_previous": False,
            "prompt_template": "MARA holds TOMAS's gaze through a long pause, then {{line_1}}",
            "start_state": "MARA and TOMAS face each other.",
            "end_state": "Both remain facing each other, lips at rest.",
        }]}
        self.assertEqual(validate_shooting_script(proposed, request, PROFILES)["seconds"], 12)

    def test_planner_receives_speech_estimates_from_exact_source_lines(self):
        source = parse_request(REQUEST, PROFILES)
        instruction = shooting_script._instruction(source, None)
        data = json.loads(instruction.split("\nSOURCE DATA\n", 1)[1].strip())
        self.assertEqual(data["timing_guidance"]["speech_estimates"], [
            {"beat_id": 2, "spoken_words": 4, "speech_seconds_at_160_wpm": 1.5},
            {"beat_id": 4, "spoken_words": 4, "speech_seconds_at_160_wpm": 1.5},
        ])
        self.assertEqual(data["timing_guidance"]["settling_seconds"], 0.5)

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
            save_shooting_script(artifact, Path(temp))
            self.assertIn("SHOT 1 | 6s", (Path(temp) / "shooting-script.txt").read_text())
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
import json, os, pathlib, sys, time
args = sys.argv
assert args[1:] == ['app-server']
for raw in sys.stdin:
    request = json.loads(raw)
    method = request.get('method')
    if method == 'initialize':
        print(json.dumps({'id': request['id'], 'result': {}}), flush=True)
    elif method == 'thread/start':
        assert request['params']['approvalPolicy'] == 'never'
        assert request['params']['sandbox'] == 'read-only'
        assert request['params']['ephemeral'] is True
        print(json.dumps({'id': request['id'], 'result': {'thread': {'id': 'fake-thread'}}}), flush=True)
    elif method == 'turn/start':
        assert request['params']['sandboxPolicy'] == {'type': 'readOnly'}
        assert request['params']['outputSchema']['type'] == 'object'
        prompt = request['params']['input'][0]['text']
        break
assert 'Create a shooting plan' in prompt
if os.environ.get('FAKE_CODEX_PID'):
    pathlib.Path(os.environ['FAKE_CODEX_PID']).write_text(str(os.getpid()))
print(json.dumps({'method': 'item/reasoning/summaryTextDelta', 'params': {
    'itemId': 'reasoning-1', 'summaryIndex': 0, 'delta': 'Mapping dialogue to shots.'}}), flush=True)
sys.stderr.write('Checking shot continuity.\\n' + 'diagnostic ' * 10000)
sys.stderr.flush()
if os.environ.get('FAKE_CODEX_GATE'):
    deadline = time.monotonic() + 5
    while not pathlib.Path(os.environ['FAKE_CODEX_GATE']).exists():
        if time.monotonic() >= deadline:
            sys.exit('activity was buffered instead of streamed')
        time.sleep(0.01)
time.sleep(float(os.environ.get('FAKE_CODEX_PAUSE', '0')))
if os.environ.get('FAKE_CODEX_FAIL') == '1':
    print('login required', file=sys.stderr)
    sys.exit(7)
response = json.loads(pathlib.Path(os.environ['FAKE_CODEX_RESPONSE']).read_text())
if os.environ.get('FAKE_CODEX_BAD_FIRST') == '1' and 'Correct this validation error:' not in prompt:
    response['shots'][0]['beat_ids'] = [2]
text = json.dumps(response)
print(json.dumps({'method': 'item/started', 'params': {'item': {
    'id': 'answer-1', 'type': 'agentMessage', 'phase': 'final_answer'}}}), flush=True)
for delta in [text[:len(text)//2], text[len(text)//2:]]:
    print(json.dumps({'method': 'item/agentMessage/delta', 'params': {
        'itemId': 'answer-1', 'delta': delta}}), flush=True)
print(json.dumps({'method': 'item/completed', 'params': {'item': {
    'id': 'answer-1', 'type': 'agentMessage', 'phase': 'final_answer', 'text': text}}}), flush=True)
print(json.dumps({'method': 'turn/completed', 'params': {'turn': {'status': 'completed', 'items': []}}}), flush=True)
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

    def test_activity_and_both_logs_arrive_before_codex_exits(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            response = base / "response.json"
            response.write_text(json.dumps(candidate()), encoding="utf-8")
            fake = self._fake(base)
            run = base / "run"
            gate = base / "continue"
            messages = []

            def activity(message):
                messages.append(message)
                if "Mapping dialogue" in message:
                    self.assertIn("Mapping dialogue", (run / "shooting-script-events-1.jsonl").read_text())
                if "Checking shot continuity" in message:
                    self.assertIn("Checking shot continuity", (run / "shooting-script-errors-1.txt").read_text())
                if not gate.exists() and any("Mapping dialogue" in line for line in messages) \
                        and any("Checking shot continuity" in line for line in messages):
                    self.assertFalse((run / "shooting-script-response-1.json").exists())
                    gate.touch()

            with patch.dict(os.environ, {"FAKE_CODEX_RESPONSE": str(response), "FAKE_CODEX_GATE": str(gate)}):
                plan = create_shooting_script(REQUEST, PROFILES, run, executable=fake, on_activity=activity)
            self.assertEqual(plan["seconds"], 18)
            self.assertTrue(any('"prompt_template"' in message for message in messages))
            self.assertIn("Shooting script ready: 3 shots, 18 seconds.", messages)
            self.assertEqual((run / "shooting-script-errors-1.txt").read_text().count("diagnostic "), 10000)

    def test_codex_streams_reasoning_and_answer_deltas_before_completion(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            response = base / "response.json"
            response.write_text(json.dumps(candidate()), encoding="utf-8")
            fake = self._fake(base)
            deltas = []
            run = base / "run"
            def delta(stream_id, text):
                self.assertFalse((run / "shooting-script-response-1.json").exists())
                deltas.append((stream_id, text))
            with patch.dict(os.environ, {"FAKE_CODEX_RESPONSE": str(response), "FAKE_CODEX_PAUSE": "0.1"}):
                create_shooting_script(REQUEST, PROFILES, run, executable=fake, on_activity_delta=delta)
            self.assertIn(("reasoning-1:summary:0", "Mapping dialogue to shots."), deltas)
            self.assertEqual("".join(text for stream_id, text in deltas if stream_id == "answer-1"),
                             json.dumps(candidate()))

    def test_timeout_and_interruption_reap_codex_and_keep_partial_logs(self):
        for interrupt in (False, True):
            with self.subTest(interrupt=interrupt), tempfile.TemporaryDirectory() as temp:
                base = Path(temp)
                fake = self._fake(base)
                pid = base / "pid"
                run = base / "run"

                def activity(message):
                    if interrupt and "Mapping dialogue" in message:
                        raise KeyboardInterrupt()

                with patch.dict(os.environ, {"FAKE_CODEX_PID": str(pid), "FAKE_CODEX_PAUSE": "10"}), \
                        patch.object(shooting_script, "_CODEX_TIMEOUT_SECONDS", 0.5):
                    if interrupt:
                        with self.assertRaises(KeyboardInterrupt):
                            create_shooting_script(REQUEST, PROFILES, run, executable=fake, on_activity=activity)
                    else:
                        with self.assertRaisesRegex(RuntimeError, "timed out"):
                            create_shooting_script(REQUEST, PROFILES, run, executable=fake, on_activity=activity)
                self.assertIn("Mapping dialogue", (run / "shooting-script-events-1.jsonl").read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(pid.read_text()), 0)

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

    def test_one_corrective_retry(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            response = base / "response.json"
            response.write_text(json.dumps(candidate()), encoding="utf-8")
            fake = self._fake(base)
            run = base / "run"
            messages = []
            with patch.dict(os.environ, {"FAKE_CODEX_RESPONSE": str(response), "FAKE_CODEX_BAD_FIRST": "1"}):
                plan = create_shooting_script(REQUEST, PROFILES, run, executable=fake, on_activity=messages.append)
            self.assertEqual(plan["seconds"], 18)
            self.assertTrue((run / "shooting-script-request-2.txt").is_file())
            self.assertIn("Correct this validation error:", (run / "shooting-script-request-2.txt").read_text())
            self.assertTrue(any("needs a correction" in message for message in messages))
            self.assertIn("Retrying shooting script with Codex (attempt 2/2).", messages)


if __name__ == "__main__":
    unittest.main()
