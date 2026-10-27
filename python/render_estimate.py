"""Clock/callback adapter for Rust-owned render estimation and timing history."""
from __future__ import annotations

from pathlib import Path
import time
import rust_engine


def __getattr__(name):
    if name == "PHASE_SECONDS":
        return dict(rust_engine.call("render_estimate_defaults"))
    raise AttributeError(name)


class RenderEstimate:
    def __init__(self, callback, *, budgets=None, clock=time.monotonic):
        self.callback = callback
        self.clock = clock
        self._state = rust_engine.call("render_estimate_new", now=clock(),
                                       budgets=list(dict(budgets).items()) if budgets else None)

    def __getattr__(self, name):
        value = self._state[name] if name in self._state else None
        if name == "budgets":
            return dict(value)
        if name == "completed":
            return set(value)
        if name in self._state:
            return value
        raise AttributeError(name)

    @classmethod
    def from_history(cls, outputs, frames, width, height, revision, callback, *,
                     profile="distilled", audio="generated"):
        budgets = rust_engine.call("render_estimate_history", outputs=str(Path(outputs).resolve()),
                                   frames=frames, width=width, height=height, revision=revision,
                                   profile=profile, audio=audio)
        return cls(callback, budgets=budgets)

    def _step(self, action, **args):
        result = rust_engine.call("render_estimate_step", state=self._state, action=action,
                                  now=self.clock(), enabled=self.callback is not None, **args)
        self._state = result["state"]
        for event in result["events"]:
            self.callback(**event)

    def start(self, key, label):
        self._step("start", key=key, label=label)

    def update(self, fraction):
        self._step("update", fraction=fraction)

    def finish(self, key):
        self._step("finish", key=key)

    def tick(self):
        self._step("tick")
