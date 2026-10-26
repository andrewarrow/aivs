"""One-shot, opt-in native joint audio/video diagnostic render for Lena or Owen."""

from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import wave

import characters
import quality_backend
from character_milestone import DEFAULT_REQUEST, _asset, _checked_asset, _fingerprint
from shot_renderer import _atomic_json
import video
from validation import validate_video


ROOT = quality_backend.ROOT
SEED = 83
FRAMES = 96
FPS = 24
WIDTH = 1280
HEIGHT = 704
PROFILES = {
    "lena": {
        "role": "SUE", "character": "lena", "line_id": "sue-line",
        "line": "We still have some time.", "pronoun": "She",
        "starting_image": "references/desert-lena.png",
    },
    "owen": {
        "role": "JOE", "character": "owen", "line_id": "joe-line",
        "line": "Then let's keep going.", "pronoun": "He",
        "starting_image": "references/desert-owen.png",
    },
}
_REVIEW = {
    "status": "pending",
    "checks": ["spoken words", "captions", "face identity", "lip sync", "source speech quality"],
    "spoken_words": "pending",
    "captions": "pending",
    "face_identity": "pending",
    "lip_sync": "pending",
    "source_speech_quality": "pending",
}


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _copy_source(path: Path, target: Path, run_dir: Path, expected_sha256: str | None = None) -> dict:
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(path, target)
    record = _asset(target, run_dir)
    if expected_sha256 is not None and record["sha256"] != expected_sha256:
        raise ValueError(f"Copied source asset changed: {path.name}")
    return record


def _read_request(request_file: Path, character_root: Path,
                  character: str = "lena") -> tuple[dict, dict, dict, Path, Path]:
    if character not in PROFILES:
        raise ValueError(f"Unsupported native-reference character: {character}")
    selection = PROFILES[character]
    request_file = request_file.resolve()
    request = json.loads(request_file.read_text(encoding="utf-8"))
    if request.get("version") != 1:
        raise ValueError("Milestone request must have version 1.")
    role, performer = selection["role"], selection["character"]
    line_id, line = selection["line_id"], selection["line"]
    cast = [item for item in request.get("cast", []) if item.get("role") == role]
    lines = [item for item in request.get("lines", []) if item.get("id") == line_id]
    shots = [item for item in request.get("shots", [])
             if item.get("line") == line_id and item.get("scene") == "Desert road at dusk"]
    if (len(cast) != 1 or cast[0].get("character") != performer
            or len(lines) != 1 or lines[0].get("role") != role or lines[0].get("text") != line
            or len(shots) != 1
            or shots[0].get("image", {}).get("file") != selection["starting_image"]):
        raise ValueError(f"Milestone request does not contain the expected {role}/{performer} shot and exact line.")

    profiles = characters.resolve_characters("", explicit=[performer], root=character_root,
                                             references_only=True)
    if len(profiles) != 1 or profiles[0].get("id") != performer or not profiles[0].get("image"):
        raise ValueError(f"{performer.title()}'s canonical portrait is unavailable.")
    profile = profiles[0]
    shot = shots[0]
    source_images = shot.get("image", {}).get("source_images")
    if not isinstance(source_images, dict) or source_images.get(profile["id"]) != profile["image"]["sha256"]:
        raise ValueError(f"Desert starting image was made from a different {performer.title()} portrait.")
    image_path = _checked_asset(request_file.parent, shot["image"])
    provenance_record = request.get("reference_provenance")
    if not isinstance(provenance_record, dict):
        raise ValueError("Milestone request is missing reference provenance.")
    provenance_path = _checked_asset(request_file.parent, provenance_record)
    return request, profile, shot, image_path, provenance_path


def _manifest_source(run_dir: Path, request_file: Path, image_path: Path,
                     provenance_path: Path, profile: dict, character_root: Path,
                     image_sha256: str, provenance_sha256: str) -> dict:
    source_dir = run_dir / "source"
    copied = {
        "reference_provenance": _copy_source(provenance_path, source_dir / "reference-provenance.json", run_dir,
                                              provenance_sha256),
        "starting_image": _copy_source(image_path, run_dir / "start.png", run_dir,
                                       image_sha256),
    }
    copied["request"] = _copy_source(request_file, source_dir / "request.json", run_dir, _sha256(request_file))
    char_image = _checked_asset(character_root / "characters" / profile["id"], profile["image"])
    copied["canonical_portrait"] = _copy_source(char_image, run_dir / "canonical-portrait.png", run_dir,
                                                profile["image"]["sha256"])

    source_files = {}
    python_dir = Path(__file__).resolve().parent
    for path in sorted([*python_dir.glob("*.py"), *python_dir.glob("requirements*.txt")]):
        source_files[path.name] = _copy_source(path, source_dir / path.name, run_dir)

    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, check=True,
                          capture_output=True, text=True).stdout.strip()
    status = subprocess.run(["git", "status", "--short"], cwd=ROOT, check=True,
                            capture_output=True, text=True).stdout
    (source_dir / "git-status.txt").write_text(status, encoding="utf-8")
    return {
        "request": copied["request"],
        "reference_provenance": copied["reference_provenance"],
        "starting_image": copied["starting_image"],
        "canonical_portrait": copied["canonical_portrait"],
        "files": source_files,
        "git_head": head,
        "git_status": status,
        "git_status_file": str((source_dir / "git-status.txt").relative_to(run_dir)),
    }


def _final_video_info(path: Path) -> dict:
    return validate_video(path, FRAMES, WIDTH, HEIGHT, expect_audio=True)


def _extract_native_audio(video_path: Path, wav_path: Path) -> dict:
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(video_path), "-map", "0:a:0", "-vn", "-c:a", "pcm_s16le",
        "-ar", "48000", "-ac", "2", "-t", "4", str(wav_path),
    ], check=True, capture_output=True, text=True)
    with wave.open(str(wav_path), "rb") as stream:
        if (stream.getcomptype() != "NONE" or stream.getsampwidth() != 2
                or stream.getframerate() != 48000 or stream.getnchannels() != 2
                or stream.getnframes() == 0):
            raise ValueError("Extracted native audio is not stereo 48 kHz PCM.")
        duration = stream.getnframes() / stream.getframerate()
    if abs(duration - 4.0) > 0.03:
        raise ValueError(f"Extracted native audio duration is {duration:.3f}s; expected 4.000s.")
    return {"file": wav_path.name, "sha256": _sha256(wav_path), "sample_rate": 48000,
            "channels": 2, "encoding": "PCM_S16LE", "duration_seconds": duration,
            "derived_from": {"file": video_path.name, "sha256": _sha256(video_path),
                             "stream": "0:a:0", "trim_seconds": 4}}


def run(run_dir: Path | None = None, *, request_file: Path = DEFAULT_REQUEST,
        character: str = "lena", character_root: Path = ROOT, render_fn=None,
        doctor_fn=None, lock_factory=None) -> dict:
    """Validate inputs, create a new immutable run, and render one native-reference take."""
    request_file, character_root = Path(request_file).resolve(), Path(character_root).resolve()
    selection = PROFILES.get(character)
    if selection is None:
        raise ValueError(f"Unsupported native-reference character: {character}")
    request, profile, shot, image_path, provenance_path = _read_request(request_file, character_root, character)
    if run_dir is None:
        run_dir = ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_diagnostic-native-voice"
    run_dir = Path(run_dir).resolve()

    lock = lock_factory or video.project_lock
    with lock(".render.lock"):
        run_dir.mkdir(parents=True, exist_ok=False)
        report = {
            "status": "preparing",
            "run_dir": str(run_dir),
            "label": "diagnostic-native-voice",
            "character_id": character,
            "role": selection["role"],
            "line": selection["line"],
            "fixed_voice_acceptance": False,
            "audio": "temporary generated native voice; not fixed-voice acceptance",
            "review": dict(_REVIEW),
            "seed": SEED,
            "frames": FRAMES,
            "fps": FPS,
            "width": WIDTH,
            "height": HEIGHT,
        }
        _atomic_json(run_dir / "render.json", report)
        try:
            os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
            source = _manifest_source(run_dir, request_file, image_path, provenance_path, profile, character_root,
                                      shot["image"]["sha256"], request["reference_provenance"]["sha256"])
            snapshots = characters.snapshot_characters([profile], run_dir, root=character_root)
            prompt = (shot["prompt"].rstrip()
                      + f' {selection["pronoun"]} speaks naturally and calmly: “{selection["line"]}”'
                      + " Leave a brief quiet moment at the beginning and end, with clean isolated speech.")
            (run_dir / "prompt.txt").write_text(prompt + "\n", encoding="utf-8")
            (run_dir / "dialogue.txt").write_text(selection["line"] + "\n", encoding="utf-8")
            characters.verify_snapshots(snapshots, run_dir)
            source_hashes = {name: source[name]["sha256"] for name in
                             ("request", "starting_image", "canonical_portrait", "reference_provenance")}
            report.update(status="rendering", source=source, character=snapshots[0],
                          source_hashes=source_hashes,
                          prompt="prompt.txt", dialogue="dialogue.txt",
                          backend={"model": quality_backend.MODEL_REPO,
                                   "model_revision": quality_backend.MODEL_REVISION,
                                   "runtime_revision": quality_backend.RUNTIME_REVISION,
                                   "pipeline": "DistilledPipeline", "stage1_steps": 8,
                                   "stage2_steps": 3, "quantization": "int8"},
                          config={"seed": SEED, "frames": FRAMES, "fps": FPS,
                                  "width": WIDTH, "height": HEIGHT,
                                  "image_conditioning": "whole-first-frame", "hold_image_framing": False,
                                  "audio_input": None})
            report["command"] = [sys.executable, str(Path(__file__).resolve()),
                                  "--character", character, "--run-dir", str(run_dir)]
            manifest = {
                "version": 1, "label": "diagnostic-native-voice",
                "fixed_voice_acceptance": False, "seed": SEED, "frames": FRAMES,
                "fps": FPS, "width": WIDTH, "height": HEIGHT,
                "role": selection["role"], "character_id": character,
                "line_id": selection["line_id"],
                "dialogue": selection["line"], "prompt": prompt, "source": source,
                "character_snapshot": snapshots[0], "backend": report["backend"],
                "config": report["config"],
                "temporary_generated_voice": True,
                "command": report["command"],
                "source_hashes": source_hashes,
            }
            manifest["source_fingerprint"] = _fingerprint(manifest)
            _atomic_json(run_dir / "manifest.json", manifest)
            _atomic_json(run_dir / "render.json", report)
            (doctor_fn or quality_backend.doctor)()
            render_call = render_fn or quality_backend.render
            report["runtime"] = render_call(
                prompt, SEED, FRAMES, run_dir / "video.mp4",
                image_file=run_dir / "start.png", hold_image_framing=False,
                width=WIDTH, height=HEIGHT, keep_native=True,
            )
            native_video = run_dir / "video.native.mp4"
            final_video = run_dir / "video.mp4"
            if not native_video.is_file() or not final_video.is_file():
                raise RuntimeError("Native render did not preserve both native and final video files.")
            report["video"] = {"file": final_video.name, "sha256": _sha256(final_video),
                               "validation": _final_video_info(final_video)}
            report["native_video"] = {"file": native_video.name, "sha256": _sha256(native_video)}
            report["native_audio"] = _extract_native_audio(final_video, run_dir / "native-audio.wav")
            report.update(status="rendered-needs-review")
            _atomic_json(run_dir / "render.json", report)
            return report
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            _atomic_json(run_dir / "render.json", report)
            raise


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, help="new output directory; existing paths are never overwritten")
    parser.add_argument("--character", choices=sorted(PROFILES), default="lena")
    args = parser.parse_args(argv)
    signal.signal(signal.SIGTERM, lambda _signum, _frame: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        report = run(args.run_dir, character=args.character)
    except KeyboardInterrupt:
        return 130
    print(json.dumps({"status": report["status"], "run_dir": report["run_dir"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
