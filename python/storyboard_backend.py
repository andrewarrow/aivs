"""Rust ImageGen controller adapter and optional local MFLUX inference routing."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path

from quality_backend import ROOT
import rust_engine

POLICY = "codex-imagegen-v2"
MAX_REFERENCE_IMAGES = 5


def _backend():
    selected = os.environ.get("AIVS_STORYBOARD_BACKEND", "codex-imagegen")
    if selected not in ("codex-imagegen", "mflux"):
        raise ValueError("AIVS_STORYBOARD_BACKEND must be codex-imagegen or mflux.")
    return selected


def _call(operation, *, on_activity=None, **args):
    return rust_engine.call(operation, root=str(ROOT),
                            on_event=rust_engine.activity_events(on_activity), **args)


def configuration() -> dict:
    if _backend() == "mflux":
        import storyboard_mflux
        return storyboard_mflux.configuration()
    return _call("imagegen_configuration")


def check_local() -> dict:
    if _backend() == "mflux":
        import storyboard_mflux
        return storyboard_mflux.check()
    return _call("imagegen_check_local")


def check(*, refresh=False, on_activity=None) -> dict:
    if _backend() == "mflux":
        import storyboard_mflux
        return storyboard_mflux.check()
    return _call("imagegen_check", refresh=refresh, on_activity=on_activity)


def _prepare_inputs(references, output, *, keep_first=False):
    prepared = _call("prepare_image_references", references=[str(p) for p in references],
                     output=str(output), keep_first=keep_first)
    return [Path(p) for p in prepared["paths"]], prepared["sources"], prepared["mapping"]


def _request(prompt, references, output, seed, width, height, composition, on_activity, **options):
    output = Path(output).resolve()
    if output.exists():
        raise FileExistsError(f"Refusing to overwrite storyboard image: {output}")
    if type(width) is not int or type(height) is not int or min(width, height) <= 0:
        raise ValueError("Storyboard dimensions must be positive integers.")
    return _call("imagegen_generate", prompt=prompt, references=[str(Path(p).resolve(strict=True)) for p in references],
                 output=str(output), seed=seed, width=width, height=height, composition=composition,
                 on_activity=on_activity, **options)


def generate(prompt: str, references: list[Path], output: Path, seed: int, *,
             width: int, height: int, composition=None, on_activity=None) -> dict:
    if _backend() == "mflux":
        return _generate_mflux(prompt, references, output, seed, width=width, height=height,
                               composition=composition, on_activity=on_activity)
    return _request(prompt, references, output, seed, width, height, composition, on_activity)


def edit(edit_target: Path, correction: str, preserve: list[str], references: list[Path],
         output: Path, seed: int, *, width: int, height: int, composition=None, on_activity=None) -> dict:
    if not correction.strip() or not preserve:
        raise ValueError("Image edits need a specific correction and preservation constraints.")
    target = Path(edit_target).resolve(strict=True)
    if _backend() == "mflux":
        inputs = [target, *[Path(p).resolve(strict=True) for p in references if Path(p).resolve() != target]]
        return _generate_mflux("Edit the first image. " + correction + " Preserve unchanged: " + ", ".join(preserve),
            inputs, output, seed, width=width, height=height, composition=composition,
            on_activity=on_activity, keep_first=True)
    return _request("Apply only the requested correction.", references, output, seed, width, height,
                    composition, on_activity, edit_target=str(target), correction=correction, preserve=preserve)


def _generate_mflux(prompt, references, output, seed, *, width, height, composition=None,
                    on_activity=None, keep_first=False):
    import storyboard_mflux
    output = Path(output).resolve()
    if output.exists():
        raise FileExistsError(f"Refusing to overwrite storyboard image: {output}")
    if type(width) is not int or type(height) is not int or min(width, height) <= 0:
        raise ValueError("Storyboard dimensions must be positive integers.")
    paths = [Path(p).resolve(strict=True) for p in references]
    inputs, sources, mapping = _prepare_inputs(paths, output, keep_first=keep_first)
    runtime = storyboard_mflux.generate(prompt + mapping, inputs, output, seed, width=width, height=height,
                                        composition=composition, on_activity=on_activity)
    for row in sources:
        if hashlib.sha256(Path(row["path"]).read_bytes()).hexdigest() != row["sha256"]:
            raise ValueError("A storyboard reference changed during local generation.")
    return dict(runtime, source_inputs=sources, reference_mapping=mapping)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check",))
    parser.add_argument("--refresh", action="store_true", help="Repeat the real generation/edit capability probe")
    args = parser.parse_args()
    print(json.dumps(check(refresh=args.refresh), indent=2))
