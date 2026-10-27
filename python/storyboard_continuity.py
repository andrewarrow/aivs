"""Schemas and legacy artifact codec for Rust-owned continuity decisions.

The Python pipeline retains artifact I/O during migration; aivs-core validates
states, transitions, observations, acceptance, and repair dependencies.
"""

from __future__ import annotations

import hashlib
import json
import rust_engine
from pathlib import Path

POLICY = "storyboard-continuity-v2"
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
    return digest({name: rust_engine.implementation_sha256(name)
                   for name in ("storyboard_continuity.py", "shooting_script.py",
                                "shooting-script-review-instructions.txt")})


def _text(value):
    if not isinstance(value, str) or not value.strip() or len(value) > 1200 or "\x00" in value:
        raise ValueError("Continuity facts must be nonempty bounded text; use unknown or not-visible.")
    return value


def facts(state):
    return {(entity, field): value for entity, field, value in rust_engine.call("facts", state=state)}


def physical_key(key):
    return key[1].startswith(("morphology.", "physical.")) or key[0].startswith("relationship:")


def canonical_physical(world):
    return {(entity, field): value for entity, field, value in
            rust_engine.call("canonical_physical", world=world)}


def validate_world(plan):
    return set(rust_engine.call("validate_world", plan=plan))


def validate_plan(plan):
    rust_engine.call("validate_plan", plan=plan)


def review_shot(plan, number, row):
    return rust_engine.call("review_shot", plan=plan, number=number, row=row)


def review_sequence(plan, rows):
    return {int(number): report for number, report in
            rust_engine.call("review_sequence", plan=plan, rows=rows).items()}


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
    rust_engine.call("validate_acceptance", plan=plan, boards=boards, report=report,
                     binding=report_binding(plan, boards), report_sha256=digest(report))
    return report


def dependent_shots(boards, changed):
    """Transitive image-reference dependencies, including legacy setting images."""
    return set(rust_engine.call("dependent_shots", boards=boards, changed=sorted(changed)))
