"""Resumable timed keyframes and FFmpeg animatics, independent of movie planning."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
import subprocess
import time

import quality_backend
import storyboard
import storyboard_backend
import storyboard_continuity as continuity
import visual_review

POLICY = "motion-previsualization-v1"
TEXT = continuity.TEXT
HANDS_SCHEMA = {"type": "array", "items": continuity.object_schema({
    "role": TEXT, "hand": {"type": "string", "enum": ["left", "right"]}, "object": TEXT,
})}
FRAME_SCHEMA = continuity.object_schema({
    "frame": {"type": "integer"}, "composition": TEXT, "action": TEXT,
    "state": continuity.STATE_SCHEMA,
    "transitions": {"type": "array", "items": continuity.TRANSITION_SCHEMA},
    "strength": {"type": "number"},
    "hands": HANDS_SCHEMA,
})
SHOT_SCHEMA = continuity.object_schema({"shot": {"type": "integer"},
    "keyframes": {"type": "array", "items": FRAME_SCHEMA}})
REVIEW_SCHEMA = continuity.object_schema({
    "continuity_review": continuity.REVIEW_SCHEMA,
    "component_counts": {"type": "array", "items": continuity.object_schema({
        "id": TEXT, "count": {"type": "integer"}})},
    "hand_findings": TEXT,
    "observed_hands": HANDS_SCHEMA,
})


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def identity():
    # New review code invalidates acceptance without touching the five-pass plan.
    return continuity.digest({"policy": POLICY, "implementation": Path(__file__).read_text(),
                              "continuity": continuity.policy_fingerprint(),
                              "image_backend": storyboard_backend.configuration()})


def _inventory(state):
    return ({c["role"] for c in state["characters"]}, {p["id"] for p in state["props"]})


def _physical_state(state, hand_states):
    """Conservative structural checks; arbitrary prose is still visually reviewed."""
    continuity.facts(state)
    roles, props = _inventory(state)
    hands, docked = {}, {}
    by_id = {p["id"]: p for p in state["props"]}
    if (not isinstance(hand_states, list) or len(hand_states) != 2 * len(roles)
            or {(h["role"], h["hand"]) for h in hand_states} != {(r, side) for r in roles for side in ("left", "right")}):
        raise ValueError("Every character needs one left-hand and one right-hand object state.")
    for h in hand_states:
        if h["object"] not in props | {"none", "unknown", "not-visible"}:
            raise ValueError("Each hand holds one tracked component, none, or an explicit unknown.")
        if h["object"] in props:
            if by_id[h["object"]]["owner"] != h["role"]:
                raise ValueError("The hand's object owner contradicts the physical state.")
            hands[h["role"], "holds-" + h["hand"]] = h["object"]
    for p in state["props"]:
        if p["owner"] not in roles | continuity.UNKNOWN | {"none"}:
            raise ValueError("A prop owner must be a tracked character or none/unknown.")
    for r in state["relationships"]:
        subject, relation, obj, value = (r[k] for k in ("subject", "relation", "object", "value"))
        if subject not in roles | props or obj not in roles | props:
            raise ValueError("Motion relationships must reference tracked characters or prop components.")
        if relation in ("holds-left", "holds-right"):
            if subject not in roles or obj not in props or value not in ("yes", "no"):
                raise ValueError("Hand relationships require a role, holds-left/right, component ID and yes/no.")
            if value == "yes":
                key = subject, relation
                if key in hands and hands[key] != obj:
                    raise ValueError("Hand-object relationships contradict the explicit hand state.")
                if next(h["object"] for h in hand_states if (h["role"], "holds-" + h["hand"]) == key) != obj:
                    raise ValueError("A holding relationship must match its hand state.")
                hands[key] = obj
                if by_id[obj]["owner"] != subject or by_id[obj]["support"] != subject + (":left-hand" if relation == "holds-left" else ":right-hand"):
                    raise ValueError("Hand ownership and component support contradict the grip.")
        if relation == "docked-in" and value == "yes":
            if subject not in props or obj not in props or by_id[subject]["support"] != obj:
                raise ValueError("A docked component must be supported by its tracked cradle/base.")
            docked[subject] = obj
    if set(hands.values()) & set(docked):
        raise ValueError("A component cannot be both held and docked.")
    for prop in state["props"]:
        for suffix, relation in ((":left-hand", "holds-left"), (":right-hand", "holds-right")):
            if prop["support"].endswith(suffix):
                role = prop["support"][:-len(suffix)]
                if hands.get((role, relation)) != prop["id"]:
                    raise ValueError("A hand-supported component needs a matching explicit grip.")


def validate_timing(plan, motion):
    """Expand intervals into existing source-backed continuity contracts."""
    if motion.get("version") != 1 or motion.get("policy") != POLICY:
        raise ValueError("Unsupported motion plan.")
    if motion.get("plan_sha256") != continuity.digest(plan):
        raise ValueError("Motion keyframes belong to a different finalized shooting plan.")
    rows = motion.get("shots")
    if not isinstance(rows, list) or [s.get("shot") for s in rows] != list(range(1, len(plan["shots"]) + 1)):
        raise ValueError("Motion plans must cover every shot exactly once in order.")
    expanded = []
    for shot, row in zip(plan["shots"], rows):
        frames = row["keyframes"]
        if not isinstance(frames, list) or len(frames) not in (1, 3, 4):
            raise ValueError("Use one static frame or three/four movement keyframes per shot.")
        times = [f["frame"] for f in frames]
        if (any(type(n) is not int for n in times) or times[0] != 0
                or any(a >= b for a, b in zip(times, times[1:]))
                or (len(frames) > 1 and times[-1] != shot["seconds"] * quality_backend.FPS - 1)):
            raise ValueError("Motion timestamps must be ordered output frames, from zero to the last visible frame.")
        if frames[0]["state"] != shot["continuity"]["opening"] or frames[0]["transitions"]:
            raise ValueError("The opening must retain the approved state with no preceding motion transitions.")
        if frames[-1]["state"] != shot["continuity"]["ending"]:
            raise ValueError("The final keyframe must reach the existing approved ending state.")
        keys = set(continuity.facts(frames[0]["state"]))
        inventory = _inventory(frames[0]["state"])
        authorized_physical = {(t["entity"], t["field"]) for t in shot["continuity"]["transitions"]
                               if t["timing"] == "during-shot"}
        for frame in frames:
            if (set(continuity.facts(frame["state"])) != keys or _inventory(frame["state"]) != inventory):
                raise ValueError("Keyframes must retain every component, character, attribute and relationship, including offscreen ones.")
            _physical_state(frame["state"], frame["hands"])
            for event in frame["transitions"]:
                field = event["field"]
                if (field.startswith(("morphology.", "physical.", "attached-to:", "connected-to:"))
                        and (event["entity"], field) not in authorized_physical):
                    raise ValueError("Motion refinement cannot invent physical transformations or detach canonical connections.")
            for key in ("composition", "action"):
                continuity._text(frame[key])
            strength = frame["strength"]
            if type(strength) not in (int, float) or not 0 < strength <= 1:
                raise ValueError("Keyframe strengths must be in (0, 1].")
        intervals = list(zip(frames, frames[1:])) or [(frames[0], frames[0])]
        for index, (before, after) in enumerate(intervals):
            transitions = copy.deepcopy(after["transitions"]) if len(frames) > 1 else []
            if any(t["timing"] != "during-shot" for t in transitions):
                raise ValueError("Keyframe interval transitions must occur during the shot.")
            if index == 0:
                transitions = [t for t in shot["continuity"]["transitions"] if t["timing"] == "across-cut"] + transitions
            state = before["state"]
            expanded.append(dict(shot, continuity={"opening": state, "ending": after["state"], "transitions": transitions},
                                 opening_visible_roles=[c["role"] for c in state["characters"] if c["presence"] == "visible"))
    continuity.validate_plan(dict(plan, shots=expanded))
    return motion


def _base(run_dir):
    plan, boards = read(run_dir / "shooting-script.json"), read(run_dir / "storyboards.json")
    if plan.get("planning_stage") != "final":
        raise ValueError("Finish the existing five-pass plan before preparing motion keyframes.")
    continuity.require_accepted(plan, boards, run_dir)
    return plan, boards


def plan_motion(run_dir, *, on_activity=print):
    """Add timed states to completed coverage; never invoke the five-pass planner."""
    plan, boards = _base(run_dir)
    folder = run_dir / "motion"
    folder.mkdir(exist_ok=True)
    binding = {"plan_sha256": continuity.digest(plan), "boards": continuity.report_binding(plan, boards),
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
                validate_timing(plan, {"version": 1, "policy": POLICY, "plan_sha256": continuity.digest(plan), "shots": provisional})
                return value
            row = visual_review._call(instruction, SHOT_SCHEMA, folder, f"plan-{number:03d}", validate,
                                      images=[opening], on_activity=on_activity)
            storyboard._save(checkpoint, {"binding": binding, "direction": row})
        rows.append(row)
    motion = {"version": 1, "policy": POLICY, "plan_sha256": continuity.digest(plan), "shots": rows}
    validate_timing(plan, motion)
    storyboard._save(folder / "plan.json", motion)
    return motion


def _fallback(shot):
    # Used only to validate not-yet-planned coverage; never published as generated keyframes.
    opening = {"frame": 0, "composition": shot["storyboard_prompt"], "action": "Opening", "state": shot["continuity"]["opening"], "transitions": [], "strength": 1,
               "hands": _fallback_hands(shot["continuity"]["opening"])}
    if shot["continuity"]["opening"] == shot["continuity"]["ending"]:
        return [opening]
    ending = dict(opening, frame=shot["seconds"] * quality_backend.FPS - 1, state=shot["continuity"]["ending"],
                  transitions=[t for t in shot["continuity"]["transitions"] if t["timing"] == "during-shot"],
                  hands=_fallback_hands(shot["continuity"]["ending"]))
    middle = dict(opening, frame=1)
    return [opening, middle, ending]


def _fallback_hands(state):
    hands = {(c["role"], side): "unknown" for c in state["characters"] for side in ("left", "right")}
    for relation in state["relationships"]:
        if relation["relation"] in ("holds-left", "holds-right") and relation["value"] == "yes":
            hands[relation["subject"], relation["relation"][6:]] = relation["object"]
    for prop in state["props"]:
        for role, side in hands:
            if prop["support"] == f"{role}:{side}-hand":
                hands[role, side] = prop["id"]
    return [{"role": role, "hand": side, "object": obj} for (role, side), obj in hands.items()]


def _review(plan, number, frame, image, references, directory, binding, on_activity):
    checkpoint = directory / "review.json"
    if checkpoint.exists():
        review = read(checkpoint)
        if review.get("binding") != binding:
            raise ValueError("Saved keyframe review belongs to different pixels or contracts.")
        return _validate_review(plan, number, frame, review["result"])
    instruction = (
        "Inspect the attached keyframe first, then locked references and preceding approved frame. "
        "This is a photograph at the specified time, not the opening of the whole shot. "
        "Observe pixels independently against the supplied state. Use exact planned values for semantic matches; "
        "unknown for unobservable facts. Do not certify concealed geometry. Check faces, morphology, costume, "
        "component counts, base/handset separation, empty cradle, cord attachment at both ends, hand grips, ownership, "
        "positions and physical transformations. List each planned prop/component and visible character role in "
        "component_counts: number of visible instances, -1 if unobservable. Unknown critical geometry must be uncertain. "
        "Describe hand-object evidence in hand_findings and independently record observed_hands for each "
        "character's left and right hand, object=held component ID or none/unknown/not-visible. Compare with references and prior pose. "
        "Use continuity_review observed_opening_state for THIS timed state's observations. "
        "Set source_transition_evidence=[]; return pass only if clear and consistent.\n"
        + json.dumps({"shot": number, "frame": frame, "world": plan["world"], "source": plan["source"]})
    )
    result = visual_review._call(instruction, REVIEW_SCHEMA, directory, "inspect",
        lambda value: _validate_review(plan, number, frame, value),
        images=[image, *references], on_activity=on_activity)
    storyboard._save(checkpoint, {"binding": binding, "result": result})
    return result


def _validate_review(plan, number, frame, value):
    continuity._text(value["hand_findings"])
    hands = value["observed_hands"]
    roles, props = _inventory(frame["state"])
    if (len(hands) != 2 * len(roles)
            or {(h["role"], h["hand"]) for h in hands} != {(r, side) for r in roles for side in ("left", "right")}
            or any(h["object"] not in props | {"none", "unknown", "not-visible"} for h in hands)):
        raise ValueError("Pixel review must observe each tracked character's two hand-object states.")
    # Reuse existing pixel/state contradiction checks for a one-frame contract.
    shot = dict(plan["shots"][number - 1], continuity={"opening": frame["state"], "ending": frame["state"], "transitions": []},
                opening_visible_roles=[c["role"] for c in frame["state"]["characters"] if c["presence"] == "visible"])
    checked = continuity.review_shot(dict(plan, shots=[shot]), 1, value)
    visible_roles = set(shot["opening_visible_roles"])
    observed_hands = {(h["role"], h["hand"]): h["object"] for h in hands}
    for hand in frame["hands"]:
        if (hand["role"] in visible_roles and hand["object"] not in continuity.UNKNOWN
                and observed_hands[hand["role"], hand["hand"]] != hand["object"]):
            checked["violations"].append(f"Unverified hand-object state: {hand['role']} {hand['hand']}")
    observations = continuity.facts(value["continuity_review"]["observed_opening_state"])
    expected = continuity.facts(frame["state"])
    visible_ids = {p["id"] for p in frame["state"]["props"] if p["visibility"] == "visible"} | {
        c["role"] for c in frame["state"]["characters"] if c["presence"] == "visible"}
    for key, wanted in expected.items():
        if key[1] == "location" and wanted not in continuity.UNKNOWN:
            if (observations.get(key) != wanted and (key[0].split(":", 1)[-1] in visible_ids
                    or observations.get(key) not in continuity.UNKNOWN | {None})):
                checked["violations"].append(f"Position mismatch: {key}")
        if (key[0].startswith("relationship:") and key[0][13:] in visible_ids
                and key[1].split(":", 1)[-1] in visible_ids and wanted not in continuity.UNKNOWN
                and observations.get(key) != wanted):
            checked["violations"].append(f"Unverified visible relationship: {key}")
    counts = value["component_counts"]
    ids = [row["id"] for row in counts]
    roles, props = _inventory(frame["state"])
    if len(set(ids)) != len(ids) or set(ids) != roles | props or any(type(c["count"]) is not int or c["count"] < -1 for c in counts):
        raise ValueError("Pixel review must count every tracked component and character exactly once.")
    if any(c["count"] > 1 or (c["id"] in visible_ids and c["count"] != 1) for c in counts):
        checked["violations"].append("Missing, uncertain or duplicated visible component/character.")
    # Keep rejected observations as independently resumable evidence too.
    return dict(value, accepted=checked["decision"] == "pass" and not checked["violations"],
                findings=checked["violations"])


def generate(run_dir, *, on_activity=print):
    plan, boards = _base(run_dir)
    motion = plan_motion(run_dir, on_activity=on_activity)
    profiles = read(run_dir / "render.json")["characters"]
    rows = []
    for number, (shot, direction) in enumerate(zip(plan["shots"], motion["shots"]), 1):
        original = boards["shots"][number - 1]
        width, height = (original["image"][k] for k in ("width", "height"))
        opening = storyboard.resolve_image(original, run_dir)
        # Use every locked prop/environment/body image, plus snapshotted cast portraits.
        references = [storyboard.resolve_image(r, run_dir) for r in boards["references"]]
        references += [storyboard.resolve_image({"image": {"file": f"characters/{p['id']}/{p['image']['file']}",
                        "sha256": p["image"]["sha256"]}}, run_dir) for p in profiles if p.get("image")]
        frames = []
        for frame in direction["keyframes"]:
            folder = run_dir / "motion" / f"{number:03d}" / f"{frame['frame']:05d}"
            folder.mkdir(parents=True, exist_ok=True)
            prior = storyboard.resolve_image(frames[-1], run_dir) if frames else opening
            inputs = list(dict.fromkeys([*references, opening, prior]))
            request = {"implementation": identity(), "plan_sha256": motion["plan_sha256"], "shot": number,
                       "frame": frame, "references": [{"file": p.relative_to(run_dir).as_posix(),
                       "sha256": quality_backend.prepare_source_image(p)["source_sha256"]} for p in inputs]}
            checkpoint = folder / "frame.json"
            if checkpoint.exists():
                entry = read(checkpoint)
                if entry["request"] != request:
                    raise ValueError("Saved keyframe inputs changed; preserve artifacts and use a new run.")
                image = storyboard.resolve_image(entry, run_dir)
            elif frame["frame"] == 0:
                image = opening
                entry = dict(frame, image=original["image"], request=request)
                storyboard._save(checkpoint, entry)
            else:
                image = folder / "frame.png"
                partial = folder / "frame.partial.png"
                for orphan in (partial, image):
                    if orphan.exists():
                        orphan.rename(orphan.with_name(f"{orphan.stem}.interrupted-{time.time_ns()}.png"))
                on_activity(f"Generating shot {number} keyframe at {frame['frame'] / 24:.3f}s")
                runtime = storyboard_backend.generate(
                    "One cinematic photograph at the specified time. Use the attached locked design/identity references "
                    "and opening/preceding frames. Preserve camera, lighting, faces, anatomy, clothing and prop designs. "
                    "Prior frames establish appearance, not current pose. Render exactly this physical state, including "
                    "empty supports and attached cords. Never duplicate moving components.\n" + json.dumps({
                        "state": frame["state"], "world": plan["world"], "time_seconds": frame["frame"] / 24,
                        "reference_files_in_order": request["references"]}), inputs, partial,
                    read(run_dir / "render.json")["seed"] + number + frame["frame"],
                    width=width, height=height, composition=frame["composition"], on_activity=on_activity)
                info = quality_backend.prepare_source_image(partial)
                if info["source_sha256"] != runtime["sha256"] or (info["width"], info["height"]) != (width, height):
                    raise ValueError("Generated keyframe has unexpected pixels or dimensions.")
                partial.replace(image)
                entry = dict(frame, request=request, runtime=runtime, image={"file": image.relative_to(run_dir).as_posix(),
                             "sha256": info["source_sha256"], "width": width, "height": height})
                storyboard._save(checkpoint, entry)
            for reference in request["references"]:
                storyboard.resolve_image({"image": reference}, run_dir)
            review_binding = {"request": continuity.digest(request), "image": entry["image"]}
            review = _review(plan, number, frame, image, inputs, folder, review_binding, on_activity)
            if not review["accepted"]:
                raise ValueError(f"Keyframe {number}/{frame['frame']} rejected; see {folder / 'review.json'}")
            entry["review"] = {"file": (folder / "review.json").relative_to(run_dir).as_posix(),
                               "sha256": continuity.digest(read(folder / "review.json"))}
            frames.append(entry)
        rows.append({"shot": number, "keyframes": frames})
    manifest = dict(motion, shots=rows, implementation=identity(), status="accepted")
    require_accepted(plan, boards, manifest, run_dir)
    storyboard._save(run_dir / "motion" / "keyframes.json", manifest)
    return manifest


def require_accepted(plan, boards, manifest, run_dir):
    continuity.require_accepted(plan, boards, run_dir)
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
            review_path = (run_dir / frame["review"]["file"]).resolve()
            if not review_path.is_relative_to(run_dir.resolve()):
                raise ValueError("Keyframe review escapes run directory.")
            review = read(review_path)
            if (continuity.digest(review) != frame["review"]["sha256"]
                    or review["binding"] != {"request": continuity.digest(frame["request"]), "image": frame["image"]}
                    or frame["request"]["frame"] != {k: frame[k] for k in FRAME_SCHEMA["properties"]}
                    or frame["request"]["plan_sha256"] != continuity.digest(plan)
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
    binding = {"keyframes": continuity.digest(manifest), "fps": 24, "style": "held-keyframes-v1"}
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
    binding = {"motion": continuity.digest(manifest), "shot": shot_number, "seed": seed,
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
    schema = continuity.object_schema({"visual_differences": TEXT, "geometry": TEXT,
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
