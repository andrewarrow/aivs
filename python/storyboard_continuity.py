"""Observable, source-backed scene states and exact-byte storyboard acceptance.

No image model lives here. The second director pass supplies pixel observations;
these checks prevent a plausible single image from certifying a broken sequence.
"""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path

POLICY = "storyboard-continuity-v1"
BATCH_SIZE = 6
UNKNOWN = {"unknown", "not-visible"}


def object_schema(properties):
    return {"type": "object", "additionalProperties": False,
            "required": list(properties), "properties": properties}


TEXT = {"type": "string"}
STRINGS = {"type": "array", "items": TEXT}
IDS = {"type": "array", "items": {"type": "integer"}}
ATTRIBUTES = {"type": "array", "items": object_schema({"key": TEXT, "value": TEXT})}
RELATIONSHIPS = {"type": "array", "items": object_schema({
    "subject": TEXT, "relation": TEXT, "object": TEXT, "value": TEXT,
})}
WORLD_SCHEMA = object_schema({
    "props": {"type": "array", "items": object_schema({
        "id": TEXT, "scene_ids": IDS, "type": TEXT, "shape": TEXT, "size": TEXT, "color": TEXT,
        "features": STRINGS,
        "components": {"type": "array", "items": object_schema({
            "id": TEXT, "type": TEXT, "shape": TEXT, "size": TEXT, "color": TEXT, "features": STRINGS,
        })},
        "relationships": RELATIONSHIPS, "reference_prompt": TEXT,
    })},
    "characters": {"type": "array", "items": object_schema({
        "role": TEXT, "morphology": ATTRIBUTES, "reference_prompt": TEXT,
    })},
    "environments": {"type": "array", "items": object_schema({
        "scene_id": {"type": "integer"}, "reference_prompt": TEXT,
    })},
})
STATE_SCHEMA = object_schema({
    "characters": {"type": "array", "items": object_schema({
        "role": TEXT, "presence": {"type": "string", "enum": ["visible", "offscreen", "absent", "unknown", "not-visible"]},
        "location": TEXT, "posture": TEXT, "costume": TEXT,
        "appearance": ATTRIBUTES, "morphology": ATTRIBUTES, "visibility": ATTRIBUTES,
    })},
    "props": {"type": "array", "items": object_schema({
        "id": TEXT, "owner": TEXT, "support": TEXT, "location": TEXT, "state": TEXT,
        "visibility": TEXT, "physical": ATTRIBUTES,
    })},
    "reveals": {"type": "array", "items": object_schema({"id": TEXT, "value": TEXT})},
    "relationships": RELATIONSHIPS,
})
EVIDENCE_SCHEMA = object_schema({"beat_ids": IDS, "quote": TEXT, "explanation": TEXT})
TRANSITION_SCHEMA = object_schema({
    "entity": TEXT, "field": TEXT, "before": TEXT, "after": TEXT,
    "timing": {"type": "string", "enum": ["during-shot", "across-cut"]},
    "mode": {"type": "string", "enum": ["on-screen", "offscreen"]},
    "beat_ids": IDS, "quote": TEXT, "motivation": TEXT,
    "physical_transformation": {"type": "boolean"}, "story_time": TEXT,
})
CONTRACT_SCHEMA = object_schema({
    "opening": STATE_SCHEMA, "ending": STATE_SCHEMA,
    "transitions": {"type": "array", "items": TRANSITION_SCHEMA},
})
REVIEW_SCHEMA = object_schema({
    "observed_opening_state": STATE_SCHEMA,
    "source_transition_evidence": {"type": "array", "items": EVIDENCE_SCHEMA},
    "violations": STRINGS, "evidence": TEXT, "neighbor_comparison": TEXT,
    "confidence": {"type": "number"},
    "decision": {"type": "string", "enum": ["pass", "repair-required", "uncertain"]},
    "failure_kind": {"type": "string", "enum": ["none", "image", "plan", "uncertain"]},
    "reference_findings": {"type": "array", "items": object_schema({
        "id": TEXT, "decision": {"type": "string", "enum": ["pass", "repair-required", "uncertain"]},
        "evidence": TEXT, "confidence": {"type": "number"},
    })},
})


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode()).hexdigest()


def policy_fingerprint():
    directory = Path(__file__).resolve().parent
    return digest({name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
                   for name in ("storyboard_continuity.py", "shooting_script.py",
                                "shooting-script-review-instructions.txt")})


def _text(value):
    if not isinstance(value, str) or not value.strip() or len(value) > 1200 or "\x00" in value:
        raise ValueError("Continuity facts must be nonempty bounded text; use unknown or not-visible.")
    return value


def facts(state):
    """Canonical (entity, field) keys, with screenplay roles and stable prop IDs."""
    result = {}
    if not isinstance(state, dict) or set(state) != {"characters", "props", "reveals", "relationships"}:
        raise ValueError("Continuity state needs characters, props and reveals arrays.")
    for group, prefix, identifier, fields in (
            ("characters", "character", "role", ("presence", "location", "posture", "costume")),
            ("props", "prop", "id", ("owner", "support", "location", "state", "visibility")),
            ("reveals", "reveal", "id", ("value",))):
        rows = state[group]
        if not isinstance(rows, list) or len(rows) > 64:
            raise ValueError("Continuity state arrays must contain at most 64 objects.")
        seen = set()
        for row in rows:
            attributes = ("appearance", "morphology", "visibility") if group == "characters" else ("physical",) if group == "props" else ()
            required = {identifier, *fields, *attributes}
            if not isinstance(row, dict) or set(row) != required:
                raise ValueError(f"Invalid {group} state object.")
            name = _text(row[identifier])
            if name in seen:
                raise ValueError(f"Duplicate continuity entity: {name}")
            seen.add(name)
            entity = f"{prefix}:{name}"
            for field in fields:
                result[entity, field] = _text(row[field])
            if group == "characters":
                if row["presence"] not in ("visible", "offscreen", "absent", "unknown", "not-visible"):
                    raise ValueError("Invalid character presence.")
            for category in attributes:
                if not isinstance(row[category], list) or len(row[category]) > 24:
                    raise ValueError("Physical/visibility facts must be compact key/value arrays.")
                for attribute in row[category]:
                    if not isinstance(attribute, dict) or set(attribute) != {"key", "value"}:
                        raise ValueError("Appearance needs key/value objects.")
                    key = entity, category + "." + _text(attribute["key"])
                    if key in result:
                        raise ValueError("Duplicate appearance attribute.")
                    result[key] = _text(attribute["value"])
    if not isinstance(state["relationships"], list) or len(state["relationships"]) > 128:
        raise ValueError("Physical relationships must be a compact array.")
    for relation in state["relationships"]:
        if not isinstance(relation, dict) or set(relation) != {"subject", "relation", "object", "value"}:
            raise ValueError("Invalid physical relationship.")
        key = "relationship:" + _text(relation["subject"]), _text(relation["relation"]) + ":" + _text(relation["object"])
        if key in result:
            raise ValueError("Duplicate physical relationship.")
        result[key] = _text(relation["value"])
    return result


def physical_key(key):
    return key[1].startswith(("morphology.", "physical.")) or key[0].startswith("relationship:")


def canonical_physical(world):
    values = {}
    for character in world["characters"]:
        values.update({("character:" + character["role"], "morphology." + a["key"]): a["value"]
                       for a in character["morphology"]})
    for prop in world["props"]:
        for entity in [prop, *prop["components"]]:
            values.update({("prop:" + entity["id"], "physical." + field): entity[field]
                           for field in ("type", "shape", "size", "color")})
            if entity["features"]:
                values["prop:" + entity["id"], "physical.distinguishing_features"] = "; ".join(entity["features"])
        for relation in prop["relationships"]:
            values["relationship:" + relation["subject"], relation["relation"] + ":" + relation["object"]] = relation["value"]
    return values


def validate_world(plan):
    world = plan.get("world")
    if not isinstance(world, dict) or set(world) != set(WORLD_SCHEMA["properties"]):
        raise ValueError("Storyboard plan needs canonical props, character morphology and environments.")
    roles = {c["role"] for c in plan["source"]["cast"]}
    if not isinstance(world["characters"], list) or {c.get("role") for c in world["characters"]} != roles or len(world["characters"]) != len(roles):
        raise ValueError("Canonical morphology must bind each source role exactly once.")
    for character in world["characters"]:
        _text(character["reference_prompt"])
        # Reuse the generic attribute validation, without inferring body facts.
        facts({"characters": [{"role": character["role"], "presence": "unknown", "location": "unknown",
                               "posture": "unknown", "costume": "unknown", "appearance": [],
                               "morphology": character["morphology"], "visibility": []}],
               "props": [], "reveals": [], "relationships": []})
    ids = set()
    if not isinstance(world["props"], list) or len(world["props"]) > 64:
        raise ValueError("Canonical props must be a compact array.")
    for prop in world["props"]:
        _text(prop["reference_prompt"])
        for entity in [prop, *prop["components"]]:
            name = _text(entity["id"])
            if name in ids:
                raise ValueError("Canonical prop/component IDs must be globally unique.")
            ids.add(name)
            for field in ("type", "shape", "size", "color"):
                _text(entity[field])
            if not isinstance(entity["features"], list):
                raise ValueError("Canonical features must be a list.")
            for feature in entity["features"]:
                _text(feature)
        if (not isinstance(prop.get("scene_ids"), list) or not prop["scene_ids"]
                or any(type(n) is not int or n not in {s["scene_id"] for s in plan["shots"]} for n in prop["scene_ids"])):
            raise ValueError("Canonical props need their relevant source scene IDs.")
        facts({"characters": [], "props": [], "reveals": [], "relationships": prop["relationships"]})
    for prop in world["props"]:
        for relation in prop["relationships"]:
            if relation["subject"] not in ids or relation["object"] not in ids:
                raise ValueError("Canonical component relationships must bind real physical entity IDs.")
    scenes = {s["scene_id"] for s in plan["shots"]}
    if (not isinstance(world["environments"], list)
            or {e.get("scene_id") for e in world["environments"]} != scenes
            or len(world["environments"]) != len(scenes)):
        raise ValueError("Each scene needs one reusable clean environment reference.")
    for environment in world["environments"]:
        _text(environment["reference_prompt"])
    return ids


def source_evidence(evidence, beats, allowed):
    ids = evidence.get("beat_ids")
    quote = evidence.get("quote")
    if (not isinstance(ids, list) or not ids or any(type(n) is not int or n not in allowed for n in ids)
            or len(ids) != len(set(ids)) or not isinstance(quote, str) or not quote.strip()
            or not any(quote in beats[n]["text"] for n in ids)):
        raise ValueError("Continuity transition requires an exact quote from its authorized source beats.")


def _changes(before, after):
    return [(key, value, after[key]) for key, value in before.items() if key in after
            and value not in UNKNOWN and after[key] not in UNKNOWN and value != after[key]]


def validate_plan(plan):
    """Require explanations for actual changes; never equate a cut with a reset."""
    roles = {row["role"] for row in plan["source"]["cast"]}
    beats = {beat["id"]: beat for beat in plan["source"]["beats"]}
    prop_ids = validate_world(plan)
    previous = None
    physical = canonical_physical(plan["world"])
    known = dict(physical)
    for number, shot in enumerate(plan["shots"], 1):
        contract = shot.get("continuity")
        if not isinstance(contract, dict) or set(contract) != {"opening", "ending", "transitions"}:
            raise ValueError(f"Shot {number} needs a structured continuity contract.")
        opening, ending = facts(contract["opening"]), facts(contract["ending"])
        for state in (opening, ending):
            if not set(physical) <= set(state):
                raise ValueError(f"Shot {number} must retain canonical physical facts even when concealed.")
            if any(state[key] in UNKNOWN and physical[key] not in UNKNOWN for key in physical):
                raise ValueError(f"Shot {number} must not forget known physical identity when it leaves the crop.")
        if number == 1 and _changes(physical, opening):
            raise ValueError("The first opening must match canonical physical definitions.")
        for state in (contract["opening"], contract["ending"]):
            if {row["role"] for row in state["characters"]} != roles:
                raise ValueError(f"Shot {number} must track presence of every source role, including offscreen roles.")
            for prop in state["props"]:
                if prop["id"] not in prop_ids:
                    raise ValueError(f"Shot {number} invents a prop/component identity: {prop['id']}")
                if prop["owner"] not in roles | UNKNOWN | {"none"}:
                    raise ValueError(f"Shot {number} prop owners must be source roles, none or unknown.")
        visible = [row["role"] for row in contract["opening"]["characters"] if row["presence"] == "visible"]
        if (not isinstance(shot.get("opening_visible_roles"), list)
                or len(set(shot["opening_visible_roles"])) != len(shot["opening_visible_roles"])
                or set(shot["opening_visible_roles"]) != set(visible)
                or not set(visible) <= set(shot["visible_roles"])):
            raise ValueError(f"Shot {number} opening_visible_roles must match time-zero presence.")
        transitions = contract["transitions"]
        if not isinstance(transitions, list) or len(transitions) > 128:
            raise ValueError("Continuity transitions must be a compact array.")
        history = known if previous is None or previous["scene_id"] == shot["scene_id"] else {
            key: value for key, value in known.items()
            if physical_key(key) or key[1] == "owner" or key[0].startswith("reveal:")}
        expected = ([] if previous is None else [("across-cut", key, a, b)
                    for key, a, b in _changes(history, opening)])
        current = dict(history)
        current.update({key: value for key, value in opening.items() if value not in UNKNOWN})
        expected += [("during-shot", key, a, b) for key, a, b in _changes(current, ending)]
        recorded = set()
        paths = {}
        for transition in transitions:
            if not isinstance(transition, dict) or set(transition) != set(TRANSITION_SCHEMA["properties"]):
                raise ValueError(f"Shot {number} has an invalid continuity transition.")
            for field in ("entity", "field", "before", "after", "motivation"):
                _text(transition[field])
            timing = transition["timing"]
            if timing not in ("during-shot", "across-cut") or transition["mode"] not in ("on-screen", "offscreen"):
                raise ValueError("Invalid transition timing or visibility.")
            if timing == "across-cut" and (previous is None or transition["mode"] != "offscreen"):
                raise ValueError("An across-cut transition needs a preceding shot and motivated offscreen action.")
            # A cut can elide mechanics already authorized by the prior coverage;
            # it cannot consume a future beat before its intended opening.
            allowed = set(shot["beat_ids"] if timing == "during-shot" else previous["beat_ids"])
            # A new scene's explicit time jump/transformation may authorize a
            # changed physical state at its opening, unlike an ordinary cut.
            if timing == "across-cut" and previous["scene_id"] != shot["scene_id"]:
                allowed |= set(shot["beat_ids"])
            source_evidence(transition, beats, allowed)
            _text(transition["story_time"])
            if type(transition["physical_transformation"]) is not bool:
                raise ValueError("Transitions must distinguish physical transformations from visibility changes.")
            if (transition["field"].startswith(("morphology.", "physical."))
                    and not transition["physical_transformation"]):
                raise ValueError("Physical identity/body changes require a source-backed transformation or sufficient story time.")
            key = timing, (transition["entity"], transition["field"]), transition["before"], transition["after"]
            if key in recorded:
                raise ValueError("Duplicate continuity transition.")
            recorded.add(key)
            paths.setdefault((timing, key[1]), []).append(transition)
        for timing, key, before, after in expected:
            cursor = before
            for event in paths.get((timing, key), []):
                if cursor not in UNKNOWN and event["before"] != cursor:
                    raise ValueError(f"Shot {number} transition starts from the wrong known state: {key}")
                cursor = event["after"]
            if cursor != after:
                raise ValueError(f"Shot {number} unexplained {timing} change: {key} {before} -> {after}")
        # Also check round trips whose endpoints are equal and reject invented
        # transition records that do not describe the actual incoming state.
        for (timing, key), events in paths.items():
            start_facts, end_facts = (history, opening) if timing == "across-cut" else (current, ending)
            if key not in start_facts or key not in end_facts:
                raise ValueError("A transition must name a tracked entity and field.")
            cursor = start_facts[key]
            for event in events:
                if cursor not in UNKNOWN and event["before"] != cursor:
                    raise ValueError(f"Shot {number} transition starts from the wrong known state: {key}")
                cursor = event["after"]
            if end_facts[key] not in UNKNOWN and cursor != end_facts[key]:
                raise ValueError(f"Shot {number} transition does not reach its recorded endpoint: {key}")
        previous = shot
        known = current
        known.update({key: value for key, value in ending.items() if value not in UNKNOWN})
        physical.update({key: value for key, value in ending.items()
                         if physical_key(key) and value not in UNKNOWN})


def review_shot(plan, number, row):
    """Validate observations, then supplement the visual judgment with state checks."""
    shot = plan["shots"][number - 1]
    review = row.get("continuity_review")
    if not isinstance(review, dict) or set(review) != set(REVIEW_SCHEMA["properties"]):
        raise ValueError(f"Shot {number} needs a complete sequence continuity review.")
    observed = facts(review["observed_opening_state"])
    expected = facts(shot["continuity"]["opening"])
    confidence = review["confidence"]
    if type(confidence) not in (int, float) or not math.isfinite(confidence) or not 0 <= confidence <= 1:
        raise ValueError("Visual evidence confidence must be between zero and one.")
    for field in ("evidence", "neighbor_comparison"):
        _text(review[field])
    if review["decision"] not in ("pass", "repair-required", "uncertain"):
        raise ValueError("Invalid storyboard review decision.")
    if review["failure_kind"] not in ("none", "image", "plan", "uncertain"):
        raise ValueError("Invalid storyboard failure kind.")
    violations = review["violations"]
    if not isinstance(violations, list):
        raise ValueError("Storyboard violations must be a list.")
    violations = [_text(v) for v in violations]
    beats = {beat["id"]: beat for beat in plan["source"]["beats"]}
    evidence = review["source_transition_evidence"]
    if not isinstance(evidence, list):
        raise ValueError("Storyboard transition evidence must be a list.")
    allowed = {n for s in plan["shots"][:number] for n in s["beat_ids"]}
    for item in evidence:
        source_evidence(item, beats, allowed)
        _text(item.get("explanation"))
    evidence = list(evidence)
    events = [t for t in shot["continuity"]["transitions"] if t["timing"] == "across-cut"]
    if number > 1:
        events += [t for t in plan["shots"][number - 2]["continuity"]["transitions"] if t["timing"] == "during-shot"]
    for event in events:
        item = {"beat_ids": event["beat_ids"], "quote": event["quote"],
                "explanation": f"Authorized {event['entity']} {event['field']}: {event['before']} -> {event['after']}. {event['motivation']}"}
        if item not in evidence:
            evidence.append(item)
    # Wording of location/costume facts can differ; the visual reviewer compares
    # those semantically. Role, ownership, presence and reveal IDs are categorical.
    critical = {key for key in expected if key[1] in ("presence", "owner", "value", "costume", "state", "visibility")
                or key[1].startswith("visibility.") or physical_key(key)}
    for key in critical:
        actual = observed.get(key, "unknown")
        if expected[key] in UNKNOWN:
            continue
        if actual in UNKNOWN:
            # Offscreen props may be unobservable. Required visible roles and
            # revealed audience information must have positive pixel evidence.
            if expected[key] in ("visible", "revealed") or (key[1] == "owner" and expected[key] in shot["opening_visible_roles"]):
                violations.append(f"Cannot verify {key}: expected {expected[key]}, observed {actual}")
        elif actual != expected[key]:
            violations.append(f"{key}: expected {expected[key]}, observed {actual}")
    for character in review["observed_opening_state"]["characters"]:
        if character["presence"] == "visible" and character["role"] not in shot["opening_visible_roles"]:
            violations.append(f"Unscripted opening presence: {character['role']}")
    observed_props = {p["id"]: p for p in review["observed_opening_state"]["props"]}
    for relation in review["observed_opening_state"]["relationships"]:
        if relation["relation"] == "docked-in" and relation["value"] == "yes":
            subject = observed_props.get(relation["subject"], {})
            support = observed_props.get(relation["object"], {})
            if (subject.get("owner", "unknown") not in UNKNOWN | {"none"}
                    and subject.get("support", "unknown") not in UNKNOWN | {relation["object"]}
                    and support.get("owner", "unknown") not in UNKNOWN | {subject["owner"]}):
                violations.append(f"Component {relation['subject']} is both docked and held by {subject['owner']}")
    decision = review["decision"]
    if violations or row.get("blocking_issue", "").strip():
        decision = "repair-required"
    elif confidence < .8 or review["failure_kind"] == "uncertain":
        decision = "uncertain"
    if review["failure_kind"] == "plan" and decision == "pass":
        decision = "repair-required"
    return {"shot": number, "expected_opening_state": shot["continuity"]["opening"],
            **review, "source_transition_evidence": evidence,
            "violations": list(dict.fromkeys(violations)), "decision": decision,
            "unobservable_facts": [list(key) for key in expected if observed.get(key, "unknown") in UNKNOWN]}


def review_sequence(plan, rows):
    """Carry actual known observations across occlusion and overlapping batches.

    Even unknown planned geometry cannot license reinventing an object once its
    earlier pixels established a design. Opening stills can differ legitimately
    when intervening source-backed transitions account for the new state.
    """
    reports = {n: review_shot(plan, n, row) for n, row in sorted(rows.items())}
    known = {}
    for number, report in reports.items():
        # A diagnosed wrong image must not become the physical authority for
        # an otherwise correct neighbor, forcing needless cascading repairs.
        if report["decision"] != "pass":
            continue
        for key, actual in facts(report["observed_opening_state"]).items():
            if actual in UNKNOWN or not (physical_key(key) or key[1] == "owner" or key[0].startswith("reveal:")):
                continue
            if key in known:
                earlier, value = known[key]
                supported = value
                # Only events before the current opening can explain a change.
                for n in range(earlier, number + 1):
                    transitions = plan["shots"][n - 1]["continuity"]["transitions"]
                    for timing in ("across-cut", "during-shot"):
                        if (n == earlier and timing == "across-cut") or (n == number and timing == "during-shot"):
                            continue
                        for event in transitions:
                            if (event["timing"] == timing and (event["entity"], event["field"]) == key
                                    and (event["before"] == supported or event["before"] in UNKNOWN)):
                                supported = event["after"]
                if actual != supported:
                    report["violations"].append(
                        f"Sequence contradiction with frame {earlier}: {key} was {value}, now {actual}; "
                        f"source-backed intervening transitions support {supported}.")
                    report["decision"] = "repair-required"
            if report["decision"] == "pass":
                known[key] = number, actual
    return reports


def report_binding(plan, boards):
    return {"policy": POLICY, "source_sha256": plan["source_sha256"],
            "review_policy_sha256": policy_fingerprint(),
            "source_snapshot_sha256": digest(plan["source"]),
            "plan_sha256": digest(plan),
            "references": boards.get("references", []),
            "images": [{"shot": e["shot"], "image": e["image"]} for e in boards["shots"]]}


def require_accepted(plan, boards, run_dir):
    """Mandatory even for Resume, cached assets, and direct renderer callers."""
    import storyboard
    validate_plan(plan)
    if (boards.get("policy") != storyboard.POLICY or boards.get("status") != "complete"
            or boards.get("source_sha256") != plan.get("source_sha256")):
        raise ValueError("Storyboard acceptance requires a complete compatible manifest for this exact source.")
    storyboard.prepare(plan, boards, run_dir)
    storyboard.prepare_references(plan, boards, run_dir)
    screenplay = Path(run_dir) / "screenplay.txt"
    if screenplay.is_file() and screenplay.read_text(encoding="utf-8") != plan["source"]["script"]:
        raise ValueError("The screenplay snapshot changed after storyboard acceptance.")
    import characters
    for supplied in plan["source"].get("characters", []):
        saved = characters._read_profile(Path(run_dir) / "characters" / supplied["id"] / "character.json")
        if any(saved.get(key) != value for key, value in supplied.items()):
            raise ValueError("The cast snapshot changed after storyboard acceptance.")
    path = Path(run_dir) / "storyboard-review.json"
    if not path.is_file():
        raise ValueError("Storyboard sequence has no accepted continuity review; stop before video inference.")
    report = json.loads(path.read_text(encoding="utf-8"))
    receipt = boards.get("continuity_acceptance")
    rows = report.get("shots", [])
    if (report.get("version") != 1 or report.get("policy") != POLICY
            or report.get("binding") != report_binding(plan, boards) or report.get("status") != "accepted"
            or not isinstance(receipt, dict) or receipt.get("policy") != POLICY
            or receipt.get("review_sha256") != digest(report)
            or not isinstance(rows, list) or len(rows) != len(plan["shots"])
            or [r.get("shot") for r in rows] != list(range(1, len(rows) + 1))
            or any(r.get("decision") != "pass" or r.get("violations")
                   or r.get("confidence", 0) < .8
                   or {f.get("id") for f in r.get("reference_findings", [])} != {f["id"] for f in boards["references"]}
                   or any(f.get("decision") != "pass" or f.get("confidence", 0) < .8 for f in r.get("reference_findings", []))
                   for r in rows)):
        raise ValueError("Storyboard continuity acceptance is failed, stale or incompatible; stop before video inference.")
    for row in rows:
        review = {key: row[key] for key in REVIEW_SCHEMA["properties"]}
        if (row.get("expected_opening_state") != plan["shots"][row["shot"] - 1]["continuity"]["opening"]
                or review_shot(plan, row["shot"], {"continuity_review": review})["decision"] != "pass"):
            raise ValueError("Storyboard acceptance contradicts its state contract or pixel observations.")
    reconstructed = {r["shot"]: {"continuity_review": {key: r[key] for key in REVIEW_SCHEMA["properties"]}} for r in rows}
    if any(r["decision"] != "pass" for r in review_sequence(plan, reconstructed).values()):
        raise ValueError("Storyboard acceptance contains a sequence-level physical or ownership contradiction.")
    return report


def dependent_shots(boards, changed):
    """Transitive image-reference dependencies, including legacy setting images."""
    affected = set(changed)
    while True:
        old = set(affected)
        assets = {(e["image"]["file"], e["image"]["sha256"]) for e in boards["shots"] if e["shot"] in affected}
        for entry in boards["shots"]:
            if any((r.get("file"), r.get("sha256")) in assets
                   for r in entry.get("generation", {}).get("references", [])):
                affected.add(entry["shot"])
        if affected == old:
            return affected
