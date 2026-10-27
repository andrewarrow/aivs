"""Machine-readable entry points for AIVS setup and Hunter menu option 8."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime
import fcntl
import functools
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import sys
import time

from quality_backend import ROOT, CACHE, FPS, WIDTH, HEIGHT, doctor, model_path, render, profile_config
from dialogue_helpers import duration_estimate
import characters
import character_conditioning
from character_conditioning import POLICY as CHARACTER_CONDITIONING_POLICY
from shooting_script import load_shooting_script, parse_request, STORYBOARD_POLICY
from shot_renderer import render_shooting_script
from rust_engine import resolve_format
import preproduction
import rust_engine
import storyboard_backend

DEFAULT_PROMPT = Path(__file__).with_name("default-prompt.txt").read_text().strip()
_token = ""


def emit(event, **data):
    # This prefix keeps upstream library stdout distinct from our protocol.
    message = "AIVS_EVENT " + json.dumps({"event": event, **data})
    print(message.replace(_token, "[redacted]") if _token else message, flush=True)


@contextmanager
def project_lock(name):
    with (ROOT / name).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError("Another AIVS operation is using this workspace. Wait for it to finish and retry.") from exc
        yield


def check_environment():
    global _token
    from dotenv import dotenv_values
    _token = dotenv_values(ROOT / ".env").get("HF_API") or ""
    if not _token.strip():
        emit("needs_token")
        raise ValueError("Get an access token from Hugging Face, then create .env with HF_API=your_token.")
    for name in ("ffmpeg", "ffprobe"):
        if not shutil.which(name):
            raise RuntimeError(f"{name} is missing. Install FFmpeg and retry setup.")


def check(profile="hq"):
    check_environment()
    emit("progress", label="Checking Python packages and Apple GPU")
    doctor()
    models = "distilled and HQ" if profile == "hq" else "distilled"
    emit("progress", label=f"Checking cached {models} models, text encoder, and audio weights")
    model_path(profile=profile)
    emit("progress", label="Checking local storyboard tools")
    # The render paths verify real image generation after the workspace opens,
    # where the user can sign in and see generation progress.
    storyboard_backend.check_local()
    emit("progress", label="Checking saved-character voice converter and cached voice weights")
    character_conditioning.check_voice_setup()
    emit("ready")


def download(profile="hq"):
    check_environment()
    from tqdm.auto import tqdm
    import time

    class DownloadProgress(tqdm):
        def __init__(self, *args, **kwargs):
            self.last_event = 0
            kwargs.update(mininterval=0.5, leave=False)
            super().__init__(*args, **kwargs)

        def display(self, *args, **kwargs):
            now = time.monotonic()
            if now - self.last_event < 0.5 and self.n != self.total:
                return
            self.last_event = now
            if self.unit == "B":
                label = f"Downloading model files: {self.n / 1e9:.2f} / {(self.total or 0) / 1e9:.2f} GB"
            else:
                label = f"Caching model files: {self.n:g} / {self.total or '?'}"
            emit("progress", label=label,
                 fraction=min(self.n / self.total, 1) if self.total else None)

    with project_lock(".setup.lock"):
        from quality_backend import preflight_download
        emit("progress", label="Checking local storyboard tools")
        storyboard_backend.check_local()
        emit("progress", label="Checking video model download space")
        preflight_download(profile=profile)
        models = "distilled and HQ models" if profile == "hq" else "distilled model"
        size = "about 62 GB; " if profile == "hq" else ""
        emit("progress", label=f"Downloading LTX-2.5 {models} with audio ({size}cached files are reused)")
        model_path(download=True, progress_class=DownloadProgress, profile=profile)
    check(profile)


def generate(seed=None, prompt=DEFAULT_PROMPT, *, character=(), character_text_only=False,
             character_references_only=False):
    check_environment()
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
    character_root = ROOT if (ROOT / "characters").is_dir() else Path(__file__).resolve().parents[1]
    profiles = characters.resolve_characters(prompt, explicit=character, root=character_root,
                                             references_only=character_references_only)
    if not character_text_only and sum(bool(profile.get("scene_image") or profile.get("image")) for profile in profiles) > 1:
        raise ValueError("Multiple saved characters have reference images. Use --character-text-only to use descriptions without image guidance.")
    timing = duration_estimate(prompt)
    seed = secrets.randbelow(2**31) if seed is None else seed
    with project_lock(".render.lock"):
        run_dir = ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_{seed}"
        run_dir.mkdir(parents=True)
        output = run_dir / "video.mp4"
        if profiles:
            profiles = characters.snapshot_characters(profiles, run_dir, root=character_root)
        image = None if character_text_only or not profiles else characters.reference_image(profiles, run_dir)
        hold_image_framing = image is not None and any(profile.get("scene_image") for profile in profiles)
        expanded_prompt = characters.expand_prompt(prompt, profiles, image_conditioned=image is not None)
        report = {
            "status": "rendering", "original_prompt": prompt, "prompt": expanded_prompt, "seed": seed,
            "seconds": timing["seconds"], "fps": FPS, "width": WIDTH, "height": HEIGHT,
            "audio": "generated", "dialogue_timing": timing,
            "characters": profiles, "character_text_only": character_text_only,
            "character_references_only": character_references_only,
        }
        if image is not None:
            image_source = {
                "file": str(image.relative_to(run_dir)),
                "conditioning": "whole-first-and-last-frame" if hold_image_framing else "whole-first-frame",
            }
            if hold_image_framing:
                frames = timing["seconds"] * FPS
                image_source["anchor_frames"] = [0] if frames <= 1 else [0, frames - 1]
            report["image_source"] = image_source
        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n")
            temp.replace(run_dir / "render.json")
        save()
        emit("progress", label=f"Generating {timing['seconds']} seconds of video")
        try:
            options = {"image_file": image, "hold_image_framing": hold_image_framing} if image is not None else {}
            report["runtime"] = render(expanded_prompt, seed, timing["seconds"] * FPS, output,
                                       on_progress=lambda **data: emit("render_progress", **data), **options)
            report.update(status="complete", sha256=hashlib.sha256(output.read_bytes()).hexdigest())
            save()
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            save()
            raise
        emit("complete", output=str(output))


def prompt_token_counter():
    """Use the renderer's cached tokenizer without loading any model weights."""
    from huggingface_hub import hf_hub_download
    from tokenizers import Tokenizer
    from quality_backend import MODEL_REPO, MODEL_REVISION

    path = hf_hub_download(MODEL_REPO, "tokenizer.json", revision=MODEL_REVISION,
                           cache_dir=str(CACHE / "hub"), local_files_only=True, token=False)
    tokenizer = Tokenizer.from_file(path)
    return lambda prompt: len(tokenizer.encode(prompt.strip()).ids)


def generate_screenplay(request, seed=None, *, shooting_script=None, plan_only=False,
                        storyboards_only=False, voice_mode=None,
                        profile=None, planning_budget_seconds=None, fresh_preproduction=False):
    """Translate arbitrary human screenplay text and its cast, then film the plan."""
    if not isinstance(request, dict) or request.get("version") != 1:
        raise ValueError("Screenplay requests must use version 1.")
    if request.get("character_conditioning", CHARACTER_CONDITIONING_POLICY) != CHARACTER_CONDITIONING_POLICY:
        raise ValueError("New screenplay renders require saved-character-assets-v1 conditioning.")
    if plan_only and storyboards_only:
        raise ValueError("Choose either --plan-only or --storyboards-only.")
    voice_mode = voice_mode if voice_mode is not None else request.get("voice_mode", "saved")
    if voice_mode not in ("saved", "native"):
        raise ValueError("Voice mode must be saved or native.")
    profile = profile if profile is not None else request.get("render_profile", "hq")
    config = profile_config(profile)
    budget = planning_budget_seconds if planning_budget_seconds is not None else request.get("planning_budget_seconds", 600)
    if type(budget) is not int or not 60 <= budget <= 86400:
        raise ValueError("Planning budget must be an integer between 60 and 86400 seconds.")
    request = dict(request, character_conditioning=CHARACTER_CONDITIONING_POLICY,
                   storyboard_policy=STORYBOARD_POLICY, voice_mode=voice_mode,
                   render_profile=profile, planning_budget_seconds=budget)
    spec = resolve_format(request["render_format"]) if "render_format" in request else None
    cast = request.get("cast")
    if not isinstance(cast, list) or any(not isinstance(binding, dict) or not isinstance(binding.get("character"), str)
                                         for binding in cast):
        raise ValueError("Screenplay cast must list role and saved character bindings.")
    character_root = ROOT if (ROOT / "characters").is_dir() else Path(__file__).resolve().parents[1]
    profiles = characters.resolve_characters("", explicit=[binding["character"] for binding in cast],
                                             root=character_root, references_only=True)
    parse_request(request, profiles)
    if not plan_only:
        check_environment()
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
    seed = secrets.randbelow(2**31) if seed is None else seed
    with project_lock(".render.lock"):
        run_dir = ROOT / "outputs" / f"{datetime.now():%Y-%m-%d_%H-%M-%S-%f}_{seed}"
        run_dir.mkdir(parents=True)
        output = run_dir / "video.mp4"
        profiles = characters.snapshot_characters(profiles, run_dir, root=character_root) if profiles else []
        (run_dir / "screenplay.txt").write_text(request["script"], encoding="utf-8")
        (run_dir / "screenplay-request.json").write_text(json.dumps(request, indent=2) + "\n", encoding="utf-8")
        report = {
            "status": "planning", "original_script": request["script"], "cast": request["cast"],
            "seed": seed, "fps": FPS,
            "width": spec["width"] if spec else WIDTH, "height": spec["height"] if spec else HEIGHT,
            **({"render_format": spec["name"], "model_width": spec["model_width"],
                "model_height": spec["model_height"]} if spec else {}),
            "audio": "native-generated" if voice_mode == "native" else "native-with-saved-voice-conversion",
            "voice_mode": voice_mode,
            "characters": profiles, "shooting_script": "shooting-script.json",
            "character_conditioning": CHARACTER_CONDITIONING_POLICY,
            "render_profile": profile, "render_config": config,
            "storyboard_policy": STORYBOARD_POLICY, "storyboards": "storyboards.json",
            "planning_budget_seconds": budget,
        }

        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            temp.replace(run_dir / "render.json")

        save()
        try:
            started = time.monotonic()
            character_conditioning.preflight(
                parse_request(request, profiles), profiles, run_dir,
                require_voice=not (plan_only or storyboards_only),
                on_activity=lambda message: emit("activity", message=message),
            )
            if not (plan_only or storyboards_only):
                model_path(profile=profile)
            if not plan_only:
                storyboard_backend.check(on_activity=lambda message: emit("activity", message=message))
            emit("progress", label="Preparing shots and storyboards with Codex")
            count_tokens = prompt_token_counter()
            plan, boards = preproduction.prepare(
                request, profiles, run_dir, seed, token_counter=count_tokens,
                planning_budget_seconds=budget, plan_only=plan_only,
                shooting_script_path=shooting_script, fresh=fresh_preproduction,
                on_activity=lambda message: emit("activity", message=message),
                on_activity_delta=lambda stream_id, delta: emit("activity_delta", id=stream_id, delta=delta),
            )
            report.update(seconds=plan["seconds"],
                          planning_seconds=round(time.monotonic() - started, 3),
                          shots=[dict(shot, status="pending") for shot in plan["shots"]])
            if plan_only:
                report["status"] = "planned"
                save()
                planned_path = run_dir / ("shooting-script.json" if boards is not None else "storyboard-plan.json")
                emit("planned", output=str(planned_path), shooting_script=str(planned_path))
                return
            if storyboards_only:
                report["status"] = "storyboards-ready"
                save()
                emit("storyboards_ready", output=str(run_dir / "storyboards.json"),
                     shooting_script=str(run_dir / "shooting-script.json"))
                return
            report["status"] = "rendering"
            save()
            emit("progress", label=f"Filming {len(plan['shots'])} shots ({plan['seconds']} seconds)")

            def shot_update(index, shot):
                report["shots"][index] = dict(plan["shots"][index], **shot)
                save()

            report["runtime"] = render_shooting_script(
                plan, seed, output, on_progress=lambda **data: emit("render_progress", **data),
                character_profiles=profiles,
                storyboard=boards, render_config=config,
                render_fn=functools.partial(render, profile=profile),
                on_shot=shot_update,
                token_counter=count_tokens,
                on_activity=lambda message: emit("activity", message=message),
                on_activity_delta=lambda stream_id, delta: emit("activity_delta", id=stream_id, delta=delta),
            )
            report.update(status=report["runtime"].get("status", "rendered-needs-review"),
                          sha256=hashlib.sha256(output.read_bytes()).hexdigest())
            save()
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            save()
            raise
        emit("complete", output=str(output), status=report["status"])


def resume_screenplay(directory, *, planning_budget_seconds=None, storyboards_only=False, motion_previs=False):
    """Continue a saved run in place using its original source, cast, and seed."""
    run_dir = Path(directory).expanduser().resolve()
    if not run_dir.is_dir():
        raise ValueError("Select an existing AIVS output directory to resume.")

    def read_object(filename):
        try:
            value = json.loads((run_dir / filename).read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ValueError(f"Cannot resume this directory: {filename} is missing or unreadable.") from exc
        if not isinstance(value, dict):
            raise ValueError(f"Cannot resume this directory: {filename} must contain an object.")
        return value

    with project_lock(".render.lock"):
        report = read_object("render.json")
        request = read_object("screenplay-request.json")
        profiles = report.get("characters")
        seed = report.get("seed")
        if not isinstance(profiles, list) or type(seed) is not int:
            raise ValueError("Cannot resume this directory: the saved cast or seed is missing.")
        spec = resolve_format(request["render_format"]) if "render_format" in request else None
        dimensions = (spec["width"], spec["height"]) if spec else (WIDTH, HEIGHT)
        if ((report.get("fps"), report.get("width"), report.get("height")) != (FPS, *dimensions)
                or report.get("render_format") != request.get("render_format")):
            raise ValueError("Cannot resume a run with different video dimensions or frame rate.")
        # Use the cast snapshot, independent of edits to the current screenplay
        # or gallery. Revalidating a saved plan never calls the Codex planner.
        source = parse_request(request, profiles)
        if report.get("character_conditioning") != request.get("character_conditioning"):
            raise ValueError("Cannot resume a run with a different character conditioning policy.")
        if report.get("voice_mode", "saved") != request.get("voice_mode", "saved"):
            raise ValueError("Cannot change the saved voice mode on Resume; start a new render.")
        profile = report.get("render_profile", "distilled")
        config = profile_config(profile)
        if request.get("render_profile", "distilled") != profile:
            raise ValueError("Cannot resume a run with a different render profile.")
        if report.get("render_config", config) != config:
            raise ValueError("This run uses a different renderer configuration; start a new render.")
        budget = planning_budget_seconds if planning_budget_seconds is not None else request.get("planning_budget_seconds", 600)
        if type(budget) is not int or not 60 <= budget <= 86400:
            raise ValueError("Planning budget must be an integer between 60 and 86400 seconds.")
        uses_storyboards = request.get("storyboard_policy") == STORYBOARD_POLICY
        if storyboards_only and not uses_storyboards:
            raise ValueError("Storyboard-only Resume requires a run with per-shot storyboards.")
        output = run_dir / "video.mp4"
        motion_manifest = None
        saved_motion = report.get("motion_previsualization")
        if motion_previs or saved_motion is not None:
            import motion_previs as previs
            motion_manifest = read_object("motion/keyframes.json")
            previs.require_accepted(read_object("shooting-script.json"), read_object("storyboards.json"),
                                   motion_manifest, run_dir)
            motion_digest = rust_engine.digest(motion_manifest)
            if saved_motion is not None and saved_motion != motion_digest:
                raise ValueError("Saved motion rendering belongs to different keyframes; preserve this run before changing them.")
        if (not storyboards_only and (motion_manifest is None or saved_motion is not None) and report.get("status") == "complete"
                and isinstance(report.get("runtime"), dict)
                and report["runtime"].get("status") == "complete" and output.is_file()
                and report.get("sha256") == hashlib.sha256(output.read_bytes()).hexdigest()):
            emit("complete", output=str(output), status=report["status"])
            return

        check_environment()
        os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", TQDM_DISABLE="1")
        if uses_storyboards and report.get("character_conditioning") == CHARACTER_CONDITIONING_POLICY:
            character_conditioning.preflight(
                source, profiles, run_dir, require_voice=not storyboards_only,
                on_activity=lambda message: emit("activity", message=message),
            )
        plan_file = run_dir / "shooting-script.json"
        count_tokens = prompt_token_counter()
        # Inspecting storyboards must not relabel or discard existing footage.
        if storyboards_only and (output.is_file() or any((run_dir / "shots").glob("*/video*.mp4"))):
            preproduction.prepare(
                request, profiles, run_dir, seed, token_counter=count_tokens,
                planning_budget_seconds=budget,
                on_activity=lambda message: emit("activity", message=message),
                on_activity_delta=lambda stream_id, delta: emit("activity_delta", id=stream_id, delta=delta),
            )
            emit("storyboards_ready", output=str(run_dir / "storyboards.json"),
                 shooting_script=str(plan_file))
            return
        if plan_file.is_file() and not uses_storyboards:
            emit("progress", label="Loading saved shooting script; planning is already complete")
            plan = load_shooting_script(plan_file, request, profiles, token_counter=count_tokens)
        else:
            plan = None

        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            temp.replace(run_dir / "render.json")

        report.pop("error", None)
        report.pop("sha256", None)
        report.pop("runtime", None)
        report["status"] = "planning" if plan is None else "rendering"
        if motion_manifest is not None:
            report["motion_previsualization"] = motion_digest
        report["planning_budget_seconds"] = budget
        if planning_budget_seconds is not None:
            request["planning_budget_seconds"] = budget
            preproduction._save(run_dir / "screenplay-request.json", request)
        save()
        try:
            boards = None
            if uses_storyboards:
                if not storyboards_only:
                    model_path(profile=profile)
                storyboard_backend.check(on_activity=lambda message: emit("activity", message=message))
                emit("progress", label="Resuming shots and storyboards")
                started = time.monotonic()
                plan, boards = preproduction.prepare(
                    request, profiles, run_dir, seed, token_counter=count_tokens,
                    planning_budget_seconds=budget,
                    on_activity=lambda message: emit("activity", message=message),
                    on_activity_delta=lambda stream_id, delta: emit(
                        "activity_delta", id=stream_id, delta=delta),
                )
                report["planning_seconds"] = round(time.monotonic() - started, 3)
            elif plan is None:
                raise ValueError("The old planner has been removed. Start Generate video again to create storyboards for this scene.")
            if storyboards_only:
                report.update(status="storyboards-ready", seconds=plan["seconds"],
                              shots=[dict(shot, status="pending") for shot in plan["shots"]])
                save()
                emit("storyboards_ready", output=str(run_dir / "storyboards.json"),
                     shooting_script=str(plan_file))
                return
            report.update(status="rendering", seconds=plan["seconds"],
                          shots=[dict(shot, status="pending") for shot in plan["shots"]])
            save()
            emit("progress", label=f"Resuming {len(plan['shots'])} shots; reusing saved takes")

            def shot_update(index, shot):
                report["shots"][index] = dict(plan["shots"][index], **shot)
                save()

            report["runtime"] = render_shooting_script(
                plan, seed, output, resume=True,
                character_profiles=(profiles if report.get("character_conditioning") == CHARACTER_CONDITIONING_POLICY
                                    else None),
                storyboard=boards,
                motion_manifest=motion_manifest,
                render_config=config if uses_storyboards else None,
                render_fn=functools.partial(render, profile=profile),
                on_progress=lambda **data: emit("render_progress", **data),
                on_shot=shot_update,
                token_counter=count_tokens,
                on_activity=lambda message: emit("activity", message=message),
                on_activity_delta=lambda stream_id, delta: emit("activity_delta", id=stream_id, delta=delta),
            )
            report.update(status=report["runtime"].get("status", "rendered-needs-review"),
                          sha256=hashlib.sha256(output.read_bytes()).hexdigest())
            save()
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            save()
            raise
        emit("complete", output=str(output), status=report["status"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "download", "render", "resume", "previsualize"))
    parser.add_argument("--motion-previs", action="store_true",
                        help="Resume final rendering with accepted motion/keyframes.json; preserves old takes")
    parser.add_argument("--output-dir", type=Path, help="Existing screenplay output directory to resume in place")
    parser.add_argument("--seed", type=int)
    parser.add_argument("--profile", choices=("hq", "distilled"),
                        help="Screenplay video profile (default: hq); both profiles use storyboards")
    parser.add_argument("--planning-budget-seconds", type=int,
                        help="Total Codex preproduction allowance (default: 600); Resume keeps time already used")
    parser.add_argument("--fresh-preproduction", action="store_true",
                        help="Design new shots and boards instead of reusing the shared preproduction cache")
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument("--character", action="append", default=[], metavar="NAME")
    parser.add_argument("--character-text-only", action="store_true")
    parser.add_argument("--character-references-only", action="store_true",
                        help="Resolve @references and --character selections without inferring cast from script names")
    screenplay = parser.add_mutually_exclusive_group()
    screenplay.add_argument("--screenplay-json", type=json.loads,
                            help="Versioned screenplay request containing human script text and cast bindings")
    screenplay.add_argument("--screenplay-file", type=Path,
                            help="UTF-8 JSON file containing a versioned screenplay request")
    screenplay.add_argument("--script-file", type=Path, help="Human screenplay text to translate and film")
    parser.add_argument("--cast", action="append", default=[], metavar="ROLE=CHARACTER",
                        help="Bind a role in --script-file to a saved character; repeat for each role")
    stop_after = parser.add_mutually_exclusive_group()
    stop_after.add_argument("--plan-only", action="store_true",
                        help="Save director shot briefs without image or video generation; a matching completed cache may be reused")
    stop_after.add_argument("--storyboards-only", action="store_true",
                           help="Finish both planning passes and storyboard images, then stop before video inference")
    parser.add_argument("--voice-mode", choices=("saved", "native"),
                        help="Screenplay voices: saved uses Seed-VC (default); native explicitly retains generated voices")
    parser.add_argument("--shooting-script", type=Path,
                        help="Import a finalized shooting-script.json with its neighboring storyboard plan and images")
    parser.add_argument("--render-format", choices=("landscape", "portrait"),
                        help="Screenplay delivery format: landscape 1280×720 or portrait 1080×1920")
    args = parser.parse_args()
    try:
        if args.motion_previs and (args.command != "resume" or args.storyboards_only):
            raise ValueError("--motion-previs requires resume without --storyboards-only.")
        if args.command == "previsualize":
            if args.output_dir is None:
                raise ValueError("Previsualization requires --output-dir with completed shooting-script.json and storyboards.")
            import motion_previs as previs
            with project_lock(".render.lock"):
                directory = args.output_dir.expanduser().resolve()
                activity = lambda message: emit("activity", message=message)
                previs.generate(directory, on_activity=activity)
                output = previs.animatic(directory, on_activity=activity)
                emit("complete", output=str(output), status="previsualized")
            return 0
        if args.command in ("check", "download") and (args.plan_only or args.storyboards_only or args.voice_mode is not None):
            raise ValueError("Planning stop points and voice mode apply only to screenplay rendering.")
        if args.command == "resume":
            if args.profile is not None or args.fresh_preproduction:
                raise ValueError("Resume uses saved shots and render profile; start a new render to change them.")
            if args.render_format is not None:
                raise ValueError("Resume uses the run's saved render format; omit --render-format.")
            if args.output_dir is None:
                raise ValueError("Resume requires --output-dir pointing to an existing AIVS output directory.")
            if args.voice_mode is not None or args.plan_only:
                raise ValueError("Resume keeps its saved voice mode; use --storyboards-only to stop before video.")
            resume_screenplay(args.output_dir, planning_budget_seconds=args.planning_budget_seconds,
                              storyboards_only=args.storyboards_only, motion_previs=args.motion_previs)
        elif args.output_dir is not None:
            raise ValueError("--output-dir is only used with resume.")
        elif args.command == "check":
            check(args.profile or "hq")
        elif args.command == "download":
            download(args.profile or "hq")
        else:
            request = args.screenplay_json
            if args.screenplay_file is not None:
                request = json.loads(args.screenplay_file.read_text(encoding="utf-8"))
            if args.script_file is not None:
                bindings = []
                for binding in args.cast:
                    role, separator, character = binding.partition("=")
                    if not separator or not role.strip() or not character.strip():
                        raise ValueError("Use --cast ROLE=CHARACTER for each role assignment.")
                    bindings.append({"role": role.strip(), "character": character.strip()})
                request = {"version": 1, "script": args.script_file.read_text(encoding="utf-8"), "cast": bindings}
            elif args.cast:
                raise ValueError("--cast requires --script-file; JSON requests include their own cast.")
            if request is not None:
                if args.render_format is not None:
                    if not isinstance(request, dict):
                        raise ValueError("Screenplay request must be an object.")
                    request = dict(request, render_format=args.render_format)
                elif args.script_file is not None:
                    request["render_format"] = "landscape"
                generate_screenplay(request, args.seed, shooting_script=args.shooting_script, plan_only=args.plan_only,
                                    storyboards_only=args.storyboards_only, voice_mode=args.voice_mode,
                                    profile=args.profile, planning_budget_seconds=args.planning_budget_seconds,
                                    fresh_preproduction=args.fresh_preproduction)
            else:
                if args.profile is not None or args.planning_budget_seconds is not None or args.fresh_preproduction:
                    raise ValueError("Profile and preproduction options require a screenplay request.")
                if args.render_format is not None:
                    raise ValueError("--render-format requires --screenplay-file, --screenplay-json, or --script-file.")
                if args.plan_only or args.storyboards_only or args.voice_mode is not None or args.shooting_script is not None:
                    raise ValueError("Shooting-script options require --screenplay-file, --screenplay-json, or --script-file.")
                generate(args.seed, args.prompt, character=args.character,
                         character_text_only=args.character_text_only,
                         character_references_only=args.character_references_only)
        return 0
    except KeyboardInterrupt:
        emit("error", message="Stopped. Partial output is kept in outputs.")
        return 130
    except Exception as exc:
        emit("error", message=str(exc))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
