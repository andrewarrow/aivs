"""Resumable timed keyframes and FFmpeg animatics, independent of movie planning."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import time

import quality_backend
import rust_engine
import storyboard
import storyboard_backend
import visual_review

POLICY = "motion-previsualization-v1"
MAX_FRAME_REPAIRS = 3
TEXT = {"type": "string"}


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def identity():
    # New review code invalidates acceptance without touching the five-pass plan.
    return rust_engine.digest({"policy": POLICY, "implementation": Path(__file__).read_text(),
                              "rust": rust_engine.implementation_sha256("motion_previs.py"),
                              "continuity": storyboard.review_policy_fingerprint(),
                              "image_backend": storyboard_backend.configuration()})


def validate_timing(plan, motion):
    """Rust validates timed states against the source-backed continuity contracts."""
    rust_engine.call("validate_motion_timing", plan=plan, motion=motion,
                     plan_sha256=rust_engine.digest(plan), fps=quality_backend.FPS)
    return motion


def _base(run_dir):
    plan, boards = read(run_dir / "shooting-script.json"), read(run_dir / "storyboards.json")
    if plan.get("planning_stage") != "final":
        raise ValueError("Finish the existing five-pass plan before preparing motion keyframes.")
    storyboard.require_accepted(plan, boards, run_dir)
    return plan, boards


def plan_motion(run_dir, *, on_activity=print):
    """Add timed states to completed coverage; never invoke the five-pass planner."""
    plan, boards = _base(run_dir)
    folder = run_dir / "motion"
    folder.mkdir(exist_ok=True)
    binding = {"plan_sha256": rust_engine.digest(plan), "boards": storyboard.review_binding(plan, boards),
               "implementation": identity()}
    rows = []
    for number, shot in enumerate(plan["shots"], 1):
        checkpoint = folder / f"{number:03d}.plan.json"
        if checkpoint.exists():
            saved = read(checkpoint)
            if saved["binding"] != binding:
                raise ValueError("Saved motion direction is stale; preserve it and use a new run directory.")
            row = saved["direction"]
        else:
            opening = storyboard.resolve_image(boards["shots"][number - 1], run_dir)
            instruction = (
                "Plan motion previsualization for ONLY this already finalized shot. Do not rewrite coverage, prompt, "
                "dialogue, source beats or any of the five planning passes. Treat attached opening pixels as evidence. "
                "Return one frame only for a physically static shot whose opening and ending contracts are identical; "
                "otherwise return three or four distinct timed states. Use frame 0 and final OUTPUT frame seconds*24-1, "
                "plus one or two meaningful intermediate poses. Composition is a static photograph brief; action describes "
                "the preceding interval. Frame zero must use the original storyboard composition and exact opening state, "
                "with no transitions. Last state must equal the exact original ending state. "
                "Preserve the exact state schema and all fact keys, including hidden physical identity and component IDs. "
                "Keep the exact endpoint vocabulary; invent no entities or transformations. Each changed fact in each interval "
                "needs a transition with exact source quote and beat IDs, before/after values and during-shot timing. "
                "Retain cord attachments, empty cradles after lifting, component counts, morphology, owner, support and position. "
                "Track each character's two hands in the separate hands array: role, hand=left/right, "
                "object=exact held component ID or none/unknown/not-visible. This adds hand detail without changing "
                "the approved endpoint state schema. Keep existing holds-left/right relationships consistent if present. "
                "Held components must belong to the holding role. Never revise approved endpoints. "
                "Use strength 1 at zero, .7 intermediate, .9 final as experimental defaults.\n"
                + json.dumps({"shot_number": number, "shot": shot, "world": plan["world"], "source": plan["source"]})
            )
            def validate(value):
                if value.get("shot") != number:
                    raise ValueError("Wrong motion shot number.")
                # Validate the full proposed sequence with untouched fallback intervals.
                provisional = rows + [value] + [{"shot": n, "keyframes": _fallback(s)}
                    for n, s in enumerate(plan["shots"][number:], number + 1)]
                validate_timing(plan, {"version": 1, "policy": POLICY, "plan_sha256": rust_engine.digest(plan), "shots": provisional})
                return value
            row = visual_review._call(instruction, rust_engine.schema("motion.shot"), folder, f"plan-{number:03d}", validate,
                                      images=[opening], on_activity=on_activity)
            storyboard._save(checkpoint, {"binding": binding, "direction": row})
        rows.append(row)
    motion = {"version": 1, "policy": POLICY, "plan_sha256": rust_engine.digest(plan), "shots": rows}
    validate_timing(plan, motion)
    storyboard._save(folder / "plan.json", motion)
    return motion


def _fallback(shot):
    # Used only to validate not-yet-planned coverage; never published as generated keyframes.
    return rust_engine.call("motion_fallback", shot=shot, fps=quality_backend.FPS)


def _review(plan, number, frame, image, references, directory, binding, on_activity):
    checkpoint = directory / "review.json"
    if checkpoint.exists():
        review = read(checkpoint)
        if review.get("binding") != binding:
            raise ValueError("Saved keyframe review belongs to different pixels or contracts.")
        return _validate_review(plan, number, frame, review["result"])
    instruction = (
        "Inspect the attached keyframe first, then preceding accepted pose, opening frame, and locked shot references. "
        "This is a photograph at the specified time, not the opening of the whole shot. "
        "Observe pixels independently against the supplied state. Use exact planned values for semantic matches; "
        "unknown for unobservable facts. Do not certify concealed geometry. Check faces, morphology, costume, "
        "component counts, base/handset separation, empty cradle, cord attachment at both ends, hand grips, ownership, "
        "positions and physical transformations. List each planned prop/component and visible character role in "
        "component_counts: number of visible instances, -1 if unobservable. Unknown critical geometry must be uncertain. "
        "Describe hand-object evidence in hand_findings and independently record observed_hands for each "
        "character's left and right hand, object=held component ID or none/unknown/not-visible. Compare with references and prior pose. "
        "Use continuity_review observed_opening_state for THIS timed state's observations. "
        "Set source_transition_evidence=[]; return pass only if clear and consistent. "
        "For a rejection, give a concrete pixel correction in correction, and choose repair_strategy=targeted-edit "
        "for a local defect or regenerate if framing, composition or anatomy is fundamentally broken. "
        "For a pass use correction=none and repair_strategy=targeted-edit.\n"
        + json.dumps({"shot": number, "frame": frame, "world": plan["world"], "source": plan["source"],
                      "reference_files_in_attachment_order": [str(p) for p in references]})
    )
    result = visual_review._call(instruction, rust_engine.schema("motion.review"), directory, "inspect",
        lambda value: _validate_review(plan, number, frame, value),
        images=[image, *references], on_activity=on_activity)
    storyboard._save(checkpoint, {"binding": binding, "result": result})
    return result


def _validate_review(plan, number, frame, value):
    """Rust recomputes acceptance for fresh and resumed pixel observations."""
    return rust_engine.call("validate_motion_review", plan=plan, number=number,
                            frame=frame, value=value)


def _keyframe_attempt(plan, number, frame, original, request, inputs, folder, run_dir,
                      seed, on_activity, *, rejected=None, review=None, fresh=False):
    """One immutable image/review attempt; completed work survives interruption."""
    folder.mkdir(parents=True, exist_ok=True)
    checkpoint = folder / "frame.json"
    width, height = (original["image"][k] for k in ("width", "height"))
    if checkpoint.exists():
        entry = read(checkpoint)
        if entry["request"] != request:
            raise ValueError("Saved keyframe inputs changed; preserve artifacts and use a new run.")
        image = storyboard.resolve_image(entry, run_dir)
    elif frame["frame"] == 0:
        image = storyboard.resolve_image(original, run_dir)
        entry = dict(frame, image=original["image"], request=request)
        storyboard._save(checkpoint, entry)
    else:
        image, partial = folder / "frame.png", folder / "frame.partial.png"
        for orphan in (partial, image):
            if orphan.exists():
                orphan.rename(orphan.with_name(f"{orphan.stem}.interrupted-{time.time_ns()}.png"))
        prompt_references = request["references"]
        if rejected is not None and not fresh:
            prompt_references = [rejected["image"], *prompt_references]
        prompt = (
            "One cinematic photograph at the specified time. Use the attached locked design/identity references "
            "and opening/preceding frames. Preserve camera, lighting, faces, anatomy, clothing and prop designs. "
            "Prior frames establish appearance, not current pose. Render exactly this physical state, including "
            "empty supports and attached cords. Never duplicate moving components. Each Image number below "
            "identifies its source reference in attachment order.\n" + json.dumps({
                "state": frame["state"], "world": plan["world"], "time_seconds": frame["frame"] / 24,
                "reference_images": [{"image_number": i, **ref} for i, ref in enumerate(prompt_references, 1)]}))
        options = dict(width=width, height=height, composition=frame["composition"], on_activity=on_activity)
        if rejected is not None and not fresh:
            on_activity(f"Editing rejected shot {number} keyframe at {frame['frame'] / 24:.3f}s")
            runtime = storyboard_backend.edit(
                storyboard.resolve_image(rejected, run_dir),
                review["correction"] + "\nFindings: " + json.dumps(review["findings"]) + "\n" + prompt,
                ["correct face and anatomy", "camera and framing", "costume, environment and lighting",
                 "all geometry and contacts already consistent with the timed physical state"],
                inputs, partial, seed, **options)
        else:
            on_activity(f"Generating shot {number} keyframe at {frame['frame'] / 24:.3f}s")
            if review is not None:
                prompt += "\nPrevious defects to avoid: " + review["correction"] + " " + json.dumps(review["findings"])
            runtime = storyboard_backend.generate(prompt, inputs, partial, seed, **options)
        info = quality_backend.prepare_source_image(partial)
        if info["source_sha256"] != runtime["sha256"] or (info["width"], info["height"]) != (width, height):
            raise ValueError("Generated keyframe has unexpected pixels or dimensions.")
        partial.replace(image)
        entry = dict(frame, request=request, runtime=runtime, image={"file": image.relative_to(run_dir).as_posix(),
                     "sha256": info["source_sha256"], "width": width, "height": height})
        storyboard._save(checkpoint, entry)
    for reference in request["references"]:
        storyboard.resolve_image({"image": reference}, run_dir)
    if rejected is not None:
        storyboard.resolve_image(rejected, run_dir)
    binding = {"request": rust_engine.digest(request), "image": entry["image"]}
    result = _review(plan, number, frame, image, inputs, folder, binding, on_activity)
    entry["review"] = {"file": (folder / "review.json").relative_to(run_dir).as_posix(),
                       "sha256": rust_engine.digest(read(folder / "review.json"))}
    return entry, result


def generate(run_dir, *, on_activity=print):
    run_dir = Path(run_dir).resolve()
    plan, boards = _base(run_dir)
    storyboard_backend.check(on_activity=on_activity)
    motion = plan_motion(run_dir, on_activity=on_activity)
    render = read(run_dir / "render.json")
    profiles, seed = render["characters"], render["seed"]
    rows = []
    for number, (shot, direction) in enumerate(zip(plan["shots"], motion["shots"]), 1):
        original = boards["shots"][number - 1]
        width, height = (original["image"][k] for k in ("width", "height"))
        opening = storyboard.resolve_image(original, run_dir)
        frames = []
        for frame in direction["keyframes"]:
            # Select only this timed state's scene, visible cast, body and prop designs.
            timed_shot = dict(shot, continuity=dict(shot["continuity"], opening=frame["state"]),
                              opening_visible_roles=[c["role"] for c in frame["state"]["characters"]
                                                     if c["presence"] == "visible"])
            selected = storyboard._frame_request(plan, timed_shot, profiles, run_dir, None,
                                                  number, seed, width, height)["references"]
            references = [storyboard.resolve_image({"image": ref}, run_dir) for ref in selected]
            descriptions = {path: ref for path, ref in zip(references, selected)}
            folder = run_dir / "motion" / f"{number:03d}" / f"{frame['frame']:05d}"
            prior = storyboard.resolve_image(frames[-1], run_dir) if frames else opening
            # Priority order: preceding accepted pose, opening, then shot-specific canonical references.
            inputs = list(dict.fromkeys([prior, opening, *references]))
            descriptions[opening] = dict(original["image"], purpose="approved opening frame")
            descriptions[prior] = dict(frames[-1]["image"] if frames else original["image"],
                                       purpose="preceding accepted pose")
            request = {"implementation": identity(), "plan_sha256": motion["plan_sha256"], "shot": number,
                       "frame": frame, "references": [{**descriptions[p], "file": p.relative_to(run_dir).as_posix(),
                       "sha256": quality_backend.prepare_source_image(p)["source_sha256"]} for p in inputs]}
            entry, review = _keyframe_attempt(plan, number, frame, original, request, inputs, folder,
                run_dir, seed + number + frame["frame"], on_activity)
            # Opening pixels are already approved and must stay identical to the storyboard.
            for retry in range(1, MAX_FRAME_REPAIRS + 1):
                if review["accepted"] or frame["frame"] == 0:
                    break
                fresh = review["repair_strategy"] == "regenerate" or retry == MAX_FRAME_REPAIRS
                repair_request = dict(request, repair={"attempt": retry,
                    "strategy": "regenerate" if fresh else "targeted-edit", "rejected_image": entry["image"],
                    "review": entry["review"], "correction": review["correction"]})
                entry, review = _keyframe_attempt(plan, number, frame, original, repair_request, inputs,
                    folder / f"repair-{retry}", run_dir, seed + number + frame["frame"] + retry * 7919,
                    on_activity, rejected=entry, review=review, fresh=fresh)
            if not review["accepted"]:
                raise ValueError(f"Keyframe {number}/{frame['frame']} rejected after bounded repair; "
                                 f"all attempts preserved under {folder}")
            frames.append(entry)
        rows.append({"shot": number, "keyframes": frames})
    manifest = dict(motion, shots=rows, implementation=identity(), status="accepted")
    require_accepted(plan, boards, manifest, run_dir)
    storyboard._save(run_dir / "motion" / "keyframes.json", manifest)
    return manifest


def require_accepted(plan, boards, manifest, run_dir):
    storyboard.require_accepted(plan, boards, run_dir)
    validate_timing(plan, manifest)
    if manifest.get("status") != "accepted" or manifest.get("implementation") != identity():
        raise ValueError("Motion keyframes have no current acceptance.")
    for row in manifest["shots"]:
        original = boards["shots"][row["shot"] - 1]
        if row["keyframes"][0]["image"] != original["image"]:
            raise ValueError("Motion opening pixels must retain the accepted original storyboard.")
        if len(row["keyframes"]) > 1:
            opening_size = original["image"]["width"], original["image"]["height"]
            quality_backend.prepare_image_anchors(anchors(row, run_dir),
                plan["shots"][row["shot"] - 1]["seconds"] * 24, *opening_size)
        for frame in row["keyframes"]:
            image = storyboard.resolve_image(frame, run_dir)
            for ref in frame["request"]["references"]:
                storyboard.resolve_image({"image": ref}, run_dir)
            repair = frame["request"].get("repair")
            if repair is not None:
                storyboard.resolve_image({"image": repair["rejected_image"]}, run_dir)
                rejected_review = (run_dir / repair["review"]["file"]).resolve()
                if (not rejected_review.is_relative_to(run_dir.resolve())
                        or rust_engine.digest(read(rejected_review)) != repair["review"]["sha256"]):
                    raise ValueError("Keyframe repair evidence changed.")
            review_path = (run_dir / frame["review"]["file"]).resolve()
            if not review_path.is_relative_to(run_dir.resolve()):
                raise ValueError("Keyframe review escapes run directory.")
            review = read(review_path)
            if (rust_engine.digest(review) != frame["review"]["sha256"]
                    or review["binding"] != {"request": rust_engine.digest(frame["request"]), "image": frame["image"]}
                    or frame["request"]["frame"] != {k: frame[k] for k in rust_engine.schema("motion.frame")["properties"]}
                    or frame["request"]["plan_sha256"] != rust_engine.digest(plan)
                    or not _validate_review(plan, row["shot"], frame, review["result"])["accepted"]):
                raise ValueError("Keyframe acceptance is stale, rejected or contradicts its physical contract.")
            decoded = quality_backend.prepare_source_image(image)
            if (decoded["width"], decoded["height"]) != (frame["image"]["width"], frame["image"]["height"]):
                raise ValueError("Keyframe dimensions changed.")
    return manifest


def anchors(row, run_dir):
    if len(row["keyframes"]) == 1:
        return None
    return [{"file": str(storyboard.resolve_image(f, run_dir)), "sha256": f["image"]["sha256"],
             "frame": f["frame"], "strength": f["strength"]} for f in row["keyframes"]]


def animatic(run_dir, *, on_activity=print):
    plan, boards = _base(run_dir)
    manifest = require_accepted(plan, boards, read(run_dir / "motion" / "keyframes.json"), run_dir)
    folder = run_dir / "motion"
    target, checkpoint = folder / "animatic.mp4", folder / "animatic.json"
    binding = {"keyframes": rust_engine.digest(manifest), "fps": 24, "style": "held-keyframes-v1"}
    if target.exists() and checkpoint.exists():
        saved = read(checkpoint)
        if saved.get("binding") == binding and saved.get("sha256") == _file_hash(target):
            return target
    # Use numbered hard copies instead of concat filenames: safe for arbitrary workspace paths.
    import shutil
    import tempfile
    with tempfile.TemporaryDirectory(prefix="animatic-", dir=folder) as temporary:
        temp = Path(temporary)
        concat = []
        index = 0
        for shot, row in zip(plan["shots"], manifest["shots"]):
            end = shot["seconds"] * 24
            for i, frame in enumerate(row["keyframes"]):
                index += 1
                name = f"{index:05d}.png"
                shutil.copyfile(storyboard.resolve_image(frame, run_dir), temp / name)
                next_frame = row["keyframes"][i + 1]["frame"] if i + 1 < len(row["keyframes"]) else end
                concat += [f"file '{name}'", "option framerate 24", f"duration {(next_frame - frame['frame']) / 24:.12f}"]
        concat += [f"file '{index:05d}.png'", "option framerate 24"]
        (temp / "frames.txt").write_text("\n".join(concat) + "\n", encoding="utf-8")
        partial = temp / "animatic.mp4"
        on_activity("Assembling timed keyframe animatic without LTX weights")
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
            "-f", "concat", "-safe", "0", "-i", str(temp / "frames.txt"), "-r", "24",
            "-frames:v", str(sum(s["seconds"] * 24 for s in plan["shots"])),
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(partial)], check=True)
        partial.replace(target)
    storyboard._save(checkpoint, {"binding": binding, "sha256": _file_hash(target),
                                "audio": "silent", "preview": "held keyframes; no inferred human motion"})
    return target


def _file_hash(path):
    import hashlib
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def compare(run_dir, *, shot_number=1, profile="distilled", on_activity=print):
    """Preserve matched-seed native single/multi-anchor outputs and visual observations."""
    plan, boards = _base(run_dir)
    manifest = require_accepted(plan, boards, read(run_dir / "motion" / "keyframes.json"), run_dir)
    if not 1 <= shot_number <= len(plan["shots"]):
        raise ValueError("Comparison shot number is outside the plan.")
    shot, row = plan["shots"][shot_number - 1], manifest["shots"][shot_number - 1]
    if shot["seconds"] != 5 or len(row["keyframes"]) not in (3, 4):
        raise ValueError("The first comparison requires a five-second shot with three or four keyframes.")
    directory = run_dir / "motion" / "comparison" / f"{shot_number:03d}-{profile}"
    directory.mkdir(parents=True, exist_ok=True)
    seed = (read(run_dir / "render.json")["seed"] + shot_number - 1) % (2**31)
    binding = {"motion": rust_engine.digest(manifest), "shot": shot_number, "seed": seed,
               "config": quality_backend.profile_config(profile), "prompt": shot["prompt"]}
    opening = row["keyframes"][0]["image"]
    for mode in ("single", "multiple"):
        folder = directory / mode
        folder.mkdir(exist_ok=True)
        output, checkpoint = folder / "video.mp4", folder / "render.json"
        if output.exists() and checkpoint.exists():
            saved = read(checkpoint)
            if saved.get("binding") != binding or saved.get("sha256") != _file_hash(output):
                raise ValueError("Saved comparison output differs; preserve it and use another run.")
            continue
        if output.exists():
            output.rename(folder / f"video.interrupted-{time.time_ns()}.mp4")
        require_accepted(plan, boards, manifest, run_dir)
        runtime = quality_backend.render(shot["prompt"], seed, 120, output,
            image_file=storyboard.resolve_image({"image": opening}, run_dir),
            width=opening["width"], height=opening["height"], profile=profile,
            **({"image_anchors": anchors(row, run_dir)} if mode == "multiple" else {}))
        storyboard._save(checkpoint, {"binding": binding, "sha256": _file_hash(output), "runtime": runtime})
    # Existing visual review extracts actual video samples rather than judging conditioning stills.
    native_plan = {k: v for k, v in plan.items() if k != "render_format"}
    for mode in ("single", "multiple"):
        visual_review.review_shot(native_plan, shot_number - 1, directory / mode / "video.mp4",
                                 directory / mode / "review", on_activity=on_activity)
    report = directory / "comparison.json"
    schema = rust_engine.object_schema({"visual_differences": TEXT, "geometry": TEXT,
        "appearance": TEXT, "motion": TEXT, "limitations": TEXT})
    review_binding = dict(binding, outputs={m: _file_hash(directory / m / "video.mp4") for m in ("single", "multiple")})
    if not report.exists():
        # Contact sheets sample both outputs at matching times.
        images = []
        for mode in ("single", "multiple"):
            images.extend(visual_review._sheets(directory / mode / "video.mp4", directory / mode / "samples",
                          5, shot_number)["contact_sheets"])
        differences = visual_review._call(
            "Compare these actual video contact sheets in order: single-anchor then multiple-anchor. "
            "Describe observed differences in telephone geometry, hands, cord, character identity and motion. "
            "Distinguish visible evidence from unsampled motion. No claim that guides guarantee exact motion or continuity. "
            "Treat these as experimental matched-seed outputs, not a general benchmark.", schema, directory, "comparison",
            lambda value: value, images=images, on_activity=on_activity)
        storyboard._save(report, {"binding": review_binding, "observations": differences})
    elif read(report).get("binding") != review_binding:
        raise ValueError("Comparison observations belong to different outputs.")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("plan", "generate", "animatic", "prepare", "compare"))
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("--shot", type=int, default=1)
    parser.add_argument("--profile", choices=("hq", "distilled"), default="distilled")
    args = parser.parse_args()
    from video import project_lock
    with project_lock(".render.lock"):
        directory = args.run_dir.expanduser().resolve()
        if args.command == "plan":
            plan_motion(directory)
        elif args.command == "generate":
            generate(directory)
        elif args.command == "animatic":
            print(animatic(directory))
        elif args.command == "prepare":
            generate(directory)
            print(animatic(directory))
        else:
            print(compare(directory, shot_number=args.shot, profile=args.profile))


if __name__ == "__main__":
    main()
