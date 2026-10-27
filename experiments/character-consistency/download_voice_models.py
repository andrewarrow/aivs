"""Download only the pinned Seed-VC experiment assets, verifying every SHA-256."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import time
import urllib.request

PINS = Path(__file__).with_name("seed-vc-models.json")
DEFAULT_CACHE = Path(__file__).resolve().parents[2] / ".cache/seed-vc-models"


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def download(cache, *, on_progress=None):
    pins = json.loads(PINS.read_text())
    cache.mkdir(parents=True, exist_ok=True)
    pending = []
    for model in pins["models"]:
        folder = cache / model["name"]
        folder.mkdir(exist_ok=True)
        for name, expected in model["files"].items():
            target = folder / name
            if target.is_file() and digest(target) == expected:
                print(f"Verified {model['name']}/{name}", flush=True)
                continue
            url = f"https://huggingface.co/{model['repo_id']}/resolve/{model['revision']}/{name}"
            pending.append((target, expected, url))

    total_bytes = 0
    if on_progress is not None:
        for _, _, url in pending:
            try:
                request = urllib.request.Request(url, method="HEAD")
                with urllib.request.urlopen(request, timeout=30) as response:
                    size = int(response.headers.get("Content-Length", "0"))
                if size <= 0:
                    total_bytes = None
                    break
                total_bytes += size
            except (OSError, ValueError):
                # Keep downloads available if a server cannot answer HEAD requests.
                total_bytes = None
                break

    downloaded_bytes, last_sent = 0, 0.0

    def report(*, force=False, complete=False):
        nonlocal last_sent
        if on_progress is None:
            return
        now = time.monotonic()
        if force or now - last_sent >= 0.25:
            fraction = (min(downloaded_bytes / total_bytes, 0.99) if total_bytes else None)
            on_progress(downloaded_bytes=downloaded_bytes, total_bytes=total_bytes,
                        fraction=1.0 if complete else fraction)
            last_sent = now

    report(force=True)
    for target, expected, url in pending:
        partial = target.with_suffix(target.suffix + ".partial")
        print(f"Downloading {target.parent.name}/{target.name}", flush=True)
        try:
            with urllib.request.urlopen(url, timeout=60) as source, partial.open("wb") as dest:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    dest.write(block)
                    downloaded_bytes += len(block)
                    report()
            if digest(partial) != expected:
                raise ValueError(f"Model checksum mismatch: {target.name}")
            partial.replace(target)
            report(force=True)
        finally:
            partial.unlink(missing_ok=True)
    shutil.copyfile(PINS, cache / "models.json")
    report(force=True, complete=True)
    print(f"Verified all models in {cache}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    download(parser.parse_args().cache.resolve())
