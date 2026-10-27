"""Prepare the pinned saved-character voice converter in an AIVS workspace."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess
import tempfile

from download_voice_models import PINS, download

DEFAULT_WORKSPACE = Path(os.environ.get("AIVS_WORKSPACE", Path(__file__).resolve().parents[2]))
REQUIREMENTS = Path(__file__).with_name("requirements-seed-vc.txt")


def _progress(label: str) -> None:
    print("AIVS_EVENT " + json.dumps({"event": "progress", "label": label}), flush=True)


def _checkout(code: Path, pins: dict) -> None:
    if not (code / ".git").exists():
        raise RuntimeError(f"{code} is not a Seed-VC Git checkout. Move it aside, then Retry setup.")
    dirty = subprocess.check_output(["git", "-C", str(code), "status", "--porcelain=v1"], text=True)
    if dirty.strip():
        raise RuntimeError(f"Seed-VC source has local changes in {code}. Preserve or resolve them, then Retry setup.")
    revision = subprocess.check_output(["git", "-C", str(code), "rev-parse", "HEAD"], text=True).strip()
    if revision != pins["code_revision"]:
        subprocess.run(["git", "-C", str(code), "fetch", "--depth", "1",
                        pins["code_repository"], pins["code_revision"]], check=True)
        subprocess.run(["git", "-C", str(code), "checkout", "--detach", pins["code_revision"]], check=True)


def setup(workspace: Path) -> None:
    workspace = workspace.resolve()
    with (workspace / ".setup.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError("Another AIVS operation is using this workspace. Wait for it to finish and retry.") from exc
        cache = workspace / ".cache"
        cache.mkdir(parents=True, exist_ok=True)
        environment = cache / "seed-vc-env"
        _progress("Preparing Python 3.10.21 for saved-character voices")
        subprocess.run(["uv", "venv", "--python", "3.10.21", "--allow-existing", str(environment)], check=True)
        _progress("Installing the pinned Seed-VC packages")
        subprocess.run(["uv", "pip", "sync", "--python", str(environment / "bin/python"),
                        str(REQUIREMENTS)], check=True)
        pins = json.loads(PINS.read_text(encoding="utf-8"))
        code = cache / "seed-vc"
        _progress("Preparing the pinned Seed-VC source")
        if code.exists():
            _checkout(code, pins)
        else:
            # Publish only a complete checkout so interrupted setup can retry.
            with tempfile.TemporaryDirectory(prefix=".seed-vc-", dir=cache) as temporary:
                staged = Path(temporary) / "source"
                subprocess.run(["git", "clone", "--depth", "1", pins["code_repository"], str(staged)], check=True)
                _checkout(staged, pins)
                staged.rename(code)
        _progress("Downloading saved-character voice models (verified files are reused)")
        download(cache / "seed-vc-models")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=DEFAULT_WORKSPACE)
    try:
        setup(parser.parse_args().workspace)
    except Exception as exc:
        print("AIVS_EVENT " + json.dumps({"event": "error", "message": f"Saved voice setup failed: {exc}"}), flush=True)
        raise SystemExit(1)
