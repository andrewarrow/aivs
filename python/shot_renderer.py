"""Render and assemble a plan of connected, independently directed shots."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import time

import quality_backend
import speech_validation
from validation import validate_video

FPS = quality_backend.FPS
WIDTH = quality_backend.WIDTH
HEIGHT = quality_backend.HEIGHT
_SEED_MODULUS = 2**31


def _atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=path.parent, prefix=f".{path.name}.",
        suffix=".tmp", delete=False,
    ) as temporary:
        temp_path = Path(temporary.name)
        json.dump(value, temporary, indent=2)
        temporary.write("\n")
    temp_path.replace(path)


def _validate_plan(plan: dict) -> tuple[list[dict], int, bool]:
    if not isinstance(plan, dict) or plan.get("version") != 1:
        raise ValueError("Shooting plan must be an object with version 1.")
    shots = plan.get("shots")
    if not isinstance(shots, list) or not shots:
        raise ValueError("Shooting plan must contain at least one shot.")
    has_scene_ids = any(isinstance(shot, dict) and "scene_id" in shot for shot in shots)
    normalized = []
    for index, shot in enumerate(shots):
        if not isinstance(shot, dict):
            raise ValueError(f"Shot {index + 1} must be an object.")
        seconds = shot.get("seconds")
        if isinstance(seconds, bool) or not isinstance(seconds, int) or not 4 <= seconds <= 12:
            raise ValueError(f"Shot {index + 1} duration must be an integer from 4 to 12 seconds.")
        if not isinstance(shot.get("prompt"), str) or not shot["prompt"].strip():
            raise ValueError(f"Shot {index + 1} needs a non-empty prompt.")
        for key in ("continue_previous",):
            if not isinstance(shot.get(key), bool):
                raise ValueError(f"Shot {index + 1} needs a boolean {key} value.")
        if has_scene_ids and (type(shot.get("scene_id")) is not int or shot["scene_id"] < 0):
            raise ValueError(f"Shot {index + 1} needs a nonnegative integer scene_id.")
        if shot["continue_previous"]:
            if index == 0:
                raise ValueError("The first shot cannot continue a previous shot.")
            if has_scene_ids and shot["scene_id"] != normalized[-1]["scene_id"]:
                raise ValueError(f"Shot {index + 1} cannot continue across scenes.")
        for key in ("start_state", "end_state"):
            if not isinstance(shot.get(key), str):
                raise ValueError(f"Shot {index + 1} needs a string {key} value.")
        beat_ids = shot.get("beat_ids")
        if not isinstance(beat_ids, list):
            raise ValueError(f"Shot {index + 1} needs a beat_ids list.")
        normalized.append(shot)
    seconds = sum(shot["seconds"] for shot in normalized)
    if isinstance(plan.get("seconds"), bool) or plan.get("seconds") != seconds:
        raise ValueError("Shooting plan seconds must equal the sum of its shot durations.")
    if not isinstance(plan.get("fade_out"), bool):
        raise ValueError("Shooting plan needs a boolean fade_out value.")
    return normalized, seconds, plan["fade_out"]


def _uses_previous_frame(shots: list[dict], index: int) -> bool:
    if index == 0:
        return False
    # Scene IDs distinguish repeated headings (e.g. returning to the same room
    # later). For source-backed plans, a director's camera-cut flag must never
    # silently discard the only visual identity/wardrobe conditioning we have.
    if "scene_id" in shots[index]:
        return shots[index]["scene_id"] == shots[index - 1]["scene_id"]
    # Older hand-authored plans have no scene metadata.
    return shots[index]["continue_previous"]


def _expected_dialogue(plan: dict, shots: list[dict]) -> list[str]:
    """Use source beats, never generated prompt prose, as the speech contract."""
    source = plan.get("source")
    beats = source.get("beats") if isinstance(source, dict) else None
    if not isinstance(beats, list) or not beats:
        raise ValueError("Speech verification requires the shooting plan's source beats.")
    by_id = {}
    for beat in beats:
        if (not isinstance(beat, dict) or type(beat.get("id")) is not int
                or beat["id"] in by_id or beat.get("kind") not in ("action", "dialogue")
                or not isinstance(beat.get("text"), str) or not beat["text"].strip()):
            raise ValueError("Speech verification requires valid, unique source beats.")
        by_id[beat["id"]] = beat
    assigned = [beat_id for shot in shots for beat_id in shot["beat_ids"]]
    if any(type(beat_id) is not int for beat_id in assigned) or assigned != list(by_id):
        raise ValueError("Shot beat IDs must cover the source exactly once in order.")
    return [" ".join(by_id[beat_id]["text"] for beat_id in shot["beat_ids"]
                     if by_id[beat_id]["kind"] == "dialogue") for shot in shots]


def _extract_last_frame(clip: Path, frame_index: int, image: Path) -> None:
    image.parent.mkdir(parents=True, exist_ok=True)
    image.unlink(missing_ok=True)
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-i", str(clip), "-vf", f"select=eq(n\\,{frame_index})",
        "-frames:v", "1", "-fps_mode", "passthrough", str(image),
    ], check=True, capture_output=True, text=True)
    if not image.is_file() or image.stat().st_size == 0:
        raise RuntimeError(f"FFmpeg did not extract the last frame from {clip}.")


def _assemble(clips: list[Path], output: Path, seconds: int, fade_out: bool, *,
              width=None, height=None, verify_audio=None) -> dict:
    width = WIDTH if width is None else width
    height = HEIGHT if height is None else height
    run_dir = output.parent
    concat_file = run_dir / "shots.concat.txt"
    # The filenames are generated by this module and contain only fixed path
    # components and three-digit shot numbers, so no user path enters concat syntax.
    concat_file.write_text(
        "".join(f"file 'shots/{index:03d}/video.mp4'\n" for index in range(1, len(clips) + 1)),
        encoding="utf-8",
    )
    partial = output.with_name(f"{output.stem}.partial{output.suffix or '.mp4'}")
    partial.unlink(missing_ok=True)
    frames = seconds * FPS
    video_filters = [
        f"scale={width}:{height}:flags=lanczos", f"fps={FPS}", "format=yuv420p",
        "setpts=PTS-STARTPTS",
    ]
    audio_filters = ["aresample=48000", "apad", f"atrim=duration={seconds}", "asetpts=PTS-STARTPTS"]
    if fade_out:
        fade_start = max(0, seconds - 0.5)
        video_filters.append(f"fade=t=out:st={fade_start}:d=0.5")
        audio_filters.append(f"afade=t=out:st={fade_start}:d=0.5")
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-f", "concat", "-safe", "1", "-i", str(concat_file),
        "-map", "0:v:0", "-map", "0:a:0",
        "-vf", ",".join(video_filters), "-af", ",".join(audio_filters),
        "-frames:v", str(frames), "-t", str(seconds),
        "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2",
        "-movflags", "+faststart", str(partial),
    ], check=True, capture_output=True, text=True, cwd=run_dir)
    info = validate_video(partial, frames, width, height, expect_audio=True)
    if verify_audio is not None:
        info["speech_validation"] = verify_audio(partial)
    partial.replace(output)
    return info


def render_shooting_script(
    plan: dict,
    seed: int,
    output: Path,
    *,
    on_progress=None,
    on_shot=None,
    render_fn=None,
) -> dict:
    """Render plan shots in order and publish only a validated assembled video.

    ``render_fn`` follows ``quality_backend.render``. Completed shot files and
    their atomic reports remain available if a later shot or assembly fails.
    """
    shots, seconds, fade_out = _validate_plan(plan)
    dialogue = _expected_dialogue(plan, shots)
    if isinstance(seed, bool) or not isinstance(seed, int):
        raise ValueError("seed must be an integer.")
    # Fail before GPU inference if verification cannot run. Missing ASR is not
    # permission to publish an unchecked performance.
    speech_validation.check_available(quality_backend.ROOT)
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    shots_dir = output.parent / "shots"
    shots_dir.mkdir(parents=True, exist_ok=True)
    renderer = render_fn or quality_backend.render
    started = time.monotonic()
    total_shots = len(shots)
    total_frames = seconds * FPS
    last_fraction = 0.0
    completed_seconds = 0
    summaries = []
    clips = []

    def emit_progress(fraction, label, remaining_seconds=None):
        nonlocal last_fraction
        if on_progress is None:
            return
        fraction = max(last_fraction, min(0.99, fraction))
        last_fraction = fraction
        on_progress(label=label, fraction=fraction, remaining_seconds=remaining_seconds)

    def save_shot(index, report):
        shot_report = shots_dir / f"{index + 1:03d}" / "render.json"
        _atomic_json(shot_report, report)
        if on_shot is not None:
            on_shot(index, report)

    try:
        for index, shot in enumerate(shots):
            shot_dir = shots_dir / f"{index + 1:03d}"
            shot_dir.mkdir(parents=True, exist_ok=True)
            clip = shot_dir / "video.mp4"
            frames = shot["seconds"] * FPS
            shot_seed = (seed + index) % _SEED_MODULUS
            use_previous_frame = _uses_previous_frame(shots, index)
            image_file = shot_dir / "start-frame.png" if use_previous_frame else None
            source_frame = shots[index - 1]["seconds"] * FPS - 1 if use_previous_frame else None
            continuity = {
                "conditioning": "previous-shot-last-frame" if use_previous_frame else "none",
                "source_shot": index if use_previous_frame else None,
                "source_frame": source_frame,
                "source_image": str(image_file) if image_file is not None else None,
            }
            report = {
                "prompt": shot["prompt"],
                "seed": shot_seed,
                "seconds": shot["seconds"],
                "status": "running",
                "runtime": None,
                "start_state": shot["start_state"],
                "end_state": shot["end_state"],
                "beat_ids": shot["beat_ids"],
                "scene_id": shot.get("scene_id"),
                "continuity": continuity,
            }
            save_shot(index, report)

            shot_started = time.monotonic()

            def shot_progress(**data):
                local_fraction = data.get("fraction")
                if local_fraction is None:
                    return
                local_fraction = max(0.0, min(1.0, float(local_fraction)))
                by_duration = (completed_seconds + shot["seconds"] * local_fraction) / seconds
                remaining = data.get("remaining_seconds")
                if remaining is not None:
                    elapsed = max(0.001, time.monotonic() - shot_started)
                    observed_rate = elapsed / max(0.05, shot["seconds"] * local_fraction)
                    future_duration = seconds - completed_seconds - shot["seconds"]
                    remaining = max(0.0, float(remaining)) + future_duration * observed_rate
                emit_progress(
                    min(0.97, 0.97 * by_duration),
                    f"Shot {index + 1}/{total_shots}: {data.get('label', 'Rendering')}",
                    remaining,
                )

            try:
                if use_previous_frame:
                    _extract_last_frame(clips[-1], source_frame, image_file)
                runtime = renderer(
                    shot["prompt"], shot_seed, frames, clip,
                    on_progress=shot_progress,
                    image_file=image_file,
                    hold_image_framing=False,
                )
                report["runtime"] = runtime
                emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Checking spoken words")
                speech = speech_validation.validate_shot_audio(
                    clip, dialogue[index], shot_dir, root=quality_backend.ROOT,
                )
                report.update(status="complete", speech_validation=speech)
                clips.append(clip)
                summaries.append({
                    "index": index,
                    "prompt": shot["prompt"],
                    "seed": shot_seed,
                    "seconds": shot["seconds"],
                    "status": "complete",
                    "runtime": runtime,
                    "continuity": continuity,
                    "speech_validation": speech,
                })
                completed_seconds += shot["seconds"]
                save_shot(index, report)
                emit_progress(
                    min(0.97, 0.97 * completed_seconds / seconds),
                    f"Shot {index + 1}/{total_shots} complete",
                )
            except KeyboardInterrupt:
                report["status"] = "interrupted"
                save_shot(index, report)
                raise
            except Exception as exc:
                report.update(status="failed", error=str(exc))
                if isinstance(exc, speech_validation.SpeechValidationError):
                    report["speech_validation"] = exc.report
                save_shot(index, report)
                raise

        emit_progress(0.97, "Assembling shots")
        def verify_assembly(clip):
            emit_progress(0.98, "Checking assembled dialogue after transitions")
            return speech_validation.validate_shot_audio(
                clip, " ".join(text for text in dialogue if text), output.parent,
                root=quality_backend.ROOT,
            )

        video_info = _assemble(clips, output, seconds, fade_out, verify_audio=verify_assembly)
        emit_progress(0.99, "Validating complete video")
    except KeyboardInterrupt:
        raise

    return {
        "backend": "mlx",
        "pipeline": "directed-shots",
        "output_frames": total_frames,
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "shots": summaries,
        "video": video_info,
    }
