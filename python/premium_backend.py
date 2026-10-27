"""Compatibility names for optional Rust-managed LTX cloud comparison jobs."""
from pathlib import Path
from production_adapter import call

MODEL = "ltx-2-5-pro"
FPS = 24


def profile_config(model=MODEL):
    return call("premium_config", model=model)


def validate_request(prompt, frames, width, height, *, model=MODEL):
    return call("premium_validate", prompt=prompt, frames=frames, width=width, height=height, model=model)


def render(prompt, seed, frames, output, **options):
    return call("premium_render", prompt=prompt, seed=seed, frames=frames, output=Path(output), **options)
