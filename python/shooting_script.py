"""Compatibility names for the Rust screenplay planner."""
from pathlib import Path
import json
import rust_engine
from production_adapter import call

DEFAULT_PLANNING_BUDGET_SECONDS = 600
PLANNER_VERSION = "storyboard-two-pass-v2"
STORYBOARD_POLICY = "per-shot-first-frame-v1"


class StoryboardRepairRequired(RuntimeError):
    def __init__(self, issues):
        self.issues = issues
        super().__init__("Storyboard repair required: " + "; ".join(
            f"shot {item['shot']}: {item['issue']}" for item in issues))


def parse_request(request, profiles):
    return call("source", request=request, profiles=profiles)


def validate_shooting_script(candidate, request, profiles, *, token_counter=None):
    return call("compile", candidate=candidate, request=request, profiles=profiles, token_counter=token_counter)


def load_shooting_script(path, request, profiles, *, token_counter=None):
    return call("load_plan", path=Path(path), request=request, profiles=profiles, token_counter=token_counter)


def save_shooting_script(plan, run_dir):
    call("publish", plan=plan, run_dir=Path(run_dir))


def _plan_fingerprint(plan):
    return call("plan_hash", plan=plan)


def create_storyboard_plan(request, profiles, run_dir, **options):
    return call("director", request=request, profiles=profiles, run_dir=Path(run_dir), **options)


def finalize_shooting_script(plan, boards, run_dir, **options):
    result = call("finalize", plan=plan, boards=boards, run_dir=Path(run_dir), **options)
    if "repair_issues" in result:
        raise StoryboardRepairRequired(result["repair_issues"])
    boards.update(call("read_creative", run_dir=Path(run_dir), name="storyboards.json"))
    return result
