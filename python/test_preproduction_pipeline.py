"""Mocked pipeline integration: real checkpoints/hashes and a built Rust engine.

Run explicitly with: cd python && python -m unittest test_preproduction_pipeline
"""

import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
import wave
from unittest.mock import patch

from PIL import Image

import character_conditioning
import characters
import preproduction
import quality_backend
import shooting_script
import shot_renderer
import storyboard
import storyboard_backend
import video


class PreproductionPipelineTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run = self.root / "run"
        self.run.mkdir()
        portrait = self.root / "characters/mara/reference.png"
        portrait.parent.mkdir(parents=True)
        Image.new("RGB", (32, 32), "navy").save(portrait)
        profile = {"schema_version": 1, "id": "mara", "name": "Mara",
                   "description": "A woman with copper hair", "voice_description": "A low clear voice",
                   "image": {"file": "reference.png", "sha256": self.digest(portrait)}}
        self.write(portrait.parent / "character.json", profile)
        self.profiles = characters.snapshot_characters([profile], self.run, root=self.root)
        self.request = {
            "version": 1, "cast": [{"role": "MARA", "character": "mara"}],
            "character_conditioning": character_conditioning.POLICY,
            "storyboard_policy": shooting_script.STORYBOARD_POLICY, "voice_mode": "native",
            "script": ('INT. ROOM - DAY\n\nMara lifts a brass key from the table.\n\n'
                       'MARA: "Found it."\n\nMara places the key inside an open box.\n\n'
                       'MARA: "Keep it safe."'),
        }
        self.actions = ["Mara lifts a brass key from the table.", "Mara places the key inside an open box."]
        self.templates = [action + " {{line_" + str(2 * number) + "}}"
                          for number, action in enumerate(self.actions, 1)]
        self.director = {
            "planning_policy": "single-render-v1", "storyboard_policy": shooting_script.STORYBOARD_POLICY,
            "cast_identity": [{"role": "MARA", "description": profile["description"],
                               "voice_description": profile["voice_description"]}],
            "revision_notes": ["Keep the key transfer visible."],
            "shots": [{"seconds": 5, "beat_ids": [2 * number - 1, 2 * number],
                       "visible_roles": ["MARA"], "coverage": "continuous", "continue_previous": False,
                       "prompt_template": self.templates[number - 1],
                       "start_state": start, "storyboard_prompt": "Static medium shot. " + start,
                       "end_state": end, "dramatic_purpose": action,
                       "visual_contract": [action],
                       "action_coverage": [{"beat_id": 2 * number - 1, "part": 1, "parts": 1,
                                            "action": action}]}
                      for number, (action, start, end) in enumerate(zip(self.actions,
                          ["Mara stands beside a key on the table.", "Mara holds the key above an open box."],
                          ["Mara holds the lifted key.", "The key rests inside the box."]), 1)],
        }
        self.director["world"] = {
            "characters": [{"role": "MARA", "morphology": [{"key": "stature", "value": "average"}],
                            "reference_prompt": "Mara with average proportions in neutral clothing."}],
            "props": [{"id": "key", "scene_ids": [1], "type": "key", "shape": "notched", "size": "palm-sized",
                       "color": "brass", "features": [], "components": [], "relationships": [],
                       "reference_prompt": "One brass notched key on a neutral surface."}],
            "environments": [{"scene_id": 1, "reference_prompt": "Empty room with table and open box, no loose props."}],
        }
        physical = [{"key": k, "value": self.director["world"]["props"][0][k]}
                    for k in ("type", "shape", "size", "color")]
        def state(owner, support):
            return {"characters": [{"role": "MARA", "presence": "visible", "location": "room", "posture": "standing",
                                    "costume": "unknown", "appearance": [], "morphology": [{"key": "stature", "value": "average"}],
                                    "visibility": []}],
                    "props": [{"id": "key", "owner": owner, "support": support, "location": "room", "state": "intact",
                               "visibility": "visible", "physical": physical}],
                    "reveals": [], "relationships": []}
        states = [state("none", "table"), state("MARA", "hand"), state("none", "box")]
        for n, shot in enumerate(self.director["shots"]):
            shot["opening_visible_roles"] = ["MARA"]
            transitions = [{"entity": "prop:key", "field": field, "before": states[n]["props"][0][field],
                            "after": states[n + 1]["props"][0][field], "timing": "during-shot", "mode": "on-screen",
                            "beat_ids": [2 * n + 1], "quote": self.actions[n], "motivation": "Visible source action.",
                            "physical_transformation": False, "story_time": "continuous"}
                           for field in ("owner", "support")]
            shot["continuity"] = {"opening": states[n], "ending": states[n + 1], "transitions": transitions}
        self.codex_calls, self.image_calls, self.render_calls = [], [], []
        self.image_inputs = []
        self.motion_inputs = []
        self.interrupt_image = self.interrupt_render = False
        self.review_transform = None
        self.mock(quality_backend, "ROOT", self.root)
        self.mock(video, "ROOT", self.root)
        self.mock(shooting_script, "_executable", return_value="mock-codex")
        self.mock(shooting_script, "_stream_codex", side_effect=self.codex)
        self.mock(storyboard_backend, "check", return_value={"backend": "mock-still"})
        self.mock(storyboard_backend, "generate", side_effect=self.generate_image)
        self.mock(shot_renderer, "validate_video", return_value={"valid": True})
        self.mock(shot_renderer, "_assemble", side_effect=self.assemble)
        self.mock(character_conditioning, "apply_voice", side_effect=AssertionError("Native mode converted audio"))

    def mock(self, target, name, *args, **kwargs):
        mocked = patch.object(target, name, *args, **kwargs)
        self.addCleanup(mocked.stop)
        return mocked.start()

    @staticmethod
    def digest(path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()

    @staticmethod
    def write(path, value):
        Path(path).write_text(json.dumps(value), encoding="utf-8")

    def codex(self, command, instruction, **options):
        properties = options["output_schema"]["properties"]
        if "storyboard_policy" in properties:
            self.codex_calls.append("director")
            result = self.director
        elif "checks" in properties:
            self.codex_calls.append("fidelity")
            data = json.loads(instruction.split("\nDATA\n", 1)[1])
            prompts = {shot["shot"]: shot["prompt"] for shot in data["shots"]}
            result = {"checks": [{"id": item["id"], "preserved": True,
                                   "evidence": [{"shot": item["shot_numbers"][0],
                                                 "quote": prompts[item["shot_numbers"][0]]}],
                                   "reason": "The assigned action and dialogue remain explicit."}
                                  for item in data["requirements"]]}
        else:
            self.codex_calls.append("motion")
            data = json.loads(instruction.rsplit("\n\n", 1)[1])
            self.motion_inputs.append(data)
            self.assertEqual(len(options["image_paths"]), len(data["images_in_shot_order"]) + len(data["reference_images_first"]))
            self.assertTrue(all(Path(path).is_file() for path in options["image_paths"]))
            result = {"shots": [{"shot": number, "prompt_template": "A steady camera. " + self.templates[number - 1],
                                 "image_observations": "The performer, key and destination are visible.",
                                 "blocking_issue": "", "storyboard_prompt": "", "plan_repair": [], "world_repair": [],
                                 "continuity_review": {
                                     "observed_opening_state": copy.deepcopy(data["plan"]["shots"][number - 1]["continuity"]["opening"]),
                                     "source_transition_evidence": [], "violations": [],
                                     "evidence": "Same performer and brass key, held or supported as expected.",
                                     "neighbor_comparison": "The previous action explains the key's current support.",
                                     "confidence": .95, "decision": "pass", "failure_kind": "none",
                                     "reference_findings": [{"id": rid, "decision": "pass", "confidence": .95,
                                                              "evidence": "Reference pixels match the canonical definition."}
                                                             for rid in data["reference_ids_to_review"]]}}
                                for number in data["shots_to_direct"]]}
        if self.codex_calls[-1] == "motion" and self.review_transform is not None:
            self.review_transform(result, data)
        raw = json.dumps(result)
        options["response_path"].write_text(raw, encoding="utf-8")
        return raw

    def generate_image(self, prompt, references, output, seed, *, width, height, on_activity, composition=None):
        self.image_inputs.append({"output": str(output), "references": list(references), "prompt": prompt,
                                  "composition": composition})
        if Path(output).name.endswith("partial.png"):
            self.image_calls.append(Path(output).name)
            self.assertTrue(references, "Every storyboard should receive the saved portrait and canonical references")
        if self.interrupt_image and Path(output).name == "002.partial.png":
            self.interrupt_image = False
            raise KeyboardInterrupt()
        Image.new("RGB", (width, height), (seed % 255, 40, 90)).save(output)
        return {"sha256": self.digest(output)}

    def render_video(self, prompt, seed, frames, output, **options):
        self.render_calls.append({"shot": Path(output).parent.name, "prompt": prompt,
                                  "seed": seed, "image": Path(options["image_file"]),
                                  "image_sha256": self.digest(options["image_file"])})
        if self.interrupt_render and Path(output).parent.name == "002":
            self.interrupt_render = False
            raise KeyboardInterrupt()
        Path(output).write_bytes(f"mock-video:{seed}:{prompt}".encode())
        return {"backend": "mock-video"}

    @staticmethod
    def assemble(clips, output, seconds, fade_out, **options):
        Path(output).write_bytes(b"".join(Path(clip).read_bytes() for clip in clips))
        return {"valid": True}

    def prepare(self):
        return preproduction.prepare(self.request, self.profiles, self.run, 17,
                                     token_counter=lambda prompt: len(prompt.split()), fresh=True)

    def render(self, plan, boards, *, resume=False):
        return shot_renderer.render_shooting_script(
            plan, 17, self.run / "video.mp4", storyboard=boards, character_profiles=self.profiles,
            render_fn=self.render_video, review_enabled=False, resume=resume)

    def test_interruption_resumes_storyboards_and_takes_without_repeating_completed_work(self):
        self.interrupt_image = True
        with self.assertRaises(KeyboardInterrupt):
            self.prepare()
        first_frame = self.digest(self.run / "storyboards/001.png")
        self.assertEqual(self.codex_calls, ["director"])
        self.assertFalse((self.run / "shooting-script.json").exists())

        plan, boards = self.prepare()
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertEqual(self.image_calls, ["001.partial.png", "002.partial.png", "002.partial.png"])
        self.assertEqual(self.digest(self.run / "storyboards/001.png"), first_frame)
        self.assertEqual(plan["planning_stage"], "final")
        self.assertEqual(plan["source_fidelity"]["plan_sha256"], shooting_script._plan_fingerprint(plan))
        self.assertIn('"Found it."', plan["shots"][0]["prompt"])
        self.assertIn('"Keep it safe."', plan["shots"][1]["prompt"])
        self.assertNotIn("{{line_", plan["shots"][0]["prompt"])

        self.interrupt_render = True
        with self.assertRaises(KeyboardInterrupt):
            self.render(plan, boards)
        first_take = self.digest(self.run / "shots/001/video.mp4")
        resumed_plan, resumed_boards = self.prepare()
        result = self.render(resumed_plan, resumed_boards, resume=True)
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertEqual([call["shot"] for call in self.render_calls], ["001", "002", "002"])
        self.assertEqual(self.render_calls[1]["seed"], self.render_calls[2]["seed"])
        self.assertTrue(result["shots"][0]["reused"])
        self.assertEqual(self.digest(self.run / "shots/001/video.mp4"), first_take)
        for call in self.render_calls:
            number = int(call["shot"])
            self.assertEqual(call["prompt"], plan["shots"][number - 1]["prompt"])
            self.assertEqual(call["image_sha256"], boards["shots"][number - 1]["image"]["sha256"])

    def test_changed_storyboard_invalidates_motion_receipt_and_saved_take(self):
        plan, boards = self.prepare()
        self.render(plan, boards)
        replacement = self.run / "storyboards/001-revised.png"
        Image.new("RGB", (quality_backend.WIDTH, quality_backend.HEIGHT), "gold").save(replacement)
        changed = copy.deepcopy(boards)
        changed["shots"][0]["image"].update(file="storyboards/001-revised.png", sha256=self.digest(replacement))
        self.write(self.run / "storyboards.json", changed)
        with self.assertRaisesRegex(ValueError, "images differ"):
            self.prepare()
        self.assertEqual(len(self.render_calls), 2)

        # The renderer itself must reject new bytes without sequence acceptance.
        self.render_calls.clear()
        with self.assertRaisesRegex(ValueError, "acceptance"):
            self.render(plan, changed, resume=True)
        self.assertEqual(self.render_calls, [])

    def test_accepted_production_storyboards_render_with_automatic_review_disabled(self):
        plan, boards = self.prepare()
        self.assertEqual(plan["storyboard_policy"], shooting_script.STORYBOARD_POLICY)
        storyboard.require_accepted(plan, boards, self.run)
        result = self.render(plan, boards)
        self.assertEqual(result["status"], "complete")
        self.assertEqual(len(self.render_calls), len(plan["shots"]))
        for call in self.render_calls:
            number = int(call["shot"])
            self.assertEqual(call["image_sha256"], boards["shots"][number - 1]["image"]["sha256"])

    def test_generator_receives_structured_opening_facts_and_composition(self):
        plan, boards = self.prepare()
        frames = [item for item in self.image_inputs if item["composition"] is not None]
        self.assertEqual(len(frames), len(plan["shots"]))
        for number, frame in enumerate(frames):
            for detail in ("MARA:", "key:", "type key", "shape notched", "color brass"):
                self.assertIn(detail, frame["prompt"])
            opening = plan["shots"][number]["continuity"]["opening"]["props"][0]
            self.assertIn("owner " + opening["owner"], frame["prompt"])
            self.assertIn("support " + opening["support"], frame["prompt"])
            self.assertEqual(frame["composition"], plan["shots"][number]["storyboard_prompt"])
            self.assertEqual(frame["prompt"], boards["shots"][number]["generation"]["prompt"])

    def test_storyboards_only_produces_final_motion_plan_without_video_setup_or_inference(self):
        events = self.mock(video, "emit")
        self.mock(video, "check_environment")
        self.mock(video, "prompt_token_counter", return_value=lambda prompt: len(prompt.split()))
        model = self.mock(video, "model_path", side_effect=AssertionError("Debug mode requested LTX weights"))
        renderer = self.mock(video, "render_shooting_script", side_effect=AssertionError("Debug mode rendered video"))
        with patch.dict(os.environ):
            video.generate_screenplay(self.request, seed=17, storyboards_only=True, fresh_preproduction=True)
        run = next((self.root / "outputs").iterdir())
        saved = json.loads((run / "render.json").read_text())
        self.assertEqual(saved["status"], "storyboards-ready")
        self.assertTrue((run / "shooting-script.json").is_file())
        self.assertTrue((run / "storyboards.json").is_file())
        self.assertFalse((run / "video.mp4").exists())
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])
        self.assertTrue(any(call.args[0] == "storyboards_ready" for call in events.call_args_list))
        model.assert_not_called()
        renderer.assert_not_called()

    def fail_first_frame(self, result, data):
        record = next(r for r in result["shots"] if r["shot"] == 1)
        record["continuity_review"]["observed_opening_state"]["props"][0]["physical"][0]["value"] = "smartphone"
        record.update(blocking_issue="The canonical key has become a smartphone in the actual pixels.",
                      storyboard_prompt="Medium shot of Mara beside one canonical brass key on the table.")

    def test_repair_is_reinspected_and_only_failed_frame_is_regenerated(self):
        def once(result, data):
            if self.codex_calls.count("motion") == 1:
                self.fail_first_frame(result, data)
        self.review_transform = once
        plan, boards = self.prepare()
        self.assertEqual(self.codex_calls.count("motion"), 2)
        self.assertEqual(self.image_calls, ["001.partial.png", "002.partial.png", "frame.partial.png"])
        self.assertEqual(boards["shots"][1]["image"]["file"], "storyboards/002.png")
        self.assertIn("repairs/01/001", boards["shots"][0]["image"]["file"])
        self.assertTrue((self.run / "storyboard-review-before-repair-1.json").is_file())
        report = storyboard.require_accepted(plan, boards, self.run)
        self.assertEqual(report["binding"]["images"][0]["image"]["sha256"], boards["shots"][0]["image"]["sha256"])
        self.assertEqual(self.render_calls, [])

    def test_source_wrong_plan_contract_is_corrected_before_frame_regeneration(self):
        correct = copy.deepcopy(self.director["shots"][0])
        wrong = self.director["shots"][0]
        wrong["continuity"]["opening"] = copy.deepcopy(wrong["continuity"]["ending"])
        wrong["continuity"]["transitions"] = []
        wrong["start_state"] = "Mara already holds the lifted key."
        wrong["storyboard_prompt"] = "Mara already holds the key, the lifting action completed."
        def once(result, data):
            if self.codex_calls.count("motion") == 1:
                record = result["shots"][0]
                record["continuity_review"].update(failure_kind="plan", decision="repair-required",
                    violations=["The planned opening has completed the source lifting action prematurely."])
                record.update(plan_repair=[correct], blocking_issue="Restore the pre-lift state contract.",
                              storyboard_prompt=correct["storyboard_prompt"])
        self.review_transform = once
        plan, boards = self.prepare()
        self.assertEqual(plan["shots"][0]["continuity"]["opening"]["props"][0]["owner"], "none")
        self.assertEqual(self.codex_calls.count("motion"), 2)
        self.assertIn("repairs/01/001", boards["shots"][0]["image"]["file"])
        storyboard.require_accepted(plan, boards, self.run)

    def test_two_failed_repair_rounds_stop_before_video_inference_and_resume(self):
        self.review_transform = self.fail_first_frame
        with self.assertRaisesRegex(RuntimeError, "2 automatic repair rounds"):
            self.prepare()
        self.assertEqual(self.codex_calls.count("motion"), 3)
        repairs = json.loads((self.run / "storyboard-repairs.json").read_text())
        self.assertEqual(len(repairs["rounds"]), 2)
        diagnostic = json.loads((self.run / "storyboard-diagnostic.json").read_text())
        self.assertFalse(diagnostic["video_inference_allowed"])
        self.assertFalse((self.run / "shooting-script.json").exists())
        before = len(self.image_calls)
        with self.assertRaisesRegex(RuntimeError, "2 automatic repair rounds"):
            self.prepare()
        self.assertEqual(len(self.image_calls), before)
        self.assertEqual(self.render_calls, [])

    def test_failed_review_cannot_be_reused_even_when_image_files_and_hashes_match(self):
        plan, boards = self.prepare()
        report = json.loads((self.run / "storyboard-review.json").read_text())
        report["status"] = "rejected"
        self.write(self.run / "storyboard-review.json", report)
        with self.assertRaisesRegex(ValueError, "acceptance"):
            self.prepare()
        with self.assertRaisesRegex(ValueError, "acceptance"):
            self.render(plan, boards, resume=True)
        self.assertEqual(self.render_calls, [])

    def test_changed_cast_snapshot_or_contract_invalidates_acceptance(self):
        plan, boards = self.prepare()
        changed = copy.deepcopy(plan)
        changed["shots"][0]["storyboard_prompt"] += " Revised camera."
        with self.assertRaises(ValueError):
            self.render(changed, boards, resume=True)
        profile_path = self.run / "characters/mara/character.json"
        profile = json.loads(profile_path.read_text())
        profile["description"] = "A different physical performer description"
        self.write(profile_path, profile)
        with self.assertRaisesRegex(ValueError, "cast snapshot"):
            self.render(plan, boards, resume=True)
        self.assertEqual(self.render_calls, [])

    def test_changed_review_policy_invalidates_resume_acceptance(self):
        plan, boards = self.prepare()
        with patch.object(storyboard, "review_policy_fingerprint", return_value="f" * 64):
            with self.assertRaisesRegex(ValueError, "acceptance"):
                self.render(plan, boards, resume=True)
        self.assertEqual(self.render_calls, [])

    def test_verified_compatible_assets_can_be_copied_from_cache(self):
        plan, boards = self.prepare()
        destination = self.root / "cached-run"
        destination.mkdir()
        characters.snapshot_characters(self.profiles, destination, root=self.root)
        copied_plan, copied_boards = preproduction.prepare(
            self.request, self.profiles, destination, 999, token_counter=lambda text: len(text.split()))
        self.assertEqual(copied_plan, plan)
        self.assertEqual(copied_boards, boards)
        storyboard.require_accepted(copied_plan, copied_boards, destination)
        self.assertEqual(self.codex_calls, ["director", "motion", "fidelity"])

    def test_prop_body_and_empty_set_references_are_reused_without_prior_storyboard_pixels(self):
        plan, boards = self.prepare()
        self.assertEqual({r["id"] for r in boards["references"]}, {"prop:key", "body:MARA", "environment:1"})
        prop = next(r for r in boards["references"] if r["id"] == "prop:key")
        for entry in boards["shots"]:
            references = entry["generation"]["references"]
            self.assertTrue(any(r.get("reference_id") == "prop:key" and r["sha256"] == prop["image"]["sha256"] for r in references))
            self.assertFalse(any(r.get("purpose") == "same-scene-setting" for r in references))
        count = len(self.image_inputs)
        self.prepare()
        self.assertEqual(len(self.image_inputs), count)

    def test_overlapping_batches_carry_persistent_observations(self):
        engine_call = shooting_script.rust_engine.call

        def one_shot_batches(operation, **args):
            result = engine_call(operation, **args)
            return dict(result, batch_size=1) if operation == "continuity_settings" else result

        with patch.object(shooting_script.rust_engine, "call", side_effect=one_shot_batches):
            plan, boards = self.prepare()
        self.assertEqual([data["shots_to_direct"] for data in self.motion_inputs], [[1], [2]])
        self.assertEqual([b["shot"] for b in self.motion_inputs[1]["images_in_shot_order"]], [1, 2])
        self.assertEqual(self.motion_inputs[1]["persistent_review_state"][0]["shot"], 1)
        storyboard.require_accepted(plan, boards, self.run)

    def test_defective_prop_reference_repairs_all_dependent_frames_and_reinspects(self):
        def once(result, data):
            if self.codex_calls.count("motion") != 1:
                return
            for record in result["shots"]:
                finding = next(f for f in record["continuity_review"]["reference_findings"] if f["id"] == "prop:key")
                finding.update(decision="repair-required", evidence="Reference is a different physical object.")
                record.update(blocking_issue="Canonical reference pixels depict the wrong object.",
                              storyboard_prompt=data["plan"]["shots"][record["shot"] - 1]["storyboard_prompt"])
        self.review_transform = once
        plan, boards = self.prepare()
        self.assertEqual(self.codex_calls.count("motion"), 2)
        self.assertTrue(all("repairs/01/" in e["image"]["file"] for e in boards["shots"]))
        storyboard.require_accepted(plan, boards, self.run)

    def test_missing_portrait_stops_before_codex_or_model_checks(self):
        profile_path = self.root / "characters/mara/character.json"
        profile = json.loads(profile_path.read_text())
        profile.pop("image")
        self.write(profile_path, profile)
        self.mock(video, "check_environment")
        model = self.mock(video, "model_path")
        tokenizer = self.mock(video, "prompt_token_counter")
        with patch.dict(os.environ), self.assertRaisesRegex(ValueError, "saved image"):
            video.generate_screenplay(self.request, seed=17)
        self.assertEqual(self.codex_calls, [])
        self.assertEqual(self.image_calls, [])
        model.assert_not_called()
        tokenizer.assert_not_called()

    def test_unavailable_saved_voice_converter_stops_before_codex(self):
        profile_path = self.root / "characters/mara/character.json"
        sample = profile_path.parent / "voice.wav"
        with wave.open(str(sample), "wb") as audio:
            audio.setparams((1, 2, 24000, 0, "NONE", "not compressed"))
            audio.writeframes(b"\x01\x00" * 2400)
        profile = json.loads(profile_path.read_text())
        profile["voice_sample"] = {"file": "voice.wav", "sha256": self.digest(sample)}
        self.write(profile_path, profile)
        self.mock(video, "check_environment")
        model = self.mock(video, "model_path")
        # The converter is now launched by Rust. Substitute its executable at
        # the process boundary rather than mocking removed Python policy.
        converter = self.root / ".cache/seed-vc-env/bin/python"
        converter.parent.mkdir(parents=True)
        called = self.root / "voice-check-called"
        converter.write_text('#!/bin/sh\ntouch "' + str(called) + '"\necho "Seed-VC unavailable" >&2\nexit 1\n')
        converter.chmod(0o755)
        with patch.dict(os.environ), self.assertRaisesRegex(RuntimeError, "Seed-VC unavailable"):
            video.generate_screenplay(self.request, seed=17, voice_mode="saved")
        self.assertTrue(called.is_file())
        self.assertEqual(self.codex_calls, [])
        self.assertEqual(self.image_calls, [])
        model.assert_not_called()


if __name__ == "__main__":
    unittest.main()
