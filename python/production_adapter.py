"""Compatibility transport for Rust-owned production; no scheduling or fallback rules."""
from pathlib import Path
import sys
import quality_backend
import rust_engine


def _wire(value):
    if isinstance(value, Path):
        return str(value.expanduser().resolve())
    if isinstance(value, dict):
        return {str(key): _wire(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_wire(item) for item in value]
    return value


def call(operation, *, on_activity=None, on_activity_delta=None, on_progress=None,
         on_shot=None, token_counter=None, **args):
    activity = rust_engine.activity_events(on_activity, on_activity_delta)

    def receive(event):
        activity(event)
        if event["type"] in ("render_progress", "inference_progress") and on_progress is not None:
            on_progress(**event["data"])
        elif event["type"] == "shot" and on_shot is not None:
            on_shot(event["index"], event["report"])

    args.setdefault("tokenize", token_counter is not None)
    return rust_engine.call("production_" + operation, root=str(quality_backend.ROOT),
                            python=sys.executable, on_event=receive, **_wire(args))
