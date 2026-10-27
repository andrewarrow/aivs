"""Render and assemble a plan of connected, independently directed shots."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile
import time

import quality_backend
import rust_engine
import storyboard as storyboard_assets
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
    rust_engine.call("extract_last_frame", clip=str(Path(clip).resolve()),
                     frame_index=frame_index, image=str(Path(image).resolve()))


def _file_sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _planned_shot_sha256(plan: dict, shot: dict) -> str:
    """Bind execution changes to the approved source, coverage and timing."""
    return storyboard_assets.planned_shot_sha256(plan, shot)


def _assemble(clips: list[Path], output: Path, seconds: int, fade_out: bool, *,
              width=None, height=None, crop=False) -> dict:
    width = WIDTH if width is None else width
    height = HEIGHT if height is None else height
    return rust_engine.call(
        "take_assemble", clip_count=len(clips), output=str(output.resolve()),
        seconds=seconds, fade_out=fade_out, width=width, height=height, crop=crop,
    )


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
        rust_engine.call("take_save", shot_dir=str(shot_report.parent.resolve()), report=report)
        if on_shot is not None:
            on_shot(index, report)

    try:
        for index, shot in enumerate(shots):
            planned_shot = shot
            shot = execution_plan["shots"][index]
            frame_binding = storyboard_bindings[index] if storyboard_bindings is not None else None
            motion_row = motion_manifest["shots"][index] if motion_manifest is not None else None
            assets = asset_bindings[index] if asset_bindings is not None else None
            portrait = assets["image"] if assets is not None else {}
            voice = assets["voice"] if assets is not None else {}
            frames = shot["seconds"] * FPS
            base_seed = (seed + index) % _SEED_MODULUS
            prepared = rust_engine.call(
                "take_prepare", plan=plan, shot=shot, index=index, base_seed=base_seed,
                run_dir=str(output.parent.resolve()), width=width, height=height,
                frame_binding=frame_binding, motion_row=motion_row, assets=assets,
                previous_summary=summaries[-1] if summaries else None,
                render_config=render_config, automatic_review=automatic_review,
                planned_shot_sha256=_planned_shot_sha256(plan, planned_shot) if automatic_review else None,
                storyboard_acceptance=storyboard["continuity_acceptance"] if requires_storyboard_review else None,
            )
            shot_dir, archive_dir, clip, native_clip = (
                Path(prepared[key]) for key in ("shot_dir", "archive_dir", "clip", "native_clip"))
            convert_voice = prepared["convert_voice"]
            use_previous_frame = prepared["use_previous_frame"]
            previous_clip = clips[-1] if use_previous_frame else None
            source_frame = prepared["source_frame"]
            report = prepared["report"]
            continuity = report["continuity"]
            image_file = Path(prepared["image_file"]) if prepared["image_file"] is not None else None
            report_committed = False

            def save_failure(status, error=None):
                failed = rust_engine.call(
                    "take_failure", shot_dir=str(shot_dir), report=report,
                    committed=report_committed, status=status,
                    error=str(error) if error is not None else None)
                if on_shot is not None:
                    on_shot(index, failed)

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
                # Keep the legacy JSON codec at this boundary. Rust owns candidate
                # selection, byte checks, restoration, and allocation of new takes.
                fingerprints = []
                if resume:
                    emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Checking saved takes")
                    for candidate in rust_engine.call(
                            "take_candidates", shot_dir=str(shot_dir), archive_dir=str(archive_dir)):
                        saved = candidate["report"]
                        if saved is not None and saved.get("storyboard") not in (None, frame_binding):
                            execution_shot = dict(planned_shot, **{
                                key: saved.get(key) for key in ("prompt", "prompt_template", "storyboard_prompt")})
                            fingerprints.append({"report": saved,
                                "fingerprint": _planned_shot_sha256(plan, execution_shot)})
                recovered = rust_engine.call(
                    "take_recover", run_dir=str(output.parent.resolve()), index=index,
                    shot_dir=str(shot_dir), archive_dir=str(archive_dir), native_clip=str(native_clip),
                    report=report, shot=shot, resume=resume, convert_voice=convert_voice,
                    automatic_review=automatic_review, adapted_fingerprints=fingerprints,
                    previous_clip=str(previous_clip) if previous_clip is not None else None)
                report, shot = recovered["report"], recovered["shot"]
                execution_plan["shots"][index] = shot
                continuity = report["continuity"]
                frame_binding = report.get("storyboard")
                image_file = Path(continuity["source_image"]) if continuity["source_image"] is not None else None
                reused, reuse_native, pending = (recovered[key] for key in ("reused", "reuse_native", "pending"))
                report_committed = True
                if on_shot is not None:
                    on_shot(index, report)
                if not reused:
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
                if not reused:
                    emit_progress(last_fraction, f"Shot {index + 1}/{total_shots}: Checking video file")
                # Verification writes its hash before decoding. On failure, use
                # that Rust checkpoint rather than this older in-memory report.
                report_committed = False
                report = rust_engine.call("take_complete", shot_dir=str(shot_dir), report=report, reused=reused)
                report_committed = True
                if on_shot is not None:
                    on_shot(index, report)
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
