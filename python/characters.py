"""Saved performer adapters; Rust owns resolution, profiles and run snapshots."""
from __future__ import annotations

import hashlib
from pathlib import Path
import rust_engine

SCHEMA_VERSION = 1
ROOT = Path(__file__).resolve().parents[1]
_ASSET_NAMES = {"image": "reference.png", "scene_image": "scene-reference.png", "voice_sample": "voice.wav"}


def _digest(path: Path) -> str:
    # Seed-VC workers use Python 3.10, which lacks hashlib.file_digest.
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _validate_asset_record(record, expected_filename, directory, label) -> Path:
    return Path(rust_engine.call("character_validate_asset", record=record,
                expected_filename=expected_filename, directory=str(Path(directory).resolve()), label=label))


def _read_profile(path: Path) -> dict:
    return rust_engine.call("character_read_profile", path=str(Path(path).resolve()))


def load_character(name: str, root=None) -> dict:
    return rust_engine.call("character_load", name=name, root=str(Path(root or ROOT).resolve()))


def resolve_characters(prompt: str, explicit=(), root=None, *, references_only=False) -> list[dict]:
    selectors = explicit if isinstance(explicit, (str, dict)) or explicit is None else list(explicit)
    return rust_engine.call("character_resolve", prompt=prompt, explicit=selectors,
                            root=str(Path(root or ROOT).resolve()), references_only=references_only)


def replace_references(prompt: str, profiles) -> str:
    return rust_engine.call("character_replace_references", prompt=prompt, profiles=list(profiles))


def expand_prompt(prompt: str, profiles, *, image_conditioned=False) -> str:
    return rust_engine.call("character_expand_prompt", prompt=prompt, profiles=list(profiles),
                            image_conditioned=image_conditioned)


def snapshot_characters(profiles, run_dir, root=None) -> list[dict]:
    try:
        return rust_engine.call("character_snapshot", profiles=list(profiles),
                                run_dir=str(Path(run_dir).resolve()), root=str(Path(root or ROOT).resolve()))
    except ValueError as exc:
        if str(exc).startswith("Character snapshot already exists:"):
            raise FileExistsError(str(exc)) from exc
        raise


def verify_snapshots(profiles, run_dir) -> None:
    rust_engine.call("character_verify_snapshots", profiles=list(profiles), run_dir=str(Path(run_dir).resolve()))


def reference_image(profiles, run_dir) -> Path | None:
    result = rust_engine.call("character_reference_image", profiles=list(profiles), run_dir=str(Path(run_dir).resolve()))
    return Path(result) if result is not None else None
