"""Two visual planning passes with reusable, source-bound storyboard assets."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil

import quality_backend
import shooting_script
import storyboard

MAX_STORYBOARD_REPAIR_ROUNDS = 2


def _save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def _read(path):
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"Expected a JSON object: {path}")
    return value


def cache_key(request, profiles):
    """Seeds, video inference profiles and time allowances do not change coverage."""
    directory = Path(__file__).resolve().parent
    files = ("shooting_script.py", "shooting-script-instructions.txt",
             "shooting-script-review-instructions.txt", "storyboard.py",
             "storyboard_backend.py", "requirements-storyboard.txt", "render_formats.py",
             "preproduction.py")
    identity = {
        "source": shooting_script.parse_request(request, profiles)["source_sha256"],
        "implementation": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
                           for name in files},
        "tokenizer_revision": quality_backend.MODEL_REVISION,
    }
    return hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()


def rebind_storyboards(director, final, boards, run_dir):
    """Bind unchanged compositions to their image-informed motion directions."""
    if (director.get("source_sha256") != final.get("source_sha256")
            or len(director["shots"]) != len(final["shots"])):
        raise ValueError("Shot direction changed the storyboard source or coverage.")
    direction_fields = {"prompt", "prompt_template"}
    for before, after in zip(director["shots"], final["shots"]):
        if ({key: value for key, value in before.items() if key not in direction_fields}
                != {key: value for key, value in after.items() if key not in direction_fields}):
            raise ValueError("Shot direction changed a storyboard composition; regenerate preproduction.")
    # A crash after finalization may leave either the draft or final binding.
    try:
        storyboard.prepare(final, boards, run_dir)
        return boards
    except ValueError:
        storyboard.prepare(director, boards, run_dir)
    rebound = dict(boards, shots=[dict(entry, planned_shot_sha256=storyboard.planned_shot_sha256(
        final, final["shots"][entry["shot"] - 1])) for entry in boards["shots"]])
    storyboard.prepare(final, rebound, run_dir)
    return rebound


def _load_final(directory, request, profiles, token_counter):
    plan = shooting_script.load_shooting_script(
        directory / "shooting-script.json", request, profiles, token_counter=token_counter)
    if plan.get("planning_stage") != "final":
        raise ValueError("Reuse requires a completed two-pass storyboard plan.")
    director = shooting_script.validate_shooting_script(
        _read(directory / "storyboard-plan.json"), request, profiles, token_counter=token_counter)
    if (director.get("planning_stage") != "storyboard"
            or plan["motion_plan"].get("director_plan_sha256") != shooting_script._plan_fingerprint(director)):
        raise ValueError("The motion plan does not match its saved storyboard direction.")
    boards = rebind_storyboards(director, plan, _read(directory / "storyboards.json"), directory)
    images = [{"shot": binding["shot"], "image": binding["image"]}
              for binding in storyboard.prepare(plan, boards, directory)]
    if plan["motion_plan"].get("images") != images:
        raise ValueError("The storyboard images differ from those inspected during shot direction.")
    if plan["motion_plan"].get("storyboard_sha256") != hashlib.sha256(
            shooting_script._json(images).encode("utf-8")).hexdigest():
        raise ValueError("The motion plan has an invalid storyboard fingerprint.")
    return plan, director, boards


def _copy_final(source, destination, plan, director, boards):
    for entry in boards["shots"]:
        original = storyboard.resolve_image(entry, source)
        relative = original.relative_to(source.resolve())
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists():
            if target.read_bytes() != original.read_bytes():
                raise ValueError(f"A different storyboard already exists: {relative}")
        else:
            shutil.copyfile(original, target)
    _save(destination / "storyboard-plan.json", director)
    if (source / "storyboard-plan.txt").is_file():
        shutil.copyfile(source / "storyboard-plan.txt", destination / "storyboard-plan.txt")
    _save(destination / "storyboards.json", boards)
    shooting_script.save_shooting_script(plan, destination)
    storyboard.prepare(plan, boards, destination)


def _repair_state(run_dir, source_sha256):
    path = run_dir / "storyboard-repairs.json"
    state = (_read(path) if path.exists() else
             {"version": 1, "source_sha256": source_sha256, "rounds": []})
    if (state.get("version") != 1 or state.get("source_sha256") != source_sha256
            or not isinstance(state.get("rounds"), list)
            or len(state["rounds"]) > MAX_STORYBOARD_REPAIR_ROUNDS
            or any(not isinstance(item, dict) or item.get("status") not in ("pending", "complete")
                   for item in state["rounds"])
            or any(item["status"] != "complete" for item in state["rounds"][:-1])):
        raise ValueError("Saved storyboard repairs do not match this run.")
    return state


def _finish_storyboard_repair(director, boards, state, request, profiles, run_dir, *,
                              token_counter, on_activity):
    """Resume a reserved repair, then publish its plan and image bindings together.

    The journal stays pending until both artifacts are saved. If interrupted
    between those writes, its saved plan and per-image checkpoints finish the
    same attempt on Resume.
    """
    repair = state["rounds"][-1]
    revised = shooting_script.validate_shooting_script(
        repair["plan"], request, profiles, token_counter=token_counter)
    if shooting_script._plan_fingerprint(director) not in (
            repair["before_plan_sha256"], shooting_script._plan_fingerprint(revised)):
        raise ValueError("Storyboard direction changed during its saved repair.")
    entries = list(boards["shots"])
    round_number = len(state["rounds"])
    for issue in repair["issues"]:
        number = issue["shot"]
        # Retain the original set/light reference across repairs and Resume.
        # Its identity stays stable even if another shot in this round changes.
        prior = next(({"image": reference} for reference in
                      entries[number - 1].get("generation", {}).get("references", [])
                      if reference.get("purpose") == "same-scene-setting"), None)
        entries[number - 1] = storyboard.regenerate_shot(
            revised, profiles, run_dir, repair["seed"], number - 1,
            reason=issue["issue"],
            directory=run_dir / "storyboards" / "repairs" / f"{round_number:02d}" / f"{number:03d}",
            prior_image=prior, on_activity=on_activity,
        )
    boards = dict(boards, shots=entries, status="complete")
    storyboard.prepare(revised, boards, run_dir)
    _save(run_dir / "storyboards.json", boards)
    _save(run_dir / "storyboard-plan.json", revised)
    display = [f"SHOT {index} | {shot['seconds']}s\n{shot['storyboard_prompt']}\n"
               for index, shot in enumerate(revised["shots"], 1)]
    (run_dir / "storyboard-plan.txt").write_text("\n".join(display), encoding="utf-8")
    repair["status"] = "complete"
    _save(run_dir / "storyboard-repairs.json", state)
    return revised, boards


def _reserve_storyboard_repair(director, issues, state, seed, run_dir):
    if len(state["rounds"]) >= MAX_STORYBOARD_REPAIR_ROUNDS:
        detail = "; ".join(f"shot {issue['shot']}: {issue['issue']}" for issue in issues)
        raise RuntimeError(
            f"Storyboards still cannot support the source after {MAX_STORYBOARD_REPAIR_ROUNDS} "
            f"automatic repair rounds: {detail}. Saved images and findings are preserved."
        )
    if not isinstance(issues, list) or not issues:
        raise ValueError("Storyboard repair needs specific shot findings.")
    shots = [dict(shot) for shot in director["shots"]]
    seen = set()
    for issue in issues:
        number = issue.get("shot")
        prompt = issue.get("storyboard_prompt")
        if (type(number) is not int or not 1 <= number <= len(shots) or number in seen
                or not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 1200
                or "\x00" in prompt or "{{" in prompt or "}}" in prompt
                or not isinstance(issue.get("issue"), str) or not issue["issue"].strip()):
            raise ValueError("Storyboard repair needs unique shots and valid replacement compositions.")
        seen.add(number)
        shots[number - 1]["storyboard_prompt"] = prompt.strip()
    revised = dict(director, shots=shots)
    state["rounds"].append({
        "status": "pending", "before_plan_sha256": shooting_script._plan_fingerprint(director),
        "plan": revised, "issues": issues,
        "seed": (seed + len(state["rounds"]) + 1) % (2**31),
    })
    _save(run_dir / "storyboard-repairs.json", state)


def prepare(request, profiles, run_dir, seed, *, token_counter=None,
            planning_budget_seconds=600, plan_only=False, shooting_script_path=None,
            fresh=False, on_activity=None, on_activity_delta=None):
    """Return (final plan, boards), or (director draft, None) for plan-only."""
    run_dir = Path(run_dir)
    activity = on_activity or (lambda message: None)
    key = cache_key(request, profiles)
    index = quality_backend.ROOT / ".cache" / "preproduction" / f"{key}.json"
    options = dict(token_counter=token_counter, planning_budget_seconds=planning_budget_seconds,
                   on_activity=activity, on_activity_delta=on_activity_delta)

    if (run_dir / "shooting-script.json").is_file():
        plan, director, boards = _load_final(run_dir, request, profiles, token_counter)
        _save(run_dir / "storyboards.json", boards)
        activity("Reusing this run's completed shooting plan and storyboard images")
        return plan, boards

    source = None
    if shooting_script_path is not None:
        supplied = Path(shooting_script_path).expanduser().resolve()
        if supplied.name != "shooting-script.json":
            raise ValueError("Select the finalized shooting-script.json beside its storyboard assets.")
        source = supplied.parent
    elif not fresh and not (run_dir / "storyboard-plan.json").exists() and index.is_file():
        try:
            record = _read(index)
            if record.get("key") == key:
                source = Path(record["directory"]).resolve()
        except (OSError, ValueError, KeyError, TypeError):
            source = None

    if source is not None:
        try:
            plan, director, boards = _load_final(source, request, profiles, token_counter)
        except (OSError, ValueError, KeyError, TypeError) as exc:
            if shooting_script_path is not None:
                raise
            activity(f"Cached preproduction is unavailable ({exc}); preparing a new plan")
        else:
            _copy_final(source, run_dir, plan, director, boards)
            _save(run_dir / "preproduction.json", {
                "cache_key": key, "reused": True, "source_directory": str(source),
                "planning_seconds": 0, "storyboard_seconds": 0,
            })
            activity("Reusing the validated shooting plan and storyboards; video seed and profile may differ")
            return plan, boards

    import time
    director = shooting_script.create_storyboard_plan(request, profiles, run_dir, **options)
    if plan_only:
        return director, None
    budget = _read(run_dir / "planning-budget.json")
    if budget["used_seconds"] >= planning_budget_seconds:
        raise RuntimeError("The total planning budget is exhausted. Increase --planning-budget-seconds before generating storyboards.")
    started = time.monotonic()
    repairs = _repair_state(run_dir, director["source_sha256"])
    if repairs["rounds"]:
        boards = _read(run_dir / "storyboards.json")
        if repairs["rounds"][-1]["status"] == "pending":
            director, boards = _finish_storyboard_repair(
                director, boards, repairs, request, profiles, run_dir,
                token_counter=token_counter, on_activity=activity)
        storyboard.prepare(director, boards, run_dir)
    else:
        boards = storyboard.generate(director, profiles, run_dir, seed, on_activity=activity)
    storyboard_seconds = round(time.monotonic() - started, 3)
    while True:
        try:
            plan = shooting_script.finalize_shooting_script(director, boards, run_dir, **options)
            break
        except shooting_script.StoryboardRepairRequired as exc:
            budget = _read(run_dir / "planning-budget.json")
            if budget["used_seconds"] >= planning_budget_seconds:
                raise RuntimeError("The planning budget is exhausted before storyboard repair; "
                                   "increase --planning-budget-seconds to continue.") from exc
            _reserve_storyboard_repair(director, exc.issues, repairs, seed, run_dir)
            activity(f"Repairing {len(exc.issues)} storyboard(s), round "
                     f"{len(repairs['rounds'])}/{MAX_STORYBOARD_REPAIR_ROUNDS}")
            started = time.monotonic()
            director, boards = _finish_storyboard_repair(
                director, boards, repairs, request, profiles, run_dir,
                token_counter=token_counter, on_activity=activity)
            storyboard_seconds += round(time.monotonic() - started, 3)
    boards = rebind_storyboards(director, plan, boards, run_dir)
    _save(run_dir / "storyboards.json", boards)
    _save(run_dir / "preproduction.json", {
        "cache_key": key, "reused": False, "storyboard_seconds": storyboard_seconds,
        "planning_budget": _read(run_dir / "planning-budget.json"),
    })
    try:
        index.parent.mkdir(parents=True, exist_ok=True)
        _save(index, {"key": key, "directory": str(run_dir.resolve())})
    except OSError as exc:
        activity(f"Preproduction is saved in this run; the shared cache index could not be updated: {exc}")
    return plan, boards
