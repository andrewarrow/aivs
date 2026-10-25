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


def generate(seed=None):
    check_environment()
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
    timing = duration_estimate(DEFAULT_PROMPT)
    seed = secrets.randbelow(2**31) if seed is None else seed
    with project_lock(".render.lock"):
        run_dir = ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_{seed}"
        run_dir.mkdir(parents=True)
        output = run_dir / "video.mp4"
        report = {
            "status": "rendering", "prompt": DEFAULT_PROMPT, "seed": seed,
            "seconds": timing["seconds"], "fps": FPS, "width": WIDTH, "height": HEIGHT,
            "audio": "generated", "dialogue_timing": timing,
        }
        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n")
            temp.replace(run_dir / "render.json")
        save()
        emit("progress", label=f"Generating {timing['seconds']} seconds of talking video")
        try:
            report["runtime"] = render(DEFAULT_PROMPT, seed, timing["seconds"] * FPS, output,
                                       on_progress=lambda **data: emit("render_progress", **data))
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
    args = parser.parse_args()
    try:
        if args.command == "check":
            check()
        elif args.command == "download":
            download()
        else:
            generate(args.seed)
        return 0
    except KeyboardInterrupt:
        emit("error", message="Stopped. Partial output is kept in outputs.")
        return 130
    except Exception as exc:
        emit("error", message=str(exc))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
