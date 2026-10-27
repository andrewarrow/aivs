"""Generate and verify run-local, reference-aware first frames for every shot."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import quality_backend
import characters
import storyboard_backend
from render_formats import resolve_format

POLICY = "per-shot-first-frame-v1"


def planned_shot_sha256(plan: dict, shot: dict) -> str:
    """Bind a frame to the exact planned source, coverage, prompt and timing."""
    content = {"source_sha256": plan.get("source_sha256"),
               "planning_policy": plan.get("planning_policy"), "shot": shot}
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


def _frame_request(plan, shot, profiles, run_dir, prior, number, seed, width, height):
    by_id = {profile["id"]: profile for profile in profiles}
    cast = {item["role"]: item["character"] for item in plan.get("source", {}).get("cast", [])}
    visible = shot.get("visible_roles", list(cast))
    references, instructions, seen = [], [], set()
    for role in visible:
        if role not in cast or cast[role] not in by_id:
            raise ValueError(f"Storyboard shot {number} has an unknown visible performer: {role}")
        profile = by_id[cast[role]]
        if profile["id"] in seen:
            continue
        seen.add(profile["id"])
        asset = profile.get("image")
        if not asset:
            raise ValueError(f"{profile['name']} needs a saved portrait for storyboard generation.")
        reference = {"file": f"characters/{profile['id']}/{asset['file']}",
                     "sha256": asset["sha256"], "role": role, "character": profile["id"]}
        resolve_image({"image": reference}, run_dir)
        references.append(reference)
        instructions.append(f"Image {len(references)}: {role}'s facial identity; preserve this exact performer.")
    if prior is not None:
        references.append(dict(prior["image"], purpose="same-scene-setting"))
        instructions.append(f"Image {len(references)}: same-scene set and light. "
                            "The composition below determines current costume, props and camera.")
    composition = shot.get("storyboard_prompt")
    if not isinstance(composition, str) or not composition.strip():
        # Compatibility for hand-authored plans: start state describes a still,
        # unlike the video prompt, whose later action must not leak into frame 1.
        composition = str(shot.get("start_state", "")).strip()
        if not composition:
            raise ValueError(f"Shot {number} needs a storyboard_prompt or a static start_state.")
        if shot.get("scene"):
            composition = str(shot["scene"]) + ". " + composition
    prompt = ("One cinematic live-action starting frame, a single photograph, no panels or lettering. "
              "Only depict people present in this opening composition; reference portraits do not require "
              "offscreen people to appear. " + " ".join(instructions) + "\n" + composition.strip() +
              "\nKeep faces, active hands and essential props clear of frame edges.")
    return {"policy": POLICY, "source_sha256": plan.get("source_sha256"), "shot": number,
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
    previous_by_scene = {}
    backend = None
    for number, shot in enumerate(plan["shots"], 1):
        scene = shot.get("scene_id")
        prior = previous_by_scene.get(scene) if scene is not None else None
        request = _frame_request(plan, shot, profiles, run_dir, prior, number, seed, width, height)
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
            previous_by_scene[scene] = entry
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
            previous_by_scene[scene] = entry
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
