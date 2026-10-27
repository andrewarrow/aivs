"""Mocked physical-world/sequence contracts. No GPU, subprocesses or image models.

Written for explicit local execution by the user; not run during implementation.
"""

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import preproduction
import storyboard
import storyboard_continuity as continuity


def attributes(**values):
    return [{"key": key, "value": value} for key, value in values.items()]


def character(role, presence="visible"):
    return {"role": role, "presence": presence, "location": "room", "posture": "standing",
            "costume": "coat", "appearance": [], "visibility": attributes(condition="concealed"),
            "morphology": attributes(condition="pregnant", abdomen_size="source-plausible") if role == "A" else []}


def entity(name, kind, shape):
    return {"id": name, "type": kind, "shape": shape, "size": "hand-sized", "color": "white", "features": []}


def fixture():
    prop = {**entity("phone", "corded-landline", "base-with-receiver"), "scene_ids": [1],
            "components": [entity("receiver", "receiver", "curved-with-earpieces"), entity("base", "base", "sloped"),
                           entity("cord", "cord", "coiled")],
            "relationships": [{"subject": "receiver", "relation": "connected-to", "object": "base", "value": "yes"}],
            "reference_prompt": "One white corded telephone, receiver, base and connecting coiled cord."}
    world = {"props": [prop], "characters": [{"role": "A", "morphology": character("A")["morphology"],
                                               "reference_prompt": "Plausible consistently pregnant full-body performer."},
                                              {"role": "B", "morphology": [], "reference_prompt": "Neutral body."}],
             "environments": [{"scene_id": 1, "reference_prompt": "Empty room with a chair, no people or story props."}]}
    props = [{"id": e["id"], "owner": "A" if e["id"] == "receiver" else "none",
              "support": "hand" if e["id"] == "receiver" else "chair", "location": "room", "state": "intact", "visibility": "visible",
              "physical": [{"key": k, "value": e[k]} for k in ("type", "shape", "size", "color")]}
             for e in [prop, *prop["components"]]]
    state = {"characters": [character("A"), character("B")], "props": props,
             "reveals": [{"id": "condition", "value": "concealed"}],
             "relationships": prop["relationships"] + [{"subject": "receiver", "relation": "docked-in", "object": "base", "value": "no"}]}
    shots = [{"scene_id": 1, "visible_roles": ["A", "B"], "opening_visible_roles": ["A", "B"], "beat_ids": [n],
              "storyboard_prompt": "Two performers in a room with one canonical landline.",
              "continuity": {"opening": copy.deepcopy(state), "ending": copy.deepcopy(state), "transitions": []}}
             for n in (1, 2)]
    return {"source_sha256": "source", "source": {"cast": [{"role": "A", "character": "actor-a"}, {"role": "B", "character": "actor-b"}],
                                                  "beats": [{"id": 1, "text": "A removes a wet coat out of view while B listens."},
                                                            {"id": 2, "text": "A reveals the concealed condition. Months later her body changes."}]},
            "world": world, "shots": shots}


def row(plan, number):
    return {"shot": number, "blocking_issue": "", "plan_repair": [], "world_repair": [],
            "continuity_review": {
                "observed_opening_state": copy.deepcopy(plan["shots"][number - 1]["continuity"]["opening"]),
                "source_transition_evidence": [], "violations": [], "evidence": "Visible physical object and performers match.",
                "neighbor_comparison": "Same object and bodies across the adjacent cut.",
                "confidence": .96, "decision": "pass", "failure_kind": "none", "reference_findings": []}}


def transition(plan, field, before, after, *, entity_id="character:A", timing="during-shot", beat=1, physical=False):
    return {"entity": entity_id, "field": field, "before": before, "after": after, "timing": timing,
            "mode": "offscreen", "beat_ids": [beat], "quote": plan["source"]["beats"][beat - 1]["text"],
            "motivation": "Source-supported motivated cut preserves cause and effect.",
            "physical_transformation": physical, "story_time": "months" if physical else "continuous"}


class StoryboardContinuityTests(unittest.TestCase):
    def setUp(self):
        self.plan = fixture()

    def review(self, result, number=2):
        return continuity.review_shot(self.plan, number, result)

    def test_two_correct_frames_with_consistent_identity_pass(self):
        continuity.validate_plan(self.plan)
        self.assertEqual(self.review(row(self.plan, 1), 1)["decision"], "pass")
        self.assertEqual(self.review(row(self.plan, 2))["decision"], "pass")

    def test_individually_plausible_frames_cannot_reset_unspecified_geometry(self):
        # The source did not specify a size, but frame one establishes it.
        self.plan["world"]["props"][0]["components"][0]["size"] = "unknown"
        for shot in self.plan["shots"]:
            for state in ("opening", "ending"):
                shot["continuity"][state]["props"][1]["physical"][2]["value"] = "unknown"
        continuity.validate_plan(self.plan)
        first, second = row(self.plan, 1), row(self.plan, 2)
        first["continuity_review"]["observed_opening_state"]["props"][1]["physical"][2]["value"] = "hand-sized"
        second["continuity_review"]["observed_opening_state"]["props"][1]["physical"][2]["value"] = "oversized"
        self.assertEqual(self.review(first, 1)["decision"], "pass")
        self.assertEqual(self.review(second)["decision"], "pass")
        sequence = continuity.review_sequence(self.plan, {1: first, 2: second})
        self.assertEqual(sequence[2]["decision"], "repair-required")
        self.assertTrue(any("Sequence contradiction" in v for v in sequence[2]["violations"]))

    def test_unexplained_grocery_or_any_prop_ownership_swap_is_rejected(self):
        # Same generic ownership constraint works regardless of the prop's name.
        self.plan["shots"][1]["continuity"]["opening"]["props"][1]["owner"] = "B"
        self.plan["shots"][1]["continuity"]["ending"]["props"][1]["owner"] = "B"
        with self.assertRaisesRegex(ValueError, "unexplained across-cut"):
            continuity.validate_plan(self.plan)
        result = row(fixture(), 2)
        result["continuity_review"]["observed_opening_state"]["props"][1]["owner"] = "B"
        review = continuity.review_shot(fixture(), 2, result)
        self.assertEqual(review["decision"], "repair-required")
        self.assertTrue(any("owner" in v for v in review["violations"]))

    def test_character_enters_before_scripted_entrance(self):
        for state in ("opening", "ending"):
            self.plan["shots"][0]["continuity"][state]["characters"][1]["presence"] = "offscreen"
        self.plan["shots"][0]["opening_visible_roles"] = ["A"]
        result = row(self.plan, 1)
        result["continuity_review"]["observed_opening_state"]["characters"][1]["presence"] = "visible"
        self.assertEqual(self.review(result, 1)["decision"], "repair-required")

    def test_premature_costume_change_or_reveal_blocks(self):
        for field, value in (("costume", "coat-off"), ("reveal", "revealed")):
            result = row(self.plan, 1)
            observed = result["continuity_review"]["observed_opening_state"]
            if field == "costume":
                observed["characters"][0][field] = value
            else:
                observed["reveals"][0]["value"] = value
            self.assertEqual(self.review(result, 1)["decision"], "repair-required")

    def test_legitimate_offscreen_garment_removal_across_motivated_cut_passes(self):
        for state in ("opening", "ending"):
            self.plan["shots"][1]["continuity"][state]["characters"][0]["costume"] = "coat-off"
        self.plan["shots"][1]["continuity"]["transitions"] = [
            transition(self.plan, "costume", "coat", "coat-off", timing="across-cut")]
        continuity.validate_plan(self.plan)
        self.assertEqual(self.review(row(self.plan, 2))["decision"], "pass")

    def test_corded_landline_becomes_smartphone(self):
        result = row(self.plan, 2)
        result["continuity_review"]["observed_opening_state"]["props"][1]["physical"][0]["value"] = "smartphone"
        self.assertEqual(self.review(result)["decision"], "repair-required")

    def test_receiver_shape_changes_during_handoff(self):
        result = row(self.plan, 2)
        result["continuity_review"]["observed_opening_state"]["props"][1]["physical"][1]["value"] = "flat-slab"
        self.assertEqual(self.review(result)["decision"], "repair-required")

    def test_receiver_appears_docked_and_held(self):
        result = row(self.plan, 2)
        result["continuity_review"]["observed_opening_state"]["relationships"][1]["value"] = "yes"
        review = self.review(result)
        self.assertEqual(review["decision"], "repair-required")
        self.assertTrue(any("both docked and held" in v for v in review["violations"]))

    def test_body_geometry_changes_without_source_supported_transformation(self):
        result = row(self.plan, 2)
        result["continuity_review"]["observed_opening_state"]["characters"][0]["morphology"][1]["value"] = "dramatically-larger"
        self.assertEqual(self.review(result)["decision"], "repair-required")
        for state in ("opening", "ending"):
            self.plan["shots"][1]["continuity"][state]["characters"][0]["morphology"][1]["value"] = "dramatically-larger"
        self.plan["shots"][1]["continuity"]["transitions"] = [
            transition(self.plan, "morphology.abdomen_size", "source-plausible", "dramatically-larger", timing="across-cut")]
        with self.assertRaisesRegex(ValueError, "Physical identity/body changes"):
            continuity.validate_plan(self.plan)

    def test_physically_consistent_concealed_body_then_reveal(self):
        self.plan["source"]["beats"][0]["text"] = "A removes the coat and reveals the existing condition."
        first = self.plan["shots"][0]["continuity"]
        first["ending"]["characters"][0]["visibility"][0]["value"] = "visible"
        first["ending"]["characters"][0]["costume"] = "coat-off"
        first["ending"]["reveals"][0]["value"] = "revealed"
        first["transitions"] = [transition(self.plan, "visibility.condition", "concealed", "visible"),
                                transition(self.plan, "costume", "coat", "coat-off"),
                                transition(self.plan, "value", "concealed", "revealed", entity_id="reveal:condition")]
        for state in ("opening", "ending"):
            self.plan["shots"][1]["continuity"][state] = copy.deepcopy(first["ending"])
        continuity.validate_plan(self.plan)
        result = row(self.plan, 1)
        # Concealment is not proof of a different anatomy, nor proof of its size.
        result["continuity_review"]["observed_opening_state"]["characters"][0]["morphology"][1]["value"] = "not-visible"
        reviewed = self.review(result, 1)
        self.assertEqual(reviewed["decision"], "pass")
        self.assertIn(["character:A", "morphology.abdomen_size"], reviewed["unobservable_facts"])
        self.assertEqual(self.review(row(self.plan, 2))["decision"], "pass")
        self.assertTrue(all(r["decision"] == "pass" for r in continuity.review_sequence(
            self.plan, {1: result, 2: row(self.plan, 2)}).values()))

    def test_legitimate_time_jump_permits_physical_change(self):
        self.plan["shots"][1]["scene_id"] = 2
        self.plan["world"]["environments"].append({"scene_id": 2, "reference_prompt": "The room months later."})
        for state in ("opening", "ending"):
            self.plan["shots"][1]["continuity"][state]["characters"][0]["morphology"][1]["value"] = "changed-after-months"
        self.plan["shots"][1]["continuity"]["transitions"] = [
            transition(self.plan, "morphology.abdomen_size", "source-plausible", "changed-after-months",
                       timing="across-cut", beat=2, physical=True)]
        continuity.validate_plan(self.plan)
        self.assertEqual(self.review(row(self.plan, 2))["decision"], "pass")
        self.assertTrue(all(r["decision"] == "pass" for r in continuity.review_sequence(
            self.plan, {1: row(self.plan, 1), 2: row(self.plan, 2)}).values()))

    def test_uncertain_critical_observation_cannot_approve(self):
        result = row(self.plan, 2)
        result["continuity_review"].update(decision="uncertain", confidence=.5, failure_kind="uncertain")
        self.assertEqual(self.review(result)["decision"], "uncertain")

    def test_upstream_repair_invalidates_transitive_downstream_images(self):
        entries = [{"shot": n, "image": {"file": f"{n}.png", "sha256": str(n)},
                    "generation": {"references": [] if n in (1, 4) else [{"file": f"{n - 1}.png", "sha256": str(n - 1)}]}}
                   for n in range(1, 5)]
        self.assertEqual(continuity.dependent_shots({"shots": entries}, {1}), {1, 2, 3})

    def test_generation_excludes_late_entrants_and_previous_character_filled_reference(self):
        shot = copy.deepcopy(self.plan["shots"][0])
        shot["opening_visible_roles"] = []
        shot["continuity"]["opening"]["characters"] = []
        with patch.object(storyboard.storyboard_backend, "configuration", return_value={}):
            request = storyboard._frame_request(self.plan, shot, [], Path("/nonexistent-run"),
                                                {"image": {"file": "old.png", "sha256": "old"}}, 1, 7, 1280, 768)
        self.assertEqual(request["references"], [])
        self.assertIn("Do not depict A, B", request["prompt"])
        self.assertNotIn("old.png", json.dumps(request))

    def test_repair_reservation_includes_downstream_but_preserves_unrelated_frames(self):
        with tempfile.TemporaryDirectory() as temporary:
            run = Path(temporary)
            plan = copy.deepcopy(self.plan)
            for shot in plan["shots"]:
                shot["seconds"] = 5
            boards = {"shots": [
                {"shot": 1, "image": {"file": "1.png", "sha256": "a"}, "generation": {"references": []}},
                {"shot": 2, "image": {"file": "2.png", "sha256": "b"}, "generation": {"references": [{"file": "1.png", "sha256": "a"}]}}]}
            state = {"rounds": []}
            issue = {"shot": 1, "issue": "Wrong owner in pixels.", "storyboard_prompt": "A holds the canonical prop."}
            preproduction._reserve_storyboard_repair(plan, [issue], state, 42, run, boards=boards)
            self.assertEqual([i["shot"] for i in state["rounds"][0]["issues"]], [1, 2])
            self.assertEqual(boards["shots"][1]["image"]["file"], "2.png")


if __name__ == "__main__":
    unittest.main()
