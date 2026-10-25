"""Time-weighted render estimates, refined by observed work and previous runs."""

from __future__ import annotations

import json
import math
from statistics import median
import time


# Initial budgets for the default 8-second, two-stage render. These are only
# priors: completed blocks/tiles and timings from this machine refine them.
PHASE_SECONDS = {
    "runtime": 3, "cache": 1, "pipeline": 20, "prompt": 45,
    "stage1": 150, "upscale": 5, "refine": 5, "stage2": 180,
    "decoders": 5, "decode": 65, "encode": 3, "validate": 2,
}


class RenderEstimate:
    def __init__(self, callback, *, budgets=None, clock=time.monotonic):
        self.callback = callback
        self.budgets = dict(budgets or PHASE_SECONDS)
        self.clock = clock
        self.started = clock()
        self.phase = None
        self.phase_started = self.started
        self.fraction = 0.0
        self.label = "Starting renderer"
        self.phase_seconds = {}
        self.completed = set()
        self.last_fraction = 0.0

    @classmethod
    def from_history(cls, outputs, frames, width, height, revision, callback):
        samples = []
        # Ignore incomplete, incompatible, or damaged reports. Older reports
        # without phase timings still provide a useful overall time budget.
        for path in sorted(outputs.glob("*/render.json"), reverse=True):
            try:
                report = json.loads(path.read_text())
                runtime = report["runtime"]
                if (report["status"] != "complete" or report["width"] != width
                        or report["height"] != height or runtime["output_frames"] != frames
                        or runtime["runtime_revision"] != revision):
                    continue
                total = float(runtime["elapsed_seconds"])
                if not math.isfinite(total) or total <= 0:
                    continue
                scale = total / sum(PHASE_SECONDS.values())
                phases = runtime.get("phase_seconds", {})
                sample = {}
                for key, default in PHASE_SECONDS.items():
                    value = float(phases.get(key, default * scale))
                    if not math.isfinite(value) or value <= 0:
                        raise ValueError("Invalid phase timing")
                    sample[key] = value
                samples.append(sample)
                if len(samples) == 5:
                    break
            except (OSError, ValueError, KeyError, TypeError, AttributeError):
                continue
        budgets = {key: median(sample[key] for sample in samples) for key in PHASE_SECONDS} if samples else None
        return cls(callback, budgets=budgets)

    def start(self, key, label):
        if self.phase is not None:
            self.finish(self.phase)
        # Moving to a later phase also accounts for optional phases the
        # pipeline skipped. Subphase logs never change the overall budget.
        keys = list(self.budgets)
        self.completed.update(keys[:keys.index(key)])
        self.phase = key
        self.phase_started = self.clock()
        self.fraction = 0.0
        self.label = label
        self.tick()

    def update(self, fraction):
        self.fraction = max(self.fraction, min(fraction, 1.0))
        self.tick()

    def finish(self, key):
        if self.phase == key and key not in self.completed:
            self.phase_seconds[key] = max(self.clock() - self.phase_started, 0.001)
            self.completed.add(key)
            self.fraction = 1.0
            self.tick()

    def tick(self):
        if self.callback is None or self.phase is None:
            return
        now = self.clock()
        elapsed = max(now - self.started, 0.0)
        active_elapsed = max(now - self.phase_started, 0.0)
        # Adapt future budgets to the observed speed, weighting by duration so
        # a tiny import/cache phase cannot dominate the whole estimate.
        observed_budget = sum(self.budgets[key] for key in self.phase_seconds)
        scale = sum(self.phase_seconds.values()) / observed_budget if observed_budget else 1.0
        scale = min(max(scale, 0.25), 4.0)
        remaining = sum(value * scale for key, value in self.budgets.items()
                        if key not in self.completed and key != self.phase)
        if self.phase not in self.completed:
            budget = self.budgets[self.phase] * scale
            if self.fraction > 0:
                measured = active_elapsed / self.fraction
                confidence = min(self.fraction * 3, 1.0)
                budget = budget * (1 - confidence) + measured * confidence
                active_remaining = max(budget - active_elapsed, measured * (1 - self.fraction))
            else:
                # An unmeasured operation can overrun; keep some time in
                # reserve instead of racing to 100% while it is still busy.
                active_remaining = max(budget - active_elapsed, budget * 0.1)
            remaining += active_remaining
        fraction = elapsed / (elapsed + remaining) if elapsed + remaining else 0.0
        # Recalibrating an estimate must not move the bar backwards. Only the
        # validated completion event in video.py may mark a render complete.
        self.last_fraction = max(self.last_fraction, min(fraction, 0.99))
        self.callback(label=self.label, fraction=self.last_fraction,
                      remaining_seconds=max(remaining, 1.0))
