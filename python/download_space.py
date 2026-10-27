"""Hugging Face metadata and filesystem observations for Rust download preflight."""
from fnmatch import fnmatchcase
from pathlib import Path
import shutil

import rust_engine


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
