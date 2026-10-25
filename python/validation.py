"""Decoded frame and audio/video timing checks migrated from Hunter."""

import json
from pathlib import Path
import subprocess

FPS = 24


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

