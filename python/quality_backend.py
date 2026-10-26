"""Hunter's native LTX-2.5 talking-video path, isolated from the Rust UI."""

from __future__ import annotations

import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import time
import wave

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
os.environ["HF_HOME"] = str(CACHE)
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")


class InvalidCache(RuntimeError):
    def __init__(self, filename):
        self.filename = filename
        super().__init__(f"Truncated or invalid cached model file: {filename}. Retry setup to repair it.")


def model_path(download=False, progress_class=None):
    from huggingface_hub import snapshot_download

    if download:
        try:
            return model_path()
        except (OSError, RuntimeError):
            pass
        if shutil.disk_usage(ROOT).free < 50 * 1024**3:
            raise RuntimeError("Model setup needs about 50 GiB free for the 43 GB model and download workspace.")
        from dotenv import dotenv_values
        token = dotenv_values(ROOT / ".env").get("HF_API")
        if not token or not token.strip():
            raise RuntimeError("Create .env with HF_API set to your Hugging Face access token.")
    else:
        token = False
    snapshot = Path(snapshot_download(
        MODEL_REPO, revision=MODEL_REVISION, cache_dir=str(CACHE / "hub"),
        allow_patterns=[*WEIGHTS, "*.json", "chat_template.jinja", "LICENSE", "README.md"],
        local_files_only=not download, token=token, max_workers=3, tqdm_class=progress_class,
    ))
    if download:
        # A cached blob may exist but be truncated. HF trusts existing blobs,
        # so explicitly replace only damaged files at this pinned revision.
        from huggingface_hub import hf_hub_download
        for _ in range(len(WEIGHTS) + len(CONFIGS)):
            try:
                validate_snapshot(snapshot)
                return snapshot
            except InvalidCache as exc:
                hf_hub_download(
                    MODEL_REPO, exc.filename, revision=MODEL_REVISION,
                    cache_dir=str(CACHE / "hub"), token=token, force_download=True,
                )
    validate_snapshot(snapshot)
    return snapshot


def validate_snapshot(snapshot):
    missing = [name for name in (*WEIGHTS, *CONFIGS)
               if not (snapshot / name).is_file() or (snapshot / name).stat().st_size == 0]
    if missing:
        raise RuntimeError("Model cache is incomplete: " + ", ".join(missing))
    # Check safetensors lengths without reading 43 GB on every startup. This
    # rejects truncated blobs, including a partial snapshot returned offline.
    for name in WEIGHTS:
        path = snapshot / name
        try:
            with path.open("rb") as reader:
                header_size = int.from_bytes(reader.read(8), "little")
                if not 1 <= header_size <= 100 * 1024**2:
                    raise ValueError("Invalid header length")
                metadata = json.loads(reader.read(header_size))
            end = max(entry["data_offsets"][1] for key, entry in metadata.items() if key != "__metadata__")
            if 8 + header_size + end != path.stat().st_size:
                raise ValueError("Invalid file length")
        except (ValueError, KeyError, TypeError, UnicodeError) as exc:
            raise InvalidCache(name) from exc
    for name in CONFIGS:
        if name.endswith(".json"):
            try:
                json.loads((snapshot / name).read_text())
            except (ValueError, UnicodeError) as exc:
                raise InvalidCache(name) from exc


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


def render(prompt, seed, frames, output, *, on_progress=None, image_file=None, hold_image_framing=False,
           audio_file=None, width=WIDTH, height=HEIGHT):
    from importlib.metadata import version
    from quality_progress import RenderProgress, ffmpeg_output, pipeline_activity
    from render_estimate import RenderEstimate
    from validation import validate_video

    output = Path(output)
    if (type(frames) is not int or frames < 1
            or any(type(value) is not int or value < 64 or value % 64 for value in (width, height))):
        raise ValueError("Render frames must be positive and dimensions must be positive multiples of 64.")
    image = prepare_source_image(image_file) if image_file is not None else None
    audio = prepare_source_audio(audio_file, frames / FPS) if audio_file is not None else None
    if image is not None and hold_image_framing:
        image["conditioning"] = "whole-first-and-last-frame"
        image["anchor_frames"] = [0] if frames <= 1 else [0, frames - 1]
    native = output.with_name("video.native.mp4")
    partial = output.with_name("video.partial.mp4")
    inference_frames = math.ceil((frames - 1) / 8) * 8 + 1
    started = time.monotonic()
    estimate = RenderEstimate.from_history(output.parent.parent, frames, width, height,
                                          RUNTIME_REVISION, on_progress)
    with RenderProgress(verbose=3, estimate=estimate) as progress:
        with progress.phase("Loading MLX renderer", "runtime"):
            import mlx.core as mx
            from ltx_pipelines_mlx.distilled import DistilledPipeline
            if audio is not None:
                from ltx_pipelines_mlx.a2vid_distilled import A2VidDistilledPipeline
        with progress.phase("Checking cached model files", "cache"):
            snapshot = model_path()
        mx.reset_peak_memory()
        with progress.phase("Preparing disk-streamed pipeline", "pipeline"):
            pipeline_type = A2VidDistilledPipeline if audio is not None else DistilledPipeline
            pipeline = pipeline_type(model_dir=str(snapshot), low_memory=True, low_ram_streaming=True)
        pipeline.generate_audio = audio is None
        generation_options = _image_generation_options(
            image["source_file"] if image is not None else None,
            frames,
            hold_image_framing,
        )
        if audio is not None:
            # The causal video grid needs up to eight extra inference frames.
            # Pad its conditioning track; final muxing uses the original audio.
            conditioning_audio = output.with_name("audio.conditioning.wav")
            subprocess.run([
                "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                "-i", audio["source_file"], "-af", f"apad,atrim=duration={inference_frames / FPS}",
                "-c:a", "pcm_s16le", str(conditioning_audio),
            ], check=True, capture_output=True, text=True)
            generation_options["audio_path"] = str(conditioning_audio)
        with pipeline_activity(pipeline, progress):
            pipeline.generate_and_save(
                prompt=prompt, output_path=str(native), width=width, height=height,
                num_frames=inference_frames, frame_rate=FPS, seed=seed,
                **generation_options,
            )
        decode_peak = mx.get_peak_memory() / 1024**3
        duration = frames / FPS
        audio_inputs = ["-i", audio["source_file"]] if audio is not None else []
        audio_map = "1:a:0" if audio is not None else "0:a:0"
        with progress.phase(f"Trimming and encoding {frames} frames with FFmpeg", "encode"):
            with ffmpeg_output([
                "ffmpeg", "-hide_banner", "-loglevel", "info", "-nostdin", "-n",
                "-i", str(native), *audio_inputs, "-map", "0:v:0", "-frames:v", str(frames),
                "-map", audio_map, "-af", f"apad,atrim=duration={duration},asetpts=PTS-STARTPTS",
                "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-t", str(duration),
                "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p",
                "-movflags", "+faststart", str(partial),
            ], progress, stdin=subprocess.DEVNULL):
                pass
        with progress.phase("Validating decoded frames and audio/video timing", "validate"):
            info = validate_video(partial, frames, width, height, expect_audio=True)
        partial.replace(output)
        native.unlink()
    return {
        "backend": "mlx", "model": MODEL_REPO, "model_revision": MODEL_REVISION,
        "runtime_revision": RUNTIME_REVISION,
        "pipeline": "audio-driven-distilled-two-stage" if audio is not None else "distilled-two-stage",
        "stage1_steps": 8, "stage2_steps": 3, "quantization": "int8",
        "audio": "supplied" if audio is not None else "generated",
        "inference_frames": inference_frames, "output_frames": frames,
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "phase_seconds": {key: round(value, 3) for key, value in estimate.phase_seconds.items()},
        "source_image": image,
        "source_audio": audio,
        "peak_decode_mlx_memory_gib": round(decode_peak, 2), "video": info,
        "software": {name: version(name) for name in (
            "mlx", "mlx-metal", "mlx-arsenal", "ltx-core-mlx", "ltx-pipelines-mlx", "transformers",
        )},
    }


def prepare_source_audio(source, minimum_duration):
    """Validate fixed PCM audio before loading any GPU model weights."""
    import hashlib

    source = Path(source).expanduser().resolve()
    try:
        with wave.open(str(source), "rb") as stream:
            rate, count = stream.getframerate(), stream.getnframes()
            if stream.getcomptype() != "NONE" or stream.getsampwidth() != 2 or stream.getnchannels() not in (1, 2):
                raise ValueError("Fixed audio must be mono or stereo 16-bit PCM WAV.")
            duration = count / rate
            if duration + 1 / rate < minimum_duration:
                raise ValueError("Fixed audio is shorter than the requested video. Pad the shot track with silence.")
            if len(stream.readframes(count)) != count * stream.getnchannels() * 2:
                raise ValueError("Fixed audio is truncated.")
    except (OSError, wave.Error, EOFError) as exc:
        raise ValueError(f"Cannot read fixed audio: {source}") from exc
    return {"source_file": str(source), "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "sample_rate": rate, "duration_seconds": duration, "conditioning": "frozen-audio"}


def prepare_source_image(source):
    """Validate Hunter's whole-first-frame image anchor before loading model weights."""
    import hashlib
    from PIL import Image

    source = Path(source).expanduser().resolve()
    if not source.is_file():
        raise FileNotFoundError(f"Image file does not exist: {source}")
    try:
        with Image.open(source) as picture:
            picture.verify()
        with Image.open(source) as picture:
            picture.load()
            width, height = picture.size
    except (OSError, ValueError, SyntaxError, Image.DecompressionBombError) as exc:
        raise ValueError(f"Cannot decode reference image: {source}") from exc
    return {
        "source_file": str(source),
        "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "width": width, "height": height,
        "conditioning": "whole-first-frame", "strength": 1.0,
    }


def _image_generation_options(source_file, frames, hold_image_framing):
    """Build the pipeline's image conditioning arguments for this render."""
    if source_file is None:
        return {}
    if not hold_image_framing:
        return {"image": source_file}

    from ltx_pipelines_mlx.utils.args import ImageConditioningInput

    images = [ImageConditioningInput.at(source_file, 0, 1.0)]
    if frames > 1:
        images.append(ImageConditioningInput.guide(source_file, frames - 1, 1.0))
    return {"images": images}
