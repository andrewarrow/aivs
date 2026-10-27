"""Compatibility entry points for Rust-owned preproduction."""
from pathlib import Path
from production_adapter import call


def _save(path, value):
    call("write_json", path=Path(path), value=value)


def _read(path):
    import json
    return json.loads(Path(path).read_text(encoding="utf-8"))


def cache_key(request, profiles):
    return call("cache_key", request=request, profiles=profiles)


def rebind_storyboards(director, final, boards, run_dir):
    return call("rebind", director=director, final=final, boards=boards, run_dir=Path(run_dir))


def prepare(request, profiles, run_dir, seed, **options):
    result = call("prepare", request=request, profiles=profiles, run_dir=Path(run_dir), seed=seed, **options)
    return result["plan"], result["boards"]
