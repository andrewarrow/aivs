"""LTX-2.5 MLX inference and pinned model setup for Rust-owned production."""

from __future__ import annotations

import json
import os
from pathlib import Path
import time
import rust_engine

from download_space import download_progress_class, preflight_downloads, valid_cached_file

ROOT = Path(os.environ.get("AIVS_WORKSPACE", Path(__file__).resolve().parents[1])).resolve()
CACHE = ROOT / ".cache" / "huggingface"
MODEL_REPO = "dgrauet/ltx-2.5-mlx-q8"
MODEL_REVISION = "746ca9aacb697d2c739f544d68b584214dedcc75"
RUNTIME_REVISION = "14a3ddd6054736beb45b4d836135fa4d44490531"
FPS = 24
WIDTH, HEIGHT = 1280, 704
WEIGHTS = (
    "transformer-distilled.safetensors", "connector.safetensors", "text_encoder.safetensors",
    "duration_head.safetensors", "vae_encoder_conv.safetensors", "vae_decoder_conv.safetensors",
    "spatial_upscaler_x2_v1_0.safetensors", "audio_vae.safetensors", "vocoder.safetensors",
)
CONFIGS = (
    "embedded_config.json", "text_encoder_config.json", "tokenizer.json",
    "tokenizer_config.json", "chat_template.jinja", "quantize_config.json",
    "spatial_upscaler_x2_v1_0_config.json",
)
HQ_WEIGHTS = (*WEIGHTS, "transformer-dev.safetensors")
os.environ["HF_HOME"] = str(CACHE)
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")


class InvalidCache(RuntimeError):
    def __init__(self, filename):
        self.filename = filename
        super().__init__(f"Truncated or invalid cached model file: {filename}. Retry setup to repair it.")


def profile_config(profile="distilled"):
    """Identity for render manifests; changing profile never happens implicitly."""
    if profile not in ("distilled", "hq"):
        raise ValueError("Local render profile must be distilled or hq.")
    return {
        "backend": "mlx", "model": MODEL_REPO, "model_revision": MODEL_REVISION,
        "runtime_revision": RUNTIME_REVISION, "profile": profile,
        "pipeline": "hq-two-stage" if profile == "hq" else "distilled-two-stage",
        "stage1_steps": 15 if profile == "hq" else 8,
        "stage2_steps": 3 if profile == "hq" else 1,
        "quantization": "int8", "low_memory": True, "low_ram_streaming": True,
        **({"sampler": "res_2s", "cfg_scale": 3.0, "stg_scale": 0.0,
            "enable_teacache": False,
            "negative_prompt_suffix": "subtitles, captions, text overlays, on-screen text"}
           if profile == "hq" else {"preview_long_edge": 320}),
    }


def download_spec(profile="distilled"):
    profile_config(profile)
    weights = HQ_WEIGHTS if profile == "hq" else WEIGHTS
    return {"repo_id": MODEL_REPO, "revision": MODEL_REVISION,
            "allow_patterns": [*weights, "*.json", "chat_template.jinja", "LICENSE", "README.md"],
            "required_files": [*weights, *CONFIGS]}


def _download_token():
    from dotenv import dotenv_values
    token = os.environ.get("HF_TOKEN") or dotenv_values(ROOT / ".env").get("HF_API")
    if not isinstance(token, str) or not token.strip():
        raise RuntimeError("Create .env with HF_API set to your Hugging Face access token.")
    return token.strip()


def preflight_download(profile="distilled"):
    """Check the local video setup footprint before the first model is downloaded."""
    specs = [download_spec(profile)]
    return preflight_downloads(specs, cache_dir=CACHE / "hub", output_dir=ROOT / "outputs",
                               token=_download_token())


def model_path(download=False, progress_class=None, *, profile="distilled", on_progress=None):
    from huggingface_hub import snapshot_download

    profile_config(profile)
    weights = HQ_WEIGHTS if profile == "hq" else WEIGHTS
    if download:
        try:
            return model_path(profile=profile)
        except (OSError, RuntimeError):
            pass
        preflight = preflight_download(profile)
        if on_progress is not None:
            on_progress(label="Downloading the distilled and HQ video models", fraction=0.0)
            progress_class = download_progress_class(preflight["missing_bytes"], on_progress)
        token = _download_token()
    else:
        token = False
    snapshot = Path(snapshot_download(
        MODEL_REPO, revision=MODEL_REVISION, cache_dir=str(CACHE / "hub"),
        allow_patterns=download_spec(profile)["allow_patterns"],
        local_files_only=not download, token=token, max_workers=3, tqdm_class=progress_class,
    ))
    if download:
        # A cached blob may exist but be truncated. HF trusts existing blobs,
        # so explicitly replace only damaged files at this pinned revision.
        from huggingface_hub import hf_hub_download
        for _ in range(len(weights) + len(CONFIGS)):
            try:
                validate_snapshot(snapshot, profile=profile)
                return snapshot
            except InvalidCache as exc:
                hf_hub_download(
                    MODEL_REPO, exc.filename, revision=MODEL_REVISION,
                    cache_dir=str(CACHE / "hub"), token=token, force_download=True,
                    tqdm_class=progress_class,
                )
    validate_snapshot(snapshot, profile=profile)
    return snapshot


def validate_snapshot(snapshot, *, profile="distilled"):
    profile_config(profile)
    weights = HQ_WEIGHTS if profile == "hq" else WEIGHTS
    missing = [name for name in (*weights, *CONFIGS)
               if not (snapshot / name).is_file() or (snapshot / name).stat().st_size == 0]
    if missing:
        hint = " Run .venv/bin/python python/quality_backend.py --download-hq first." if profile == "hq" else ""
        raise RuntimeError("Model cache is incomplete: " + ", ".join(missing) + hint)
    # Check safetensors lengths without reading all weights on every startup. This
    # rejects truncated blobs, including a partial snapshot returned offline.
    for name in (*weights, *CONFIGS):
        if not valid_cached_file(snapshot / name):
            raise InvalidCache(name)


def doctor():
    from importlib.metadata import distribution, version
    import sys
    import mlx.core as mx
    from ltx_pipelines_mlx.distilled import DistilledPipeline  # noqa: F401

    if sys.version_info[:2] != (3, 12):
        raise RuntimeError("This renderer needs Python 3.12. Retry setup.")
    # Check the tested package resolution and the source runtime's commit.
    for line in Path(__file__).with_name("requirements.txt").read_text().splitlines():
        if "==" in line:
            name, expected = line.split("==", 1)
            if version(name) != expected:
                raise RuntimeError(f"Python package {name} needs setup.")
        elif " @ git+" in line:
            name = line.split(" @", 1)[0]
            direct = json.loads(distribution(name).read_text("direct_url.json") or "{}")
            if direct.get("vcs_info", {}).get("commit_id") != RUNTIME_REVISION:
                raise RuntimeError(f"Python package {name} needs the pinned runtime.")
    if not mx.metal.is_available() or mx.sum(mx.ones((4,)) * 2).item() != 8:
        raise RuntimeError("The MLX Apple GPU calculation failed.")


def _render_model(prompt, seed, frames, output, *, on_progress=None, image_file=None, hold_image_framing=False,
           audio_file=None, width=WIDTH, height=HEIGHT, keep_native=False, profile="distilled",
           image_anchors=None):
    from importlib.metadata import version
    from quality_progress import RenderProgress, pipeline_activity
    from render_estimate import RenderEstimate

    config = profile_config(profile)
    output = Path(output)
    prepared = rust_engine.call("prepare_ltx_render", profile=profile, frames=frames,
        width=width, height=height, output=str(output.resolve()), image_file=image_file,
        audio_file=audio_file, image_anchors=image_anchors, hold_image_framing=hold_image_framing)
    image, anchors, audio = (prepared[key] for key in ("image", "anchors", "audio"))
    native, inference_frames = prepared["native"], prepared["inference_frames"]
    started = time.monotonic()
    estimate = RenderEstimate.from_history(
        ROOT / "outputs", frames, width, height, RUNTIME_REVISION, on_progress,
        profile=profile, audio="supplied" if audio is not None else "generated",
    )
    with RenderProgress(verbose=3, estimate=estimate) as progress:
        with progress.phase("Loading MLX renderer", "runtime"):
            import mlx.core as mx
            from ltx_pipelines_mlx.distilled import DistilledPipeline
            if profile == "hq":
                from ltx_pipelines_mlx.ti2vid_two_stages_hq import TI2VidTwoStagesHQPipeline
            if audio is not None:
                from ltx_pipelines_mlx.a2vid_distilled import A2VidDistilledPipeline
        with progress.phase("Checking cached model files", "cache"):
            snapshot = model_path(profile=profile)
        mx.reset_peak_memory()
        with progress.phase("Preparing disk-streamed pipeline", "pipeline"):
            pipeline_type = (TI2VidTwoStagesHQPipeline if profile == "hq" else
                             A2VidDistilledPipeline if audio is not None else DistilledPipeline)
            pipeline = pipeline_type(model_dir=str(snapshot), low_memory=True, low_ram_streaming=True)
        pipeline.generate_audio = audio is None
        generation_options = _image_generation_options(
            image["source_file"] if image is not None else None,
            frames,
            hold_image_framing,
            anchors,
        )
        generation_options.update(stage1_steps=config["stage1_steps"],
                                  stage2_steps=config["stage2_steps"])
        if profile == "hq":
            from ltx_pipelines_mlx.utils.constants import DEFAULT_NEGATIVE_PROMPT
            # Extend the pinned runtime's quality exclusions instead of replacing them.
            generation_options.update(stage1_steps=15, stage2_steps=3, cfg_scale=3.0,
                                      stg_scale=0.0, enable_teacache=False,
                                      negative_prompt=f"{DEFAULT_NEGATIVE_PROMPT} {config['negative_prompt_suffix']}")
        if audio is not None:
            generation_options["audio_path"] = prepared["conditioning_audio"]
        with pipeline_activity(pipeline, progress, profile=profile):
            pipeline.generate_and_save(
                prompt=prompt, output_path=str(native), width=width, height=height,
                num_frames=inference_frames, frame_rate=FPS, seed=seed,
                **generation_options,
            )
        decode_peak = mx.get_peak_memory() / 1024**3
        with progress.phase(f"Encoding and validating {frames} frames with FFmpeg", "encode"):
            info = rust_engine.call("finish_ltx_render", prepared=prepared, frames=frames,
                width=width, height=height, output=str(output.resolve()), keep_native=keep_native)
    return {
        **config,
        "pipeline": "audio-driven-distilled-two-stage" if audio is not None else config["pipeline"],
        "audio": "supplied" if audio is not None else "generated",
        "inference_frames": inference_frames, "output_frames": frames,
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "phase_seconds": {key: round(value, 3) for key, value in estimate.phase_seconds.items()},
        "source_image": image,
        **({"image_anchors": anchors} if anchors is not None else {}),
        "source_audio": audio,
        "peak_decode_mlx_memory_gib": round(decode_peak, 2), "video": info,
        "software": {name: version(name) for name in (
            "mlx", "mlx-metal", "mlx-arsenal", "ltx-core-mlx", "ltx-pipelines-mlx", "transformers",
        )},
    }


def _image_generation_options(source_file, frames, hold_image_framing, anchors=None):
    """Build the pipeline's image conditioning arguments for this render."""
    if anchors is not None:
        from ltx_pipelines_mlx.utils.args import ImageConditioningInput
        return {"images": [
            (ImageConditioningInput.replace if row["frame"] == 0 else ImageConditioningInput.guide)(
                row["source_file"], row["frame"], row["strength"])
            for row in anchors]}
    if source_file is None:
        return {}
    if not hold_image_framing:
        return {"image": source_file}

    from ltx_pipelines_mlx.utils.args import ImageConditioningInput

    images = [ImageConditioningInput.at(source_file, 0, 1.0)]
    if frames > 1:
        images.append(ImageConditioningInput.guide(source_file, frames - 1, 1.0))
    return {"images": images}


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="Download the required pinned LTX distilled and HQ weights.")
    parser.add_argument("--download-hq", action="store_true", required=True,
                        help="Download the dev transformer; adds about 19.2 GB to a complete default cache.")
    parser.parse_args()
    print(model_path(download=True, profile="hq"))
