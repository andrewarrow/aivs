"""Prepare or render an opt-in character consistency experiment.

Preparation uses CPU speech and pre-authored scene references. Video generation
uses the existing workspace lock. Every take remains pending identity review.
"""

from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import wave

import characters
import quality_backend
from shot_renderer import _assemble, _atomic_json

ROOT = quality_backend.ROOT
DEFAULT_REQUEST = Path(__file__).resolve().parents[1] / "experiments/character-consistency/request.json"
_REVIEW = {"status": "pending", "checks": ["face identity", "voice identity", "spoken words", "lip sync"]}


def _fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _file(root: Path, relative: str) -> Path:
    if not isinstance(relative, str) or not relative:
        raise ValueError("Asset path must be nonempty text.")
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError(f"Missing asset or path outside its directory: {relative}")
    return path


def _asset(path: Path, root: Path) -> dict:
    return {"file": str(path.relative_to(root)), "sha256": characters._digest(path)}


def _checked_asset(root: Path, record: dict) -> Path:
    if not isinstance(record, dict):
        raise ValueError("Asset needs a file and checksum.")
    path = _file(root, record.get("file"))
    if characters._digest(path) != record.get("sha256"):
        raise ValueError(f"Asset changed: {record['file']}")
    return path


def _speech(profile, text, output, voice_python, cache):
    text_file = output.with_suffix(".txt")
    text_file.write_text(text, encoding="utf-8")
    subprocess.run([
        str(voice_python), str(Path(__file__).with_name("speech.py")), "synthesize",
        "--voice-id", profile["voice_id"], "--text-file", str(text_file),
        "--output", str(output), "--cache", str(cache),
    ], check=True)
    return json.loads(output.with_suffix(".json").read_text(encoding="utf-8"))


def _shot_audio(source: Path, output: Path) -> dict:
    with wave.open(str(source), "rb") as stream:
        if stream.getnchannels() != 1 or stream.getsampwidth() != 2 or stream.getcomptype() != "NONE":
            raise ValueError("Prepared speech must be mono 16-bit PCM WAV.")
        rate, count = stream.getframerate(), stream.getnframes()
        pcm = stream.readframes(count)
        if len(pcm) != count * 2 or count == 0:
            raise ValueError("Prepared speech is empty or truncated.")
    lead = round(0.35 * rate)
    seconds = max(4, math.ceil((lead + count) / rate + 0.65))
    if seconds > 12:
        raise ValueError("Milestone dialogue must fit a 12-second shot with pauses.")
    with wave.open(str(output), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(rate)
        stream.writeframes(b"\0\0" * lead + pcm + b"\0\0" * (seconds * rate - lead - count))
    return {"seconds": seconds, "start_seconds": lead / rate, "end_seconds": (lead + count) / rate}


def prepare(request_file: Path, run_dir: Path, *, character_root=ROOT, voice_python=None,
            cache=None, speech_fn=None) -> dict:
    request_file, run_dir = Path(request_file).resolve(), Path(run_dir).resolve()
    request = json.loads(request_file.read_text(encoding="utf-8"))
    if request.get("version") != 1 or not isinstance(request.get("cast"), list):
        raise ValueError("Milestone request must have version 1 and a cast list.")
    cast = request["cast"]
    if (not cast or any(not isinstance(item, dict) or set(item) != {"role", "character"}
                        or any(not isinstance(item[key], str) or not item[key].strip() for key in item)
                        for item in cast)):
        raise ValueError("Each cast binding needs a nonempty role and character.")
    profiles = characters.resolve_characters("", explicit=[item["character"] for item in cast],
                                             root=character_root, references_only=True)
    by_id = {profile["id"]: profile for profile in profiles}
    roles = {item["role"]: item["character"] for item in cast}
    if len({role.casefold() for role in roles}) != len(cast):
        raise ValueError("Cast roles must be nonempty and unique.")
    lines = request.get("lines", [])
    by_line = {line["id"]: line for line in lines}
    shots = request.get("shots", [])
    if not lines or len(by_line) != len(lines) or not shots:
        raise ValueError("Milestone needs unique dialogue lines and at least one shot.")
    if any(not p.get("image") for p in profiles):
        raise ValueError("Every performer needs a canonical portrait.")
    provenance = request.get("reference_provenance")
    if provenance is not None:
        _checked_asset(request_file.parent, provenance)
    for line in lines:
        if (line.get("role") not in roles or not isinstance(line.get("text"), str)
                or not line["text"].strip() or len(line["text"]) > 4000):
            raise ValueError("Dialogue needs a bound role and nonempty text of at most 4000 characters.")
        if not by_id[roles[line["role"]]].get("voice_id"):
            raise ValueError(f"Saved performer has no fixed voice: {line['role']}")
    for shot in shots:
        if shot.get("line") not in by_line or not isinstance(shot.get("prompt"), str) or not shot["prompt"].strip():
            raise ValueError("Each shot needs a dialogue line and prompt.")
        _checked_asset(request_file.parent, shot["image"])
        if shot["image"].get("source_images") != {p["id"]: p.get("image", {}).get("sha256") for p in profiles}:
            raise ValueError("Shot reference was made for different character portraits.")
    run_dir.mkdir(parents=True, exist_ok=False)
    report = {"status": "preparing", "review": _REVIEW}
    _atomic_json(run_dir / "render.json", report)
    try:
        profiles = characters.snapshot_characters(profiles, run_dir, root=character_root)
        (run_dir / "request.json").write_text(json.dumps(request, indent=2) + "\n", encoding="utf-8")
        if provenance is not None:
            shutil.copyfile(_checked_asset(request_file.parent, provenance), run_dir / "reference-provenance.json")
        line_dir = run_dir / "lines"
        line_dir.mkdir()
        prepared_lines = {}
        for index, line in enumerate(lines, 1):
            profile = by_id[roles[line["role"]]]
            path = line_dir / f"{index:03d}.wav"
            print(f"Preparing {line['role']} with {profile['name']} / {profile['voice_id']}", flush=True)
            record = (speech_fn(profile, line["text"], path) if speech_fn else
                      _speech(profile, line["text"], path,
                              voice_python or ROOT / ".venv-voice/bin/python", cache or ROOT / ".cache/kokoro"))
            if (record.get("sha256") != characters._digest(path) or record.get("text") != line["text"]
                    or record.get("voice", {}).get("voice_id") != profile["voice_id"]):
                raise ValueError("Synthesized speech does not match the selected character and dialogue.")
            prepared_lines[line["id"]] = {**record, **_asset(path, run_dir),
                                          "role": line["role"], "character": profile["id"]}
        prepared_shots = []
        for index, shot in enumerate(shots, 1):
            directory = run_dir / "shots" / f"{index:03d}"
            directory.mkdir(parents=True)
            image_path = directory / "start.png"
            shutil.copyfile(_checked_asset(request_file.parent, shot["image"]), image_path)
            line = prepared_lines[shot["line"]]
            timing = _shot_audio(run_dir / line["file"], directory / "audio.wav")
            prompt = (shot["prompt"].rstrip() + " After a brief pause, the visible speaker says, "
                      + json.dumps(line["text"], ensure_ascii=False)
                      + ". The lips follow this exact soundtrack, then close as the speaker listens silently. "
                        "Natural breathing and small eye movements. No subtitles, captions or visible writing.")
            prepared_shots.append({"index": index, "scene": shot["scene"], "prompt": prompt,
                                   "line": shot["line"], "role": line["role"], "character": line["character"],
                                   "seconds": timing["seconds"], "frames": timing["seconds"] * quality_backend.FPS,
                                   "speaker": {"role": line["role"], "character": line["character"],
                                               "start_seconds": timing["start_seconds"], "end_seconds": timing["end_seconds"]},
                                   "image": {**_asset(image_path, run_dir), "source_images": shot["image"]["source_images"]},
                                   "audio": _asset(directory / "audio.wav", run_dir)})
        manifest = {"version": 1, "fps": quality_backend.FPS, "width": 768, "height": 448,
                    "cast": cast, "characters": profiles, "lines": prepared_lines, "shots": prepared_shots,
                    "backend": {"model": quality_backend.MODEL_REPO, "model_revision": quality_backend.MODEL_REVISION,
                                "runtime_revision": quality_backend.RUNTIME_REVISION}}
        if provenance is not None:
            manifest["reference_provenance"] = _asset(run_dir / "reference-provenance.json", run_dir)
        manifest["source_sha256"] = _fingerprint(manifest)
        _atomic_json(run_dir / "manifest.json", manifest)
        report.update(status="prepared", source_sha256=manifest["source_sha256"],
                      shots=[{"index": s["index"], "status": "pending"} for s in prepared_shots])
        _atomic_json(run_dir / "render.json", report)
        return manifest
    except BaseException as exc:
        report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed", error=str(exc))
        _atomic_json(run_dir / "render.json", report)
        raise


def verify(run_dir: Path) -> dict:
    manifest = json.loads((run_dir / "manifest.json").read_text(encoding="utf-8"))
    payload = {key: value for key, value in manifest.items() if key != "source_sha256"}
    if manifest.get("version") != 1 or manifest.get("source_sha256") != _fingerprint(payload):
        raise ValueError("Prepared milestone manifest has changed.")
    characters.verify_snapshots(manifest["characters"], run_dir)
    if "reference_provenance" in manifest:
        _checked_asset(run_dir, manifest["reference_provenance"])
    if manifest["backend"] != {"model": quality_backend.MODEL_REPO, "model_revision": quality_backend.MODEL_REVISION,
                               "runtime_revision": quality_backend.RUNTIME_REVISION}:
        raise ValueError("Prepared milestone uses a different video backend revision.")
    for line in manifest["lines"].values():
        _checked_asset(run_dir, line)
    for shot in manifest["shots"]:
        _checked_asset(run_dir, shot["image"])
        audio = _checked_asset(run_dir, shot["audio"])
        quality_backend.prepare_source_audio(audio, shot["seconds"])
    return manifest


def render(run_dir: Path, *, seed=83, shot_number=None, render_fn=None) -> dict:
    from video import project_lock

    run_dir = Path(run_dir).resolve()
    with project_lock(".render.lock"):
        manifest = verify(run_dir)
        if render_fn is None:
            quality_backend.doctor()
        os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
        selected = manifest["shots"]
        if shot_number is not None:
            selected = [shot for shot in selected if shot["index"] == shot_number]
            if not selected:
                raise ValueError("Shot number is outside the prepared experiment.")
        report = {"status": "rendering", "source_sha256": manifest["source_sha256"], "review": _REVIEW}
        _atomic_json(run_dir / "render.json", report)
        try:
            for shot in selected:
                output = run_dir / "shots" / f"{shot['index']:03d}" / "video.mp4"
                record_path = output.with_name("render.json")
                shot_seed = (seed + shot["index"] - 1) % 2**31
                record = {"status": "rendering", "source_sha256": manifest["source_sha256"],
                          "seed": shot_seed, "review": _REVIEW}
                if output.exists() and not record_path.exists():
                    raise ValueError(f"Existing take has no matching render record: {output}")
                if record_path.exists() and output.exists():
                    previous = json.loads(record_path.read_text())
                    if (previous.get("status") == "rendered-needs-review" and previous.get("seed") == shot_seed
                            and previous.get("source_sha256") == manifest["source_sha256"]
                            and previous.get("sha256") == characters._digest(output)):
                        continue
                    raise ValueError(f"Existing take differs from this request: {output}")
                _atomic_json(record_path, record)
                try:
                    runtime = (render_fn or quality_backend.render)(
                        shot["prompt"], shot_seed, shot["frames"], output,
                        image_file=_checked_asset(run_dir, shot["image"]),
                        audio_file=_checked_asset(run_dir, shot["audio"]),
                        width=manifest["width"], height=manifest["height"],
                    )
                    record.update(status="rendered-needs-review", runtime=runtime, sha256=characters._digest(output))
                except BaseException as exc:
                    record.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed", error=str(exc))
                    raise
                finally:
                    _atomic_json(record_path, record)
            completed = []
            for shot in manifest["shots"]:
                output = run_dir / "shots" / f"{shot['index']:03d}" / "video.mp4"
                record_path = output.with_name("render.json")
                if record_path.exists() and output.exists():
                    record = json.loads(record_path.read_text())
                    if (record.get("status") == "rendered-needs-review"
                            and record.get("source_sha256") == manifest["source_sha256"]
                            and record.get("seed") == (seed + shot["index"] - 1) % 2**31
                            and record.get("sha256") == characters._digest(output)):
                        completed.append(output)
            if len(completed) == len(manifest["shots"]):
                report["video"] = _assemble(completed, run_dir / "video.mp4",
                                            sum(s["seconds"] for s in manifest["shots"]), False,
                                            width=manifest["width"], height=manifest["height"])
                report["status"] = "rendered-needs-review"
            else:
                report.update(status="partially-rendered", rendered_shots=len(completed))
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed", error=str(exc))
            raise
        finally:
            _atomic_json(run_dir / "render.json", report)
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "verify", "render"))
    parser.add_argument("--request", type=Path, default=DEFAULT_REQUEST)
    parser.add_argument("--run-dir", type=Path)
    parser.add_argument("--voice-python", type=Path)
    parser.add_argument("--seed", type=int, default=83)
    parser.add_argument("--shot", type=int)
    args = parser.parse_args()
    run_dir = args.run_dir or ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_character-milestone"
    if args.command != "prepare" and args.run_dir is None:
        parser.error("verify/render require --run-dir")
    try:
        if args.command == "prepare":
            prepare(args.request, run_dir, voice_python=args.voice_python)
        elif args.command == "verify":
            verify(run_dir.resolve())
        else:
            render(run_dir, seed=args.seed, shot_number=args.shot)
        print(f"{args.command}: {run_dir.resolve()}", flush=True)
        return 0
    except Exception as exc:
        print(f"{args.command} failed: {exc}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
