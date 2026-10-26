"""Pinned offline Seed-VC worker for diagnostic takes and saved-character GUI audio."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import random
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CODE = ROOT / ".cache/seed-vc"
DEFAULT_MODELS = ROOT / ".cache/seed-vc-models"
CONFIG_RELATIVE = Path("configs/presets/config_dit_mel_seed_uvit_whisper_small_wavenet.yml")
EXPECTED_REVISION = "51383efd921027683c89e5348211d93ff12ac2a8"
REQUIRED_MODELS = {
    "seed": ("Plachta/Seed-VC", ("DiT_seed_v2_uvit_whisper_small_wavenet_bigvgan_pruned.pth",
                                   "config_dit_mel_seed_uvit_whisper_small_wavenet.yml")),
    "campplus": ("funasr/campplus", ("campplus_cn_common.bin",)),
    "bigvgan": ("nvidia/bigvgan_v2_22khz_80band_256x", ("config.json", "bigvgan_generator.pt")),
    "whisper": ("openai/whisper-small", ("config.json", "preprocessor_config.json", "model.safetensors")),
}
SEED = 83
BINDINGS = {
    "lena": {"role": "SUE", "line_id": "sue-line", "dialogue": "We still have some time.",
             "voice_id": "af_sarah"},
    "owen": {"role": "JOE", "line_id": "joe-line", "dialogue": "Then let's keep going.",
             "voice_id": "am_michael"},
}

sys.path.insert(0, str(ROOT / "python"))
import characters  # noqa: E402


def _hash(path: Path) -> str:
    return characters._digest(Path(path))


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


def validate_prepared(run_dir: Path) -> dict:
    run_dir = Path(run_dir).resolve()
    manifest_path = _safe_file(run_dir / "manifest.json", "prepared manifest")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    character_id = manifest.get("character", {}).get("id")
    binding = BINDINGS.get(character_id) if isinstance(character_id, str) else None
    if binding is None:
        raise ValueError("Prepared voice reference has an unsupported character identity.")
    voice_id = binding["voice_id"]
    if (manifest.get("status") != "reference-prepared-needs-quality-review"
            or manifest.get("fixed_voice_acceptance") is not False
            or manifest.get("character", {}).get("voice_id") != voice_id):
        raise ValueError("Input is not an unaccepted, review-pending supported voice-reference preparation.")
    acceptance_path = _safe_file(run_dir / "acceptance.json", "acceptance copy")
    acceptance = json.loads(acceptance_path.read_text(encoding="utf-8"))
    if (_hash(acceptance_path) != manifest.get("source_acceptance_sha256")
            or (acceptance.get("character_id") is not None
                and acceptance.get("character_id") != character_id)
            or acceptance.get("status") != "accepted-diagnostic-native-take"
            or not (acceptance.get("native_take_accepted") is True
                    or (character_id == "lena" and acceptance.get("milestone_a_passed") is True))
            or acceptance.get("fixed_voice_acceptance") is not False
            or acceptance.get("human_review", {}).get("status") != "accepted"
            or any(acceptance.get("human_review", {}).get(k) != "accepted"
                   for k in ("lip_sync", "face_identity", "spoken_words"))):
        raise ValueError("Prepared acceptance copy is missing, changed, or no longer describes the approved native take.")
    source_quality_path = _safe_file(run_dir / "source-quality.json", "user source-quality confirmation")
    source_quality = json.loads(source_quality_path.read_text(encoding="utf-8"))
    if (source_quality.get("version") != 1 or source_quality.get("status") != "clean-speech-confirmed"
            or source_quality.get("reviewer") != "user"
            or source_quality.get("source_video_sha256") != manifest.get("source_video_sha256")
            or source_quality.get("source_audio_sha256") != manifest.get("source_audio_sha256")):
        raise ValueError("User source-quality confirmation is missing, invalid, or bound to different source media.")
    copied = manifest.get("inputs", {})
    source_manifest = _safe_file(run_dir / "source-manifest.json", "source manifest copy")
    source_render = _safe_file(run_dir / "source-render.json", "source render copy")
    evidence = _safe_file(run_dir / "source-review-evidence.json", "review evidence copy")
    for name, path, expected in (
        ("source-video.mp4", run_dir / "source-video.mp4", manifest.get("source_video_sha256")),
        ("source-native-video.mp4", run_dir / "source-native-video.mp4", manifest.get("source_native_video_sha256")),
        ("source-audio.wav", run_dir / "source-audio.wav", manifest.get("source_audio_sha256")),
        ("source-manifest.json", source_manifest, manifest.get("source_manifest_sha256")),
        ("source-render.json", source_render, manifest.get("source_render_sha256")),
        ("source-review-evidence.json", evidence, manifest.get("source_evidence_sha256")),
    ):
        path = _safe_file(path, name)
        if _hash(path) != expected or copied.get(name, {}).get("sha256") != expected:
            raise ValueError(f"Prepared source file hash does not match its manifest: {name}")
    source_manifest_obj = json.loads(source_manifest.read_text(encoding="utf-8"))
    source_snapshot = source_manifest_obj.get("character_snapshot", {})
    if (source_manifest_obj.get("character_id") != character_id
            or source_snapshot.get("id") != character_id
            or source_snapshot.get("voice_id") != voice_id
            or (source_manifest_obj.get("role"), source_manifest_obj.get("line_id"),
                source_manifest_obj.get("dialogue"))
            != (binding["role"], binding["line_id"], binding["dialogue"])):
        raise ValueError("Prepared source manifest character, voice, or dialogue binding is invalid.")
    accepted_source = acceptance.get("source", {})
    for key, expected in (("manifest", _hash(source_manifest)), ("render_report", _hash(source_render))):
        if accepted_source.get(key, {}).get("sha256") != expected:
            raise ValueError(f"Acceptance no longer anchors the copied {key} hash.")
    evidence_obj = json.loads(evidence.read_text(encoding="utf-8"))
    evidence_keys = {"manifest": _hash(source_manifest), "render": _hash(source_render),
                     "video": _hash(run_dir / "source-video.mp4"),
                     "native_audio": _hash(run_dir / "source-audio.wav")}
    for key, expected in evidence_keys.items():
        if evidence_obj.get("inputs", {}).get(key, {}).get("sha256") != expected:
            raise ValueError(f"Copied review evidence does not anchor {key}.")
    source_run = manifest.get("source_run")
    if not isinstance(source_run, str):
        raise ValueError("Prepared manifest has no accepted source run path.")
    accepted_outputs = acceptance.get("outputs", {})
    for accepted_key, filename, local_name in (
        ("video", "video.mp4", "source-video.mp4"),
        ("original_native_video", "video.native.mp4", "source-native-video.mp4"),
        ("native_audio", "native-audio.wav", "source-audio.wav"),
    ):
        record = accepted_outputs.get(accepted_key, {})
        actual_hash = _hash(run_dir / local_name)
        if record.get("file") != f"{source_run}/{filename}" or record.get("sha256") != actual_hash:
            raise ValueError(f"Acceptance does not directly anchor copied {accepted_key}.")
    accepted_evidence = acceptance.get("automated_observations", {}).get("evidence", {})
    if (accepted_evidence.get("file") != f"{source_run}/review/evidence.json"
            or accepted_evidence.get("sha256") != _hash(evidence)):
        raise ValueError("Acceptance does not directly anchor copied review evidence.")
    for name, record in acceptance.get("visual_review", {}).get("sheets", {}).items():
        path = _safe_file(run_dir / f"evidence-{name}", f"visual evidence {name}")
        if _hash(path) != record.get("sha256"):
            raise ValueError(f"Copied visual evidence hash mismatch: {name}")
    profile = characters._read_profile(run_dir / f"characters/{character_id}/character.json")
    if profile.get("id") != character_id or profile.get("voice_id") != voice_id:
        raise ValueError("Prepared character profile does not match the selected character and voice.")
    characters.verify_snapshots([profile], run_dir)
    if profile != source_snapshot:
        raise ValueError("Prepared character snapshot differs from the accepted take snapshot.")
    reference_path = f"characters/{character_id}/voice.wav"
    reference = _safe_file(run_dir / reference_path, "canonical voice reference")
    expected_ref = manifest.get("character", {}).get("voice_reference_sha256")
    if (_hash(reference) != expected_ref or _hash(reference) != profile.get("voice_sample", {}).get("sha256")
            or manifest.get("source_character_voice_sha256") != expected_ref
            or manifest.get("character", {}).get("voice_reference") != reference_path
            or manifest.get("canonical_reference_path") != reference_path):
        raise ValueError("Prepared canonical voice reference hash does not match its accepted snapshot.")
    audio = _safe_file(run_dir / "source-audio.wav", "accepted native source audio")
    return {"manifest": manifest, "acceptance": acceptance, "source_video": run_dir / "source-video.mp4",
            "source_audio": audio, "reference": reference, "character": profile,
            "dialogue": binding["dialogue"],
            "voice_id": voice_id,
            "source_hashes": {"prepared_manifest": _hash(manifest_path), "acceptance": _hash(acceptance_path),
                              "source_quality": _hash(source_quality_path),
                              "video": _hash(run_dir / "source-video.mp4"),
                              "native_video": _hash(run_dir / "source-native-video.mp4"),
                              "audio": _hash(audio), "manifest": _hash(source_manifest),
                              "render": _hash(source_render), "evidence": _hash(evidence),
                              "voice_reference": _hash(reference)}}


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
        raise RuntimeError("Seed-VC experiment requires an available Apple MPS device.")
    return {"device": "mps", "torch": torch.__version__, "mps_available": True,
            "mps_fallback": os.environ.get("PYTORCH_ENABLE_MPS_FALLBACK", "1")}


def _dependency_freeze() -> list[str]:
    return sorted(f"{item.metadata['Name']}=={item.version}" for item in importlib.metadata.distributions()
                  if item.metadata.get("Name"))


@contextmanager
def _project_lock():
    import video
    with video.project_lock(".render.lock"):
        yield


def _capture_code_archive(code_dir: Path, target: Path) -> dict:
    archive = subprocess.check_output(["git", "archive", "--format=tar", "HEAD"], cwd=code_dir)
    target.write_bytes(archive)
    return {"file": target.name, "sha256": _hash(target), "bytes": len(archive)}


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


def _recheck_inputs(run_dir: Path, inputs: dict, code_dir: Path, models_dir: Path,
                    metadata_path: Path, metadata: dict, initial_model_hashes: dict,
                    initial_metadata_hash: str, initial_code: dict) -> None:
    try:
        current = validate_prepared(run_dir)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise ValueError("Prepared source, acceptance, evidence, video, audio, or voice reference changed during inference.") from exc
    if current["source_hashes"] != inputs["source_hashes"]:
        raise ValueError("Prepared source, acceptance, evidence, video, audio, or voice reference changed during inference.")
    if _hash(metadata_path) != initial_metadata_hash:
        raise ValueError("Seed-VC models.json changed during inference.")
    if validate_models(models_dir, metadata) != initial_model_hashes:
        raise ValueError("One or more pinned model files changed during inference.")
    if validate_code(code_dir, metadata) != initial_code:
        raise ValueError("Pinned Seed-VC source changed during inference.")


def _run_inference(code_dir: Path, models_dir: Path, conversion_dir: Path, inputs: dict,
                   *, inference_fn=None, device_info_fn=None) -> dict:
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "1"
    os.chdir(code_dir)
    for path in (str(code_dir),):
        if path not in sys.path:
            sys.path.insert(0, path)
    device = (device_info_fn or _mps_info)()

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

    if inference_fn is None:
        import inference
        inference_fn = inference.main
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
    inference_fn(Args())
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


def run(run_dir: Path, *, code_dir: Path = DEFAULT_CODE, models_dir: Path = DEFAULT_MODELS,
        inference_fn=None, device_info_fn=None, lock_factory=None) -> dict:
    run_dir = Path(run_dir).resolve()
    code_dir, models_dir = Path(code_dir).resolve(), Path(models_dir).resolve()
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "1"
    inputs = validate_prepared(run_dir)
    conversion_dir = run_dir / "conversion"
    status_path = run_dir / "conversion-status.json"
    if conversion_dir.exists() or status_path.exists():
        raise FileExistsError("Conversion output/status already exists; use a new prepared run.")
    metadata_path = _safe_file(models_dir / "models.json", "model manifest")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    code = validate_code(code_dir, metadata)
    model_files = validate_models(models_dir, metadata)
    device_info = (device_info_fn or _mps_info)()
    conversion_dir.mkdir()
    status = {"version": 1, "status": "converting", "fixed_voice_acceptance": False,
              "source_hashes": inputs["source_hashes"], "voice_id": inputs["voice_id"],
              "code": code, "models_manifest_sha256": _hash(metadata_path),
              "model_files": model_files, "python": sys.version, "platform": platform.platform(),
              "device": device_info, "dependencies": _dependency_freeze(),
              "command": sys.argv, "started_at": datetime.now(timezone.utc).isoformat(),
              "source_paths": {"prepared_run": str(run_dir), "source_audio": str(inputs["source_audio"]),
                               "voice_reference": str(inputs["reference"]),
                               "source_video": str(inputs["source_video"])},
              "settings": {"seed": SEED, "diffusion_steps": 30, "length_adjust": 1.0,
                           "inference_cfg_rate": 0.7, "f0_condition": False,
                           "auto_f0_adjust": False, "semi_tone_shift": 0, "fp16": False}}
    _atomic_json(status_path, status)
    try:
        archive = _capture_code_archive(code_dir, conversion_dir / "seed-vc-source.tar")
        (conversion_dir / "models.json").write_bytes(metadata_path.read_bytes())
        (conversion_dir / "run_voice_conversion.py").write_bytes(Path(__file__).read_bytes())
        pinned_config = models_dir / "seed" / "config_dit_mel_seed_uvit_whisper_small_wavenet.yml"
        shutil.copyfile(pinned_config, conversion_dir / "config.pinned.yml")
        status.update(code_archive=archive,
                      snapshots={"models.json": _hash(conversion_dir / "models.json"),
                                 "run_voice_conversion.py": _hash(conversion_dir / "run_voice_conversion.py"),
                                 "config.pinned.yml": _hash(conversion_dir / "config.pinned.yml")})
        _atomic_json(status_path, status)
        lock = lock_factory or _project_lock
        with lock():
            result = _run_inference(code_dir, models_dir, conversion_dir, inputs,
                                    inference_fn=inference_fn, device_info_fn=lambda: device_info)
        _recheck_inputs(run_dir, inputs, code_dir, models_dir, metadata_path, metadata,
                        model_files, status["models_manifest_sha256"], code)
        # Import the PCM16 WAV analyzer from the sibling preparation helper.
        import importlib.util
        helper_path = Path(__file__).with_name("prepare_voice_reference.py")
        spec = importlib.util.spec_from_file_location("prepare_voice_reference", helper_path)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        converted = conversion_dir / "converted.wav"
        audio_provenance = _materialize_pcm16(result["raw_output"], converted)
        stats = helper._wav_stats(converted)
        output_hash = _hash(converted)
        status.update(status="converted-needs-review", fixed_voice_acceptance=False,
                      converted_output={"file": "conversion/converted.wav", "sha256": output_hash,
                                        "duration_seconds": stats["duration_seconds"], "pcm16_stats": stats,
                                        "encoding": audio_provenance},
                      elapsed_seconds=result["elapsed_seconds"], settings=result["settings"],
                      derived_config={"file": "conversion/config.local.yml",
                                      "sha256": _hash(result["config_file"]),
                                      "pinned_config_sha256": _hash(result["pinned_config"])},
                      completed_at=datetime.now(timezone.utc).isoformat(),
                      source_video_unchanged=True)
        _atomic_json(status_path, status)
        return status
    except BaseException as exc:
        status.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                      fixed_voice_acceptance=False, error=str(exc) or type(exc).__name__,
                      error_type=type(exc).__name__, completed_at=datetime.now(timezone.utc).isoformat())
        _atomic_json(status_path, status)
        raise


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
    """Convert a GUI shot to its saved voice, preserving the native video take.

    This uses the same pinned inference as the diagnostic experiment. It makes
    no claim of perceptual acceptance and never changes experiment evidence.
    The GUI owns the workspace lock; this child must not acquire it again.
    """
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
    mode.add_argument("--run-dir", type=Path, help="Prepared immutable voice-reference run")
    mode.add_argument("--check", action="store_true", help="Check shared GUI converter prerequisites")
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
        result = run(args.run_dir, code_dir=args.code_dir, models_dir=args.models_dir)
    except KeyboardInterrupt:
        return 130
    except Exception as exc:
        print(f"Voice conversion failed: {exc}", file=sys.stderr, flush=True)
        return 1
    print(json.dumps({"status": result["status"], "output": result["converted_output"]}, indent=2), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
