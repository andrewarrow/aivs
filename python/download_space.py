"""Hugging Face metadata and filesystem observations for Rust download preflight."""
from fnmatch import fnmatchcase
from pathlib import Path
import shutil
import threading
import time

import rust_engine


def download_progress_class(total_bytes, on_progress):
    """Report completed bytes against preflight's fixed total, including resumed bytes."""
    from tqdm.auto import tqdm

    lock = threading.RLock()
    completed, last_sent = 0, 0.0

    class DownloadProgress(tqdm):
        def __init__(self, *args, **kwargs):
            # HF exposes both network transfer and file reconstruction bars.
            # Count reconstruction only so Xet transfers aren't counted twice.
            self._count_bytes = (kwargs.get("unit") == "B"
                                 and not kwargs.get("desc", "").lower().endswith("downloading bytes"))
            self._reported = 0
            kwargs["disable"] = False
            super().__init__(*args, **kwargs)
            self._publish()

        def display(self, *args, **kwargs):
            # Progress goes over the worker protocol, never terminal escape codes.
            pass

        def _publish(self):
            nonlocal completed, last_sent
            if not self._count_bytes or not total_bytes:
                return
            with lock:
                completed += max(0, self.n - self._reported)
                self._reported = self.n
                now = time.monotonic()
                if now - last_sent >= 0.25:
                    on_progress(label="Downloading the distilled and HQ video models",
                                fraction=min(completed / total_bytes, 0.99))
                    last_sent = now

        def update(self, n=1):
            with lock:
                result = super().update(n)
                self._publish()
                return result

    return DownloadProgress


def valid_cached_file(path, *, filename=None, expected_size=None):
    return rust_engine.call("valid_cached_model_file", path=str(Path(path).resolve()),
                            filename=filename, expected_size=expected_size)


def _existing_directory(path):
    path = Path(path).resolve()
    while not path.exists():
        path = path.parent
    return path if path.is_dir() else path.parent


def preflight_downloads(specs, *, cache_dir, output_dir, token=False):
    from huggingface_hub import HfApi

    api, models = HfApi(), []
    for spec in specs:
        repo, revision = spec["repo_id"], spec["revision"]
        try:
            info = api.model_info(repo, revision=revision, files_metadata=True, token=token, timeout=30)
        except Exception as exc:
            raise RuntimeError(f"Cannot check download sizes for {repo}. Check your connection and Hugging Face "
                               "access, then retry setup. This check did not start any model downloads.") from exc
        files = [{"name": item.rfilename, "size": item.size,
                  "blob": item.lfs.get("sha256") if getattr(item, "lfs", None) else getattr(item, "blob_id", None)}
                 for item in (info.siblings or [])
                 if any(fnmatchcase(item.rfilename, pattern) for pattern in spec["allow_patterns"])]
        models.append(dict(repo_id=repo, revision=revision, sha=info.sha,
                           required_files=spec["required_files"], files=files))
    volumes = []
    for role, directory in (("cache", cache_dir), ("output", output_dir)):
        existing = _existing_directory(directory)
        volumes.append(dict(role=role, path=str(existing), device=existing.stat().st_dev,
                            free_bytes=shutil.disk_usage(existing).free))
    return rust_engine.call("preflight_downloads", models=models, volumes=volumes,
                            cache_dir=str(Path(cache_dir).resolve()))
