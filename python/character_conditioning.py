"""Inference-boundary adapters for Rust-owned saved-character production."""
from __future__ import annotations

import gc
from pathlib import Path
import sys

import quality_backend
import rust_engine

POLICY = "saved-character-assets-v1"


def _call(operation, *, on_activity=None, **args):
    def event(value):
        if on_activity is not None and "activity" in value:
            on_activity(value["activity"])
    return rust_engine.call(operation, root=str(quality_backend.ROOT.resolve()),
                            on_event=event, **args)


def check_voice_setup() -> dict:
    return _call("conditioning_check_voice")


def preflight(source: dict, profiles: list[dict], run_dir: Path, *,
              on_activity=None, require_voice=True) -> dict:
    return _call("conditioning_preflight", source=source, profiles=profiles,
                 run_dir=str(Path(run_dir).resolve()), on_activity=on_activity,
                 require_voice=require_voice)


def prepare(plan: dict, profiles: list[dict], run_dir: Path, activity, *,
            storyboard_conditioned=False) -> list[dict]:
    return _call("conditioning_prepare", plan=plan, profiles=profiles,
                 run_dir=str(Path(run_dir).resolve()), on_activity=activity,
                 storyboard_conditioned=storyboard_conditioned)


def apply_voice(source: Path, clip: Path, voice: dict, run_dir: Path, seconds: int, *,
                width: int, height: int) -> dict:
    # Release Python/MLX buffers before Rust launches the isolated MPS worker.
    gc.collect()
    mlx = sys.modules.get("mlx.core")
    if mlx is not None:
        mlx.clear_cache()
    return _call("conditioning_apply_voice", source=str(Path(source).resolve()),
                 clip=str(Path(clip).resolve()), voice=voice,
                 run_dir=str(Path(run_dir).resolve()), seconds=seconds,
                 width=width, height=height)
