"""Two visual planning passes with reusable, source-bound storyboard assets."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil

import quality_backend
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
    boards = storyboard.generate(director, profiles, run_dir, seed, on_activity=activity)
    storyboard_seconds = round(time.monotonic() - started, 3)
    plan = shooting_script.finalize_shooting_script(director, boards, run_dir, **options)
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
