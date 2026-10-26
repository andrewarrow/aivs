"""Decoded frame and audio/video timing checks migrated from Hunter."""

import json
from array import array
import math
from pathlib import Path
import subprocess
import wave

FPS = 24


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
    result = subprocess.run([
        "ffprobe", "-v", "error", "-count_frames",
        "-show_entries", "stream=codec_type,width,height,nb_read_frames,r_frame_rate,duration,codec_name,pix_fmt,start_time,sample_rate,channels",
        "-of", "json", str(path),
    ], check=True, capture_output=True, text=True)
    streams = json.loads(result.stdout)["streams"]
    videos = [stream for stream in streams if stream.get("codec_type") == "video"]
    if not videos:
        raise RuntimeError("Video validation failed: no video stream.")
    info = videos[0]
    if (int(info["nb_read_frames"]), info["width"], info["height"]) != (frames, width, height):
        raise RuntimeError(f"Video validation failed: {info}")
    if info["r_frame_rate"] != f"{FPS}/1" or abs(float(info["duration"]) - frames / FPS) > 0.001:
        raise RuntimeError(f"Unexpected video timing: {info}")
    if expect_audio:
        audios = [stream for stream in streams if stream.get("codec_type") == "audio"]
        if len(audios) != 1:
            raise RuntimeError("Audio validation failed: expected one audio stream.")
        sound = audios[0]
        # AAC packet boundaries can differ by a few milliseconds from video frames.
        tolerance = 1 / FPS + 0.01
        if (int(sound.get("sample_rate", 0)) < 1 or int(sound.get("channels", 0)) < 1
                or abs(float(sound.get("duration", 0)) - frames / FPS) > tolerance
                or abs(float(sound.get("start_time", 0)) - float(info.get("start_time", 0))) > tolerance):
            raise RuntimeError(f"Unexpected audio timing or format: {sound}")
        info["audio"] = sound
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-xerror", "-nostdin",
        "-i", str(path), "-map", "0:v:0", "-map", "0:a?", "-f", "null", "-",
    ], check=True)
    return info

