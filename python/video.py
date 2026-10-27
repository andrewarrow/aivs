"""Compatibility launcher for the Rust AIVS production CLI."""
from contextlib import contextmanager
from pathlib import Path
import fcntl
import os
import sys
from quality_backend import ROOT
from production_adapter import call
import rust_engine


@contextmanager
def project_lock(name):
    # Standalone motion commands share Rust's workspace lock.
    with (ROOT / name).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError("Another AIVS operation is using this workspace. Wait for it to finish and retry.") from exc
        yield


def prompt_token_counter():
    return lambda prompt: rust_engine.call("inference", root=str(ROOT), python=sys.executable,
        worker_operation="token_count", payload={"prompts": [prompt]})[0]


def generate_screenplay(request, seed=None, **options):
    return call("run", request=request, seed=seed, tokenize=True, **options)


def resume_screenplay(directory, **options):
    return call("resume", run_dir=Path(directory), tokenize=True, **options)


def main():
    executable = rust_engine._command()[0]
    os.environ["AIVS_PYTHON"] = sys.executable
    os.environ["AIVS_WORKSPACE"] = str(ROOT)
    os.execv(executable, [executable, "--production", *sys.argv[1:]])


if __name__ == "__main__":
    main()
