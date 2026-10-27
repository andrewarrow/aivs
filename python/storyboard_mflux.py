"""Local reference-aware FLUX.2 stills in a separate MFLUX environment.

Install with ``uv venv .venv-storyboard --python 3.12`` then
``uv pip install --python .venv-storyboard/bin/python -r python/requirements-storyboard.txt``.
Run ``.venv/bin/python python/storyboard_mflux.py download`` once. Rendering
uses only the pinned local snapshot; it never submits images to a service.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

from quality_backend import ROOT, CACHE
from download_space import preflight_downloads, valid_cached_file

MODEL_REPO = "black-forest-labs/FLUX.2-klein-4B"
MODEL_REVISION = "e7b7dc27f91deacad38e78976d1f2b499d76a294"
MFLUX_VERSION = "0.24.0"
_FILES = (
    "transformer/diffusion_pytorch_model.safetensors", "transformer/config.json",
    "vae/diffusion_pytorch_model.safetensors", "vae/config.json",
    "text_encoder/model-00001-of-00002.safetensors",
    "text_encoder/model-00002-of-00002.safetensors",
    "text_encoder/model.safetensors.index.json", "text_encoder/config.json",
    "tokenizer/tokenizer.json", "tokenizer/tokenizer_config.json",
    "tokenizer/merges.txt", "tokenizer/vocab.json", "tokenizer/chat_template.jinja",
)
_SETUP = ("Install the separate storyboard environment following README.md, then run "
          ".venv/bin/python python/storyboard_mflux.py download and Resume.")


class InvalidStoryboardCache(RuntimeError):
    def __init__(self, filename):
        self.filename = filename
        super().__init__(f"Local storyboard model file is missing or incomplete: {filename}. {_SETUP}")


def configuration() -> dict:
    return {"backend": "mflux", "runtime_version": MFLUX_VERSION,
            "model": MODEL_REPO, "model_revision": MODEL_REVISION,
            "quantization": 8, "steps": 4, "guidance": 1.0,
            "scheduler": "flow_match_euler_discrete"}


def _snapshot() -> Path:
    path = CACHE / "hub" / ("models--" + MODEL_REPO.replace("/", "--")) / "snapshots" / MODEL_REVISION
    for filename in _FILES:
        if not valid_cached_file(path / filename):
            raise InvalidStoryboardCache(filename)
    return path


def _worker(command: str, payload=None, *, on_activity=None, log_file=None) -> dict:
    """Rust owns process lifetime; this environment only supplies MFLUX inference."""
    import rust_engine
    return rust_engine.call("inference", root=str(ROOT), worker_operation="mflux_" + command,
        payload=payload or {}, log_file=str(Path(log_file).resolve()) if log_file else None,
        on_event=rust_engine.activity_events(on_activity))


def check() -> dict:
    """Check the isolated package and cached weights without loading a model."""
    return _worker("check")


def download_spec():
    return {"repo_id": MODEL_REPO, "revision": MODEL_REVISION,
            "allow_patterns": ["transformer/*", "vae/*", "text_encoder/*", "tokenizer/*",
                               "model_index.json", "LICENSE.md"],
            "required_files": list(_FILES)}


def preflight_download():
    from dotenv import dotenv_values
    token = os.environ.get("HF_TOKEN") or dotenv_values(ROOT / ".env").get("HF_API") or False
    return preflight_downloads([download_spec()], cache_dir=CACHE / "hub", output_dir=ROOT / "outputs",
                               token=token)


def download() -> None:
    """Explicit setup only; download pinned files, without loading a model."""
    from huggingface_hub import hf_hub_download, snapshot_download
    from dotenv import dotenv_values

    token = os.environ.get("HF_TOKEN") or dotenv_values(ROOT / ".env").get("HF_API") or False
    preflight_download()
    snapshot_download(MODEL_REPO, revision=MODEL_REVISION, cache_dir=str(CACHE / "hub"),
                      allow_patterns=download_spec()["allow_patterns"], token=token, max_workers=3)
    # Hugging Face trusts existing blobs. A retry must also replace a blob
    # damaged by a previously interrupted copy or a full disk.
    for _ in range(len(_FILES)):
        try:
            _snapshot()
            return
        except InvalidStoryboardCache as exc:
            hf_hub_download(MODEL_REPO, exc.filename, revision=MODEL_REVISION,
                            cache_dir=str(CACHE / "hub"), token=token, force_download=True)
    _snapshot()


def _identity() -> dict:
    from importlib.metadata import PackageNotFoundError, version

    try:
        installed = version("mflux")
    except PackageNotFoundError as exc:
        raise RuntimeError(f"The local storyboard environment is incomplete. {_SETUP}") from exc
    if installed != MFLUX_VERSION:
        raise RuntimeError(f"Storyboard rendering requires mflux=={MFLUX_VERSION}. {_SETUP}")
    _snapshot()
    return {**configuration(), "packages": {name: version(name) for name in
            ("mflux", "mlx", "transformers", "huggingface-hub", "Pillow")}}


def _fit_prompt(tokenizer, prompt, composition=None, compact_prompt=None):
    """Use accepted reference geometry to budget state, then fit composition prose."""
    def count(value):
        formatted = tokenizer.apply_chat_template([{"role": "user", "content": value}],
                                                   tokenize=False, add_generation_prompt=True,
                                                   enable_thinking=False)
        return len(tokenizer.encode(formatted, add_special_tokens=True, truncation=False))

    candidate = prompt + ("\nComposition: " + composition if composition else "")
    if compact_prompt is not None and count(candidate) > 512 and count(compact_prompt) < count(prompt):
        prompt = compact_prompt
    required_tokens = count(prompt)
    if required_tokens > 512:
        raise ValueError(f"Essential storyboard state needs {required_tokens} tokens; simplify the visible composition to fit 512.")
    if composition is None:
        return prompt, required_tokens, False
    words = composition.split()
    # Only shorten optional prose. Counts, role exclusions, reference mapping,
    # ownership, placement and concealment rules are never token-truncated.
    candidate = prompt + "\nComposition: " + composition
    tokens = count(candidate)
    if tokens <= 512:
        return candidate, tokens, False
    low, high = 0, len(words)
    best, best_tokens = prompt, required_tokens
    while low <= high:
        middle = (low + high) // 2
        candidate = prompt + ("\nComposition: " + " ".join(words[:middle]) if middle else "")
        tokens = count(candidate)
        if tokens <= 512:
            best, best_tokens = candidate, tokens
            low = middle + 1
        else:
            high = middle - 1
    return best, best_tokens, True


def _generate(payload: dict) -> dict:
    identity = _identity()
    from transformers import Qwen2TokenizerFast
    from mflux.models.common.config import ModelConfig
    from mflux.models.flux2.variants import Flux2Klein, Flux2KleinEdit

    output = Path(payload["output"])
    if output.exists():
        raise FileExistsError(f"Storyboard output already exists: {output}")
    started = time.monotonic()
    tokenizer = Qwen2TokenizerFast.from_pretrained(str(_snapshot() / "tokenizer"), local_files_only=True)
    prompt, token_count, shortened = _fit_prompt(tokenizer, payload["prompt"], payload.get("composition"),
                                                payload.get("compact_prompt"))
    if shortened:
        print("Shortened composition prose to preserve essential opening state within 512 tokens.", flush=True)
    # The edit pipeline concatenates reference latents unconditionally. Empty
    # scenery shots use the same weights through its text-to-image variant.
    has_references = bool(payload["references"])
    pipeline = Flux2KleinEdit if has_references else Flux2Klein
    model = pipeline(model_config=ModelConfig.flux2_klein_4b(),
                     model_path=str(_snapshot()), quantize=8)
    image = model.generate_image(
        prompt=prompt, seed=payload["seed"],
        width=payload["width"], height=payload["height"], num_inference_steps=4,
        guidance=1.0, scheduler="flow_match_euler_discrete",
        **({"image_paths": payload["references"]} if has_references else {}),
    )
    image.save(path=str(output))
    with output.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {**identity, "pipeline": "reference-edit" if has_references else "text-to-image",
            "prompt": prompt, "prompt_tokens": token_count, "composition_shortened": shortened,
            "state_compacted": bool(payload.get("compact_prompt") and prompt.startswith(payload["compact_prompt"])),
            "elapsed_seconds": round(time.monotonic() - started, 3), "sha256": digest}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "download", "_check", "_generate"))
    args = parser.parse_args()
    if args.command == "download":
        download()
        print("Local storyboard model downloaded. Install python/requirements-storyboard.txt "
              "in .venv-storyboard if needed, then Resume.")
    elif args.command == "check":
        print(json.dumps(check(), indent=2))
    else:
        payload = json.load(sys.stdin)
        result = _identity() if args.command == "_check" else _generate(payload)
        print("AIVS_STORYBOARD " + json.dumps(result), flush=True)
