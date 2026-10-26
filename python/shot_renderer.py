"""Render and assemble a plan of connected, independently directed shots."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

import quality_backend
from validation import validate_video
from render_formats import resolve_format

FPS = quality_backend.FPS
WIDTH = quality_backend.WIDTH
HEIGHT = quality_backend.HEIGHT
_SEED_MODULUS = 2**31
_RETAKE_SEED_STRIDE = 1_000_003


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


def _file_sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _planned_shot_sha256(plan: dict, shot: dict) -> str:
    """Bind execution changes to the approved source, coverage and timing."""
    content = {"source_sha256": plan.get("source_sha256"),
               "planning_policy": plan.get("planning_policy"), "shot": shot}
    return hashlib.sha256(json.dumps(content, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def _take_seed(base_seed: int, take: int) -> int:
    return (base_seed + (take - 1) * _RETAKE_SEED_STRIDE) % _SEED_MODULUS


def _saved_report(shot_dir: Path) -> dict | None:
    try:
        report = json.loads((shot_dir / "render.json").read_text(encoding="utf-8"))
    except (FileNotFoundError, ValueError):
        return None
    if (not isinstance(report, dict) or not isinstance(report.get("prompt"), str)
            or type(report.get("seed")) is not int
            or type(report.get("seconds")) is not int
            or not isinstance(report.get("beat_ids"), list)
            or not isinstance(report.get("continuity"), dict)
            or type(report.get("take", 1)) is not int or report.get("take", 1) < 1):
        return None
    return report


def _last_archived_take(archive_dir: Path) -> int:
    if not archive_dir.is_dir():
        return 0
    return max((int(entry.name) for entry in archive_dir.iterdir()
                if entry.is_dir() and entry.name.isdecimal()), default=0)


def _archive_take(shot_dir: Path, archive_dir: Path, take: int) -> int:
    """Move a whole take atomically before assigning a new seed to its path.

    Archives live outside the active shot directory, so a stop between the
    rename and the next checkpoint cannot mistake old media for the new take.
    """
    take = max(take, _last_archived_take(archive_dir) + 1)
    archive_dir.mkdir(parents=True, exist_ok=True)
    destination = archive_dir / f"{take:03d}"
    _atomic_json(shot_dir / "archive.json", {
        "original_directory": str(shot_dir),
        "directory": str(destination),
        "clip": str(destination / "video.mp4"),
    })
    shot_dir.rename(destination)
    # Reports must open the rejected clip after its old path becomes a new take.
    # The manifest also locates assets referenced by historical runtime paths.
    for filename in ("speech-validation.json", "render.json"):
        try:
            data = json.loads((destination / filename).read_text(encoding="utf-8"))
        except (FileNotFoundError, ValueError):
            continue
        if not isinstance(data, dict):
            continue
        speech = data.get("speech_validation") if filename == "render.json" else data
        if isinstance(speech, dict) and "clip" in speech:
            speech["original_clip"] = speech["clip"]
            speech["clip"] = str(destination / "video.mp4")
            _atomic_json(destination / filename, data)
    shot_dir.mkdir()
    return take


def _matches_saved_take(saved: dict, report: dict, clip: Path,
                        previous_clip: Path | None) -> bool:
    take = saved.get("take", 1)
    if type(take) is not int or take < 1:
        return False
    if (saved.get("base_seed", report["base_seed"]) != report["base_seed"]
            or saved.get("seed") != _take_seed(report["base_seed"], take)
            or any(saved.get(key) != report[key]
                   for key in ("seconds", "beat_ids", "scene_id"))):
        return False
    if saved.get("planned_shot_sha256") is not None:
        # A previously rendered adaptation is authoritative on Resume. A new
        # visual opinion must never invalidate otherwise reusable expensive media.
        if (saved["planned_shot_sha256"] != report.get("planned_shot_sha256")
                or saved.get("planned_prompt") != report.get("planned_prompt")):
            return False
    elif saved.get("prompt") != report["prompt"]:
        return False
    continuity = saved.get("continuity")
    if not isinstance(continuity, dict) or any(
        continuity.get(key) != report["continuity"][key]
        for key in ("conditioning", "source_shot", "source_frame")
    ):
        return False
    if previous_clip is not None:
        recorded_hash = continuity.get("source_clip_sha256")
        if recorded_hash is not None:
            if recorded_hash != report["continuity"]["source_clip_sha256"]:
                return False
        else:
            # Older runs recorded the input frame hash in the renderer runtime.
            # Compare it with today's predecessor instead of trusting paths or
            # an invocation-local "reused" flag across repeated resumes.
            runtime = saved.get("runtime")
            reference = runtime.get("source_image") if isinstance(runtime, dict) else None
            if not isinstance(reference, dict) or not reference.get("source_sha256"):
                return False
            with tempfile.TemporaryDirectory(prefix=".resume-frame-", dir=clip.parent) as temporary:
                frame = Path(temporary) / "frame.png"
                _extract_last_frame(previous_clip, report["continuity"]["source_frame"], frame)
                if _file_sha256(frame) != reference["source_sha256"]:
                    return False
    if saved.get("sha256") is not None and saved["sha256"] != _file_sha256(clip):
        raise ValueError(f"Saved video changed: {clip}. Review it before replacing the take.")
    return True


def _restore_take(source: Path, shot_dir: Path, archive_dir: Path,
                  saved: dict) -> None:
    """Restore an accepted archive without losing it or the current attempt."""
    with tempfile.TemporaryDirectory(prefix=".restore-take-", dir=shot_dir.parent) as temporary:
        staged = Path(temporary) / "take"
        shutil.copytree(source, staged)
        (staged / "archive.json").unlink(missing_ok=True)
        restored = dict(saved, status="verifying", restored_from=str(source), reused=True)
        restored.pop("error", None)
        restored.pop("speech_validation", None)
        restored.pop("saved_take_review", None)
        _atomic_json(staged / "render.json", restored)
        if any(shot_dir.iterdir()):
            current = _saved_report(shot_dir) or {}
            take = current.get("take", 1)
            _archive_take(shot_dir, archive_dir, take if type(take) is int and take > 0 else 1)
        shot_dir.rmdir()
        staged.rename(shot_dir)


def _assemble(clips: list[Path], output: Path, seconds: int, fade_out: bool, *,
              width=None, height=None, crop=False) -> dict:
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
        f"crop={width}:{height}" if crop else f"scale={width}:{height}:flags=lanczos",
        "setsar=1", f"fps={FPS}", "format=yuv420p",
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
    partial.replace(output)
    return info


def render_shooting_script(
    plan: dict,
    seed: int,
    output: Path,
    *,
    on_progress=None,
    on_shot=None,
    on_activity=None,
    on_activity_delta=None,
    token_counter=None,
    render_fn=None,
    resume=False,
) -> dict:
    """Render plan shots in order and publish only a validated assembled video.

    ``render_fn`` follows ``quality_backend.render``. Completed shot files and
    their atomic reports remain available if a later shot or assembly fails.
    With ``resume``, matching current and archived takes are checked and reused
    before any new video inference. Generated dialogue is trusted; no speech
    recognizer or transcript comparison participates in rendering or resume.
    Every saved take remains available. New plans use automated visual evidence
    to adapt only unrendered shots. Neither a quality issue nor unavailable review
    starts another video inference or asks for human approval.
    """
    shots, seconds, fade_out = _validate_plan(plan)
    spec = resolve_format(plan["render_format"]) if "render_format" in plan else None
    width, height = (spec["model_width"], spec["model_height"]) if spec else (WIDTH, HEIGHT)
    if isinstance(seed, bool) or not isinstance(seed, int):
        raise ValueError("seed must be an integer.")
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
    visual_reviews = []
    automatic_review = plan.get("planning_policy") == "single-render-v1"
    execution_plan = dict(plan, shots=[dict(shot) for shot in shots])
    activity = on_activity or (lambda message: None)
    review_backend = None
    if automatic_review:
        try:
            import visual_review as review_backend
        except Exception as exc:
            activity(f"Automatic visual review unavailable: {exc}. Rendering will continue.")

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
            planned_shot = shot
            shot = execution_plan["shots"][index]
            shot_dir = shots_dir / f"{index + 1:03d}"
            shot_dir.mkdir(parents=True, exist_ok=True)
            clip = shot_dir / "video.mp4"
            frames = shot["seconds"] * FPS
            base_seed = (seed + index) % _SEED_MODULUS
            archive_dir = output.parent / "takes" / f"{index + 1:03d}"
            take = _last_archived_take(archive_dir) + 1
            use_previous_frame = _uses_previous_frame(shots, index)
            previous_clip = clips[-1] if use_previous_frame else None
            image_file = shot_dir / "start-frame.png" if use_previous_frame else None
            source_frame = shots[index - 1]["seconds"] * FPS - 1 if use_previous_frame else None
            continuity = {
                "conditioning": "previous-shot-last-frame" if use_previous_frame else "none",
                "source_shot": index if use_previous_frame else None,
                "source_frame": source_frame,
                "source_image": str(image_file) if image_file is not None else None,
                "source_clip_sha256": summaries[-1]["sha256"] if use_previous_frame else None,
            }
            report = {
                "prompt": shot["prompt"],
                "base_seed": base_seed,
                "seed": _take_seed(base_seed, take),
                "take": take,
                "seconds": shot["seconds"],
                "width": width,
                "height": height,
                "status": "running",
                "runtime": None,
                "start_state": shot["start_state"],
                "end_state": shot["end_state"],
                "beat_ids": shot["beat_ids"],
                "scene_id": shot.get("scene_id"),
                "continuity": continuity,
            }
            if automatic_review:
                report.update(planned_prompt=planned_shot["prompt"],
                              planned_shot_sha256=_planned_shot_sha256(plan, planned_shot),
                              prompt_template=shot["prompt_template"])

            reused = False
            saved = _saved_report(shot_dir)
            report_committed = False

            def save_failure(status, error=None):
                # Verification must not relabel old footage with the seed and
                # metadata of a proposed new take if it fails or is stopped.
                checkpoint = report if report_committed else _saved_report(shot_dir)
                failed = dict(checkpoint or report, status=status)
                if error is not None:
                    failed["error"] = str(error)
                if checkpoint is None and clip.is_file():
                    # Do not invent provenance for an unidentifiable clip.
                    if on_shot is not None:
                        on_shot(index, failed)
                else:
                    save_shot(index, failed)

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
                    f"Shot {index + 1}/{total_shots}, take {report['take']}: {data.get('label', 'Rendering')}",
                    remaining,
                )

            try:
                if resume:
                    candidates = [(shot_dir, saved)]
                    if archive_dir.is_dir():
                        candidates.extend((path, _saved_report(path)) for path in sorted(
                            (path for path in archive_dir.iterdir()
                             if path.is_dir() and path.name.isdecimal()),
                            key=lambda path: int(path.name),
                        ))
                    failures = []
                    for candidate_dir, candidate in candidates:
                        candidate_clip = candidate_dir / "video.mp4"
                        if not candidate_clip.is_file():
                            continue
                        if candidate is None:
                            failures.append((candidate_dir, candidate, RuntimeError(
                                "Saved video has no valid render report; its provenance needs review.")))
                            continue
                        try:
                            if not _matches_saved_take(candidate, report, candidate_clip, previous_clip):
                                continue
                            emit_progress(last_fraction,
                                          f"Shot {index + 1}/{total_shots}: Checking saved take {candidate.get('take', 1)}")
                            validate_video(candidate_clip, frames, width, height, expect_audio=True)
                        except (ValueError, KeyError, RuntimeError, subprocess.CalledProcessError) as exc:
                            failures.append((candidate_dir, candidate, exc))
                            continue
                        # A historical speech mismatch does not invalidate a
                        # technically sound take with matching provenance.
                        if candidate_dir != shot_dir:
                            _restore_take(candidate_dir, shot_dir, archive_dir, candidate)
                            report["restored_from"] = str(candidate_dir)
                        report.update(take=candidate.get("take", 1), seed=candidate["seed"],
                                      runtime=candidate.get("runtime"), reused=True)
                        if automatic_review:
                            shot.update(prompt=candidate["prompt"],
                                        prompt_template=candidate.get("prompt_template", shot["prompt_template"]))
                            report.update(prompt=shot["prompt"], prompt_template=shot["prompt_template"])
                            if isinstance(candidate.get("adaptation"), dict):
                                shot["adaptation"] = candidate["adaptation"]
                                report["adaptation"] = candidate["adaptation"]
                        reused = True
                        report_committed = True
                        break
                    if not reused and failures:
                        candidate_dir, candidate, failure = failures[0]
                        message = (f"Shot {index + 1} needs review: {failure} "
                                   f"Saved take: {candidate_dir / 'video.mp4'}. "
                                   "No automatic retake was made. Resume rechecks saved footage.")
                        raise RuntimeError(message) from failure

                if not reused:
                    pending = (resume and saved is not None and not clip.exists()
                               and saved.get("runtime") is None and saved.get("sha256") is None
                               and _matches_saved_take(saved, report, clip, previous_clip))
                    if pending:
                        # An interruption during text planning did not consume a
                        # video take. Keep its seed, frame and adaptation cache.
                        report.update(take=saved.get("take", 1), seed=saved["seed"])
                        if automatic_review:
                            shot.update(prompt=saved["prompt"],
                                        prompt_template=saved.get("prompt_template", shot["prompt_template"]))
                            report.update(prompt=shot["prompt"], prompt_template=shot["prompt_template"])
                            if isinstance(saved.get("adaptation"), dict):
                                shot["adaptation"] = saved["adaptation"]
                                report["adaptation"] = saved["adaptation"]
                    elif any(shot_dir.iterdir()):
                        saved_take = saved.get("take", 1) if saved is not None else 1
                        if type(saved_take) is not int or saved_take < 1:
                            saved_take = 1
                        take = _archive_take(shot_dir, archive_dir, saved_take) + 1
                        report.update(take=take, seed=_take_seed(base_seed, take))
                    report_committed = True
                    save_shot(index, report)
                    if use_previous_frame:
                        _extract_last_frame(previous_clip, source_frame, image_file)
                    if (automatic_review and use_previous_frame and visual_reviews
                            and not (pending and report.get("adaptation", {}).get("status") in ("applied", "unchanged"))):
                        emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Reconciling observed continuity")
                        try:
                            if review_backend is None:
                                raise RuntimeError("Visual review backend unavailable")
                            adapted = review_backend.adapt_shot(
                                execution_plan, index, visual_reviews[-1], shot_dir / "adaptation",
                                token_counter=token_counter, on_activity=activity,
                                on_activity_delta=on_activity_delta,
                            )
                            if (not isinstance(adapted, dict)
                                    or any(not isinstance(adapted.get(key), str) or not adapted[key].strip()
                                           for key in ("prompt", "prompt_template"))):
                                raise ValueError("Continuity adaptation returned no valid directions")
                            shot = adapted
                        except Exception as exc:
                            shot = dict(shot, adaptation={"status": "unavailable", "reason": str(exc)})
                            activity(f"Shot {index + 1}: continuity adaptation unavailable; keeping planned directions.")
                        execution_plan["shots"][index] = shot
                        report.update(prompt=shot["prompt"], prompt_template=shot["prompt_template"])
                        if isinstance(shot.get("adaptation"), dict):
                            report["adaptation"] = shot["adaptation"]
                        save_shot(index, report)
                    shot_started = time.monotonic()
                    report["runtime"] = renderer(
                        shot["prompt"], report["seed"], frames, clip,
                        on_progress=shot_progress,
                        image_file=image_file,
                        hold_image_framing=False,
                        **({"width": width, "height": height} if spec else {}),
                    )
                runtime = report["runtime"]
                report["sha256"] = _file_sha256(clip)
                report["status"] = "verifying"
                save_shot(index, report)
                if not reused:
                    emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Checking video file")
                    validate_video(clip, frames, width, height, expect_audio=True)
                if automatic_review:
                    emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Inspecting rendered action")
                    try:
                        if review_backend is None:
                            raise RuntimeError("Visual review backend unavailable")
                        observed = review_backend.review_shot(
                            execution_plan, index, clip, shot_dir / "visual-review",
                            on_activity=activity, on_activity_delta=on_activity_delta,
                        )
                        if not isinstance(observed, dict) or observed.get("status") not in (
                                "passed", "issues_detected", "unavailable"):
                            raise ValueError("Visual assessment returned no valid report")
                    except Exception as exc:
                        observed = {"status": "unavailable", "summary": f"Visual assessment unavailable: {exc}",
                                    "clip_sha256": report["sha256"], "observed_end_state": "",
                                    "continuity_risk": False, "issues": [], "contract_evidence": []}
                    visual_reviews.append(observed)
                    report["visual_review"] = observed
                    report["observed_end_state"] = observed.get("observed_end_state", "")
                    if observed.get("status") != "passed":
                        activity(f"Shot {index + 1}: {observed.get('summary', 'Visual assessment unavailable.')} "
                                 "Keeping this take under the single-render policy.")
                report.pop("error", None)
                report.update(status="complete")
                clips.append(clip)
                summaries.append({
                    "index": index,
                    "prompt": shot["prompt"],
                    "base_seed": base_seed,
                    "seed": report["seed"],
                    "take": report["take"],
                    "seconds": shot["seconds"],
                    "status": "complete",
                    "runtime": runtime,
                    "continuity": continuity,
                    "sha256": report["sha256"],
                    **({key: report[key] for key in (
                        "planned_prompt", "planned_shot_sha256", "prompt_template", "adaptation",
                        "visual_review", "observed_end_state") if key in report}),
                    **({"reused": True} if reused else {}),
                    **({"restored_from": report["restored_from"]} if "restored_from" in report else {}),
                })
                completed_seconds += shot["seconds"]
                save_shot(index, report)
                emit_progress(
                    min(0.97, 0.97 * completed_seconds / seconds),
                    f"Shot {index + 1}/{total_shots} {'reused' if reused else 'complete'}",
                )
            except KeyboardInterrupt:
                save_failure("interrupted")
                raise
            except Exception as exc:
                save_failure("failed", exc)
                raise

        emit_progress(0.97, "Assembling shots")
        video_info = _assemble(
            clips, output, seconds, fade_out,
            **({"width": spec["width"], "height": spec["height"], "crop": True} if spec else {}),
        )
        scene_review = None
        if automatic_review:
            emit_progress(0.98, "Assessing scene fidelity")
            try:
                if review_backend is None:
                    raise RuntimeError("Visual review backend unavailable")
                scene_review = review_backend.review_scene(
                    execution_plan, visual_reviews, output.parent / "scene-review",
                    on_activity=activity, on_activity_delta=on_activity_delta,
                )
                if not isinstance(scene_review, dict) or scene_review.get("status") not in (
                        "passed", "issues_detected", "unavailable"):
                    raise ValueError("Scene assessment returned no valid report")
            except Exception as exc:
                scene_review = {"status": "unavailable", "summary": f"Scene assessment unavailable: {exc}"}
            activity(f"Scene visual assessment: {scene_review.get('status', 'unavailable')}. "
                     f"{scene_review.get('summary', '')}")
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
        **({"scene_review": scene_review, "render_policy": "single-render-v1"} if automatic_review else {}),
    }
