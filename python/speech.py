"""Optional, CPU-only fixed-voice speech worker for character experiments.

Run in .venv-voice, separate from the pinned video environment. Model assets are
downloaded only by the explicit setup command and verified before synthesis.
"""

from __future__ import annotations

import argparse
from importlib.metadata import version
import json
import os
from pathlib import Path
import tempfile
import urllib.request
import wave

from characters import _digest

ENGINE = "kokoro-onnx"
ENGINE_VERSION = "0.6.1"
MODEL_RELEASE = "model-files-v1.1"
MODEL_ASSETS = {
    "kokoro-v1.0.onnx": "beb0d1848dee9a49da392cc3df26958d46cfa35d321edf434f52949153f0df3a",
    "voices-v1.0.bin": "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
}
DEFAULT_CACHE = Path(__file__).resolve().parents[1] / ".cache" / "kokoro"


def model_identity(cache: Path) -> dict:
    for filename, expected in MODEL_ASSETS.items():
        path = Path(cache) / filename
        if not path.is_file() or _digest(path) != expected:
            raise ValueError(f"Speech model is missing or changed: {filename}. Run speech.py setup.")
    if version(ENGINE) != ENGINE_VERSION:
        raise ValueError(f"Speech requires {ENGINE}=={ENGINE_VERSION}.")
    return {"engine": ENGINE, "engine_version": ENGINE_VERSION,
            "onnxruntime_version": version("onnxruntime"), "release": MODEL_RELEASE,
            "phonemizer_version": version("phonemizer"), "espeakng_loader_version": version("espeakng-loader"),
            "assets": dict(MODEL_ASSETS), "provider": "CPUExecutionProvider"}


def setup(cache: Path) -> None:
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    for filename, expected in MODEL_ASSETS.items():
        target = cache / filename
        if target.is_file() and _digest(target) == expected:
            continue
        url = f"https://github.com/thewh1teagle/kokoro-onnx/releases/download/{MODEL_RELEASE}/{filename}"
        print(f"Downloading {filename}", flush=True)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=cache, prefix=f".{filename}.", delete=False) as stream:
                temporary = Path(stream.name)
                with urllib.request.urlopen(url, timeout=60) as response:
                    while block := response.read(1024 * 1024):
                        stream.write(block)
            if _digest(temporary) != expected:
                raise ValueError(f"Speech model checksum mismatch: {filename}")
            temporary.replace(target)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    print(json.dumps(model_identity(cache)), flush=True)


def synthesize(text: str, voice_id: str, output: Path, cache: Path = DEFAULT_CACHE) -> dict:
    if not isinstance(text, str) or not text.strip() or len(text) > 4000:
        raise ValueError("Speech text must contain 1 to 4000 characters.")
    identity = model_identity(cache)
    # Explicit provider avoids placing speech inference on the video GPU.
    os.environ["ONNX_PROVIDER"] = "CPUExecutionProvider"
    import numpy as np
    from kokoro_onnx import Kokoro

    engine = Kokoro(str(cache / "kokoro-v1.0.onnx"), str(cache / "voices-v1.0.bin"))
    if voice_id not in engine.get_voices():
        raise ValueError(f"Unknown saved Kokoro voice: {voice_id}")
    samples, rate = engine.create(text, voice=voice_id, speed=1.0, lang="en-us")
    samples = np.asarray(samples)
    if samples.ndim != 1 or not samples.size or not np.isfinite(samples).all():
        raise ValueError("Speech synthesis returned invalid audio.")
    if np.max(np.abs(samples)) < 0.001:
        raise ValueError("Speech synthesis returned silent audio.")
    pcm = (np.clip(samples, -1, 1) * 32767).astype("<i2").tobytes()
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists() or output.with_suffix(".json").exists():
        raise FileExistsError(f"Speech output already exists: {output}")
    with wave.open(str(output), "wb") as stream:
        stream.setnchannels(1)
        stream.setsampwidth(2)
        stream.setframerate(rate)
        stream.writeframes(pcm)
    record = {"file": output.name, "sha256": _digest(output), "text": text,
              "sample_rate": rate, "samples": len(samples),
              "duration_seconds": len(samples) / rate,
              "voice": {**identity, "voice_id": voice_id, "speed": 1.0, "language": "en-us"}}
    output.with_suffix(".json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("setup", "synthesize"))
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    parser.add_argument("--voice-id")
    parser.add_argument("--text-file", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.command == "setup":
        setup(args.cache)
    else:
        if not args.voice_id or args.text_file is None or args.output is None:
            parser.error("synthesize requires --voice-id, --text-file, and --output")
        print(json.dumps(synthesize(args.text_file.read_text(encoding="utf-8"),
                                    args.voice_id, args.output, args.cache)), flush=True)


if __name__ == "__main__":
    main()
