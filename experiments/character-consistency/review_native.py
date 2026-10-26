"""Collect independent, reviewable evidence for one native character diagnostic take.

Run after python/native_reference.py has finished. This command creates a fresh
``review`` directory and never changes the render or existing review evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFont, ImageOps


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = Path(__file__).resolve()
VISION_SCRIPT = SCRIPT.with_name("inspect_native.swift")
MODEL = ROOT / ".cache/whisper/ggml-base.en.bin"
MODEL_RECORD = MODEL.with_name("model.json")
BINDINGS = {
    "lena": ("SUE", "sue-line", "We still have some time."),
    "owen": ("JOE", "joe-line", "Then let's keep going."),
}
FRAMES, FPS, WIDTH, HEIGHT = 96, 24, 1280, 704


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def file_record(path: Path, run: Path) -> dict:
    return {"file": str(path.relative_to(run)), "sha256": sha256(path), "bytes": path.stat().st_size}


def normalize(text: str) -> str:
    return " ".join(re.findall(r"[a-z0-9]+(?:'[a-z0-9]+)?", text.casefold()))


def validate_manifest(manifest: dict, render: dict) -> tuple[str, str]:
    character = manifest.get("character_id")
    binding = BINDINGS.get(character)
    snapshot = manifest.get("character_snapshot", {})
    if (binding is None
            or (manifest.get("role"), manifest.get("line_id"), manifest.get("dialogue")) != binding
            or snapshot.get("id") != character or render.get("character", {}).get("id") != character):
        raise ValueError("Native diagnostic character, role, dialogue, and snapshot binding do not match")
    if (manifest.get("label") != "diagnostic-native-voice" or render.get("status") != "rendered-needs-review"
            or manifest.get("fixed_voice_acceptance") is not False
            or render.get("fixed_voice_acceptance") is not False
            or manifest.get("seed") != 83
            or (manifest.get("frames"), manifest.get("fps"), manifest.get("width"), manifest.get("height"))
            != (FRAMES, FPS, WIDTH, HEIGHT)):
        raise ValueError("Run is not a completed 96-frame native-reference diagnostic")
    source_manifest = {key: value for key, value in manifest.items() if key != "source_fingerprint"}
    fingerprint = hashlib.sha256(json.dumps(source_manifest, sort_keys=True,
                                            separators=(",", ":")).encode()).hexdigest()
    if fingerprint != manifest.get("source_fingerprint"):
        raise ValueError("Native-reference manifest changed after preparation")
    return character, binding[2]


def command(args: list[str], *, stdout: Path | None = None, stderr: Path | None = None) -> None:
    with (stdout.open("wb") if stdout else open("/dev/null", "wb")) as out:
        with (stderr.open("wb") if stderr else open("/dev/null", "wb")) as err:
            subprocess.run(args, check=True, stdout=out, stderr=err, cwd=ROOT, stdin=subprocess.DEVNULL)


def tool_record(name: str, version_args: list[str]) -> dict:
    executable = shutil.which(name)
    if executable is None:
        raise RuntimeError(f"Required review tool is missing: {name}")
    resolved = Path(executable).resolve()
    result = subprocess.run([executable, *version_args], cwd=ROOT, capture_output=True, text=True,
                            stdin=subprocess.DEVNULL, check=False)
    lines = (result.stdout + result.stderr).strip().splitlines()
    version = next((line for line in lines if "version:" in line.lower()),
                   lines[0] if lines else "unavailable")
    return {"executable": str(resolved), "sha256": sha256(resolved),
            "version": version}


def font(size: int) -> ImageFont.ImageFont:
    path = Path("/System/Library/Fonts/Menlo.ttc")
    return ImageFont.truetype(str(path), size) if path.is_file() else ImageFont.load_default()


def paste_contained(sheet: Image.Image, image: Image.Image, box: tuple[int, int, int, int]) -> None:
    x, y, width, height = box
    fitted = ImageOps.contain(image.convert("RGB"), (width, height))
    sheet.paste(fitted, (x + (width - fitted.width) // 2, y + (height - fitted.height) // 2))


def main_face_crop(image: Image.Image, frame: dict) -> Image.Image:
    faces = frame.get("faces") or []
    if not faces:
        return image
    face = max(faces, key=lambda item: item["box"][2] * item["box"][3])
    x, y, width, height = face["box"]  # Apple Vision uses a bottom-left origin.
    margin_x, margin_y = width * 0.20, height * 0.12
    left = max(0, int((x - margin_x) * image.width))
    right = min(image.width, int((x + width + margin_x) * image.width))
    top = max(0, int((1 - y - height - margin_y) * image.height))
    bottom = min(image.height, int((1 - y + margin_y) * image.height))
    return image.crop((left, top, right, bottom)) if right > left and bottom > top else image


def make_sheets(video: Path, frames: list[dict], portrait: Path,
                review: Path, ffmpeg: str) -> dict:
    face_sheets = [Image.new("RGB", (1200, 660), "white") for _ in range(4)]
    whole_sheet = Image.new("RGB", (1280, 1200), "white")
    comparison = Image.new("RGB", (1280, 420), "white")
    label_font = font(13)
    heading_font = font(17)
    for index, label in enumerate(("Canonical portrait", "First frame", "Middle frame", "Last frame")):
        ImageDraw.Draw(comparison).text((index * 320 + 8, 8), label, fill="black", font=heading_font)
    with Image.open(portrait) as image:
        paste_contained(comparison, image, (0, 38, 320, 360))

    command_line = [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
                    "-i", str(video), "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]
    frame_bytes = WIDTH * HEIGHT * 3
    with (review / "decode.log").open("wb") as error_log:
        process = subprocess.Popen(command_line, stdout=subprocess.PIPE, stderr=error_log,
                                   cwd=ROOT, stdin=subprocess.DEVNULL)
        try:
            assert process.stdout is not None
            for number, evidence in enumerate(frames):
                raw = process.stdout.read(frame_bytes)
                if len(raw) != frame_bytes:
                    raise RuntimeError(f"FFmpeg decoded only {number} of {FRAMES} frames")
                image = Image.frombytes("RGB", (WIDTH, HEIGHT), raw)
                if number in (0, 48, 95):
                    position = {0: 1, 48: 2, 95: 3}[number]
                    paste_contained(comparison, image, (position * 320, 38, 320, 360))
                if number % 4 == 0:
                    sample = number // 4
                    x, y = sample % 4 * 320, sample // 4 * 200
                    paste_contained(whole_sheet, image, (x, y, 320, 176))
                    ImageDraw.Draw(whole_sheet).text((x + 7, y + 178),
                        f"{number:02d}  {number / FPS:.3f}s", fill="black", font=label_font)
                sheet = face_sheets[number // 24]
                local = number % 24
                x, y = local % 6 * 200, local // 6 * 165
                paste_contained(sheet, main_face_crop(image, evidence), (x, y, 200, 140))
                suffix = "" if evidence.get("faces") else "  no face"
                ImageDraw.Draw(sheet).text((x + 5, y + 143),
                    f"{number:02d}  {number / FPS:.3f}s{suffix}", fill="black", font=label_font)
            if process.stdout.read(1):
                raise RuntimeError(f"FFmpeg decoded more than {FRAMES} frames")
            if process.wait() != 0:
                raise RuntimeError("FFmpeg frame decode failed; see review/decode.log")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            if process.stdout is not None:
                process.stdout.close()

    whole_sheet.save(review / "whole-frames-6fps.png")
    comparison.save(review / "portrait-first-middle-last.png")
    for index, sheet in enumerate(face_sheets):
        sheet.save(review / f"faces-{index * 24:02d}-{index * 24 + 23:02d}.png")
    return {path.name: file_record(path, review.parent) for path in
            [review / "whole-frames-6fps.png", review / "portrait-first-middle-last.png",
             *(review / f"faces-{index * 24:02d}-{index * 24 + 23:02d}.png" for index in range(4))]}


def review_native(run: Path) -> dict:
    run = run.expanduser().resolve()
    if not run.is_dir():
        raise FileNotFoundError(run)
    review = run / "review"
    review.mkdir(exist_ok=False)
    evidence: dict = {"version": 1, "status": "collecting", "review": {
        "spoken_words": "pending-human-listening", "captions": "pending-human-visual-review",
        "face_identity": "pending-human-visual-review", "lip_sync": "pending-human-audiovisual-review",
        "source_speech_quality": "pending-human-listening",
        "fixed_voice_acceptance": False}}

    def save() -> None:
        (review / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")

    try:
        shutil.copyfile(SCRIPT, review / SCRIPT.name)
        shutil.copyfile(VISION_SCRIPT, review / VISION_SCRIPT.name)
        model_record = json.loads(MODEL_RECORD.read_text(encoding="utf-8"))
        if sha256(MODEL) != model_record["sha256"]:
            raise RuntimeError("Cached Whisper model differs from its recorded SHA-256")
        shutil.copyfile(MODEL_RECORD, review / "whisper-model.json")

        manifest_path, render_path = run / "manifest.json", run / "render.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        render = json.loads(render_path.read_text(encoding="utf-8"))
        character, expected = validate_manifest(manifest, render)
        evidence["character_id"] = character
        evidence["role"] = manifest["role"]
        evidence["line_id"] = manifest["line_id"]
        inputs = {name: run / relative for name, relative in {
            "manifest": "manifest.json", "render": "render.json", "video": "video.mp4",
            "native_video": "video.native.mp4", "native_audio": "native-audio.wav",
            "canonical_portrait": f"characters/{character}/reference.png", "starting_image": "start.png",
            "prompt": "prompt.txt", "dialogue": "dialogue.txt",
        }.items()}
        evidence["inputs"] = {name: file_record(path, run) for name, path in inputs.items()}
        for name, field in (("video", "video"), ("native_video", "native_video"),
                            ("native_audio", "native_audio")):
            recorded = render.get(field, {}).get("sha256")
            if recorded != evidence["inputs"][name]["sha256"]:
                raise ValueError(f"Rendered {name} hash does not match render.json")
        if inputs["dialogue"].read_text(encoding="utf-8").strip() != expected:
            raise ValueError("Saved dialogue differs from the requested line")
        if inputs["prompt"].read_text(encoding="utf-8").strip() != manifest["prompt"]:
            raise ValueError("Saved prompt differs from the rendered prompt")
        if (evidence["inputs"]["canonical_portrait"]["sha256"]
                != manifest["character_snapshot"]["image"]["sha256"]):
            raise ValueError("Canonical portrait changed after rendering")
        if (evidence["inputs"]["starting_image"]["sha256"]
                != manifest["source"]["starting_image"]["sha256"]):
            raise ValueError("Starting image changed after rendering")

        tools = {name: tool_record(name, args) for name, args in {
            "ffmpeg": ["-version"], "whisper-cli": ["-ng", "--version"], "swift": ["--version"],
        }.items()}
        evidence["provenance"] = {
            "tools": tools,
            "whisper_model": {**model_record, "actual_sha256": sha256(MODEL)},
            "scripts": {name: file_record(review / name, run) for name in
                        (SCRIPT.name, VISION_SCRIPT.name, "whisper-model.json")},
        }
        save()

        ffmpeg = tools["ffmpeg"]["executable"]
        whisper = tools["whisper-cli"]["executable"]
        swift = tools["swift"]["executable"]
        wav = review / "speech-16k-mono.wav"
        extract_command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                           "-i", str(inputs["video"]), "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000",
                           "-c:a", "pcm_s16le", str(wav)]
        command(extract_command, stderr=review / "extract.log")
        transcript_prefix = review / "transcript"
        whisper_command = [whisper, "-ng", "-m", str(MODEL), "-f", str(wav), "-l", "en",
                           "-ojf", "-of", str(transcript_prefix)]
        command(whisper_command, stdout=review / "whisper.stdout.log",
                stderr=review / "whisper.stderr.log")
        vision_command = [swift, str(review / VISION_SCRIPT.name), str(inputs["video"])]
        command(vision_command, stdout=review / "inspection.json", stderr=review / "vision.log")
        evidence["commands"] = {
            "extract_audio": extract_command, "transcribe_cpu": whisper_command,
            "inspect_all_frames": vision_command,
            "decode_frames": [ffmpeg, "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
                              "-i", str(inputs["video"]), "-map", "0:v:0", "-f", "rawvideo",
                              "-pix_fmt", "rgb24", "pipe:1"],
        }
        transcript_path = transcript_prefix.with_suffix(".json")
        transcript = json.loads(transcript_path.read_text(encoding="utf-8"))
        observed = " ".join(item.get("text", "").strip() for item in transcript.get("transcription", []))
        match = normalize(observed) == normalize(expected)
        evidence["transcript"] = {
            "expected": expected, "observed": observed, "normalized_expected": normalize(expected),
            "normalized_observed": normalize(observed), "automated_result": "match" if match else "mismatch",
            "method": "whisper.cpp base.en, CPU, no initial prompt; verify by listening",
        }
        inspection = json.loads((review / "inspection.json").read_text(encoding="utf-8"))
        frames = inspection.get("frames", [])
        if len(frames) != FRAMES or any(frame.get("frame") != index for index, frame in enumerate(frames)):
            raise RuntimeError(f"Vision inspected {len(frames)} frames; expected {FRAMES}")
        text_frames = [frame for frame in frames if frame.get("text")]
        evidence["ocr"] = {
            "frames_inspected": len(frames), "text_bearing_frames": len(text_frames),
            "candidates": [{"frame": frame["frame"], "seconds": frame["seconds"],
                            "text": frame["text"]} for frame in text_frames],
            "automated_result": "no-text-detected" if not text_frames else "text-detected",
            "limitation": "OCR misses are possible; inspect whole frames for captions and overlays.",
            "vision_revision": inspection["ocr_revision"],
        }
        evidence["faces"] = {
            "frames_with_detected_face": sum(bool(frame.get("faces")) for frame in frames),
            "frames_with_multiple_faces": sum(len(frame.get("faces", [])) > 1 for frame in frames),
            "frames_inspected": len(frames), "vision_revision": inspection["face_revision"],
            "limitation": "Face detection and landmarks do not establish the performer's identity or lip sync.",
        }
        evidence["contact_sheets"] = make_sheets(inputs["video"], frames,
            inputs["canonical_portrait"], review, ffmpeg)
        evidence["outputs"] = {path.name: file_record(path, run) for path in
            (wav, transcript_path, review / "inspection.json", review / "extract.log",
             review / "whisper.stdout.log", review / "whisper.stderr.log", review / "vision.log",
             review / "decode.log")}
        evidence["status"] = "diagnostics-complete"
        save()
        return evidence
    except BaseException as exc:
        evidence["status"] = "interrupted" if isinstance(exc, KeyboardInterrupt) else "failed"
        evidence["error"] = str(exc) or type(exc).__name__
        save()
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path, help="completed native-reference run directory")
    args = parser.parse_args(argv)
    evidence = review_native(args.run_dir)
    print(json.dumps({"status": evidence["status"], "evidence": str(args.run_dir / "review/evidence.json")},
                     indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
