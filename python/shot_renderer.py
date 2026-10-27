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
import rust_engine
import storyboard as storyboard_assets
from validation import validate_video
from rust_engine import resolve_format

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
    validated = rust_engine.call("validate_render_plan", plan=plan)
    return plan["shots"], validated["seconds"], validated["fade_out"]


def _uses_previous_frame(shots: list[dict], index: int) -> bool:
    return rust_engine.call("uses_previous_frame", shots=shots, index=index)


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
    return storyboard_assets.planned_shot_sha256(plan, shot)


def _take_seed(base_seed: int, take: int) -> int:
    return rust_engine.call("take_seed", base_seed=base_seed, take=take)


def _saved_report(shot_dir: Path) -> dict | None:
    try:
        report = json.loads((shot_dir / "render.json").read_text(encoding="utf-8"))
    except (FileNotFoundError, ValueError):
        return None
    return rust_engine.call("validate_saved_take_report", report=report)


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
    # Rust decides eligibility and requests only the missing byte evidence.
    # In particular, old continuation runs need a decoded predecessor frame;
    # native-picture resume skips the final voice-conversion identity check.
    evidence = dict(saved=saved, report=report, clip_path=str(clip),
                    is_final_clip=clip.name == "video.mp4" and clip.is_file(),
                    has_previous_clip=previous_clip is not None)
    decision = rust_engine.call("match_saved_take", **evidence)
    if decision["status"] == "needs_previous_frame":
        with tempfile.TemporaryDirectory(prefix=".resume-frame-", dir=clip.parent) as temporary:
            frame = Path(temporary) / "frame.png"
            _extract_last_frame(previous_clip, decision["source_frame"], frame)
            evidence["legacy_frame_sha256"] = _file_sha256(frame)
        decision = rust_engine.call("match_saved_take", **evidence)
    if decision["status"] == "needs_clip_hash":
        evidence["clip_sha256"] = _file_sha256(clip)
        decision = rust_engine.call("match_saved_take", **evidence)
    if decision["status"] not in ("matched", "mismatch"):
        raise RuntimeError("The Rust engine returned an incomplete take verification.")
    return decision["status"] == "matched"


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
    character_profiles=None,
    storyboard=None,
    review_enabled=True,
    render_config=None,
    motion_manifest=None,
) -> dict:
    """Render plan shots in order and publish only a validated assembled video.

    ``render_fn`` follows ``quality_backend.render``. Completed shot files and
    their atomic reports remain available if a later shot or assembly fails.
    With ``resume``, matching current and archived takes are checked and reused
    before any new video inference. Generated dialogue is trusted; no speech
    recognizer or transcript comparison participates in rendering or resume.
    Every saved take remains available. Storyboard plans use their accepted
    sequence unchanged; legacy continuation plans may adapt unrendered shots.
    Issues and unavailable reviews remain explicit in the returned production status;
    neither starts another video inference or asks for human approval.
    New GUI runs pass ``character_profiles`` to anchor eligible scene openings
    with saved portraits and convert single-speaker audio to saved voice samples.
    Native picture checkpoints make interrupted conversion resumable separately.
    An optional run-local ``storyboard`` manifest binds one first frame to every
    planned shot, overriding inherited frames and portraits. ``review_enabled``
    can freeze prompts and suppress automatic review for controlled comparisons.
    ``motion_manifest`` adds independently accepted timed keyframes; its complete
    per-shot provenance participates in take matching and native-picture resume.
    """
    shots, seconds, fade_out = _validate_plan(plan)
    spec = resolve_format(plan["render_format"]) if "render_format" in plan else None
    width, height = (spec["model_width"], spec["model_height"]) if spec else (WIDTH, HEIGHT)
    if isinstance(seed, bool) or not isinstance(seed, int):
        raise ValueError("seed must be an integer.")
    if type(review_enabled) is not bool:
        raise ValueError("review_enabled must be a boolean.")
    # Manual comparison frames use the image/provenance contract in prepare().
    # Production declares its stronger policy in the plan or source snapshot;
    # review_enabled=False must never disable that acceptance gate.
    requires_storyboard_review = (plan.get("storyboard_policy") == "per-shot-first-frame-v1"
                                 or plan.get("source", {}).get("storyboard_policy") == "per-shot-first-frame-v1")
    if requires_storyboard_review and storyboard is None:
        raise ValueError("This plan requires its validated first-frame storyboard for every shot.")
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
    automatic_review = review_enabled and plan.get("planning_policy") == "single-render-v1"
    execution_plan = dict(plan, shots=[dict(shot) for shot in shots])
    activity = on_activity or (lambda message: None)
    storyboard_bindings = (storyboard_assets.prepare(plan, storyboard, output.parent)
                           if storyboard is not None else None)
    if requires_storyboard_review:
        storyboard_assets.require_accepted(plan, storyboard, output.parent)
    if motion_manifest is not None:
        import motion_previs
        if storyboard is None:
            raise ValueError("Timed keyframes require the original accepted storyboard sequence.")
        motion_previs.require_accepted(plan, storyboard, motion_manifest, output.parent)
    asset_bindings = None
    if character_profiles is not None:
        import character_conditioning
        asset_bindings = character_conditioning.prepare(
            plan, character_profiles, output.parent, activity,
            **({"storyboard_conditioned": True} if storyboard_bindings is not None else {}),
        )
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
            frame_binding = storyboard_bindings[index] if storyboard_bindings is not None else None
            motion_row = motion_manifest["shots"][index] if motion_manifest is not None else None
            use_previous_frame = frame_binding is None and _uses_previous_frame(shots, index)
            previous_clip = clips[-1] if use_previous_frame else None
            image_file = shot_dir / "start-frame.png" if use_previous_frame else None
            assets = asset_bindings[index] if asset_bindings is not None else None
            portrait = assets["image"] if assets is not None else {}
            voice = assets["voice"] if assets is not None else {}
            convert_voice = "file" in voice
            native_clip = shot_dir / "video.source.mp4" if convert_voice else clip
            if frame_binding is not None:
                image_file = storyboard_assets.resolve_image(frame_binding, output.parent)
            elif not use_previous_frame and "file" in portrait:
                image_file = output.parent / portrait["file"]
            source_frame = shots[index - 1]["seconds"] * FPS - 1 if use_previous_frame else None
            continuity = {
                "conditioning": ("storyboard-first-frame" if frame_binding is not None else
                                 "previous-shot-last-frame" if use_previous_frame else
                                 "saved-character-portrait" if image_file is not None else "none"),
                "source_shot": index if use_previous_frame else None,
                "source_frame": source_frame,
                "source_image": str(image_file) if image_file is not None else None,
                "source_clip_sha256": summaries[-1]["sha256"] if use_previous_frame else None,
                **({"source_image_sha256": frame_binding["image"]["sha256"]}
                   if frame_binding is not None else {}),
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
                **({"character_assets": assets} if assets is not None else {}),
                **({"storyboard": frame_binding, "planned_storyboard": frame_binding,
                    "storyboard_prompt": shot.get("storyboard_prompt")}
                   if frame_binding is not None else {}),
                **({"render_config": render_config} if render_config is not None else {}),
                **({"motion_keyframes": motion_row} if motion_row is not None else {}),
                **({"storyboard_acceptance": storyboard["continuity_acceptance"]} if requires_storyboard_review else {}),
            }
            if automatic_review:
                report.update(planned_prompt=planned_shot["prompt"],
                              planned_shot_sha256=_planned_shot_sha256(plan, planned_shot),
                              prompt_template=shot["prompt_template"],
                              observed_source_clip_sha256=summaries[-1]["sha256"] if summaries else None)

            reused = False
            saved = _saved_report(shot_dir)
            report_committed = False

            def restore_storyboard(candidate):
                nonlocal frame_binding, image_file
                binding = candidate.get("storyboard")
                if binding is None or binding == frame_binding:
                    return
                if requires_storyboard_review:
                    raise ValueError("Saved adapted storyboard has no acceptance for this exact sequence.")
                execution_shot = dict(planned_shot, prompt=candidate["prompt"],
                                      prompt_template=candidate["prompt_template"],
                                      storyboard_prompt=candidate["storyboard_prompt"])
                if binding.get("planned_shot_sha256") != _planned_shot_sha256(plan, execution_shot):
                    raise ValueError("Saved adapted storyboard belongs to different execution directions.")
                image_file = storyboard_assets.resolve_image(binding, output.parent)
                frame_binding = binding
                shot["storyboard_prompt"] = candidate["storyboard_prompt"]
                report.update(storyboard=binding, storyboard_prompt=shot["storyboard_prompt"])
                continuity.update(source_image=str(image_file), source_image_sha256=binding["image"]["sha256"])

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
                            restore_storyboard(candidate)
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
                        for key in ("native_sha256", "voice_conversion"):
                            if key in candidate:
                                report[key] = candidate[key]
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
                    # A completed native picture survives failed/interrupted
                    # conversion. Resume must finish its soundtrack, not infer
                    # the expensive video again or assign it a new seed.
                    reuse_native = (resume and convert_voice and saved is not None
                                    and native_clip.is_file() and saved.get("runtime") is not None
                                    and saved.get("native_sha256") == _file_sha256(native_clip)
                                    and _matches_saved_take(dict(saved, sha256=None), report,
                                                            native_clip, previous_clip))
                    if reuse_native:
                        validate_video(native_clip, frames, width, height, expect_audio=True)
                    if (resume and convert_voice and native_clip.is_file() and saved is not None
                            and saved.get("native_sha256") is not None
                            and saved["native_sha256"] != _file_sha256(native_clip)):
                        raise ValueError(f"Saved native video changed: {native_clip}. Review it before resuming.")
                    pending = reuse_native or (
                        resume and saved is not None and not clip.exists()
                        and saved.get("runtime") is None and saved.get("sha256") is None
                        and _matches_saved_take(saved, report, clip, previous_clip)
                    )
                    if pending:
                        # An interruption during text planning did not consume a
                        # video take. Keep its seed, frame and adaptation cache.
                        report.update(take=saved.get("take", 1), seed=saved["seed"])
                        restore_storyboard(saved)
                        if reuse_native:
                            report.update(runtime=saved["runtime"], native_sha256=saved["native_sha256"])
                        if automatic_review:
                            shot.update(prompt=saved["prompt"],
                                        prompt_template=saved.get("prompt_template", shot["prompt_template"]))
                            report.update(prompt=shot["prompt"], prompt_template=shot["prompt_template"])
                            if isinstance(saved.get("adaptation"), dict):
                                shot["adaptation"] = saved["adaptation"]
                                report["adaptation"] = saved["adaptation"]
                    elif any(shot_dir.iterdir()):
                        if (shot_dir / "cloud-job.json").exists():
                            raise ValueError(
                                "A saved cloud job cannot be matched to this take. "
                                "Its checkpoint is preserved to prevent a duplicate paid submission; "
                                "inspect the candidate and provider job before creating another candidate."
                            )
                        saved_take = saved.get("take", 1) if saved is not None else 1
                        if type(saved_take) is not int or saved_take < 1:
                            saved_take = 1
                        take = _archive_take(shot_dir, archive_dir, saved_take) + 1
                        report.update(take=take, seed=_take_seed(base_seed, take))
                    report_committed = True
                    save_shot(index, report)
                    if use_previous_frame and not reuse_native:
                        _extract_last_frame(previous_clip, source_frame, image_file)
                    if (automatic_review and storyboard_bindings is None and _uses_previous_frame(shots, index) and visual_reviews and not reuse_native
                            and not (pending and report.get("adaptation", {}).get("status") in ("applied", "unchanged"))):
                        emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Reconciling observed continuity")
                        try:
                            if review_backend is None:
                                raise RuntimeError("Visual review backend unavailable")
                            adapted = review_backend.adapt_shot(
                                execution_plan, index, visual_reviews[-1], shot_dir / "adaptation",
                                token_counter=token_counter, on_activity=activity,
                                on_activity_delta=on_activity_delta,
                                storyboard_mode=frame_binding is not None,
                            )
                            if (not isinstance(adapted, dict)
                                    or any(not isinstance(adapted.get(key), str) or not adapted[key].strip()
                                           for key in ("prompt", "prompt_template"))):
                                raise ValueError("Continuity adaptation returned no valid directions")
                            if frame_binding is not None and adapted.get("adaptation", {}).get("status") == "applied":
                                if character_profiles is None:
                                    raise ValueError("Storyboard adaptation needs the saved character profiles.")
                                frame_shot = {key: value for key, value in adapted.items() if key != "adaptation"}
                                frame_plan = dict(execution_plan, shots=list(execution_plan["shots"]))
                                frame_plan["shots"][index] = frame_shot
                                frame_key = hashlib.sha256(json.dumps(
                                    [frame_shot, summaries[-1]["sha256"]], sort_keys=True).encode()).hexdigest()
                                directory = output.parent / "storyboard-adaptations" / f"{index + 1:03d}" / frame_key
                                evidence = visual_reviews[-1].get("evidence", {})
                                prior_frame = evidence.get("full_final_frame", evidence.get("final_frame"))
                                prior = None
                                if prior_frame and Path(prior_frame).is_file():
                                    prior_path = Path(prior_frame).resolve()
                                    prior = {"image": {"file": prior_path.relative_to(output.parent.resolve()).as_posix(),
                                                       "sha256": _file_sha256(prior_path)}}
                                replacement = storyboard_assets.regenerate_shot(
                                    frame_plan, character_profiles, output.parent, seed, index,
                                    reason=adapted["adaptation"]["reason"], directory=directory,
                                    prior_image=prior, on_activity=activity,
                                )
                                replacement_image = storyboard_assets.resolve_image(replacement, output.parent)
                                checked = review_backend.review_adapted_storyboard(
                                    execution_plan, index, adapted, visual_reviews[-1], replacement_image,
                                    directory / "visual-review", on_activity=activity,
                                    on_activity_delta=on_activity_delta,
                                )
                                if not checked["approved"]:
                                    raise ValueError("Regenerated storyboard was rejected: " + checked["source_preservation"])
                                adapted["adaptation"]["storyboard_review"] = checked
                                frame_binding, image_file = replacement, replacement_image
                                report.update(storyboard=frame_binding, storyboard_prompt=adapted["storyboard_prompt"])
                                continuity.update(source_image=str(image_file),
                                                  source_image_sha256=frame_binding["image"]["sha256"])
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
                    if not reuse_native:
                        if frame_binding is not None:
                            image_file = storyboard_assets.resolve_image(frame_binding, output.parent)
                        elif (not use_previous_frame and "file" in portrait
                                and _file_sha256(image_file) != portrait["sha256"]):
                            raise ValueError("Saved character portrait changed before rendering.")
                        if requires_storyboard_review:
                            storyboard_assets.require_accepted(plan, storyboard, output.parent)
                        image_anchors = None
                        if motion_manifest is not None:
                            motion_previs.require_accepted(plan, storyboard, motion_manifest, output.parent)
                            image_anchors = motion_previs.anchors(motion_row, output.parent)
                        report["runtime"] = renderer(
                            shot["prompt"], report["seed"], frames, native_clip,
                            on_progress=shot_progress,
                            image_file=image_file,
                            hold_image_framing=False,
                            **({"image_anchors": image_anchors} if image_anchors is not None else {}),
                            **({"width": width, "height": height} if spec else {}),
                        )
                        if convert_voice:
                            report["native_sha256"] = _file_sha256(native_clip)
                            report["status"] = "converting-voice"
                            save_shot(index, report)
                    if convert_voice:
                        emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Applying saved character voice")
                        report["voice_conversion"] = character_conditioning.apply_voice(
                            native_clip, clip, voice, output.parent, shot["seconds"],
                            width=width, height=height,
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
                shot_status = rust_engine.call(
                    "render_review_status", reviews=[observed] if automatic_review else [])
                report.update(status=shot_status)
                clips.append(clip)
                summaries.append({
                    "index": index,
                    "prompt": shot["prompt"],
                    "base_seed": base_seed,
                    "seed": report["seed"],
                    "take": report["take"],
                    "seconds": shot["seconds"],
                    "status": shot_status,
                    "runtime": runtime,
                    "continuity": continuity,
                    "sha256": report["sha256"],
                    **({key: report[key] for key in ("character_assets", "storyboard", "planned_storyboard",
                                                   "storyboard_prompt", "render_config", "native_sha256", "voice_conversion", "motion_keyframes")
                        if key in report}),
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

    status = rust_engine.call("render_review_status",
                              reviews=visual_reviews + ([scene_review] if scene_review is not None else []))
    return {
        "status": status,
        "backend": "mlx",
        "pipeline": "directed-shots",
        "output_frames": total_frames,
        "elapsed_seconds": round(time.monotonic() - started, 2),
        "shots": summaries,
        "video": video_info,
        **({"render_config": render_config} if render_config is not None else {}),
        **({"image_conditioning": "storyboard-first-frame"} if storyboard_bindings is not None else {}),
        **({"character_conditioning": "saved-character-assets-v1",
            "identity_review": "pending: compare portraits, voices, spoken words, and lip sync"}
           if asset_bindings is not None else {}),
        **({"scene_review": scene_review, "render_policy": "single-render-v1"} if automatic_review else {}),
    }
