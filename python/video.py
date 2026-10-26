"""Machine-readable entry points for AIVS setup and Hunter menu option 8."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import sys

from quality_backend import ROOT, CACHE, FPS, WIDTH, HEIGHT, doctor, model_path, render
from dialogue_helpers import duration_estimate
import characters

DEFAULT_PROMPT = Path(__file__).with_name("default-prompt.txt").read_text().strip()
_token = ""


def emit(event, **data):
    # This prefix keeps upstream library stdout distinct from our protocol.
    message = "AIVS_EVENT " + json.dumps({"event": event, **data})
    print(message.replace(_token, "[redacted]") if _token else message, flush=True)


@contextmanager
def project_lock(name):
    with (ROOT / name).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError("Another AIVS operation is using this workspace. Wait for it to finish and retry.") from exc
        yield


def check_environment():
    global _token
    from dotenv import dotenv_values
    _token = dotenv_values(ROOT / ".env").get("HF_API") or ""
    if not _token.strip():
        emit("needs_token")
        raise ValueError("Get an access token from Hugging Face, then create .env with HF_API=your_token.")
    for name in ("ffmpeg", "ffprobe"):
        if not shutil.which(name):
            raise RuntimeError(f"{name} is missing. Install FFmpeg and retry setup.")


def check():
    check_environment()
    emit("progress", label="Checking Python packages and Apple GPU")
    doctor()
    emit("progress", label="Checking cached model, text encoder, and audio weights")
    model_path()
    emit("ready")


def download():
    check_environment()
    from tqdm.auto import tqdm
    import time

    class DownloadProgress(tqdm):
        def __init__(self, *args, **kwargs):
            self.last_event = 0
            kwargs.update(mininterval=0.5, leave=False)
            super().__init__(*args, **kwargs)

        def display(self, *args, **kwargs):
            now = time.monotonic()
            if now - self.last_event < 0.5 and self.n != self.total:
                return
            self.last_event = now
            if self.unit == "B":
                label = f"Downloading model files: {self.n / 1e9:.2f} / {(self.total or 0) / 1e9:.2f} GB"
            else:
                label = f"Caching model files: {self.n:g} / {self.total or '?'}"
            emit("progress", label=label,
                 fraction=min(self.n / self.total, 1) if self.total else None)

    with project_lock(".setup.lock"):
        emit("progress", label="Downloading LTX-2.5 with audio (about 43 GB; cached files are reused)")
        model_path(download=True, progress_class=DownloadProgress)
    check()


def generate(seed=None, prompt=DEFAULT_PROMPT, *, character=(), character_text_only=False,
             character_references_only=False):
    check_environment()
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
    character_root = ROOT if (ROOT / "characters").is_dir() else Path(__file__).resolve().parents[1]
    profiles = characters.resolve_characters(prompt, explicit=character, root=character_root,
                                             references_only=character_references_only)
    if not character_text_only and sum(bool(profile.get("scene_image") or profile.get("image")) for profile in profiles) > 1:
        raise ValueError("Multiple saved characters have reference images. Use --character-text-only to use descriptions without image guidance.")
    timing = duration_estimate(prompt)
    seed = secrets.randbelow(2**31) if seed is None else seed
    with project_lock(".render.lock"):
        run_dir = ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_{seed}"
        run_dir.mkdir(parents=True)
        output = run_dir / "video.mp4"
        if profiles:
            profiles = characters.snapshot_characters(profiles, run_dir, root=character_root)
        image = None if character_text_only or not profiles else characters.reference_image(profiles, run_dir)
        hold_image_framing = image is not None and any(profile.get("scene_image") for profile in profiles)
        expanded_prompt = characters.expand_prompt(prompt, profiles, image_conditioned=image is not None)
        report = {
            "status": "rendering", "original_prompt": prompt, "prompt": expanded_prompt, "seed": seed,
            "seconds": timing["seconds"], "fps": FPS, "width": WIDTH, "height": HEIGHT,
            "audio": "generated", "dialogue_timing": timing,
            "characters": profiles, "character_text_only": character_text_only,
            "character_references_only": character_references_only,
        }
        if image is not None:
            image_source = {
                "file": str(image.relative_to(run_dir)),
                "conditioning": "whole-first-and-last-frame" if hold_image_framing else "whole-first-frame",
            }
            if hold_image_framing:
                frames = timing["seconds"] * FPS
                image_source["anchor_frames"] = [0] if frames <= 1 else [0, frames - 1]
            report["image_source"] = image_source
        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n")
            temp.replace(run_dir / "render.json")
        save()
        emit("progress", label=f"Generating {timing['seconds']} seconds of talking video")
        try:
            options = {"image_file": image, "hold_image_framing": hold_image_framing} if image is not None else {}
            report["runtime"] = render(expanded_prompt, seed, timing["seconds"] * FPS, output,
                                       on_progress=lambda **data: emit("render_progress", **data), **options)
            report.update(status="complete", sha256=hashlib.sha256(output.read_bytes()).hexdigest())
            save()
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            save()
            raise
        emit("complete", output=str(output))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "download", "render"))
    parser.add_argument("--seed", type=int)
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument("--character", action="append", default=[], metavar="NAME")
    parser.add_argument("--character-text-only", action="store_true")
    parser.add_argument("--character-references-only", action="store_true",
                        help="Resolve @references and --character selections without inferring cast from script names")
    args = parser.parse_args()
    try:
        if args.command == "check":
            check()
        elif args.command == "download":
            download()
        else:
            generate(args.seed, args.prompt, character=args.character,
                     character_text_only=args.character_text_only,
                     character_references_only=args.character_references_only)
        return 0
    except KeyboardInterrupt:
        emit("error", message="Stopped. Partial output is kept in outputs.")
        return 130
    except Exception as exc:
        emit("error", message=str(exc))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
