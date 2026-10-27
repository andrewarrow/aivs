"""Freeze and render a storyboard-controlled A/B/C production experiment."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import functools
import hashlib
import html
import json
from pathlib import Path
import shutil
import sys

import premium_backend
import quality_backend
import shot_renderer
import storyboard
import rust_engine

DIMENSIONS = (
    "physical_plausibility", "acting", "cinematography", "dramatic_storytelling",
    "editing", "continuity", "audio_performance",
)
ARMS = {
    "A": "Baseline plan / local distilled / inherited frames",
    "B": "Controlled plan / local / storyboard frames",
    "C": "Controlled plan / LTX Pro / storyboard frames",
}
CODE_FILES = (
    "production_comparison.py", "storyboard.py", "shot_renderer.py", "quality_backend.py",
    "premium_backend.py", "rust_engine.py", "validation.py",
    "inference_worker.py",
    "quality_progress.py", "render_estimate.py", "requirements.txt",
)


def _hash(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _fingerprint(value):
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _read(path):
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"Expected a JSON object: {path}")
    return value


def _path(base, value):
    if not isinstance(value, str) or not value.strip():
        raise ValueError("Input paths must be nonempty strings.")
    path = Path(value).expanduser()
    return (path if path.is_absolute() else base / path).resolve()


def _asset_path(directory, relative):
    path = (directory / relative).resolve()
    if not path.is_relative_to(directory.resolve()):
        raise ValueError(f"Snapshot path escapes experiment: {relative}")
    return path


def _plan(path):
    plan = _read(path)
    shot_renderer._validate_plan(plan)
    if plan.get("render_format") != "landscape":
        raise ValueError("Comparisons currently require landscape plans (1280×720 delivery).")
    return plan


def _beats(plan):
    ordered = []
    for shot in plan["shots"]:
        for beat in shot["beat_ids"]:
            if not ordered or ordered[-1] != beat:
                ordered.append(beat)
    return ordered


def init(baseline, output, controlled=None):
    baseline = Path(baseline).expanduser().resolve()
    controlled = Path(controlled).expanduser().resolve() if controlled else baseline
    plan = _plan(controlled)
    _plan(baseline)
    output = Path(output).expanduser().resolve()
    source = baseline.parent / "screenplay.txt"
    request = {
        "version": 1,
        "screenplay": str(source),
        "baseline_plan": str(baseline),
        "controlled_plan": str(controlled),
        "requirements": ["Describe the essential event and emotional change the audience must understand."],
        "candidate_seeds": [83, 1000086, 2000089],
        "local_profile": "hq",
        "premium_model": "ltx-2-5-pro",
        "storyboards": [{
            "shot": index,
            "image": f"frames/{index:03d}.png",
            "description": shot["start_state"] or "Describe the verified starting composition.",
        } for index, shot in enumerate(plan["shots"], 1)],
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("x", encoding="utf-8") as stream:
        json.dump(request, stream, indent=2)
        stream.write("\n")
    print(f"Edit the requirements, controlled plan and storyboard paths in {output}")


def prepare(request_file, run_dir):
    request_file = Path(request_file).expanduser().resolve()
    request = _read(request_file)
    if request.get("version") != 1:
        raise ValueError("Comparison request must have version 1.")
    base = request_file.parent
    source_path = _path(base, request.get("screenplay"))
    screenplay = source_path.read_text(encoding="utf-8")
    if not screenplay.strip():
        raise ValueError("The source screenplay is empty.")
    baseline = _plan(_path(base, request.get("baseline_plan")))
    controlled = _plan(_path(base, request.get("controlled_plan")))
    for plan in (baseline, controlled):
        source = plan.get("source")
        if source is not None:
            if (not isinstance(source, dict) or not isinstance(source.get("script"), str)
                    or source["script"].strip() != screenplay.strip()):
                raise ValueError("A plan's embedded screenplay differs from the requested scene.")
    if baseline.get("source_sha256") != controlled.get("source_sha256"):
        raise ValueError("Baseline and controlled plans must describe the same source.")
    # Hand-authored experiment plans have no cast-bound production identity.
    # Bind both explicitly to the snapshotted screenplay before binding frames.
    if baseline.get("source_sha256") is None:
        source_hash = hashlib.sha256(screenplay.encode("utf-8")).hexdigest()
        baseline = dict(baseline, source_sha256=source_hash)
        controlled = dict(controlled, source_sha256=source_hash)
    if not _beats(baseline) or _beats(baseline) != _beats(controlled):
        raise ValueError("Baseline and controlled plans must preserve the same ordered source beats.")
    if any(shot["seconds"] not in (6, 8, 10) for shot in controlled["shots"]):
        raise ValueError("Controlled shots must be 6, 8 or 10 seconds for the LTX Pro comparison.")
    requirements = request.get("requirements")
    if (not isinstance(requirements, list) or not requirements
            or any(not isinstance(item, str) or not item.strip() for item in requirements)):
        raise ValueError("List the scene's dramatic requirements before preparing the experiment.")
    seeds = request.get("candidate_seeds")
    if (not isinstance(seeds, list) or not seeds or len(seeds) > 20
            or any(type(seed) is not int or not 0 <= seed < 2**31 for seed in seeds)
            or len(set(seeds)) != len(seeds)):
        raise ValueError("candidate_seeds must contain 1–20 distinct integers from 0 to 2147483647.")
    profile = request.get("local_profile", "hq")
    configs = {
        "A": quality_backend.profile_config("distilled"),
        "B": quality_backend.profile_config(profile),
        "C": premium_backend.profile_config(request.get("premium_model", "ltx-2-5-pro")),
    }
    for shot in controlled["shots"]:
        premium_backend.validate_request(
            shot["prompt"], shot["seconds"] * quality_backend.FPS, 1280, 768,
            model=request.get("premium_model", "ltx-2-5-pro"),
        )
    entries = request.get("storyboards")
    if not isinstance(entries, list) or len(entries) != len(controlled["shots"]):
        raise ValueError("Supply one storyboard image for every controlled shot.")
    images = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("Each storyboard must be an object.")
        index = entry.get("shot")
        if type(index) is not int or not 1 <= index <= len(entries) or index in images:
            raise ValueError("Storyboard shot numbers must cover the controlled plan exactly once.")
        if not isinstance(entry.get("description"), str) or not entry["description"].strip():
            raise ValueError(f"Storyboard {index} needs a description of its composition.")
        path = _path(base, entry.get("image"))
        info = quality_backend.prepare_source_image(path)
        if path.suffix.lower() not in (".png", ".jpg", ".jpeg", ".webp"):
            raise ValueError("Storyboard images must be PNG, JPEG or WebP.")
        if ((path.stat().st_size + 2) // 3) * 4 + 32 > 7_000_000:
            raise ValueError(f"Storyboard {index} exceeds the cloud API's 7 MB encoded image limit.")
        images[index] = (path, info, entry["description"])

    run_dir = Path(run_dir).expanduser().resolve()
    run_dir.mkdir(parents=True, exist_ok=False)
    assets = {}

    def save(relative, value):
        path = run_dir / relative
        shot_renderer._atomic_json(path, value)
        assets[relative] = _hash(path)

    (run_dir / "screenplay.txt").write_text(screenplay, encoding="utf-8")
    assets["screenplay.txt"] = _hash(run_dir / "screenplay.txt")
    save("request.json", request)
    save("baseline-plan.json", baseline)
    save("controlled-plan.json", controlled)
    bindings = []
    for index in range(1, len(entries) + 1):
        source, info, description = images[index]
        relative = f"storyboards/{index:03d}{source.suffix.lower()}"
        target = run_dir / relative
        target.parent.mkdir(exist_ok=True)
        shutil.copyfile(source, target)
        digest = _hash(target)
        if digest != info["source_sha256"]:
            raise ValueError(f"Storyboard {index} changed while being copied. Prepare a new directory.")
        assets[relative] = digest
        bindings.append({
            "shot": index,
            "planned_shot_sha256": storyboard.planned_shot_sha256(controlled, controlled["shots"][index - 1]),
            "image": {"file": relative, "sha256": digest},
            "description": description,
        })
    boards = {"version": 1, "shots": bindings}
    storyboard.prepare(controlled, boards, run_dir)
    save("storyboards.json", boards)
    code = {}
    for name in CODE_FILES:
        source = Path(__file__).with_name(name)
        target = run_dir / "source" / name
        target.parent.mkdir(exist_ok=True)
        shutil.copyfile(source, target)
        code[name] = _hash(target)
        assets[f"source/{name}"] = code[name]
    manifest = {
        "version": 1, "created_at": datetime.now(timezone.utc).isoformat(),
        "assets": assets, "code": code, "candidate_seeds": seeds,
        "requirements": requirements, "local_profile": profile,
        "premium_model": request.get("premium_model", "ltx-2-5-pro"),
        "backends": configs, "arms": ARMS,
        "engine_sha256": rust_engine.implementation_sha256("production_comparison.py"),
        "audio_policy": "native generated audio in every arm; no saved voice conversion",
        "review_policy": "frozen prompts; full-scene audiovisual review remains pending",
        "publication": "experiment candidates only; no automatic quality acceptance",
    }
    manifest["experiment_sha256"] = _fingerprint(manifest)
    shot_renderer._atomic_json(run_dir / "experiment.json", manifest)
    report(run_dir)
    print(f"Prepared {run_dir}; {len(seeds)} candidates per arm. No inference was run.")


def _load(run_dir, *, check_code=False):
    manifest = _read(run_dir / "experiment.json")
    content = {key: value for key, value in manifest.items() if key != "experiment_sha256"}
    if manifest.get("version") != 1 or manifest.get("experiment_sha256") != _fingerprint(content):
        raise ValueError("Experiment metadata changed. Prepare a new experiment.")
    for relative, digest in manifest["assets"].items():
        path = _asset_path(run_dir, relative)
        if not path.is_file() or _hash(path) != digest:
            raise ValueError(f"Frozen input changed or is missing: {relative}")
    if check_code:
        if (manifest.get("engine_sha256") is not None
                and manifest["engine_sha256"] != rust_engine.implementation_sha256("production_comparison.py")):
            raise ValueError("Rust production engine changed; prepare a new experiment.")
        for name, digest in manifest["code"].items():
            if _hash(Path(__file__).with_name(name)) != digest:
                raise ValueError(f"Renderer code changed ({name}); prepare a new experiment.")
    return manifest


def _candidate_dir(run_dir, arm, candidate):
    return run_dir / "candidates" / arm / f"{candidate:03d}"


def render(run_dir, arm, candidate):
    # The same lock as the desktop app prevents simultaneous local inference.
    from video import project_lock

    run_dir = Path(run_dir).expanduser().resolve()
    with project_lock(".render.lock"):
        manifest = _load(run_dir, check_code=True)
        if arm not in ARMS or not 1 <= candidate <= len(manifest["candidate_seeds"]):
            raise ValueError("Choose A, B or C and an existing 1-based candidate number.")
        seed = manifest["candidate_seeds"][candidate - 1]
        plan_name = "baseline-plan.json" if arm == "A" else "controlled-plan.json"
        plan = _plan(run_dir / plan_name)
        if arm == "C":
            config = premium_backend.profile_config(manifest["premium_model"])
            renderer = functools.partial(premium_backend.render, model=manifest["premium_model"])
        else:
            profile = "distilled" if arm == "A" else manifest["local_profile"]
            config = quality_backend.profile_config(profile)
            renderer = functools.partial(quality_backend.render, profile=profile)
        if config != manifest["backends"][arm]:
            raise ValueError("Backend configuration changed; prepare a new experiment.")
        directory = _candidate_dir(run_dir, arm, candidate)
        directory.mkdir(parents=True, exist_ok=True)
        report_file = directory / "candidate.json"
        binding = {"experiment_sha256": manifest["experiment_sha256"], "arm": arm,
                   "candidate": candidate, "seed": seed, "backend": config}
        state = _read(report_file) if report_file.exists() else dict(binding, status="pending")
        if any(state.get(key) != value for key, value in binding.items()):
            raise ValueError("Candidate belongs to different inputs; prepare a new experiment.")
        output = directory / "video.mp4"
        if state.get("status") == "complete":
            if not output.is_file() or _hash(output) != state.get("sha256"):
                raise ValueError("Completed candidate video changed or is missing.")
            print(f"Already complete: {output}")
            return
        shot_renderer._atomic_json(directory / "plan.json", plan)
        boards = None
        if arm != "A":
            boards = _read(run_dir / "storyboards.json")
            for entry in boards["shots"]:
                relative = entry["image"]["file"]
                destination = directory / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                if destination.exists():
                    if _hash(destination) != entry["image"]["sha256"]:
                        raise ValueError(f"Candidate storyboard changed: {relative}")
                else:
                    shutil.copyfile(_asset_path(run_dir, relative), destination)
            shot_renderer._atomic_json(directory / "storyboards.json", boards)
        state.update(status="rendering", quality_status="unreviewed")
        state.pop("error", None)
        shot_renderer._atomic_json(report_file, state)

        def progress(**data):
            print(data.get("label", "Rendering"), flush=True)

        try:
            if arm != "C":
                quality_backend.doctor()
                quality_backend.model_path(profile=profile)
            runtime = shot_renderer.render_shooting_script(
                plan, seed, output, render_fn=renderer, resume=True,
                storyboard=boards, review_enabled=False,
                on_progress=progress, on_activity=lambda message: print(message, flush=True),
            )
            # The generic assembler historically labels every backend as MLX.
            runtime["backend"] = "ltx-cloud" if arm == "C" else "mlx"
            state.update(status="complete", sha256=_hash(output), runtime=runtime)
        except BaseException as exc:
            state.update(status="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
                         error=str(exc) or type(exc).__name__)
            shot_renderer._atomic_json(report_file, state)
            try:
                report(run_dir)
            except Exception:
                pass  # Keep the render error authoritative if the report cannot refresh.
            raise
        shot_renderer._atomic_json(report_file, state)
        report(run_dir)
        print(f"Candidate ready for audiovisual review: {output}")


def report(run_dir):
    run_dir = Path(run_dir).expanduser().resolve()
    manifest = _load(run_dir)
    score_file = run_dir / "review.json"
    if score_file.exists():
        review = _read(score_file)
        if review.get("experiment_sha256") != manifest["experiment_sha256"]:
            raise ValueError("Review belongs to a different experiment.")
    else:
        review = {
            "version": 1, "experiment_sha256": manifest["experiment_sha256"],
            "method": "Watch each complete scene at normal speed with sound before rating 1–5.",
            "decision": "pending", "selected_candidate": None,
            "candidates": {f"{arm}{index:03d}": {
                "video_sha256": None, "watched_with_sound": False,
                "scores": dict.fromkeys(DIMENSIONS), "acceptable": None,
                "requirement_evidence": [{"requirement": item, "met": None, "evidence": ""}
                                         for item in manifest["requirements"]],
                "notes": "",
            } for arm in ARMS for index in range(1, len(manifest["candidate_seeds"]) + 1)},
        }
        shot_renderer._atomic_json(score_file, review)
    sections = []
    for arm, label in ARMS.items():
        for index in range(1, len(manifest["candidate_seeds"]) + 1):
            directory = _candidate_dir(run_dir, arm, index)
            state_path = directory / "candidate.json"
            state = _read(state_path) if state_path.is_file() else {"status": "pending"}
            output = directory / "video.mp4"
            status = state.get("status", "pending")
            media = ""
            if status == "complete":
                if (state.get("experiment_sha256") != manifest["experiment_sha256"]
                        or not output.is_file() or _hash(output) != state.get("sha256")):
                    status = "changed or missing media"
                else:
                    relative = output.relative_to(run_dir).as_posix()
                    media = (f'<video controls preload="metadata" aria-label="Candidate {arm}{index:03d}" '
                             f'src="{relative}"></video>'
                             f'<p><a href="{relative}">Open complete scene</a> · '
                             f'SHA-256: <code>{state["sha256"]}</code></p>')
            sections.append(f'<section><h2>{arm}{index:03d}</h2><p>{html.escape(label)} · '
                            f'{html.escape(str(status))}</p>{media}</section>')
    requirements = "".join(f"<li>{html.escape(item)}</li>" for item in manifest["requirements"])
    document = f'''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>AIVS production comparison</title>
<style>body{{font:16px/1.5 system-ui;margin:24px auto;padding:0 16px;max-width:960px;color:#202124}}
h1{{font-size:24px}}h2{{font-size:20px;margin-bottom:4px}}section{{margin:32px 0}}p{{margin:8px 0}}
video{{display:block;width:100%;max-height:540px;background:#111}}code{{overflow-wrap:anywhere;font-size:12px}}
a{{color:#1457a1}}a:focus-visible{{outline:3px solid #1457a1;outline-offset:3px}}</style>
<main><h1>Production comparison</h1>
<p>Watch the complete scenes with sound. Record 1–5 scores and timestamped evidence in
<a href="review.json">review.json</a>; copy each reviewed video's SHA-256 into its scorecard.
All candidates remain unreviewed until assessed. Rejecting every candidate is a valid outcome.</p>
<ul>{requirements}</ul><p>B uses {html.escape(manifest["local_profile"])} inference.
B and C share the same prompts and images. Cloud seeds are unsupported, so C candidates are independent samples.
All arms use generated audio, without saved voice conversion.</p>
<p>A/B changes can include coverage, conditioning and inference. Use a separate distilled B experiment
to isolate visual conditioning from HQ inference. Frozen prompts disable adaptive rewriting in every arm.</p>
{"".join(sections)}</main></html>'''
    (run_dir / "review.html").write_text(document, encoding="utf-8")
    return review


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    start = commands.add_parser("init", help="Write an editable comparison request; no generation")
    start.add_argument("--baseline", type=Path, required=True)
    start.add_argument("--controlled", type=Path)
    start.add_argument("--output", type=Path, required=True)
    freeze = commands.add_parser("prepare", help="Snapshot plans, references, seeds and backends")
    freeze.add_argument("--request", type=Path, required=True)
    freeze.add_argument("--output-dir", type=Path, required=True)
    film = commands.add_parser("render", help="Render or resume one complete candidate scene")
    film.add_argument("run_dir", type=Path)
    film.add_argument("--arm", choices=tuple(ARMS), required=True)
    film.add_argument("--candidate", type=int, required=True)
    review = commands.add_parser("report", help="Refresh the audiovisual review page, preserving scores")
    review.add_argument("run_dir", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "init":
            init(args.baseline, args.output, args.controlled)
        elif args.command == "prepare":
            prepare(args.request, args.output_dir)
        elif args.command == "render":
            render(args.run_dir, args.arm, args.candidate)
        else:
            report(args.run_dir)
            print(Path(args.run_dir).resolve() / "review.html")
        return 0
    except KeyboardInterrupt:
        print("Stopped. Candidate checkpoints are preserved.", file=sys.stderr)
        return 130
    except Exception as exc:
        print(f"Production comparison failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
