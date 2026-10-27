"""Account for all selected model files before setup starts downloading weights."""

from __future__ import annotations

from fnmatch import fnmatchcase
import json
from pathlib import Path
import shutil

GIB = 1024**3
TEMPORARY_RESERVE_BYTES = 2 * GIB
OUTPUT_RESERVE_BYTES = 5 * GIB


def valid_cached_file(path, *, filename=None, expected_size=None):
    """Recognize reusable files without loading model weights into memory."""
    path = Path(path)
    filename = filename or path.name
    try:
        size = path.stat().st_size
        if not path.is_file() or size == 0 or (expected_size is not None and size != expected_size):
            return False
        if filename.endswith(".safetensors"):
            with path.open("rb") as stream:
                length = int.from_bytes(stream.read(8), "little")
                if not 1 <= length <= min(100 * 1024**2, size - 8):
                    return False
                header = json.loads(stream.read(length))
            offsets = [entry["data_offsets"] for name, entry in header.items() if name != "__metadata__"]
            if not offsets or any(not isinstance(pair, list) or len(pair) != 2
                                  or any(type(value) is not int for value in pair)
                                  or not 0 <= pair[0] <= pair[1] for pair in offsets):
                return False
            return 8 + length + max(pair[1] for pair in offsets) == size
        if filename.endswith(".json"):
            json.loads(path.read_text(encoding="utf-8"))
        return True
    except (OSError, ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        return False


def _existing_directory(path):
    path = Path(path).resolve()
    while not path.exists():
        parent = path.parent
        if parent == path:
            raise RuntimeError(f"Cannot locate the model storage volume: {path}")
        path = parent
    return path if path.is_dir() else path.parent


def preflight_downloads(specs, *, cache_dir, output_dir, token=False):
    """Fetch only pinned file metadata, then check combined remaining disk use.

    Missing and damaged files are charged at their full size. Existing partial
    downloads already consume free space and are deliberately not credited:
    a repair may need to replace them instead of resuming. Completed HF blobs
    can be reused even when their snapshot link has not yet been created.
    """
    from huggingface_hub import HfApi

    cache_dir = Path(cache_dir)
    api = HfApi()
    models = []
    for spec in specs:
        repo_id, revision = spec["repo_id"], spec["revision"]
        try:
            info = api.model_info(repo_id, revision=revision, files_metadata=True, token=token, timeout=30)
        except Exception as exc:
            raise RuntimeError(
                f"Cannot check download sizes for {repo_id}. Check your connection and Hugging Face "
                "access, then retry setup. This check did not start any model downloads."
            ) from exc
        if info.sha != revision:
            raise RuntimeError(f"Download metadata for {repo_id} does not match its pinned revision.")
        selected = {item.rfilename: item for item in (info.siblings or [])
                    if any(fnmatchcase(item.rfilename, pattern) for pattern in spec["allow_patterns"])}
        if set(spec["required_files"]) - selected.keys():
            raise RuntimeError(f"Pinned download metadata for {repo_id} is missing required model files.")
        storage = cache_dir / ("models--" + repo_id.replace("/", "--"))
        snapshot = storage / "snapshots" / revision
        missing_bytes = cached_bytes = 0
        missing_files = []
        for name, item in selected.items():
            size = item.size
            if type(size) is not int or size < 0:
                raise RuntimeError(f"Download size is unavailable for {repo_id}/{name}; retry setup.")
            if Path(name).is_absolute() or ".." in Path(name).parts:
                raise RuntimeError(f"Invalid model filename in download metadata for {repo_id}.")
            asset = snapshot / name
            cached = valid_cached_file(asset, expected_size=size)
            # Only credit a detached blob when the snapshot file is absent.
            # An existing invalid snapshot will be force-downloaded by setup.
            if not cached and not asset.exists():
                lfs = getattr(item, "lfs", None)
                blob = lfs.get("sha256") if lfs else getattr(item, "blob_id", None)
                if (isinstance(blob, str) and len(blob) in (40, 64)
                        and all(character in "0123456789abcdef" for character in blob)):
                    cached = valid_cached_file(storage / "blobs" / blob,
                                               filename=name, expected_size=size)
            if cached:
                cached_bytes += size
            else:
                missing_bytes += size
                missing_files.append(name)
        models.append({"repo_id": repo_id, "revision": revision, "missing_bytes": missing_bytes,
                       "cached_bytes": cached_bytes, "missing_files": missing_files})

    missing_bytes = sum(model["missing_bytes"] for model in models)
    temporary = TEMPORARY_RESERVE_BYTES if missing_bytes else 0
    volumes = {}
    for directory, required in ((cache_dir, missing_bytes + temporary), (output_dir, OUTPUT_RESERVE_BYTES)):
        existing = _existing_directory(directory)
        device = existing.stat().st_dev
        volume = volumes.setdefault(device, {"path": str(existing), "required_bytes": 0,
                                             "free_bytes": shutil.disk_usage(existing).free})
        volume["required_bytes"] += required
    for volume in volumes.values():
        if volume["free_bytes"] < volume["required_bytes"]:
            raise RuntimeError(
                f"Model setup needs {volume['required_bytes'] / GIB:.1f} GiB free on {volume['path']}; "
                f"only {volume['free_bytes'] / GIB:.1f} GiB is available. The combined download has "
                f"{missing_bytes / GIB:.1f} GiB of missing or damaged model files, plus "
                f"{temporary / GIB:.0f} GiB for temporary files and {OUTPUT_RESERVE_BYTES / GIB:.0f} GiB "
                "for generated outputs across their storage volumes. Cached complete files are reused. "
                "Free space and retry. This check did not start any model downloads."
            )
    return {"missing_bytes": missing_bytes, "cached_bytes": sum(model["cached_bytes"] for model in models),
            "temporary_reserve_bytes": temporary, "output_reserve_bytes": OUTPUT_RESERVE_BYTES,
            "required_bytes": missing_bytes + temporary + OUTPUT_RESERVE_BYTES,
            "models": models, "filesystems": list(volumes.values())}
