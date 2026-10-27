"""Transport to the Rust production engine; no fallback rule implementation.

The desktop supplies its own executable. Standalone Python commands use a built
workspace binary or AIVS_ENGINE_PATH. Never build or download code at runtime.
"""
from __future__ import annotations

import atexit
import hashlib
import json
import os
from pathlib import Path
import subprocess
import threading

ROOT = Path(__file__).resolve().parent.parent
VERSION = 1
_lock = threading.RLock()
_process = None
_owner = None
_next_id = 0
_implementation = None


def _command():
    configured = os.environ.get("AIVS_ENGINE_PATH")
    if configured:
        return [configured, "--engine"]
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target.is_absolute():
        target = ROOT / target
    suffix = ".exe" if os.name == "nt" else ""
    candidates = [target / profile / (name + suffix)
                  for profile in ("debug", "release") for name in ("aivs-engine", "aivs")]
    candidates = [path for path in candidates if path.is_file()]
    if not candidates:
        raise RuntimeError(
            "The Rust production engine is not built. Run cargo build -p aivs-core --bin aivs-engine "
            "or set AIVS_ENGINE_PATH to a built AIVS executable."
        )
    return [str(max(candidates, key=lambda path: path.stat().st_mtime_ns)), "--engine"]


def close():
    global _process, _implementation
    with _lock:
        process, _process = _process, None
        _implementation = None
        if process is not None and _owner == os.getpid():
            # EOF cancels an active Rust session and reaps its Codex child.
            # Close output first so a failing callback cannot leave Rust blocked
            # while publishing progress into a pipe we no longer drain.
            process.stdout.close()
            try:
                process.stdin.close()
            except OSError:
                pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


atexit.register(close)


def _exchange(operation, args, on_event=None):
    global _next_id
    _next_id += 1
    request = {"version": VERSION, "id": _next_id, "operation": operation, "args": args}
    try:
        _process.stdin.write(json.dumps(request, ensure_ascii=False, allow_nan=False) + "\n")
        _process.stdin.flush()
        while True:
            line = _process.stdout.readline()
            if not line:
                raise RuntimeError("The Rust production engine stopped before returning a result.")
            response = json.loads(line)
            if response.get("version") != VERSION or response.get("id") != _next_id:
                raise RuntimeError("The Rust production engine returned an incompatible response.")
            if "event" not in response:
                break
            if on_event is not None:
                on_event(response["event"])
    except BaseException:
        close()
        raise
    if "error" in response:
        if response.get("error_kind") == "timeout":
            raise subprocess.TimeoutExpired(args.get("command", []), args.get("timeout_seconds", 0))
        if response.get("error_kind") in ("codex", "cancelled"):
            raise RuntimeError(response["error"])
        raise ValueError(response["error"])
    return response["result"]


def call(operation, *, on_event=None, **args):
    global _process, _owner, _implementation
    with _lock:
        if _process is None or _owner != os.getpid() or _process.poll() is not None:
            close()
            try:
                _process = subprocess.Popen(_command(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                            text=True, encoding="utf-8", bufsize=1)
            except OSError as exc:
                raise RuntimeError(f"Cannot launch the Rust production engine: {exc}") from exc
            _owner = os.getpid()
            try:
                compiled = _exchange("implementation", {})
                _implementation = {name: hashlib.sha256(source.encode("utf-8")).hexdigest()
                                   for name, source in compiled.items()}
                for name, compiled_sha256 in _implementation.items():
                    source = ROOT / name
                    if source.is_file() and hashlib.sha256(source.read_bytes()).hexdigest() != compiled_sha256:
                        raise RuntimeError(
                            "The Rust production engine is older than its source. Rebuild AIVS or run "
                            "cargo build -p aivs-core --bin aivs-engine."
                        )
            except BaseException:
                close()
                raise
        return _exchange(operation, args, on_event)


def implementation_sha256(name):
    """Retain legacy identities only for the exact recorded migration sources.

    Changing any migrated Python file, bridge, or compiled rule source invalidates
    the compatibility mapping. JSON artifact serialization stays unchanged.
    """
    with _lock:
        # Also ensures the worker matches the checkout before accepting cached work.
        if _process is None or _owner != os.getpid() or _process.poll() is not None:
            call("implementation")
        actual = hashlib.sha256((ROOT / "python" / name).read_bytes()).hexdigest()
        compatibility = json.loads(Path(__file__).with_name("engine-compatibility.json").read_text(encoding="utf-8"))
        migrated = compatibility["python"].get(name)
        if migrated is None:
            return actual
        bridge = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        if (compatibility["version"] == VERSION and _implementation == compatibility["rust"]
                and bridge == compatibility["bridge_sha256"] and actual == migrated["migrated_sha256"]):
            return migrated["legacy_sha256"]
        identity = {"python": actual, "rust": _implementation, "bridge": bridge}
        return hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
