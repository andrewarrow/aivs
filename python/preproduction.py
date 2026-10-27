"""Two visual planning passes with reusable, source-bound storyboard assets."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import quality_backend
import rust_engine
import shooting_script
import storyboard


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


def director_cache_key(request, profiles):
    """Image renderer changes never invalidate source-bound director coverage."""
    files = ("shooting_script.py", "shooting-script-instructions.txt")
    identity = {
        "source": shooting_script.parse_request(request, profiles)["source_sha256"],
        "implementation": {name: rust_engine.implementation_sha256(name)
                           for name in files},
        "tokenizer_revision": quality_backend.MODEL_REVISION,
    }
    return hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()


def cache_key(request, profiles):
    """Final motion direction also depends on the rendered and inspected images."""
    files = ("shooting-script-review-instructions.txt", "storyboard.py", "storyboard_backend.py",
             "storyboard_mflux.py", "prop_reference.py", "preproduction.py")
    return rust_engine.digest({"director": director_cache_key(request, profiles),
        "backend": storyboard.storyboard_backend.configuration(),
        "implementation": {name: rust_engine.implementation_sha256(name) for name in files}})


def _prepare_director(request, profiles, run_dir, *, fresh, options):
    key = director_cache_key(request, profiles)
    index = quality_backend.ROOT / ".cache" / "preproduction" / "directors" / f"{key}.json"
    draft = run_dir / "storyboard-plan.json"
    if not fresh and not draft.exists() and index.is_file():
        try:
            saved = _read(index)
            candidate = saved["plan"]
            if (saved["key"] != key or saved["plan_sha256"] != rust_engine.digest(candidate)
                    or candidate.get("planner_version") != shooting_script.PLANNER_VERSION
                    or candidate.get("planning_stage") != "storyboard"):
                raise ValueError("Director cache identity changed.")
            shooting_script.validate_shooting_script(candidate, request, profiles,
                                                     token_counter=options["token_counter"])
        except (OSError, ValueError, KeyError, TypeError) as exc:
            options["on_activity"](f"Cached director plan is unavailable ({exc}); preparing coverage")
        else:
            _save(draft, candidate)
            (run_dir / "storyboard-plan.txt").write_text("\n".join(
                f"SHOT {n} | {shot['seconds']}s\n{shot['storyboard_prompt']}\n"
                for n, shot in enumerate(candidate["shots"], 1)), encoding="utf-8")
            options["on_activity"]("Reusing director coverage; image rendering and inspection have separate caches")
    director = shooting_script.create_storyboard_plan(request, profiles, run_dir, **options)
    # Repairs stay local to the image run; share the original source-backed coverage.
    if not (run_dir / "storyboard-repairs.json").exists():
        try:
            index.parent.mkdir(parents=True, exist_ok=True)
            _save(index, {"key": key, "plan_sha256": rust_engine.digest(director), "plan": director})
        except OSError as exc:
            options["on_activity"](f"Director plan is saved locally; shared cache could not be updated: {exc}")
    return director


def rebind_storyboards(director, final, boards, run_dir):
    """Bind unchanged compositions to their image-informed motion directions."""
    rust_engine.call("validate_storyboard_direction", director=director, final=final)
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
    storyboard.require_accepted(plan, boards, directory)
    return plan, director, boards


def _copy_final(source, destination, plan, director, boards):
    rust_engine.call("copy_final_preproduction", source=str(Path(source).resolve()),
        destination=str(Path(destination).resolve()), plan=plan, director=director, boards=boards,
        fingerprint=shooting_script._plan_fingerprint(plan))
    storyboard.require_accepted(plan, boards, destination)


def _repair_state(run_dir, source_sha256):
    path = run_dir / "storyboard-repairs.json"
    state = (_read(path) if path.exists() else
             {"version": 1, "source_sha256": source_sha256, "rounds": []})
    return rust_engine.call("validate_storyboard_repairs", state=state, source_sha256=source_sha256)


def _finish_storyboard_repair(director, boards, state, request, profiles, run_dir, *,
                              token_counter, on_activity, planning_budget_seconds=600,
                              on_activity_delta=None):
    """Resume a reserved repair, then publish its plan and image bindings together.

    The journal stays pending until both artifacts are saved. If interrupted
    between those writes, its saved plan and per-image checkpoints finish the
    same attempt on Resume.
    """
    repair = state["rounds"][-1]
    revised = shooting_script.validate_shooting_script(
        repair["plan"], request, profiles, token_counter=token_counter)
    context_args = dict(state=state, source_sha256=director["source_sha256"],
                        director_sha256=shooting_script._plan_fingerprint(director),
                        revised_sha256=shooting_script._plan_fingerprint(revised))
    context = rust_engine.call("resume_storyboard_repair", **context_args)
    entries = list(boards["shots"])
    round_number = context["round_number"]
    reference_ids = context["reference_ids"]
    library = storyboard.generate_references(revised, profiles, run_dir, repair["reference_seed"],
                                            repair_ids=reference_ids, round_number=round_number,
                                            on_activity=on_activity,
                                            review_options={"planning_budget_seconds": planning_budget_seconds,
                                                            "on_activity_delta": on_activity_delta})
    for issue in repair["issues"]:
        number = issue["shot"]
        entries[number - 1] = storyboard.regenerate_shot(
            revised, profiles, run_dir, repair["seed"], number - 1,
            reason=issue["issue"],
            directory=run_dir / "storyboards" / "repairs" / f"{round_number:02d}" / f"{number:03d}",
            prior_image=repair.get("prior_images", {}).get(str(number)),
            on_activity=on_activity,
        )
    boards = dict(boards, shots=entries, references=library["references"], status="complete")
    boards.pop("continuity_acceptance", None)
    storyboard.prepare(revised, boards, run_dir)
    _save(run_dir / "storyboards.json", boards)
    _save(run_dir / "storyboard-plan.json", revised)
    display = [f"SHOT {index} | {shot['seconds']}s\n{shot['storyboard_prompt']}\n"
               for index, shot in enumerate(revised["shots"], 1)]
    (run_dir / "storyboard-plan.txt").write_text("\n".join(display), encoding="utf-8")
    state.update(rust_engine.call("complete_storyboard_repair", **context_args))
    _save(run_dir / "storyboard-repairs.json", state)
    return revised, boards


def _reserve_storyboard_repair(director, issues, state, seed, run_dir, *, boards=None,
                               request=None, profiles=None, token_counter=None):
    decision = rust_engine.call("storyboard_repair_candidate", director=director, issues=issues, state=state)
    if decision["status"] == "exhausted":
        diagnostic = decision["diagnostic"]
        _save(run_dir / "storyboard-diagnostic.json", diagnostic)
        review_path = run_dir / "storyboard-review.json"
        if review_path.is_file():
            report = _read(review_path)
            report.update(status=diagnostic["status"], reason=diagnostic["reason"])
            _save(review_path, report)
        raise RuntimeError(decision["message"])
    revised = decision["plan"]
    if request is not None:
        revised = shooting_script.validate_shooting_script(revised, request, profiles, token_counter=token_counter)
    # Keep the legacy JSON fingerprint codec while Rust selects all affected images.
    changed = [n for n, (before, after) in enumerate(zip(director["shots"], revised["shots"]), 1)
               if storyboard.planned_shot_sha256(director, before) != storyboard.planned_shot_sha256(revised, after)]
    reserved = rust_engine.call("reserve_storyboard_repair", state=state, director=director,
                                revised=revised, issues=issues, boards=boards, changed_shots=changed,
                                before_plan_sha256=shooting_script._plan_fingerprint(director), seed=seed)
    # Preserve every review, including failed evidence, before the next round.
    review_path = run_dir / "storyboard-review.json"
    if review_path.is_file():
        _save(run_dir / f"storyboard-review-before-repair-{len(state['rounds']) + 1}.json", _read(review_path))
    _save(run_dir / "storyboard-repairs.json", reserved)
    state.update(reserved)
    return decision["limit"]


def prepare(request, profiles, run_dir, seed, *, token_counter=None,
            planning_budget_seconds=600, plan_only=False, shooting_script_path=None,
            fresh=False, on_activity=None, on_activity_delta=None):
    """Return (final plan, boards), or (director draft, None) for plan-only."""
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
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
    if not plan_only:
        storyboard.storyboard_backend.check(on_activity=activity)
    director = _prepare_director(request, profiles, run_dir, fresh=fresh, options=options)
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
                token_counter=token_counter, on_activity=activity,
                planning_budget_seconds=planning_budget_seconds, on_activity_delta=on_activity_delta)
        storyboard.prepare(director, boards, run_dir)
    else:
        boards = storyboard.generate(director, profiles, run_dir, seed, on_activity=activity,
                                     review_options={"planning_budget_seconds": planning_budget_seconds,
                                                     "on_activity_delta": on_activity_delta})
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
            repair_limit = _reserve_storyboard_repair(director, exc.issues, repairs, seed, run_dir,
                                                      boards=boards, request=request, profiles=profiles,
                                                      token_counter=token_counter)
            activity(f"Repairing {len(exc.issues)} storyboard(s), round "
                     f"{len(repairs['rounds'])}/{repair_limit}")
            started = time.monotonic()
            director, boards = _finish_storyboard_repair(
                director, boards, repairs, request, profiles, run_dir,
                token_counter=token_counter, on_activity=activity,
                planning_budget_seconds=planning_budget_seconds, on_activity_delta=on_activity_delta)
            storyboard_seconds += round(time.monotonic() - started, 3)
    boards = rebind_storyboards(director, plan, boards, run_dir)
    _save(run_dir / "storyboards.json", boards)
    storyboard.require_accepted(plan, boards, run_dir)
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
