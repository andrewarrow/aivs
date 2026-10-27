"""Legacy digest adapter for Rust-owned canonical prop inspections."""
from pathlib import Path

import rust_engine

POLICY = "prop-structure-v2"
MAX_ATTEMPTS = 3


def generation_prompt(definition, replacement=None):
    options = {} if replacement is None else {"replacement": replacement}
    return rust_engine.call("prop_generation_prompt", definition=definition, **options)


def assess(definition, observation):
    return rust_engine.call("assess_prop", definition=definition, observation=observation)


def failure_summary(report):
    return rust_engine.call("prop_failure_summary", report=report)


def binding(spec, image):
    return {"policy": POLICY, "reviewer_sha256": rust_engine.implementation_sha256("prop_reference.py"),
            "definition_sha256": rust_engine.digest(spec), "image_sha256": image["sha256"]}


def require_accepted(spec, entry):
    return rust_engine.call("validate_prop_acceptance", spec=spec, entry=entry,
                            binding=binding(spec, entry["image"]))


def review(spec, entry, image_path, run_dir, checkpoint, *, source_sha256,
           on_activity=None, planning_budget_seconds=600, executable=None, on_activity_delta=None):
    expected = binding(spec, entry["image"])
    return rust_engine.call("prop_review", spec=spec, binding=expected,
        name="prop-reference-" + rust_engine.digest(expected)[:16], source_sha256=source_sha256,
        image_path=str(Path(image_path).resolve()), run_dir=str(Path(run_dir).resolve()),
        checkpoint=str(Path(checkpoint).resolve()), executable=str(executable) if executable is not None else None,
        limit=planning_budget_seconds, on_event=rust_engine.activity_events(on_activity, on_activity_delta))
