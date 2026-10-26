"""Prepare an immutable, reviewed native take and its canonical voice reference."""

from __future__ import annotations

import argparse
from array import array
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import wave

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ACCEPTANCE = ROOT / "experiments/character-consistency/native-reference-control.json"
SUPPORTED_VOICES = {"lena": "af_sarah", "owen": "am_michael"}
ACTIVITY_THRESHOLD_DBFS = -45.0
PCM16_FULL_SCALE = 32768.0
SOURCE_FILES = {
    "prepare_voice_reference.py": Path(__file__),
    "characters.py": ROOT / "python/characters.py",
    "speech.py": ROOT / "python/speech.py",
    "dialogue_helpers.py": ROOT / "python/dialogue_helpers.py",
    "requirements-voice.txt": ROOT / "python/requirements-voice.txt",
}

sys.path.insert(0, str(ROOT / "python"))
import characters  # noqa: E402


def _json(path: Path) -> dict:
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"Cannot read JSON: {path}") from exc
    if not isinstance(result, dict):
        raise ValueError(f"Expected a JSON object: {path}")
    return result


def _repo_file(value: str, label: str) -> Path:
    path = (ROOT / value).resolve()
    if not path.is_relative_to(ROOT) or not path.is_file():
        raise ValueError(f"Missing or unsafe {label}: {value}")
    return path


def _verified_file(record: dict, label: str) -> Path:
    if not isinstance(record, dict) or not isinstance(record.get("file"), str):
        raise ValueError(f"Missing {label} file record.")
    path = _repo_file(record["file"], label)
    if characters._digest(path) != record.get("sha256"):
        raise ValueError(f"{label} hash does not match accepted evidence: {path}")
    return path


def _wav_stats(path: Path) -> dict:
    try:
        with wave.open(str(path), "rb") as wav:
            channels, width, rate, frames = (wav.getnchannels(), wav.getsampwidth(),
                                              wav.getframerate(), wav.getnframes())
            if channels < 1 or width != 2 or rate < 1 or frames < 1:
                raise ValueError("Reference audio must be non-empty PCM16 WAV with valid rate/channels.")
            raw = wav.readframes(frames)
            if len(raw) != frames * channels * width or wav.readframes(1):
                raise ValueError("Reference WAV frame data is truncated or inconsistent.")
    except (wave.Error, EOFError, OSError) as exc:
        raise ValueError(f"Malformed PCM16 WAV: {path}") from exc
    values = array("h")
    values.frombytes(raw)
    if sys.byteorder != "little":
        values.byteswap()
    if len(values) != frames * channels:
        raise ValueError("Reference WAV has inconsistent PCM sample data.")
    peak = max(abs(value) for value in values)
    mean = sum(values) / len(values)
    rms = math.sqrt(sum(value * value for value in values) / len(values))
    threshold = PCM16_FULL_SCALE * 10 ** (ACTIVITY_THRESHOLD_DBFS / 20)
    window_frames = max(1, round(rate * 0.020))
    active = []
    for start in range(0, frames, window_frames):
        end = min(frames, start + window_frames)
        section = values[start * channels:end * channels]
        section_mean = sum(section) / len(section)
        demeaned_rms = math.sqrt(sum((sample - section_mean) ** 2 for sample in section) / len(section))
        if demeaned_rms >= threshold:
            active.append((start, end))
    if not active:
        first = last = None
    else:
        first = active[0][0] / rate
        last = active[-1][1] / rate
    issues = []
    if peak == 0:
        issues.append("silent")
    if any(value in (-32768, 32767) for value in values):
        issues.append("pcm16-full-scale-samples")
    return {
        "duration_seconds": frames / rate, "sample_rate_hz": rate, "channels": channels,
        "frames": frames, "sample_width_bytes": width, "peak_pcm16": peak,
        "peak_dbfs": None if peak == 0 else 20 * math.log10(peak / PCM16_FULL_SCALE),
        "rms_pcm16": rms, "rms_dbfs": None if rms == 0 else 20 * math.log10(rms / PCM16_FULL_SCALE),
        "dc_offset_pcm16": mean, "clipped_sample_count": sum(v in (-32768, 32767) for v in values),
        "activity": {"method": "20ms demeaned PCM RMS; approximate activity only, not VAD or perceptual review",
                     "threshold_dbfs": ACTIVITY_THRESHOLD_DBFS, "active_windows": len(active),
                     "window_count": math.ceil(frames / window_frames),
                     "first_active_seconds": first, "last_active_end_seconds": last},
        "quality_issues": issues,
    }


def _git_head() -> str:
    try:
        return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True,
                                       stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return "unavailable"


def prepare_voice_reference(acceptance_path: Path, run_dir: Path) -> dict:
    acceptance_path = Path(acceptance_path).expanduser().resolve()
    run_dir = Path(run_dir).expanduser()
    run_dir = (ROOT / run_dir).resolve() if not run_dir.is_absolute() else run_dir.resolve()
    if run_dir.exists():
        raise FileExistsError(f"Output directory already exists: {run_dir}")

    acceptance = _json(acceptance_path)
    human = acceptance.get("human_review", {})
    if (acceptance.get("status") != "accepted-diagnostic-native-take"
            or acceptance.get("fixed_voice_acceptance") is not False
            or human.get("status") != "accepted"
            or any(human.get(key) != "accepted" for key in ("lip_sync", "face_identity", "spoken_words"))):
        raise ValueError("Acceptance must record a human-approved native take with fixed voice still unaccepted.")

    acceptance_hash = characters._digest(acceptance_path)
    source_run_rel = acceptance.get("run")
    if not isinstance(source_run_rel, str):
        raise ValueError("Acceptance has no source run path.")
    source_run = _repo_file(source_run_rel + "/manifest.json", "source manifest").parent
    manifest_path = _verified_file(acceptance.get("source", {}).get("manifest"), "source manifest")
    render_path = _verified_file(acceptance.get("source", {}).get("render_report"), "source render report")
    if manifest_path.parent != source_run or render_path.parent != source_run:
        raise ValueError("Accepted manifest/render report do not belong to the named source run.")
    manifest = _json(manifest_path)
    render = _json(render_path)
    if manifest.get("fixed_voice_acceptance") is not False or render.get("fixed_voice_acceptance") is not False:
        raise ValueError("Source run incorrectly claims fixed voice acceptance.")
    character = manifest.get("character_id")
    accepted_profile = manifest.get("character_snapshot", manifest.get("character"))
    if (not isinstance(character, str) or character not in SUPPORTED_VOICES
            or not isinstance(accepted_profile, dict) or accepted_profile.get("id") != character
            or acceptance.get("character_id", character) != character):
        raise ValueError("Accepted source character and snapshot must identify the same supported performer.")
    if (acceptance.get("native_take_accepted") is not True
            and not (character == "lena" and acceptance.get("milestone_a_passed") is True)):
        raise ValueError("Acceptance must explicitly approve this native take; legacy Milestone A approval applies only to Lena.")
    voice_id = SUPPORTED_VOICES[character]
    evidence_record = acceptance.get("automated_observations", {}).get("evidence")
    evidence_path = _verified_file(evidence_record, "review evidence")
    expected_evidence = (source_run / "review/evidence.json").resolve()
    if evidence_path != expected_evidence:
        raise ValueError("Accepted review evidence does not belong to the named source run.")
    evidence = _json(evidence_path)
    input_paths = {
        "manifest": ("manifest.json", manifest_path),
        "render": ("render.json", render_path),
        "video": ("video.mp4", None),
        "native_video": ("video.native.mp4", None),
        "native_audio": ("native-audio.wav", None),
    }
    evidence_inputs = evidence.get("inputs", {})
    verified_inputs = {}
    for key, (expected_name, _) in input_paths.items():
        record = evidence_inputs.get(key)
        if not isinstance(record, dict) or record.get("file") != expected_name:
            raise ValueError(f"Source {key} does not name the expected file in review evidence.")
        verified_inputs[key] = _verified_file(
            {**record, "file": f"{source_run_rel}/{expected_name}"}, f"source {key}")
    if verified_inputs["manifest"] != manifest_path or verified_inputs["render"] != render_path:
        raise ValueError("Source manifest/render paths differ from review evidence.")

    accepted_outputs = acceptance.get("outputs", {})
    output_fields = {"video": "video.mp4", "native_video": "video.native.mp4",
                     "native_audio": "native-audio.wav"}
    output_paths = {}
    for key, filename in output_fields.items():
        accepted_key = "original_native_video" if key == "native_video" else key
        expected_path = (source_run / filename).resolve()
        output_path = _verified_file(accepted_outputs.get(accepted_key), f"accepted {key}")
        if output_path != expected_path or output_path != verified_inputs[key]:
            raise ValueError(f"Accepted {key} does not match the named source run and review evidence.")
        output_paths[key] = output_path

    for key in output_fields:
        rendered = render.get(key)
        if not isinstance(rendered, dict):
            raise ValueError(f"Source render report has no {key} record.")
        if rendered.get("file") != output_fields[key] or rendered.get("sha256") != evidence_inputs[key].get("sha256"):
            raise ValueError(f"Source render {key} differs from review evidence.")

    video = output_paths["video"]
    audio = output_paths["native_audio"]
    stats = _wav_stats(audio)

    snapshot_path = source_run / "characters" / character / "character.json"
    source_profile = characters._read_profile(snapshot_path)
    if source_profile != accepted_profile:
        raise ValueError("Source character snapshot differs from the accepted manifest.")
    source_voice = snapshot_path.parent / "voice.wav"
    if (not isinstance(accepted_profile, dict)
            or accepted_profile.get("voice_sample", {}).get("file") != "voice.wav"
            or not source_voice.is_file()
            or characters._digest(source_voice) != accepted_profile.get("voice_sample", {}).get("sha256")):
        raise ValueError("Source voice reference does not match the accepted manifest snapshot.")
    current = characters.load_character(character, ROOT)
    if accepted_profile != current or accepted_profile.get("voice_id") != voice_id:
        raise ValueError(f"Current {character.title()} profile differs from the accepted snapshot or expected voice ID {voice_id}.")
    accepted_voice_hash = accepted_profile.get("voice_sample", {}).get("sha256")
    if current.get("voice_sample", {}).get("sha256") != accepted_voice_hash:
        raise ValueError(f"Current {character.title()} voice reference hash differs from the accepted take snapshot.")

    expected_outputs = {
        "source-video.mp4": video,
        "source-native-video.mp4": output_paths["native_video"],
        "source-audio.wav": audio,
        "acceptance.json": acceptance_path,
        "source-manifest.json": manifest_path,
        "source-render.json": render_path,
        "source-review-evidence.json": evidence_path,
    }
    sheet_files = {}
    for name, record in acceptance.get("visual_review", {}).get("sheets", {}).items():
        sheet_files[f"evidence-{name}"] = _verified_file(record, f"visual evidence {name}")
    run_dir.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{run_dir.name}.", dir=run_dir.parent))
    try:
        files = {}
        for name, source in {**expected_outputs, **sheet_files}.items():
            target = staging / name
            shutil.copyfile(source, target)
            digest = characters._digest(target)
            if digest != characters._digest(source):
                raise ValueError(f"Copied input failed checksum: {name}")
            files[name] = {"file": name, "sha256": digest}
        characters.snapshot_characters([current], staging, ROOT)
        characters.verify_snapshots([current], staging)
        source_snap = staging / "source"
        source_snap.mkdir()
        source_code = {}
        for name, source in SOURCE_FILES.items():
            shutil.copyfile(source, source_snap / name)
            source_code[name] = characters._digest(source_snap / name)
        reference_path = f"characters/{character}/voice.wav"
        voice_path = staging / reference_path
        voice_stats = _wav_stats(voice_path)
        manifest_out = {
            "version": 1, "status": "reference-prepared-needs-quality-review",
            "fixed_voice_acceptance": False, "source_acceptance_sha256": acceptance_hash,
            "source_run": source_run_rel, "source_manifest_sha256": characters._digest(manifest_path),
            "source_render_sha256": characters._digest(render_path), "source_evidence_sha256": characters._digest(evidence_path),
            "source_video_sha256": characters._digest(video), "source_native_video_sha256": characters._digest(output_paths["native_video"]),
            "source_audio_sha256": characters._digest(audio),
            "source_character_voice_sha256": accepted_voice_hash,
            "character": {"id": character, "voice_id": current["voice_id"],
                          "voice_reference": reference_path,
                          "voice_reference_sha256": characters._digest(voice_path)},
            "canonical_reference_path": reference_path,
            "inputs": files, "source_code_sha256": source_code,
            "accepted_git_sha": acceptance.get("source", {}).get("git_head"), "prepared_git_sha": _git_head(),
            "native_audio_stats": stats, "voice_reference_stats": voice_stats,
            "prepared_at": datetime.now(timezone.utc).isoformat(),
        }
        (staging / "manifest.json").write_text(json.dumps(manifest_out, indent=2) + "\n", encoding="utf-8")
        if run_dir.exists():
            raise FileExistsError(f"Output directory already exists: {run_dir}")
        staging.rename(run_dir)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return manifest_out


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--acceptance", type=Path, default=DEFAULT_ACCEPTANCE)
    parser.add_argument("--run-dir", type=Path,
                        default=ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_voice-conversion")
    args = parser.parse_args(argv)
    try:
        manifest = prepare_voice_reference(args.acceptance, args.run_dir)
    except Exception as exc:
        print(f"Voice reference preparation failed: {exc}", file=sys.stderr, flush=True)
        return 1
    target = args.run_dir.expanduser()
    target = target.resolve() if target.is_absolute() else (ROOT / target).resolve()
    print(f"Prepared voice reference: {target} ({manifest['character']['voice_reference_sha256']})", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
