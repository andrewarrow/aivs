"""Generate and verify run-local, reference-aware first frames for every shot."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import quality_backend
import characters
import storyboard_backend
from render_formats import resolve_format

POLICY = "per-shot-first-frame-v2"


def planned_shot_sha256(plan: dict, shot: dict) -> str:
    """Bind a frame to the exact planned source, coverage, prompt and timing."""
    content = {"source_sha256": plan.get("source_sha256"),
               "planning_policy": plan.get("planning_policy"), "shot": shot}
    if "world" in plan:
        content["world"] = plan["world"]
    return hashlib.sha256(json.dumps(content, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def resolve_image(binding: dict, run_dir: Path) -> Path:
    """Resolve a snapshotted image and reject missing, replaced or escaping files."""
    asset = binding.get("image")
    if not isinstance(asset, dict):
        raise ValueError("Each storyboard shot needs an image asset.")
    filename, digest = asset.get("file"), asset.get("sha256")
    if (not isinstance(filename, str) or not filename.strip() or "\x00" in filename
            or Path(filename).is_absolute()):
        raise ValueError("Storyboard image paths must be relative to the run directory.")
    if (not isinstance(digest, str) or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)):
        raise ValueError("Storyboard images need a lowercase SHA-256 digest.")
    root = Path(run_dir).resolve()
    path = (root / filename).resolve()
    if not path.is_relative_to(root):
        raise ValueError("Storyboard images must remain inside the run directory.")
    if not path.is_file():
        raise ValueError(f"Storyboard image is missing: {filename}")
    with path.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != digest:
        raise ValueError(f"Storyboard image changed: {filename}")
    return path


def prepare(plan: dict, manifest: dict, run_dir: Path) -> list[dict]:
    """Validate every frame before inference and return bindings in shot order.

    The caller snapshots image bytes and the manifest into the run directory.
    This module never reads live source images outside that snapshot. Extra
    manifest metadata is allowed; only execution inputs enter take provenance.
    """
    if not isinstance(manifest, dict) or type(manifest.get("version")) is not int or manifest["version"] != 1:
        raise ValueError("Storyboard manifests must use version 1.")
    entries = manifest.get("shots")
    if not isinstance(entries, list) or len(entries) != len(plan["shots"]):
        raise ValueError("A storyboard must provide exactly one frame for every planned shot.")
    bindings = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("Storyboard shots must be objects.")
        number = entry.get("shot")
        if type(number) is not int or not 1 <= number <= len(plan["shots"]) or number in bindings:
            raise ValueError("Storyboard shot numbers must cover the plan once, starting at 1.")
        fingerprint = planned_shot_sha256(plan, plan["shots"][number - 1])
        if entry.get("planned_shot_sha256") != fingerprint:
            raise ValueError(f"Storyboard frame {number} belongs to a different planned shot.")
        path = resolve_image(entry, run_dir)
        decoded = quality_backend.prepare_source_image(path)
        if decoded["source_sha256"] != entry["image"]["sha256"]:
            raise ValueError(f"Storyboard image changed while preparing shot {number}.")
        bindings[number] = {
            "shot": number,
            "planned_shot_sha256": fingerprint,
            "image": {"file": path.relative_to(Path(run_dir).resolve()).as_posix(),
                      "sha256": decoded["source_sha256"],
                      "width": decoded["width"], "height": decoded["height"]},
        }
    return [bindings[number] for number in range(1, len(plan["shots"]) + 1)]


def _save(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def _fingerprint(value: dict) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def reference_specs(plan):
    world = plan["world"]
    specs = []
    for item in world["environments"]:
        specs.append({"id": f"environment:{item['scene_id']}", "kind": "environment", "definition": item,
                      "prompt": "One clean cinematic environment plate. No people, body parts, loose garments or movable story props. " + item["reference_prompt"]})
    for item in world["props"]:
        specs.append({"id": "prop:" + item["id"], "kind": "prop", "definition": item,
                      "prompt": "Single physical prop reference photograph on a neutral surface, no people or text. Show one instance of each specified component and its actual connections. " + item["reference_prompt"]})
    for item in world["characters"]:
        if item["morphology"]:
            specs.append({"id": "body:" + item["role"], "kind": "body", "definition": item,
                          "prompt": "Full body neutral character reference photograph. Preserve the attached performer's identity, plausible body proportions and neutral fitted clothing. " + item["reference_prompt"]})
    return specs


def prepare_references(plan, boards, run_dir):
    """Resolve exact physical references, including the actors' identity inputs."""
    expected = {s["id"]: _fingerprint(s) for s in reference_specs(plan)}
    rows = boards.get("references")
    if (not isinstance(rows, list) or {r.get("id") for r in rows} != set(expected)
            or len(rows) != len(expected)):
        raise ValueError("Storyboard references do not cover the canonical physical world.")
    for row in rows:
        if row.get("definition_sha256") != expected[row["id"]]:
            raise ValueError("Storyboard reference belongs to a different physical definition.")
        resolve_image(row, run_dir)
        for portrait in row.get("generation", {}).get("inputs", []):
            resolve_image({"image": portrait}, run_dir)
    return rows


def generate_references(plan, profiles, run_dir, seed, *, repair_ids=(), round_number=0, on_activity=None):
    """One checkpointed library shared by every frame; review it in pass two."""
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
        if attempt == 0 and old and old.get("definition_sha256") == _fingerprint(spec):
            resolve_image(old, run_dir)
            continue
        request = {"policy": POLICY, "spec": spec, "inputs": inputs,
                   "seed": (seed + 100003 + index + attempt * 1009) % (2**31),
                   "backend": storyboard_backend.configuration()}
        identity = _fingerprint(request)
        folder = run_dir / "storyboards" / "references" / identity
        folder.mkdir(parents=True, exist_ok=True)
        checkpoint = folder / "reference.json"
        if checkpoint.is_file():
            entry = json.loads(checkpoint.read_text())
            if entry.get("generation_sha256") != identity:
                raise ValueError("Reference checkpoint inputs changed.")
            resolve_image(entry, run_dir)
        else:
            activity("Generating reusable " + spec["id"] + " reference")
            storyboard_backend.check()
            _save(folder / "request.json", request)
            target = folder / "reference.png"
            if target.exists():
                import time
                target.rename(folder / f"interrupted-{time.time_ns()}.png")
            runtime = storyboard_backend.generate(spec["prompt"], [resolve_image({"image": r}, run_dir) for r in inputs],
                                                   target, request["seed"], width=quality_backend.WIDTH,
                                                   height=quality_backend.HEIGHT, on_activity=activity)
            info = quality_backend.prepare_source_image(target)
            if info["source_sha256"] != runtime["sha256"]:
                raise ValueError("Physical reference changed during generation.")
            entry = {"id": spec["id"], "kind": spec["kind"], "definition_sha256": _fingerprint(spec),
                     "generation_sha256": identity, "generation": request,
                     "image": {"file": target.relative_to(run_dir).as_posix(), "sha256": info["source_sha256"],
                               "width": info["width"], "height": info["height"]}}
            _save(checkpoint, entry)
        entries[spec["id"]] = entry
        _save(path, {"version": 1, "source_sha256": plan["source_sha256"], "references": list(entries.values())})
    result = {"version": 1, "source_sha256": plan["source_sha256"],
              "references": [entries[s["id"]] for s in reference_specs(plan)]}
    _save(path, result)
    return result


def _frame_request(plan, shot, profiles, run_dir, prior, number, seed, width, height):
    by_id = {profile["id"]: profile for profile in profiles}
    cast = {item["role"]: item["character"] for item in plan.get("source", {}).get("cast", [])}
    visible = shot.get("opening_visible_roles")
    if not isinstance(visible, list):
        raise ValueError(f"Storyboard shot {number} needs explicit opening_visible_roles.")
    references, instructions = [], []
    for role in visible:
        if role not in cast or cast[role] not in by_id:
            raise ValueError(f"Storyboard shot {number} has an unknown visible performer: {role}")
        profile = by_id[cast[role]]
        asset = profile.get("image")
        if not asset:
            raise ValueError(f"{profile['name']} needs a saved portrait for storyboard generation.")
        reference = {"file": f"characters/{profile['id']}/{asset['file']}",
                     "sha256": asset["sha256"], "role": role, "character": profile["id"]}
        resolve_image({"image": reference}, run_dir)
        references.append(reference)
        instructions.append(f"Image {len(references)}: {role}'s face identity.")
    library_path = Path(run_dir) / "storyboard-references.json"
    if library_path.is_file():
        library = json.loads(library_path.read_text())
        needed = {f"environment:{shot['scene_id']}"} | {"body:" + role for role in visible}
        visible_props = {p["id"] for p in shot["continuity"]["opening"]["props"] if p["visibility"] == "visible"}
        needed |= {"prop:" + p["id"] for p in plan["world"]["props"] if shot["scene_id"] in p["scene_ids"]
                   and {p["id"], *(c["id"] for c in p["components"])} & visible_props}
        for reference in library["references"]:
            if reference["id"] not in needed:
                continue
            resolve_image(reference, run_dir)
            references.append(dict(reference["image"], purpose=reference["kind"], reference_id=reference["id"]))
            instructions.append(f"Image {len(references)}: {reference['id']} physical reference.")
    # A previous frame is a scene full of people and mutable story facts, not a
    # clean environment plate. Never feed it back as an authoritative set image.
    # Independent frames share the director's scene description instead.
    composition = shot.get("storyboard_prompt")
    if not isinstance(composition, str) or not composition.strip():
        # Compatibility for hand-authored plans: start state describes a still,
        # unlike the video prompt, whose later action must not leak into frame 1.
        composition = str(shot.get("start_state", "")).strip()
        if not composition:
            raise ValueError(f"Shot {number} needs a storyboard_prompt or a static start_state.")
        if shot.get("scene"):
            composition = str(shot["scene"]) + ". " + composition
    presence = ("Visible people at time zero: " + (", ".join(visible) if visible else "none; an empty composition") + ". ")
    absent = [role for role in cast if role not in visible]
    if absent:
        presence += "Do not depict " + ", ".join(absent) + "; their entrances occur later. "
    prompt = ("One cinematic live-action starting frame, a single photograph, no panels or lettering. "
              + presence + "Use references for named identities and physical features; apply the current physical state, visibility and ownership below. "
              + " ".join(instructions) + "\n" + composition.strip() +
              "\nKeep faces, active hands and essential props clear of frame edges.")
    return {"policy": POLICY, "source_sha256": plan.get("source_sha256"), "shot": number,
            "opening_state": shot.get("continuity", {}).get("opening"),
            "world_sha256": _fingerprint(plan.get("world", {})),
            "prompt": prompt, "references": references, "seed": (seed + number - 1) % (2**31),
            "width": width, "height": height, "backend": storyboard_backend.configuration()}


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
    backend = storyboard_backend.check()
    _save(directory / "request.json", request)
    target, partial = directory / "frame.png", directory / "frame.partial.png"
    # Preserve evidence if a worker was interrupted before committing its row.
    for orphan in (partial, target):
        if orphan.exists():
            import time
            orphan.rename(orphan.with_name(f"{orphan.stem}.interrupted-{time.time_ns()}{orphan.suffix}"))
    references = [resolve_image({"image": reference}, run_dir) for reference in request["references"]]
    activity(f"Regenerating storyboard {shot_index + 1}: {reason}")
    runtime = storyboard_backend.generate(request["prompt"], references, partial, request["seed"],
                                          width=width, height=height, on_activity=activity)
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


def generate(plan: dict, profiles: list[dict], run_dir: Path, seed: int, *, on_activity=None) -> dict:
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
    references = generate_references(plan, profiles, run_dir, seed, on_activity=activity)
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
                    or entry.get("generation_sha256") != request_hash
                    or entry.get("generation") != request):
                raise ValueError(f"Saved storyboard {number} has different inputs; start a new run.")
            resolve_image(entry, run_dir)
            activity(f"Reusing saved storyboard {number}/{len(plan['shots'])}")
            continue
        if backend is None:
            activity("Checking the local storyboard model")
            backend = storyboard_backend.check()
            if manifest.get("backend") not in (None, backend):
                raise ValueError("The storyboard runtime changed during this run; restore it or start a new run.")
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
                                                   width=width, height=height, on_activity=activity)
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
