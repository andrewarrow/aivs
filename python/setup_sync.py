"""Install the isolated, pinned CPU lip-sync reviewer during application setup."""
from __future__ import annotations

import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request

ROOT = Path(os.environ.get("AIVS_WORKSPACE", Path(__file__).resolve().parents[1]))
HERE = Path(__file__).resolve().parent


def setup():
    with (ROOT / ".setup.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        target = ROOT / ".cache/sync-review"
        target.mkdir(parents=True, exist_ok=True)
        environment = target / "env"
        subprocess.run(["uv", "venv", "--python", "3.12", "--allow-existing", str(environment)], check=True)
        subprocess.run(["uv", "pip", "install", "--python", str(environment / "bin/python"),
                        "-r", str(HERE / "requirements-sync.txt")], check=True)
        pins = json.loads((HERE / "sync-assets.json").read_text())
        for asset in pins["files"]:
            destination = target / asset["name"]
            if destination.is_file():
                with destination.open("rb") as source:
                    if hashlib.file_digest(source, "sha256").hexdigest() == asset["sha256"]:
                        continue
            print(f"Preparing lip-sync review: {asset['name']}", flush=True)
            with tempfile.NamedTemporaryFile(dir=target, delete=False) as staged:
                temporary = Path(staged.name)
                try:
                    with urllib.request.urlopen(asset["url"], timeout=120) as response:
                        while chunk := response.read(1024 * 1024):
                            staged.write(chunk)
                    staged.flush()
                    with temporary.open("rb") as downloaded:
                        if hashlib.file_digest(downloaded, "sha256").hexdigest() != asset["sha256"]:
                            raise RuntimeError(f"Lip-sync asset checksum mismatch: {asset['name']}")
                    temporary.replace(destination)
                finally:
                    temporary.unlink(missing_ok=True)
        # Use the review interpreter's HF client; its dependency set is isolated.
        subprocess.run([str(environment / "bin/python"), "-c",
                        "from huggingface_hub import snapshot_download; "
                        f"snapshot_download({pins['asr']['repository']!r}, revision={pins['asr']['revision']!r}, "
                        f"local_dir={str(target / 'asr')!r}, "
                        "allow_patterns=['model.bin', 'config.json', 'tokenizer.json', 'vocabulary.*', 'README.md'])"], check=True)
        # Receipt binds the ASR files too; interrupted or changed downloads cannot pass readiness.
        manifest = {"pins": pins, "asr_files": {}}
        for name in ("model.bin", "config.json", "tokenizer.json", "vocabulary.txt"):
            with (target / "asr" / name).open("rb") as source:
                manifest["asr_files"][name] = hashlib.file_digest(source, "sha256").hexdigest()
        receipt = target / "assets.json"
        temporary = receipt.with_suffix(".partial.json")
        temporary.write_text(json.dumps(manifest, indent=2) + "\n")
        temporary.replace(receipt)


if __name__ == "__main__":
    setup()
