"""Independent local transcription checks for generated dialogue.

Whisper receives only the generated audio, never the expected screenplay. This
is a conservative recognition gate, not proof of intelligibility, speaker
identity, or lip synchronization. Keep the raw transcript for human review.
"""

from __future__ import annotations

from difflib import SequenceMatcher
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import unicodedata
import urllib.request
import wave

MODEL_REPO = "ggerganov/whisper.cpp"
MODEL_REVISION = "5359861c739e955e79d9a303bcbc70fb988958b1"
MODEL_FILENAME = "ggml-small.bin"
MODEL_SHA256 = "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b"
MODEL_BYTES = 487601967
_TIMEOUT_SECONDS = 180
_WORDS = re.compile(r"[^\W_]+(?:['’][^\W_]+)*", re.UNICODE)
_CLI_OPTIONS = ("--model", "--file", "--language", "--no-gpu", "--threads",
                "--output-json-full", "--output-file", "--no-prints",
                "--suppress-nst", "--temperature", "--no-fallback")


class SpeechValidationError(RuntimeError):
    """A shot cannot be accepted; ``report`` is also saved beside its audio."""

    def __init__(self, message: str, report: dict):
        super().__init__(message)
        self.report = report


def _root(root: Path | None) -> Path:
    return Path(root or os.environ.get("AIVS_WORKSPACE", Path(__file__).resolve().parents[1])).resolve()


def _sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _atomic_json(path: Path, value: dict) -> None:
    with tempfile.NamedTemporaryFile("w", dir=path.parent, prefix=f".{path.name}.",
                                     suffix=".tmp", encoding="utf-8", delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    temporary.replace(path)


def _check_cli(executable: str) -> None:
    try:
        result = subprocess.run([executable, "--help"], check=True, capture_output=True,
                                text=True, timeout=15)
        help_text = result.stdout + result.stderr
    except (OSError, subprocess.SubprocessError) as exc:
        raise RuntimeError("Speech verification could not start whisper-cli. Repair whisper-cpp and retry setup.") from exc
    missing = [option for option in _CLI_OPTIONS if option not in help_text]
    if missing:
        raise RuntimeError("The installed whisper-cli is incompatible with speech verification. "
                           "Upgrade whisper-cpp and retry setup. Missing options: " + ", ".join(missing))


def check_available(root: Path | None = None) -> dict:
    """Check the recognizer and pinned model without network access or inference."""
    executable = shutil.which("whisper-cli")
    if executable is None:
        raise RuntimeError("Speech verification needs whisper-cli. Install whisper-cpp and retry setup.")
    _check_cli(executable)
    model = _root(root) / ".cache" / "whisper" / MODEL_FILENAME
    if (not model.is_file() or model.stat().st_size != MODEL_BYTES
            or _sha256(model) != MODEL_SHA256):
        raise RuntimeError("The speech verification model is missing or changed. Retry setup to download it.")
    return {"engine": "whisper.cpp", "executable": executable,
            "model": str(model), "model_repo": MODEL_REPO,
            "model_revision": MODEL_REVISION, "model_sha256": MODEL_SHA256,
            "language": "auto", "device": "CPU"}


def setup(root: Path | None = None, *, on_progress=None) -> dict:
    """Download the pinned recognition model only during explicit app setup."""
    executable = shutil.which("whisper-cli")
    if executable is None:
        raise RuntimeError("Speech verification needs whisper-cli. Install whisper-cpp and retry setup.")
    _check_cli(executable)
    root = _root(root)
    model = root / ".cache" / "whisper" / MODEL_FILENAME
    if model.is_file() and model.stat().st_size == MODEL_BYTES and _sha256(model) == MODEL_SHA256:
        return check_available(root)
    model.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://huggingface.co/{MODEL_REPO}/resolve/{MODEL_REVISION}/{MODEL_FILENAME}"
    temporary = None
    try:
        with tempfile.NamedTemporaryFile("wb", dir=model.parent, prefix=f".{MODEL_FILENAME}.",
                                         suffix=".download", delete=False) as stream:
            temporary = Path(stream.name)
            count, last_progress = 0, 0.0
            with urllib.request.urlopen(url, timeout=60) as response:
                while block := response.read(1024 * 1024):
                    stream.write(block)
                    count += len(block)
                    now = time.monotonic()
                    if on_progress is not None and (now - last_progress >= 0.5 or count == MODEL_BYTES):
                        on_progress(label=f"Downloading speech verification model: {count / 1e6:.1f} / {MODEL_BYTES / 1e6:.1f} MB",
                                    fraction=min(1.0, count / MODEL_BYTES))
                        last_progress = now
        if temporary.stat().st_size != MODEL_BYTES or _sha256(temporary) != MODEL_SHA256:
            raise RuntimeError("Speech verification model checksum mismatch. Retry setup.")
        temporary.replace(model)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return check_available(root)


def normalize_words(text: str) -> list[str]:
    """Ignore written case/punctuation, preserving lexical order and repetition."""
    text = unicodedata.normalize("NFKC", text).casefold().replace("’", "'")
    return [word.replace("'", "") for word in _WORDS.findall(text)]


def compare_transcript(expected_text: str, transcript: str) -> dict:
    expected, actual = normalize_words(expected_text), normalize_words(transcript)
    differences = []
    for operation, a, b, c, d in SequenceMatcher(None, expected, actual, autojunk=False).get_opcodes():
        if operation != "equal":
            differences.append({"operation": operation, "expected": expected[a:b], "recognized": actual[c:d],
                                "expected_word_range": [a, b], "recognized_word_range": [c, d]})
    return {"status": "passed" if expected == actual else "mismatch",
            "expected_text": expected_text, "transcript": transcript,
            "expected_words": expected, "recognized_words": actual,
            "differences": differences}


def _read_transcript(raw: dict, duration: float) -> tuple[str, list[dict]]:
    if not isinstance(raw, dict) or not isinstance(raw.get("transcription"), list):
        raise ValueError("Recognizer output has no transcription list.")
    segments = []
    for segment in raw["transcription"]:
        if not isinstance(segment, dict) or not isinstance(segment.get("text"), str):
            raise ValueError("Recognizer returned an invalid transcript segment.")
        offsets = segment.get("offsets")
        if not isinstance(offsets, dict):
            raise ValueError("Recognizer returned a segment without timestamps.")
        start, end = offsets.get("from"), offsets.get("to")
        if (type(start) not in (float, int) or type(end) not in (float, int)
                or not math.isfinite(start) or not math.isfinite(end)
                or not 0 <= start <= end or start > duration * 1000 + 50):
            raise ValueError("Recognizer returned invalid speech timestamps.")
        # Segment boundaries can extend into Whisper's zero padding; report a
        # clipped estimate without treating it as an edit decision.
        segments.append({"text": segment["text"], "start_seconds": start / 1000,
                         "end_seconds": min(duration, end / 1000)})
    return " ".join(segment["text"].strip() for segment in segments).strip(), segments


def validate_shot_audio(clip: Path, expected_text: str, report_dir: Path | None = None,
                        *, root: Path | None = None) -> dict:
    """Reject missing, repeated, substituted, or extra recognized words.

    All failures save a report and raise ``SpeechValidationError``. An exact
    normalized transcript match does not prove absence of unintelligible sound;
    speech timing, identity, and performance still require audiovisual review.
    No audio is removed or replaced by this check.
    """
    if not isinstance(expected_text, str):
        raise TypeError("Expected dialogue must be a string.")
    clip = Path(clip).resolve()
    directory = Path(report_dir).resolve() if report_dir is not None else clip.parent
    directory.mkdir(parents=True, exist_ok=True)
    raw_file = directory / "speech-recognition.json"
    transcript_file = directory / "speech-transcript.txt"
    report_file = directory / "speech-validation.json"
    report = {"status": "failed", "expected_text": expected_text,
              "clip": str(clip), "recognition_file": raw_file.name,
              "transcript_file": transcript_file.name,
              "method": "independent-transcript-word-comparison",
              "limitations": "Recognition can miss unintelligible sounds or mishear valid words; speaker identity and lip sync are not checked."}
    raw_file.unlink(missing_ok=True)
    transcript_file.unlink(missing_ok=True)
    try:
        identity = check_available(root)
        report["recognizer"] = identity
        with tempfile.TemporaryDirectory(prefix="aivs-speech-", dir=directory) as temporary:
            wav_file = Path(temporary) / "audio.wav"
            subprocess.run([
                "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                "-i", str(clip), "-map", "0:a:0", "-vn", "-ar", "16000", "-ac", "1",
                "-c:a", "pcm_s16le", str(wav_file),
            ], check=True, capture_output=True, text=True, timeout=_TIMEOUT_SECONDS)
            with wave.open(str(wav_file), "rb") as stream:
                duration = stream.getnframes() / stream.getframerate()
            if duration <= 0:
                raise ValueError("Generated audio is empty.")
            command = [
                identity["executable"], "--model", identity["model"], "--file", str(wav_file),
                "--language", "auto", "--no-gpu", "--threads", "4",
                "--output-json-full", "--output-file", str(raw_file.with_suffix("")),
                "--no-prints", "--suppress-nst", "--temperature", "0", "--no-fallback",
            ]
            result = subprocess.run(command, check=True, capture_output=True, text=True,
                                    timeout=_TIMEOUT_SECONDS)
            (directory / "speech-recognition.log").write_text(result.stderr, encoding="utf-8")
        raw = json.loads(raw_file.read_text(encoding="utf-8"))
        transcript, segments = _read_transcript(raw, duration)
        transcript_file.write_text(transcript + "\n", encoding="utf-8")
        report.update(compare_transcript(expected_text, transcript))
        report.update(duration_seconds=duration, segments=segments)
        if isinstance(raw.get("result"), dict):
            report["detected_language"] = raw["result"].get("language")
        spoken_segments = [segment for segment in segments if normalize_words(segment["text"])]
        report["last_speech_end_seconds"] = max((segment["end_seconds"] for segment in spoken_segments), default=None)
        if report["last_speech_end_seconds"] is not None:
            report["trailing_seconds_estimate"] = round(duration - report["last_speech_end_seconds"], 3)
        if report["status"] != "passed":
            report["error"] = "Generated speech does not match the screenplay. Review speech-validation.json and the shot audio."
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, wave.Error) as exc:
        report.update(status="failed", error=f"Could not verify generated speech: {exc}")
        if isinstance(exc, subprocess.CalledProcessError):
            (directory / "speech-recognition.log").write_text(exc.stderr or "", encoding="utf-8")
    _atomic_json(report_file, report)
    if report["status"] != "passed":
        raise SpeechValidationError(report["error"], report)
    return report
