"""Pinned offline Seed-VC inference for saved-character production audio."""

from __future__ import annotations

import argparse
import importlib.metadata
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CODE = ROOT / ".cache/seed-vc"
DEFAULT_MODELS = ROOT / ".cache/seed-vc-models"
EXPECTED_REVISION = "51383efd921027683c89e5348211d93ff12ac2a8"
REQUIRED_MODELS = {
    "seed": ("Plachta/Seed-VC", ("DiT_seed_v2_uvit_whisper_small_wavenet_bigvgan_pruned.pth",
                                   "config_dit_mel_seed_uvit_whisper_small_wavenet.yml")),
    "campplus": ("funasr/campplus", ("campplus_cn_common.bin",)),
    "bigvgan": ("nvidia/bigvgan_v2_22khz_80band_256x", ("config.json", "bigvgan_generator.pt")),
    "whisper": ("openai/whisper-small", ("config.json", "preprocessor_config.json", "model.safetensors")),
}
SEED = 83

def _hash(path: Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _atomic_json(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    temp.replace(path)


def _safe_file(path: Path, label: str) -> Path:
    path = Path(path).resolve()
    if not path.is_file():
        raise ValueError(f"Missing {label}: {path}")
    return path


def validate_code(code_dir: Path, models_manifest: dict) -> dict:
    code_dir = Path(code_dir).resolve()
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=code_dir, text=True).strip()
    dirty = subprocess.check_output(["git", "status", "--porcelain=v1"], cwd=code_dir, text=True)
    expected = models_manifest.get("code_revision")
    if revision != EXPECTED_REVISION or revision != expected or dirty.strip():
        raise ValueError(f"Seed-VC code must be clean at {EXPECTED_REVISION}; got {revision} (dirty={bool(dirty.strip())}).")
    return {"revision": revision, "clean": True}


def validate_models(models_dir: Path, metadata: dict) -> dict:
    models_dir = Path(models_dir).resolve()
    listed = {item.get("name"): item for item in metadata.get("models", []) if isinstance(item, dict)}
    found = {}
    for name, (repo_id, filenames) in REQUIRED_MODELS.items():
        item = listed.get(name)
        if not isinstance(item, dict) or item.get("repo_id") != repo_id or not item.get("revision"):
            raise ValueError(f"Model manifest lacks pinned {name} repository/revision metadata.")
        hashes = item.get("files", {})
        for filename in filenames:
            expected = hashes.get(filename)
            path = _safe_file(models_dir / name / filename, f"{name} model file {filename}")
            if not isinstance(expected, str) or _hash(path) != expected:
                raise ValueError(f"Model file missing a matching pinned hash: {name}/{filename}")
            found[f"{name}/{filename}"] = {"path": str(path), "sha256": expected}
    return found


def _mps_info() -> dict:
    import torch
    if not torch.backends.mps.is_available():
        raise RuntimeError("Saved voice conversion requires an available Apple MPS device.")
    return {"device": "mps", "torch": torch.__version__, "mps_available": True,
            "mps_fallback": os.environ.get("PYTORCH_ENABLE_MPS_FALLBACK", "1")}


def _materialize_pcm16(raw_output: Path, converted: Path) -> dict:
    import numpy as np
    import soundfile as sf

    raw_output, converted = Path(raw_output), Path(converted)
    info = sf.info(str(raw_output))
    samples, sample_rate = sf.read(str(raw_output), dtype="float64", always_2d=True)
    if samples.size == 0 or sample_rate < 1 or samples.shape[1] < 1:
        raise ValueError("Converter returned empty or structurally invalid audio.")
    if not np.isfinite(samples).all():
        raise ValueError("Converter output contains non-finite audio samples.")
    peak = float(np.max(np.abs(samples)))
    if peak == 0:
        raise ValueError("Converter output is silent.")
    if peak > 1.0:
        raise ValueError(f"Converter output exceeds PCM full scale ({peak:.8f}); refusing clipped conversion.")
    clipped = int(np.count_nonzero(np.abs(samples) >= 1.0))
    if info.subtype == "PCM_16":
        shutil.copyfile(raw_output, converted)
        encoding = "PCM_16 passthrough; exact raw WAV copied"
    else:
        sf.write(str(converted), samples, sample_rate, format="WAV", subtype="PCM_16")
        encoding = f"{info.subtype} to PCM_16; no normalization; out-of-range rejected"
    return {"encoding": encoding, "source_subtype": info.subtype, "sample_rate_hz": sample_rate,
            "channels": int(samples.shape[1]), "frames": int(samples.shape[0]),
            "prequantization_peak": peak, "prequantization_clipped_sample_count": clipped}


def _run_inference(code_dir: Path, models_dir: Path, conversion_dir: Path, inputs: dict) -> dict:
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "1"
    os.chdir(code_dir)
    for path in (str(code_dir),):
        if path not in sys.path:
            sys.path.insert(0, path)
    device = _mps_info()

    import numpy as np
    import torch
    random.seed(SEED)
    np.random.seed(SEED)
    torch.manual_seed(SEED)
    if hasattr(torch, "mps") and hasattr(torch.mps, "manual_seed"):
        torch.mps.manual_seed(SEED)

    import yaml
    metadata = json.loads((models_dir / "models.json").read_text(encoding="utf-8"))
    pinned_config = models_dir / "seed" / "config_dit_mel_seed_uvit_whisper_small_wavenet.yml"
    config = yaml.safe_load(pinned_config.read_text(encoding="utf-8"))
    config["model_params"]["speech_tokenizer"]["name"] = str((models_dir / "whisper").resolve())
    config["model_params"]["vocoder"]["name"] = str((models_dir / "bigvgan").resolve())
    config_path = conversion_dir / "config.local.yml"
    config_path.write_text(yaml.safe_dump(config, sort_keys=False), encoding="utf-8")

    import hf_utils
    campplus_file = (models_dir / "campplus/campplus_cn_common.bin").resolve()
    def local_model(repo_id, model_filename="pytorch_model.bin", config_filename=None):
        expected = ("funasr/campplus", "campplus_cn_common.bin", None)
        if (repo_id, model_filename, config_filename) != expected or _hash(campplus_file) != metadata["models"][
                [item["name"] for item in metadata["models"]].index("campplus")]["files"]["campplus_cn_common.bin"]:
            raise RuntimeError(f"Unexpected or changed model request from converter: {repo_id}/{model_filename}")
        return str(campplus_file)
    hf_utils.load_custom_model_from_hf = local_model

    import inference
    class Args:
        source = str(inputs["source_audio"])
        target = str(inputs["reference"])
        output = str(conversion_dir / "raw")
        checkpoint = str((models_dir / "seed/DiT_seed_v2_uvit_whisper_small_wavenet_bigvgan_pruned.pth").resolve())
        config = str(config_path)
        diffusion_steps = 30
        length_adjust = 1.0
        inference_cfg_rate = 0.7
        f0_condition = False
        auto_f0_adjust = False
        semi_tone_shift = 0
        fp16 = False
    started = time.monotonic()
    inference.main(Args())
    elapsed = time.monotonic() - started
    raw_dir = conversion_dir / "raw"
    outputs = sorted(raw_dir.glob("*.wav")) if raw_dir.is_dir() else []
    if len(outputs) != 1:
        raise RuntimeError(f"Expected exactly one converter WAV output, found {len(outputs)}.")
    return {"raw_output": outputs[0], "elapsed_seconds": elapsed, "device": device,
            "config_file": config_path, "pinned_config": pinned_config,
            "settings": {"seed": SEED, "diffusion_steps": 30,
                "length_adjust": 1.0, "inference_cfg_rate": 0.7, "f0_condition": False,
                "auto_f0_adjust": False, "semi_tone_shift": 0, "fp16": False,
                "whisper_encoder_dtype": "float16 (upstream default)",
                "cfm_and_vocoder_dtype": "float32", "network_offline": True}}


def check_converter(code_dir: Path, models_dir: Path) -> dict:
    """Check the shared offline converter without requiring an experiment take."""
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTORCH_ENABLE_MPS_FALLBACK="1")
    pins = {}
    for line in Path(__file__).with_name("requirements-seed-vc.txt").read_text().splitlines():
        if line.strip() and not line.lstrip().startswith("#"):
            package, expected = line.split("==")
            installed = importlib.metadata.version(package)
            if installed != expected:
                raise ValueError(f"Seed-VC requires {package}=={expected}; got {installed}. Reinstall its pinned environment.")
            pins[package] = installed
    metadata_path = _safe_file(models_dir / "models.json", "model manifest")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    code = validate_code(code_dir, metadata)
    validate_models(models_dir, metadata)
    return {"code": code, "models_manifest_sha256": _hash(metadata_path),
            "device": _mps_info(), "dependencies": pins, "policy": "seed-vc-native-timing-v1"}


def convert_audio(source: Path, reference: Path, output_dir: Path, *,
                  code_dir: Path, models_dir: Path) -> dict:
    """Convert one production soundtrack; Rust owns the workspace lock and video."""
    source = _safe_file(source, "native speech")
    reference = _safe_file(reference, "saved voice sample")
    output_dir = output_dir.resolve()
    identity = check_converter(code_dir, models_dir)
    inputs = {"source_audio": source, "reference": reference}
    hashes = {key: _hash(path) for key, path in inputs.items()}
    output_dir.mkdir(parents=True, exist_ok=True)
    # A fresh staging directory makes interrupted audio inference retryable
    # without replacing the original expensive video or reusing partial WAVs.
    import tempfile
    with tempfile.TemporaryDirectory(prefix=".convert-", dir=output_dir) as temporary:
        result = _run_inference(code_dir, models_dir, Path(temporary), inputs)
        staged = Path(temporary) / "converted.wav"
        encoding = _materialize_pcm16(result["raw_output"], staged)
        import wave
        with wave.open(str(source), "rb") as audio:
            source_seconds = audio.getnframes() / audio.getframerate()
        with wave.open(str(staged), "rb") as audio:
            converted_seconds = audio.getnframes() / audio.getframerate()
        if abs(converted_seconds - source_seconds) > 0.1:
            raise ValueError("Converted voice changed the shot duration by more than 100 ms.")
        if ({key: _hash(path) for key, path in inputs.items()} != hashes
                or check_converter(code_dir, models_dir) != identity):
            raise ValueError("Voice inputs or converter changed during inference.")
        target = output_dir / "converted.wav"
        staged.replace(target)
        record = {"status": "converted-needs-review", "identity": identity,
                  "inputs": hashes, "sha256": _hash(target),
                  "source_seconds": source_seconds, "converted_seconds": converted_seconds,
                  "encoding": encoding, "settings": result["settings"],
                  "elapsed_seconds": result["elapsed_seconds"]}
        _atomic_json(output_dir / "conversion.json", record)
    return record


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="Check production converter prerequisites")
    mode.add_argument("--source-audio", type=Path)
    parser.add_argument("--reference", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--code-dir", type=Path, default=DEFAULT_CODE)
    parser.add_argument("--models-dir", type=Path, default=DEFAULT_MODELS)
    args = parser.parse_args(argv)
    if args.source_audio and not all((args.reference, args.output_dir)):
        parser.error("--source-audio requires --reference and --output-dir")
    if not args.source_audio and (args.reference or args.output_dir):
        parser.error("--reference and --output-dir require --source-audio")
    signal.signal(signal.SIGTERM, lambda _signum, _frame: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        if args.check:
            result = check_converter(args.code_dir.resolve(), args.models_dir.resolve())
            print("AIVS_VOICE " + json.dumps(result), flush=True)
            return 0
        if args.source_audio:
            result = convert_audio(args.source_audio, args.reference, args.output_dir,
                                   code_dir=args.code_dir.resolve(), models_dir=args.models_dir.resolve())
            print("AIVS_VOICE " + json.dumps(result), flush=True)
            return 0
    except KeyboardInterrupt:
        return 130
    except Exception as exc:
        print(f"Voice conversion failed: {exc}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
