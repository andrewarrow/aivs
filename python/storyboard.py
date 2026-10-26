"""Verified, run-local first frames for opt-in storyboard renders."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import quality_backend


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
