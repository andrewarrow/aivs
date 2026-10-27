"""Compatibility entry points for Rust-owned storyboard production."""
from pathlib import Path
import rust_engine
import storyboard_backend
from production_adapter import call

POLICY = "per-shot-first-frame-v2"


def _save(path, value):
    call("write_json", path=Path(path), value=value)


def planned_shot_sha256(plan, shot):
    content = {"source_sha256": plan.get("source_sha256"), "planning_policy": plan.get("planning_policy"), "shot": shot}
    if "world" in plan:
        content["world"] = plan["world"]
    return rust_engine.digest(content)


def resolve_image(binding, run_dir):
    return Path(call("resolve_image", binding=binding, run_dir=Path(run_dir)))


def prepare(plan, manifest, run_dir):
    return call("board_prepare", plan=plan, boards=manifest, run_dir=Path(run_dir))


def require_accepted(plan, boards, run_dir):
    return call("board_accepted", plan=plan, boards=boards, run_dir=Path(run_dir))


def prepare_references(plan, boards, run_dir):
    return call("board_references", plan=plan, boards=boards, run_dir=Path(run_dir))


def review_policy_fingerprint():
    return call("review_policy")


def review_binding(plan, boards):
    return call("review_binding", plan=plan, boards=boards)


def reference_specs(plan):
    return rust_engine.call("storyboard_reference_specs", plan=plan)


def visible_state_prompt(plan, shot):
    return rust_engine.call("storyboard_visible_state", plan=plan, shot=shot)


def generate_references(plan, profiles, run_dir, seed, **options):
    options.update(options.pop("review_options", None) or {})
    return call("reference_generate", plan=plan, profiles=profiles, run_dir=Path(run_dir), seed=seed, **options)


def generate(plan, profiles, run_dir, seed, **options):
    options.update(options.pop("review_options", None) or {})
    return call("storyboards", plan=plan, profiles=profiles, run_dir=Path(run_dir), seed=seed, **options)


def regenerate_shot(plan, profiles, run_dir, seed, shot_index, **options):
    return call("regenerate", plan=plan, profiles=profiles, run_dir=Path(run_dir), seed=seed, shot_index=shot_index, **options)
