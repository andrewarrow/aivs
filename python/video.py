"""Machine-readable entry points for AIVS setup and Hunter menu option 8."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import sys
import time

from quality_backend import ROOT, CACHE, FPS, WIDTH, HEIGHT, doctor, model_path, render
from dialogue_helpers import duration_estimate
import characters
from shooting_script import create_shooting_script, load_shooting_script, parse_request, save_shooting_script
from shot_renderer import render_shooting_script

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


def check():
    check_environment()
    emit("progress", label="Checking Python packages and Apple GPU")
    doctor()
    emit("progress", label="Checking cached model, text encoder, and audio weights")
    model_path()
    emit("ready")


def download():
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
        emit("progress", label="Downloading LTX-2.5 with audio (about 43 GB; cached files are reused)")
        model_path(download=True, progress_class=DownloadProgress)
    check()


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
    from tokenizers import Tokenizer

    tokenizer = Tokenizer.from_file(str(model_path() / "tokenizer.json"))
    return lambda prompt: len(tokenizer.encode(prompt.strip()).ids)


def generate_screenplay(request, seed=None, *, shooting_script=None, plan_only=False):
    """Translate arbitrary human screenplay text and its cast, then film the plan."""
    if not isinstance(request, dict) or request.get("version") != 1:
        raise ValueError("Screenplay requests must use version 1.")
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
            "seed": seed, "fps": FPS, "width": WIDTH, "height": HEIGHT, "audio": "generated",
            "characters": profiles, "shooting_script": "shooting-script.json",
        }

        def save():
            temp = run_dir / "render.json.tmp"
            temp.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            temp.replace(run_dir / "render.json")

        save()
        try:
            started = time.monotonic()
            emit("progress", label="Preparing shooting script with Codex")
            count_tokens = prompt_token_counter()
            if shooting_script is not None:
                plan = load_shooting_script(shooting_script, request, profiles, token_counter=count_tokens)
            else:
                plan = create_shooting_script(request, profiles, run_dir, token_counter=count_tokens,
                                              on_activity=lambda message: emit("activity", message=message),
                                              on_activity_delta=lambda stream_id, delta: emit(
                                                  "activity_delta", id=stream_id, delta=delta))
            save_shooting_script(plan, run_dir)
            report.update(seconds=plan["seconds"],
                          planning_seconds=round(time.monotonic() - started, 3),
                          shots=[dict(shot, status="pending") for shot in plan["shots"]])
            if plan_only:
                report["status"] = "planned"
                save()
                emit("planned", output=str(run_dir / "shooting-script.txt"),
                     shooting_script=str(run_dir / "shooting-script.json"))
                return
            report["status"] = "rendering"
            save()
            emit("progress", label=f"Filming {len(plan['shots'])} shots ({plan['seconds']} seconds)")

            def shot_update(index, shot):
                report["shots"][index].update(shot)
                save()

            report["runtime"] = render_shooting_script(
                plan, seed, output, on_progress=lambda **data: emit("render_progress", **data),
                on_shot=shot_update,
            )
            report.update(status="complete", sha256=hashlib.sha256(output.read_bytes()).hexdigest())
            save()
        except BaseException as exc:
            report.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                          error=str(exc) or type(exc).__name__)
            save()
            raise
        emit("complete", output=str(output))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "download", "render"))
    parser.add_argument("--seed", type=int)
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument("--character", action="append", default=[], metavar="NAME")
    parser.add_argument("--character-text-only", action="store_true")
    parser.add_argument("--character-references-only", action="store_true",
                        help="Resolve @references and --character selections without inferring cast from script names")
    screenplay = parser.add_mutually_exclusive_group()
    screenplay.add_argument("--screenplay-json", type=json.loads,
                            help="Versioned screenplay request containing human script text and cast bindings")
    screenplay.add_argument("--script-file", type=Path, help="Human screenplay text to translate and film")
    parser.add_argument("--cast", action="append", default=[], metavar="ROLE=CHARACTER",
                        help="Bind a role in --script-file to a saved character; repeat for each role")
    parser.add_argument("--plan-only", action="store_true", help="Save the shooting script without running video inference")
    parser.add_argument("--shooting-script", type=Path, help="Reuse a shooting script matching this source and cast")
    args = parser.parse_args()
    try:
        if args.command == "check":
            check()
        elif args.command == "download":
            download()
        else:
            request = args.screenplay_json
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
                generate_screenplay(request, args.seed, shooting_script=args.shooting_script, plan_only=args.plan_only)
            else:
                if args.plan_only or args.shooting_script is not None:
                    raise ValueError("Shooting-script options require --screenplay-json or --script-file.")
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
