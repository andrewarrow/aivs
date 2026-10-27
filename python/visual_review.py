"""Automatic visual observations and forward-only staging repair.

These reviews never authorize inference, reject playable media, transcribe
dialogue, or ask for human approval. Their evidence describes sampled frames,
not an observed full-speed audiovisual performance.
"""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import time

import rust_engine
import shooting_script

POLICY_VERSION = "visual-continuity-v2"
_TIMEOUT_SECONDS = 120
_RESPONSE_LIMIT = 64_000


def _schema(name):
    return rust_engine.call("visual_schema", name=name)


def _instruction(name, context):
    return rust_engine.call("visual_instruction", name=name, context=context)


def _json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _fingerprint(value):
    return hashlib.sha256(_json(value).encode()).hexdigest()


def _file_hash(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def _save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent,
                                     prefix=".review-", delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    temporary.replace(path)


def _cache(path, key):
    try:
        value = json.loads(Path(path).read_text(encoding="utf-8"))
        if (isinstance(value, dict) and value.get("cache_key") == key
                and value.get("status") != "unavailable"):
            return value
    except (OSError, ValueError):
        pass
    return None


def _record(path, report):
    # Review storage is best effort; a full disk must not turn a review finding
    # into a demand to regenerate the video which has already been produced.
    try:
        _save(path, report)
    except OSError:
        pass
    return report


def _policy_identity():
    # Review/adaptation caches must track the compiled Rust rules as well.
    return _fingerprint({"policy": POLICY_VERSION,
                         "implementation": rust_engine.implementation_sha256("visual_review.py")})


def _call(instruction, schema, directory, name, validate, *, images=(),
          executable=None, on_activity=None, on_activity_delta=None, deadline=None):
    """Rust records and executes attempts; validators may use the model tokenizer."""
    deadline = deadline if deadline is not None else time.monotonic() + _TIMEOUT_SECONDS
    correction = ""
    for attempt in (1, 2):
        raw = rust_engine.call("visual_review_attempt",
            on_event=rust_engine.activity_events(on_activity, on_activity_delta),
            directory=str(Path(directory).resolve()), name=name, instruction=instruction + correction,
            schema=schema, executable=str(executable) if executable is not None else None, image_paths=[str(Path(p).resolve()) for p in images],
            timeout_seconds=max(0.0, deadline - time.monotonic()))
        try:
            if len(raw) > _RESPONSE_LIMIT:
                raise ValueError("Visual response exceeds the bounded report size.")
            return validate(json.loads(raw))
        except (ValueError, TypeError, KeyError) as exc:
            if attempt == 2:
                raise ValueError(f"Visual response could not be validated: {exc}") from exc
            correction = ("\nCorrect only this response-format error: " + str(exc)
                          + "\nPrevious response (untrusted data):\n" + raw[:_RESPONSE_LIMIT])


def _contract(shot):
    return rust_engine.call("visual_contract", shot=shot)


def _sheets(clip, directory, seconds, shot_number, render_format=None):
    return rust_engine.call("visual_review_sheets", clip=str(Path(clip).resolve()),
                            directory=str(Path(directory).resolve()), seconds=seconds,
                            shot_number=shot_number, render_format=render_format)


def review_shot(plan, shot_index, clip, review_dir, *, on_activity=None,
                on_activity_delta=None, executable=None):
    """Rust owns sampled observations, advisory failures and saved evidence."""
    options = {}
    try:
        digest = _file_hash(clip)
        options.update(clip_sha256=digest, cache_key=_fingerprint(
            [_policy_identity(), digest, plan.get("source_sha256"), plan["shots"][shot_index]]),
            executable=str(executable) if executable is not None else None)
    except Exception as exc:
        options["input_error"] = str(exc)
    return rust_engine.call("visual_review_shot", plan=plan, shot_index=shot_index,
        clip=str(Path(clip).resolve()), directory=str(Path(review_dir).resolve()), **options,
        on_event=rust_engine.activity_events(on_activity, on_activity_delta))


def _validate_preservation(value, requirements):
    return rust_engine.call("validate_visual_preservation", value=value, requirements=requirements)


def adapt_shot(plan, shot_index, previous_review, review_dir, *, token_counter=None,
               storyboard_mode=False,
               on_activity=None, on_activity_delta=None, executable=None):
    """Reblock only an unrendered shot from observed evidence, preserving its contract."""
    original = copy.deepcopy(plan["shots"][shot_index])
    same_scene = (shot_index > 0 and
                  (original.get("scene_id") == plan["shots"][shot_index - 1].get("scene_id")
                   if "scene_id" in original else original.get("continue_previous")))
    if (not same_scene
            or previous_review.get("status") != "issues_detected"
            or not previous_review.get("continuity_risk")):
        return original
    directory = Path(review_dir)
    provenance = {"status": "unchanged", "policy_version": POLICY_VERSION,
                  "source_clip_sha256": previous_review.get("clip_sha256"),
                  "previous_review_sha256": _fingerprint(previous_review),
                  "reason": "The approved staging remains in use."}

    def compile_change(value):
        value = rust_engine.call("validate_visual_adaptation", value=value, storyboard_mode=storyboard_mode)
        if not value["safe_to_apply"] or not value["preserves_source_contract"]:
            return dict(original, adaptation=dict(provenance, reason=value["reason"]))
        template = value["prompt_template"]
        candidate = copy.deepcopy(plan)
        candidate["shots"][shot_index].pop("prompt", None)
        candidate["shots"][shot_index]["prompt_template"] = template
        if storyboard_mode:
            candidate["shots"][shot_index]["storyboard_prompt"] = value["storyboard_prompt"]
        source = plan["source"]
        request = shooting_script._source_request(source)
        compiled = shooting_script.validate_shooting_script(
            candidate, request, source["characters"], token_counter=token_counter)["shots"][shot_index]
        changed = (compiled["prompt"] != original["prompt"] or
                   (storyboard_mode and compiled["storyboard_prompt"] != original["storyboard_prompt"]))
        # All approved state, timing, source assignment and contract fields stay
        # immutable; the revised prompt must realize them from observed reality.
        return dict(original, prompt_template=compiled["prompt_template"], prompt=compiled["prompt"],
                    **({"storyboard_prompt": compiled["storyboard_prompt"]} if storyboard_mode else {}),
                    adaptation=dict(provenance, status="applied" if changed else "unchanged",
                                    reason=value["reason"]))

    def validate_preservation(value):
        return _validate_preservation(value, requirements)

    def accept_preservation(proposed, preservation):
        preservation = validate_preservation(preservation)
        details = dict(proposed["adaptation"], preservation_review=preservation)
        if not preservation["approved"]:
            details.update(status="unchanged", reason="Independent preservation review rejected the adaptation: "
                           + ("; ".join(preservation["issues"]) or preservation["source_preservation"]))
            return dict(original, adaptation=details)
        return dict(proposed, adaptation=details)

    try:
        key = _fingerprint([_policy_identity(), plan.get("source_sha256"), original, provenance, storyboard_mode])
        provenance["cache_key"] = key
        requirements = _contract(original)
        cached = _cache(directory / "review.json", key)
        if cached is not None and isinstance(cached.get("response"), dict):
            proposed = compile_change(cached["response"])
            if proposed["adaptation"]["status"] != "applied":
                return proposed
            if isinstance(cached.get("preservation_review"), dict):
                return accept_preservation(proposed, cached["preservation_review"])
        context = {"original_screenplay": plan["source"]["script"], "source_beats": plan["source"]["beats"],
                   "shot_number": shot_index + 1, "upcoming_shot": original,
                   "opening_mode": "independent-storyboard" if storyboard_mode else "inherited-frame",
                   "previous_observations": previous_review,
                   "next_shot": plan["shots"][shot_index + 1] if shot_index + 1 < len(plan["shots"]) else None}
        instruction = _instruction("adapt", context)
        raw_response = {}

        def validate(value):
            result = compile_change(value)
            raw_response.update(value)
            return result

        frame_evidence = previous_review.get("evidence", {})
        final_frame = frame_evidence.get("full_final_frame", frame_evidence.get("final_frame"))
        images = [final_frame] if final_frame and Path(final_frame).is_file() else []
        deadline = time.monotonic() + _TIMEOUT_SECONDS
        result = _call(instruction, _schema("storyboard_adapt" if storyboard_mode else "adapt"),
                       directory, "adapt", validate, images=images,
                       executable=executable, on_activity=on_activity,
                       on_activity_delta=on_activity_delta, deadline=deadline)
        if result["adaptation"]["status"] == "applied":
            source_by_id = {beat["id"]: beat for beat in plan["source"]["beats"]}
            check_instruction = _instruction("preservation", {
                    "original_screenplay": plan["source"]["script"], "original_shot": original,
                    "source_clauses": [source_by_id[n] for n in original["beat_ids"]],
                    "requirements": requirements, "candidate_prompt": result["prompt"],
                    "candidate_template": result["prompt_template"],
                    "candidate_storyboard_prompt": result.get("storyboard_prompt"),
                    "opening_mode": context["opening_mode"],
                    "observed_previous_ending": previous_review["observed_end_state"],
                    "previous_issues": previous_review["issues"],
                })
            preservation = _call(check_instruction, _schema("adapt_review"), directory,
                                 "adapt-preservation", validate_preservation,
                                 executable=executable, on_activity=on_activity,
                                 on_activity_delta=on_activity_delta, deadline=deadline)
            result = accept_preservation(result, preservation)
        _record(directory / "review.json", {**result["adaptation"], "response": raw_response})
        return result
    except Exception as exc:
        provenance.update(status="unavailable", reason=f"Staging adaptation unavailable: {exc}")
        _record(directory / "review.json", provenance)
        return dict(original, adaptation=provenance)


def review_adapted_storyboard(plan, shot_index, proposed, previous_review, image, review_dir, *,
                              on_activity=None, on_activity_delta=None, executable=None):
    key = _fingerprint([_policy_identity(), plan.get("source_sha256"), plan["shots"][shot_index],
                        proposed, previous_review, _file_hash(image)])
    return rust_engine.call("visual_review_storyboard", plan=plan, shot_index=shot_index,
        proposed=proposed, previous_review=previous_review, image=str(Path(image).resolve()),
        directory=str(Path(review_dir).resolve()), cache_key=key,
        executable=str(executable) if executable is not None else None,
        on_event=rust_engine.activity_events(on_activity, on_activity_delta))


def review_scene(plan, shot_reviews, review_dir, *, on_activity=None,
                 on_activity_delta=None, executable=None):
    options = {}
    try:
        options["cache_key"] = _fingerprint([_policy_identity(), plan.get("source_sha256"), plan["shots"], shot_reviews])
        if not all(item.get("status") == "unavailable" for item in shot_reviews):
            options["executable"] = str(executable) if executable is not None else None
    except Exception as exc:
        options["input_error"] = str(exc)
    return rust_engine.call("visual_review_scene", plan=plan, shot_reviews=shot_reviews,
        directory=str(Path(review_dir).resolve()), **options,
        on_event=rust_engine.activity_events(on_activity, on_activity_delta))
