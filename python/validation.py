"""Compatibility adapters for Rust video and supplied-PCM validation."""
from pathlib import Path
import rust_engine


def validate_supplied_audio(video: Path, source: Path, duration: float) -> dict:
    return rust_engine.call("validate_supplied_audio", video=str(Path(video).resolve()),
                            source=str(Path(source).resolve()), duration=duration)


def validate_video(path: Path, frames: int, width: int, height: int,
                   expect_audio: bool = False) -> dict:
    return rust_engine.call("validate_video", path=str(Path(path).resolve()), frames=frames,
                            width=width, height=height, expect_audio=expect_audio)
