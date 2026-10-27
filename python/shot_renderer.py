"""Compatibility names for the Rust shot execution engine."""
from pathlib import Path
import rust_engine
from production_adapter import call

FPS, WIDTH, HEIGHT = 24, 1280, 704


def _atomic_json(path, value):
    call("write_json", path=Path(path), value=value)


def _validate_plan(plan):
    checked = rust_engine.call("validate_render_plan", plan=plan)
    return plan["shots"], checked["seconds"], checked["fade_out"]


def _assemble(clips, output, seconds, fade_out, *, width=WIDTH, height=HEIGHT, crop=False):
    return call("assemble", clip_count=len(clips), output=Path(output), seconds=seconds,
                fade_out=fade_out, width=width, height=height, crop=crop)


def render_shooting_script(plan, seed, output, *, render_fn=None, **options):
    if render_fn is not None:
        function = getattr(render_fn, "func", render_fn)
        settings = getattr(render_fn, "keywords", {})
        if function.__module__ == "quality_backend":
            options["profile"] = settings.get("profile", "distilled")
        elif function.__module__ == "premium_backend":
            import premium_backend
            options["render_config"] = premium_backend.profile_config(settings.get("model", "ltx-2-5-pro"))
        else:
            raise ValueError("Rust production requires a registered model backend.")
    return call("render", plan=plan, seed=seed, output=Path(output), **options)
