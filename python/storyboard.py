"""Generate and verify run-local, reference-aware first frames for every shot."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import quality_backend
import rust_engine
import characters
import prop_reference
import storyboard_backend
from rust_engine import resolve_format

POLICY = "per-shot-first-frame-v2"


def review_policy_fingerprint():
    return rust_engine.digest({name: rust_engine.implementation_sha256(name)
                              for name in ("storyboard.py", "shooting_script.py",
                                           "shooting-script-review-instructions.txt")})


def review_binding(plan, boards):
    return rust_engine.call("storyboard_review_binding", plan=plan, boards=boards,
                            review_policy_sha256=review_policy_fingerprint(),
                            source_snapshot_sha256=rust_engine.digest(plan["source"]),
                            plan_sha256=rust_engine.digest(plan))


def require_accepted(plan, boards, run_dir):
    """Verify snapshot bytes and require Rust acceptance on every inference path."""
    run_dir = Path(run_dir)
    rust_engine.call("validate_storyboard_manifest", plan=plan, boards=boards)
    prepare(plan, boards, run_dir)
    prepare_references(plan, boards, run_dir)
    screenplay = run_dir / "screenplay.txt"
    profiles = [characters._read_profile(run_dir / "characters" / supplied["id"] / "character.json")
                for supplied in plan["source"].get("characters", [])]
    rust_engine.call("validate_storyboard_snapshots", plan=plan, profiles=profiles,
                     screenplay=screenplay.read_text(encoding="utf-8") if screenplay.is_file() else None)
    path = run_dir / "storyboard-review.json"
    if not path.is_file():
        raise ValueError("Storyboard sequence has no accepted continuity review; stop before video inference.")
    report = json.loads(path.read_text(encoding="utf-8"))
    rust_engine.call("validate_acceptance", plan=plan, boards=boards, report=report,
                     binding=review_binding(plan, boards), report_sha256=rust_engine.digest(report))
    return report


def planned_shot_sha256(plan: dict, shot: dict) -> str:
    """Bind a frame to the exact planned source, coverage, prompt and timing."""
    content = {"source_sha256": plan.get("source_sha256"),
               "planning_policy": plan.get("planning_policy"), "shot": shot}
    if "world" in plan:
        content["world"] = plan["world"]
    return hashlib.sha256(json.dumps(content, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def resolve_image(binding: dict, run_dir: Path) -> Path:
    return Path(rust_engine.call("storyboard_resolve_image", binding=binding,
                                 run_dir=str(Path(run_dir).resolve())))


def prepare(plan: dict, manifest: dict, run_dir: Path) -> list[dict]:
    return rust_engine.call("storyboard_prepare", plan=plan, manifest=manifest,
        run_dir=str(Path(run_dir).resolve()),
        fingerprints=[planned_shot_sha256(plan, shot) for shot in plan["shots"]])


def _save(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def _fingerprint(value: dict) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def reference_specs(plan):
    return rust_engine.call("storyboard_reference_specs", plan=plan)


def prepare_references(plan, boards, run_dir):
    return rust_engine.call("storyboard_prepare_references", plan=plan, boards=boards,
        run_dir=str(Path(run_dir).resolve()), **_reference_contracts(plan, boards))


def _reference_attempt(spec, request, run_dir, activity):
    identity = _fingerprint(request)
    folder = run_dir / "storyboards" / "references" / identity
    folder.mkdir(parents=True, exist_ok=True)
    checkpoint = folder / "reference.json"
    if checkpoint.is_file():
        entry = json.loads(checkpoint.read_text())
        if entry.get("generation_sha256") != identity or entry.get("generation") != request:
            raise ValueError("Reference checkpoint inputs changed.")
        resolve_image(entry, run_dir)
        return entry, folder
    activity("Generating reusable " + spec["id"] + " reference")
    storyboard_backend.check(on_activity=activity)
    _save(folder / "request.json", request)
    target = folder / "reference.png"
    if target.exists():
        import time
        target.rename(folder / f"interrupted-{time.time_ns()}.png")
    runtime = storyboard_backend.generate(request["prompt"],
                                           [resolve_image({"image": r}, run_dir) for r in request["inputs"]],
                                           target, request["seed"], width=quality_backend.WIDTH,
                                           height=quality_backend.HEIGHT,
                                           on_activity=activity)
    info = quality_backend.prepare_source_image(target)
    if info["source_sha256"] != runtime["sha256"]:
        raise ValueError("Physical reference changed during generation.")
    entry = {"id": spec["id"], "kind": spec["kind"], "definition_sha256": _fingerprint(spec),
             "generation_sha256": identity, "generation": request,
             "image": {"file": target.relative_to(run_dir).as_posix(), "sha256": info["source_sha256"],
                       "width": info["width"], "height": info["height"]}, "runtime": runtime}
    _save(checkpoint, entry)
    return entry, folder


def generate_references(plan, profiles, run_dir, seed, *, repair_ids=(), round_number=0,
                        on_activity=None, review_options=None):
    """Publish prop references only after inspecting their actual component structure."""
    run_dir = Path(run_dir)
    activity = on_activity or (lambda message: None)
    path = run_dir / "storyboard-references.json"
    saved = json.loads(path.read_text()) if path.is_file() else {"version": 1, "references": []}
    if saved.get("source_sha256", plan["source_sha256"]) != plan["source_sha256"]:
        raise ValueError("Saved physical references belong to another source.")
    entries = {row["id"]: row for row in saved["references"]}
    by_role = {c["role"]: c["character"] for c in plan["source"]["cast"]}
    by_id = {p["id"]: p for p in profiles}
    for index, spec in enumerate(reference_specs(plan)):
        inputs = []
        if spec["kind"] == "body":
            profile = by_id[by_role[spec["definition"]["role"]]]
            inputs = [{"file": f"characters/{profile['id']}/{profile['image']['file']}", "sha256": profile["image"]["sha256"]}]
        attempt = round_number if spec["id"] in repair_ids else 0
        old = entries.get(spec["id"])
        if (attempt == 0 and old and old.get("definition_sha256") == _fingerprint(spec)
                and old.get("generation", {}).get("backend") == storyboard_backend.configuration()):
            resolve_image(old, run_dir)
            if spec["kind"] != "prop":
                continue
            try:
                prop_reference.require_accepted(spec, old)
            except ValueError:
                pass
            else:
                continue
        prompt = spec["prompt"]
        for retry in range(prop_reference.MAX_ATTEMPTS if spec["kind"] == "prop" else 1):
            request = {"policy": POLICY, "spec": spec, "inputs": inputs, "prompt": prompt,
                       "seed": (seed + 100003 + index + attempt * 1009 + retry * 7919) % (2**31),
                       "backend": storyboard_backend.configuration()}
            if retry:
                request.update(structure_retry=retry, repair_of=entry["image"]["sha256"])
            entry, folder = _reference_attempt(spec, request, run_dir, activity)
            if spec["kind"] != "prop":
                break
            entry["prop_validation"] = prop_reference.review(
                spec, entry, resolve_image(entry, run_dir), run_dir, folder / "structure-review.json",
                source_sha256=plan["source_sha256"], on_activity=activity, **(review_options or {}))
            _save(folder / "reference.json", entry)
            if entry["prop_validation"]["approved"]:
                break
            prompt = prop_reference.generation_prompt(
                spec["definition"], entry["prop_validation"]["observation"]["regeneration_prompt"])
            activity(f"Rejected {spec['id']} reference ({retry + 1}/{prop_reference.MAX_ATTEMPTS}): "
                     + prop_reference.failure_summary(entry["prop_validation"]))
        else:
            raise RuntimeError(f"Canonical {spec['id']} failed structural inspection after "
                               f"{prop_reference.MAX_ATTEMPTS} images. "
                               f"{prop_reference.failure_summary(entry['prop_validation'])} "
                               f"Full inspection: {folder / 'structure-review.json'}")
        entries[spec["id"]] = entry
        _save(path, {"version": 1, "source_sha256": plan["source_sha256"], "references": list(entries.values())})
    result = {"version": 1, "source_sha256": plan["source_sha256"],
              "references": [entries[s["id"]] for s in reference_specs(plan)]}
    _save(path, result)
    return result


def visible_state_prompt(plan, shot):
    return rust_engine.call("storyboard_visible_state", plan=plan, shot=shot)


def _reference_contracts(plan, boards):
    """Retain the historical JSON codec; Rust owns reference selection and checks."""
    specs = reference_specs(plan) if plan.get("world") else []
    entries = {row["id"]: row for row in (boards or {}).get("references", [])}
    bindings, shared = {}, {}
    for spec in specs:
        if spec["kind"] != "prop":
            continue
        if spec["id"] in entries:
            bindings[spec["id"]] = prop_reference.binding(spec, entries[spec["id"]]["image"])
        definition = spec["definition"]
        if not definition["components"] and not definition["relationships"]:
            shared[spec["id"]] = _fingerprint({key: definition[key] for key in
                ("type", "shape", "size", "color", "features")})
    return dict(specs=specs, spec_hashes={spec["id"]: _fingerprint(spec) for spec in specs},
                prop_bindings=bindings, shared_designs=shared)


def _frame_request(plan, shot, profiles, run_dir, prior, number, seed, width, height):
    library_path = Path(run_dir) / "storyboard-references.json"
    library = json.loads(library_path.read_text()) if library_path.is_file() else None
    return rust_engine.call("storyboard_frame_request", plan=plan, shot=shot, profiles=profiles,
        run_dir=str(Path(run_dir).resolve()), prior=prior, number=number, seed=seed,
        width=width, height=height, library=library, world_sha256=_fingerprint(plan.get("world", {})),
        backend=storyboard_backend.configuration(), **_reference_contracts(plan, library))


def regenerate_shot(plan: dict, profiles: list[dict], run_dir: Path, seed: int,
                    shot_index: int, *, reason: str, directory: Path,
                    prior_image=None, on_activity=None) -> dict:
    """Generate a repaired first frame without overwriting any approved image.

    The caller owns the attempt limit and publication of the returned binding.
    A request-bound checkpoint lets Resume finish an interrupted repair without
    changing its seed or discarding a completed image.
    """
    activity = on_activity or (lambda message: None)
    run_dir = Path(run_dir).resolve()
    directory = Path(directory).resolve()
    if not directory.is_relative_to(run_dir) or directory == run_dir:
        raise ValueError("Repaired storyboard images must stay inside the run directory.")
    if type(shot_index) is not int or not 0 <= shot_index < len(plan["shots"]):
        raise ValueError("Invalid storyboard repair shot index.")
    if type(seed) is not int or not isinstance(reason, str) or not reason.strip():
        raise ValueError("Storyboard repair requires a seed and a concrete reason.")
    characters.verify_snapshots(profiles, run_dir)
    shot = plan["shots"][shot_index]
    spec = resolve_format(plan["render_format"]) if "render_format" in plan else None
    width, height = ((spec["model_width"], spec["model_height"]) if spec else
                     (quality_backend.WIDTH, quality_backend.HEIGHT))
    request = _frame_request(plan, shot, profiles, run_dir, prior_image, shot_index + 1,
                             seed, width, height)
    request["repair_reason"] = reason
    request["edit_target"] = prior_image
    request["preserve"] = ["correct facial identity and anatomy", "camera framing and lighting",
                           "correct costume and environment", "all correct prop geometry and placement"]
    request_hash = _fingerprint(request)
    fingerprint = planned_shot_sha256(plan, shot)
    directory.mkdir(parents=True, exist_ok=True)
    checkpoint = directory / "storyboard.json"
    if checkpoint.is_file():
        entry = json.loads(checkpoint.read_text(encoding="utf-8"))
        if (not isinstance(entry, dict) or entry.get("generation_sha256") != request_hash
                or entry.get("generation") != request
                or entry.get("planned_shot_sha256") != fingerprint
                or entry.get("shot") != shot_index + 1):
            raise ValueError("Saved storyboard repair belongs to different inputs.")
        resolve_image(entry, run_dir)
        activity(f"Reusing repaired storyboard {shot_index + 1}")
        return entry
    backend = storyboard_backend.check(on_activity=activity)
    _save(directory / "request.json", request)
    target, partial = directory / "frame.png", directory / "frame.partial.png"
    # Preserve evidence if a worker was interrupted before committing its row.
    for orphan in (partial, target):
        if orphan.exists():
            import time
            orphan.rename(orphan.with_name(f"{orphan.stem}.interrupted-{time.time_ns()}{orphan.suffix}"))
    references = [resolve_image({"image": reference}, run_dir) for reference in request["references"]]
    activity(f"Regenerating storyboard {shot_index + 1}: {reason}")
    if prior_image is not None:
        runtime = storyboard_backend.edit(
            resolve_image({"image": prior_image}, run_dir), reason + "\n" + request["prompt"],
            request["preserve"], references, partial, request["seed"],
            width=width, height=height, composition=request["composition"], on_activity=activity)
    else:
        runtime = storyboard_backend.generate(request["prompt"] + "\nCorrection: " + reason,
            references, partial, request["seed"], width=width, height=height,
            composition=request["composition"], on_activity=activity)
    info = quality_backend.prepare_source_image(partial)
    if info["source_sha256"] != runtime["sha256"] or (info["width"], info["height"]) != (width, height):
        raise ValueError("Repaired storyboard has unexpected image dimensions or bytes.")
    for reference in request["references"]:
        resolve_image({"image": reference}, run_dir)
    partial.replace(target)
    entry = {"shot": shot_index + 1, "planned_shot_sha256": fingerprint,
             "generation_sha256": request_hash, "generation": request, "runtime": runtime,
             "backend": backend,
             "image": {"file": target.relative_to(run_dir).as_posix(),
                       "sha256": info["source_sha256"], "width": width, "height": height}}
    _save(checkpoint, entry)
    return entry


def generate(plan: dict, profiles: list[dict], run_dir: Path, seed: int, *,
             on_activity=None, review_options=None) -> dict:
    """Create one independent first frame per shot, retaining completed frames.

    The caller holds the workspace render lock. Each image is bound to its exact
    planned shot and generation inputs. A crash preserves completed rows in
    storyboards.json; partial files never count as completed images.
    """
    activity = on_activity or (lambda message: None)
    run_dir = Path(run_dir).resolve()
    characters.verify_snapshots(profiles, run_dir)
    for supplied in plan.get("source", {}).get("characters", []):
        saved = next((profile for profile in profiles if profile["id"] == supplied["id"]), None)
        if saved is None or any(saved.get(key) != value for key, value in supplied.items()):
            raise ValueError("Storyboard plan and saved character snapshots do not match.")
    if type(seed) is not int:
        raise ValueError("Storyboard seed must be an integer.")
    references = generate_references(plan, profiles, run_dir, seed, on_activity=activity,
                                     review_options=review_options)
    spec = resolve_format(plan["render_format"]) if "render_format" in plan else None
    width, height = ((spec["model_width"], spec["model_height"]) if spec else
                     (quality_backend.WIDTH, quality_backend.HEIGHT))
    directory = run_dir / "storyboards"
    directory.mkdir(exist_ok=True)
    path = run_dir / "storyboards.json"
    if path.exists():
        manifest = json.loads(path.read_text(encoding="utf-8"))
        if (not isinstance(manifest, dict) or manifest.get("version") != 1
                or manifest.get("policy") != POLICY or manifest.get("seed") != seed
                or manifest.get("source_sha256") != plan.get("source_sha256")
                or not isinstance(manifest.get("shots"), list)
                or len(manifest["shots"]) > len(plan["shots"])):
            raise ValueError("Saved storyboards belong to different generation inputs.")
    else:
        manifest = {"version": 1, "policy": POLICY, "source_sha256": plan.get("source_sha256"),
                    "seed": seed, "status": "generating", "shots": []}
        _save(path, manifest)
    manifest["references"] = references["references"]
    backend = None
    for number, shot in enumerate(plan["shots"], 1):
        request = _frame_request(plan, shot, profiles, run_dir, None, number, seed, width, height)
        request_hash = _fingerprint(request)
        fingerprint = planned_shot_sha256(plan, shot)
        if number <= len(manifest["shots"]):
            entry = manifest["shots"][number - 1]
            if (not isinstance(entry, dict) or entry.get("shot") != number
                    or entry.get("planned_shot_sha256") != fingerprint
                    or entry.get("generation_sha256") != _fingerprint(entry.get("generation", {}))):
                raise ValueError(f"Saved storyboard {number} has different inputs; start a new run.")
            old_image = resolve_image(entry, run_dir)
            if entry.get("generation_sha256") == request_hash and entry.get("generation") == request:
                activity(f"Reusing saved storyboard {number}/{len(plan['shots'])}")
                continue
            # A revised prompt or newly inspected reference must regenerate its
            # dependent frame. Preserve both the old pixels and their binding;
            # the updated path makes a crash here resumable too.
            target = directory / f"{number:03d}.png"
            if old_image == target:
                import time
                archived = directory / f"{number:03d}.superseded-{time.time_ns()}.png"
                old_image.rename(archived)
                entry["image"] = dict(entry["image"], file=archived.relative_to(run_dir).as_posix())
                _save(path, manifest)
                _save(archived.with_suffix(".json"), entry)
            manifest.pop("continuity_acceptance", None)
            activity(f"Regenerating storyboard {number} after reference or prompt changes")
        if backend is None:
            activity("Checking Codex storyboard image generation")
            backend = storyboard_backend.check(on_activity=activity)
            manifest["backend"] = backend
        activity(f"Generating storyboard {number}/{len(plan['shots'])}")
        manifest.update(status="generating", current_shot=number)
        manifest.pop("error", None)
        _save(path, manifest)
        _save(directory / f"{number:03d}.request.json", request)
        target = directory / f"{number:03d}.png"
        partial = directory / f"{number:03d}.partial.png"
        # An interruption may leave a partial image or an uncommitted final
        # image. Keep that evidence; only a manifest row authorizes reuse.
        for orphan in (partial, target):
            if orphan.exists():
                import time
                orphan.rename(orphan.with_name(f"{orphan.stem}.interrupted-{time.time_ns()}{orphan.suffix}"))
        try:
            references = [resolve_image({"image": reference}, run_dir) for reference in request["references"]]
            runtime = storyboard_backend.generate(request["prompt"], references, partial, request["seed"],
                                                   width=width, height=height, composition=request["composition"],
                                                   on_activity=activity)
            info = quality_backend.prepare_source_image(partial)
            if info["source_sha256"] != runtime["sha256"] or (info["width"], info["height"]) != (width, height):
                raise ValueError(f"Storyboard {number} did not produce the requested image dimensions or bytes.")
            # Detect a reference being replaced while the worker was running.
            for reference in request["references"]:
                resolve_image({"image": reference}, run_dir)
            partial.replace(target)
            entry = {"shot": number, "planned_shot_sha256": fingerprint,
                     "generation_sha256": request_hash, "generation": request, "runtime": runtime,
                     "image": {"file": target.relative_to(run_dir).as_posix(),
                               "sha256": info["source_sha256"], "width": width, "height": height}}
            if number <= len(manifest["shots"]):
                manifest["shots"][number - 1] = entry
            else:
                manifest["shots"].append(entry)
            _save(path, manifest)
        except BaseException as exc:
            manifest.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                            error=str(exc) or type(exc).__name__)
            _save(path, manifest)
            raise
    prepare(plan, manifest, run_dir)
    manifest.update(status="complete")
    manifest.pop("current_shot", None)
    manifest.pop("error", None)
    _save(path, manifest)
    return manifest
