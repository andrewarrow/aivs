"""Download only the pinned Seed-VC experiment assets, verifying every SHA-256."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import urllib.request

PINS = Path(__file__).with_name("seed-vc-models.json")
DEFAULT_CACHE = Path(__file__).resolve().parents[2] / ".cache/seed-vc-models"


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def download(cache):
    pins = json.loads(PINS.read_text())
    cache.mkdir(parents=True, exist_ok=True)
    for model in pins["models"]:
        folder = cache / model["name"]
        folder.mkdir(exist_ok=True)
        for name, expected in model["files"].items():
            target = folder / name
            if target.is_file() and digest(target) == expected:
                print(f"Verified {model['name']}/{name}", flush=True)
                continue
            url = f"https://huggingface.co/{model['repo_id']}/resolve/{model['revision']}/{name}"
            partial = target.with_suffix(target.suffix + ".partial")
            print(f"Downloading {model['name']}/{name}", flush=True)
            try:
                with urllib.request.urlopen(url, timeout=60) as source, partial.open("wb") as dest:
                    shutil.copyfileobj(source, dest)
                if digest(partial) != expected:
                    raise ValueError(f"Model checksum mismatch: {name}")
                partial.replace(target)
            finally:
                partial.unlink(missing_ok=True)
    shutil.copyfile(PINS, cache / "models.json")
    print(f"Verified all models in {cache}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    download(parser.parse_args().cache.resolve())
