"""Saved performer assets for the normal screenplay rendering path."""

from __future__ import annotations

import gc
import json
from pathlib import Path
import subprocess
import sys

import characters
import quality_backend

POLICY = "saved-character-assets-v1"
RUNNER = Path(__file__).resolve().parents[1] / "experiments/character-consistency/run_voice_conversion.py"


def _worker(*arguments) -> dict:
    root = quality_backend.ROOT
    python = root / ".cache/seed-vc-env/bin/python"
    if not python.is_file():
        raise RuntimeError(
            "Saved-character voices require the separate Seed-VC environment. "
            "Follow README.md's Saved character rendering setup, then Resume this run."
        )
    result = subprocess.run([
        str(python), "-u", str(RUNNER),
        "--code-dir", str(root / ".cache/seed-vc"),
        "--models-dir", str(root / ".cache/seed-vc-models"),
        *map(str, arguments),
    ], capture_output=True, text=True)
    if result.returncode:
        detail = (result.stderr or result.stdout).strip()[-2000:]
        raise RuntimeError(f"Saved voice conversion failed: {detail}. See README.md's Saved character rendering setup.")
    for line in reversed(result.stdout.splitlines()):
        if line.startswith("AIVS_VOICE "):
            return json.loads(line.removeprefix("AIVS_VOICE "))
    raise RuntimeError("Saved voice converter returned no provenance record.")


def prepare(plan: dict, profiles: list[dict], run_dir: Path, activity) -> list[dict]:
    """Resolve only source-bound, snapshotted assets; preflight before inference."""
    characters.verify_snapshots(profiles, run_dir)
    by_id = {profile["id"]: profile for profile in profiles}
    source = plan["source"]
    if source.get("character_conditioning") != POLICY:
        raise ValueError("Regenerate the shooting plan for saved character asset conditioning.")
    for profile in source["characters"]:
        saved = by_id.get(profile["id"])
        if saved is None or any(saved.get(key) != value for key, value in profile.items()):
            raise ValueError("Shooting plan and saved character assets do not match.")
    cast = {binding["role"]: by_id[binding["character"]] for binding in source["cast"]}
    beats = {beat["id"]: beat for beat in source["beats"]}
    prepared = []
    for number, shot in enumerate(plan["shots"], 1):
        visible = {cast[role]["id"] for role in shot.get("visible_roles", cast)}
        speakers = {cast[beats[n]["speaker"]]["id"] for n in shot["beat_ids"]
                    if beats[n]["kind"] == "dialogue"}
        binding = {"policy": POLICY}
        for kind, selected, field in (("image", visible, "image"), ("voice", speakers, "voice_sample")):
            if len(selected) == 1:
                profile = by_id[next(iter(selected))]
                asset = profile.get(field)
                if asset is None:
                    raise ValueError(f"{profile['name']} needs a saved {field.replace('_', ' ')} for character rendering.")
                binding[kind] = {"character": profile["id"], "sha256": asset["sha256"],
                                 "file": str(Path("characters") / profile["id"] / asset["file"])}
            elif selected:
                binding[kind] = {"status": "unsupported", "reason": (
                    "Multiple visible performers need a joint scene reference; using descriptions and frame continuity."
                    if kind == "image" else
                    "Multiple speakers need isolated dialogue tracks; retaining generated voices."
                )}
                activity(f"Shot {number}: {binding[kind]['reason']}")
            else:
                binding[kind] = {"status": "not-needed"}
        prepared.append(binding)
    if any("file" in binding["voice"] for binding in prepared):
        activity("Checking saved-character voice converter before video inference")
        identity = _worker("--check")
        for binding in prepared:
            if "file" in binding["voice"]:
                binding["voice"]["converter"] = identity
    return prepared


def apply_voice(source: Path, clip: Path, voice: dict, run_dir: Path, seconds: int, *,
                width: int, height: int) -> dict:
    """Keep native picture/timing and replace its soundtrack with the saved voice."""
    reference = run_dir / voice["file"]
    if characters._digest(reference) != voice["sha256"]:
        raise ValueError("Saved character voice changed before conversion.")
    directory = clip.parent / "voice"
    directory.mkdir(exist_ok=True)
    audio = directory / "source.wav"
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(source), "-vn", "-map", "0:a:0", "-ar", "24000", "-ac", "1",
        "-c:a", "pcm_s16le", str(audio),
    ], check=True, capture_output=True, text=True)
    inputs = {"source_audio": characters._digest(audio), "reference": voice["sha256"]}
    converted = directory / "converted.wav"
    try:
        record = json.loads((directory / "conversion.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        record = {}
    if not isinstance(record, dict):
        record = {}
    if not (record.get("inputs") == inputs and record.get("identity") == voice["converter"]
            and converted.is_file() and record.get("sha256") == characters._digest(converted)):
        # The video pipeline is out of scope, but MLX may still cache its GPU
        # buffers. Release them before the separate PyTorch MPS worker starts.
        gc.collect()
        mlx = sys.modules.get("mlx.core")
        if mlx is not None:
            mlx.clear_cache()
        record = _worker("--source-audio", audio, "--reference", reference, "--output-dir", directory)
    if (record.get("inputs") != inputs or record.get("identity") != voice["converter"]
            or record.get("sha256") != characters._digest(converted)
            or characters._digest(reference) != voice["sha256"]):
        raise ValueError("Converted soundtrack does not match the saved character and native take.")
    partial = clip.with_name("video.voice.partial.mp4")
    soundtrack = directory / "soundtrack.wav"
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(converted), "-af", f"apad,atrim=duration={seconds},asetpts=PTS-STARTPTS",
        "-c:a", "pcm_s16le", str(soundtrack),
    ], check=True, capture_output=True, text=True)
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(source), "-i", str(soundtrack), "-map", "0:v:0", "-map", "1:a:0",
        "-c:v", "copy",
        "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2",
        "-t", str(seconds), "-movflags", "+faststart", str(partial),
    ], check=True, capture_output=True, text=True)
    from validation import validate_supplied_audio, validate_video
    validate_video(partial, seconds * quality_backend.FPS, width, height, expect_audio=True)
    audio_validation = validate_supplied_audio(partial, soundtrack, seconds)
    partial.replace(clip)
    return dict(record, character=voice["character"], native_picture_unchanged=True,
                soundtrack_sha256=characters._digest(soundtrack), audio_validation=audio_validation,
                review="pending: voice identity, spoken words, and lip sync")
