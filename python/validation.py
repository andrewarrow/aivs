"""Rust media-validation adapter and supplied-PCM waveform comparison."""

from array import array
import math
from pathlib import Path
import subprocess
import wave

import rust_engine

def validate_supplied_audio(video: Path, source: Path, duration: float) -> dict:
    """Check that the final soundtrack retains the supplied PCM performance.

    AAC and mono/stereo conversion change samples and gain. Correlation checks
    waveform identity and alignment while allowing those encoding differences;
    it is not an independent speaker-recognition or transcription check.
    """
    with wave.open(str(source), "rb") as stream:
        rate, channels = stream.getframerate(), stream.getnchannels()
        reference = array("h", stream.readframes(round(duration * rate)))
    decoded = subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin",
        "-i", str(video), "-map", "0:a:0", "-t", str(duration),
        "-f", "s16le", "-ar", str(rate), "-ac", str(channels), "pipe:1",
    ], check=True, capture_output=True).stdout
    actual = array("h", decoded)
    if not reference or len(actual) != len(reference):
        raise RuntimeError("Fixed soundtrack validation failed: decoded duration differs from source.")
    source_energy = sum(value * value for value in reference)
    output_energy = sum(value * value for value in actual)
    if source_energy == 0:
        if output_energy:
            raise RuntimeError("Fixed soundtrack validation failed: expected silence.")
        return {"waveform_correlation": 1.0, "gain": 1.0, "samples": len(actual)}
    dot = sum(a * b for a, b in zip(reference, actual))
    correlation = dot / math.sqrt(source_energy * output_energy) if output_energy else 0.0
    gain = math.sqrt(output_energy / source_energy)
    if correlation < 0.995 or not 0.5 <= gain <= 2.0:
        raise RuntimeError("Fixed soundtrack validation failed: output differs from the supplied performance.")
    return {"waveform_correlation": correlation, "gain": gain, "samples": len(actual)}


def validate_video(path: Path, frames: int, width: int, height: int,
                   expect_audio: bool = False) -> dict:
    return rust_engine.call("validate_video", path=str(Path(path).resolve()), frames=frames,
                            width=width, height=height, expect_audio=expect_audio)
