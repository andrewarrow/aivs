"""Translate an arbitrary screenplay into validated, bounded video shots.

Codex proposes staging and camera direction. This module owns the source of truth:
speaker names, spoken words, beat order, cast identity, and artifact freshness.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import tempfile
import time


_HEADING = re.compile(r"^(?:INT\.?\s*/\s*EXT\.?|EXT\.?\s*/\s*INT\.?|I\s*/\s*E\.?|INT\.?|EXT\.?|EST\.?)\s+", re.I)
_TRANSITION = re.compile(r"^(?:FADE\s+(?:IN|OUT)\.?|CUT TO:|DISSOLVE TO:|SMASH CUT TO:|THE END\.?|[A-Z][A-Z\s]+ TO:)\s*$", re.I)
_INLINE = re.compile(r"^(?P<role>[\w][\w .'-]*?)(?:\s*\((?P<cue>[^\n()]*)\))?\s*:\s*[\"“](?P<speech>.*?)[\"”]\s*$")
_ROLE = re.compile(r"^[\w][\w .'-]*(?:\s*\([^\n()]*\))?$")
_PLACEHOLDER = re.compile(r"\{\{line_(\d+)\}\}")
_WORDS = re.compile(r"\b[\w]+(?:['’][\w]+)*\b", re.UNICODE)
_MAX_SCRIPT = 100_000
_MAX_TEXT = 4_000
_PROFILE_FIELDS = ("id", "name", "description", "voice_description", "voice_id",
                   "image", "scene_image", "voice_sample")
_CODEX_TIMEOUT_SECONDS = 360


def _json(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _bounded(value: object, label: str, limit: int = _MAX_TEXT) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > limit or "\x00" in value:
        raise ValueError(f"{label} must be nonempty text of at most {limit} characters.")
    return value.strip()


def _profile_map(profiles: list[dict]) -> dict[str, dict]:
    if not isinstance(profiles, list):
        raise ValueError("Character profiles must be a list.")
    result = {}
    for profile in profiles:
        if not isinstance(profile, dict):
            raise ValueError("Invalid character profile.")
        char_id = _bounded(profile.get("id"), "Character id", 128)
        if char_id in result:
            raise ValueError(f"Duplicate character profile: {char_id}.")
        _bounded(profile.get("description"), f"Appearance for {char_id}")
        voice = profile.get("voice_description", "")
        if not isinstance(voice, str):
            raise ValueError(f"Voice description for {char_id} must be text.")
        result[char_id] = profile
    return result


def _split_role_cue(label: str) -> tuple[str, str | None]:
    match = re.fullmatch(r"(?P<role>.+?)(?:\s*\((?P<cue>[^()]*)\))?", label.strip())
    if not match:
        raise ValueError(f"Invalid dialogue speaker: {label!r}.")
    role = match.group("role").strip()
    cue = match.group("cue")
    return role, cue.strip() if cue and cue.strip() else None


def parse_request(request: dict, profiles: list[dict]) -> dict:
    """Validate input and parse ordered action/dialogue beats from human script."""
    if not isinstance(request, dict) or request.get("version") != 1:
        raise ValueError("Shooting-script request version must be 1.")
    script = request.get("script")
    if not isinstance(script, str) or not script.strip() or len(script) > _MAX_SCRIPT or "\x00" in script:
        raise ValueError(f"Screenplay must be nonempty text of at most {_MAX_SCRIPT} characters.")
    cast = request.get("cast")
    if not isinstance(cast, list):
        raise ValueError("Cast must be a list of role and character bindings.")
    known = _profile_map(profiles)
    bindings: list[dict] = []
    roles: dict[str, str] = {}
    for item in cast:
        if not isinstance(item, dict) or set(item) != {"role", "character"}:
            raise ValueError("Each cast binding needs role and character only.")
        role = _bounded(item["role"], "Cast role", 128)
        char_id = _bounded(item["character"], "Cast character", 128)
        if role.casefold() in roles:
            raise ValueError(f"Duplicate cast role: {role}.")
        if char_id not in known:
            raise ValueError(f"Unknown cast character: {char_id}.")
        roles[role.casefold()] = role
        bindings.append({"role": role, "character": char_id})

    beats: list[dict] = []
    action: list[str] = []
    scene: str | None = None
    scene_id = 0
    transition: str | None = None
    fade_out = False
    lines = script.splitlines()

    def add_action() -> None:
        nonlocal transition
        if action:
            text = " ".join(part.strip() for part in action).strip()
            if text:
                beats.append({"id": len(beats) + 1, "kind": "action", "text": text,
                              "scene": scene, "scene_id": scene_id, "transition": transition})
                transition = None
            action.clear()

    def add_dialogue(role: str, cue: str | None, speech: str) -> None:
        nonlocal transition
        canonical = roles.get(role.casefold())
        if canonical is None:
            raise ValueError(f"Dialogue speaker {role!r} has no cast binding.")
        speech = _bounded(speech, f"Dialogue for {canonical}")
        beats.append({"id": len(beats) + 1, "kind": "dialogue", "speaker": canonical,
                      "cue": cue, "text": speech, "scene": scene, "scene_id": scene_id,
                      "transition": transition})
        transition = None

    i = 0
    while i < len(lines):
        line = lines[i].strip()
        i += 1
        if not line:
            add_action()
            continue
        if _HEADING.match(line):
            add_action()
            scene = line
            scene_id += 1
            transition = None
            continue
        if _TRANSITION.fullmatch(line):
            add_action()
            if re.fullmatch(r"FADE\s+OUT\.?", line, re.I):
                fade_out = True
            else:
                transition = line
                if not re.fullmatch(r"FADE\s+IN\.?", line, re.I):
                    scene_id += 1
            continue
        inline = _INLINE.fullmatch(line)
        if inline:
            add_action()
            add_dialogue(inline.group("role").strip(), inline.group("cue"), inline.group("speech"))
            continue
        # Fountain-style cue: uppercase cast role, optional parenthetical on cue
        # or following line, followed by unquoted dialogue until a blank line.
        role, cue = _split_role_cue(line) if _ROLE.fullmatch(line) else ("", None)
        if role and role.upper() == role and len(role) <= 48 and len(role.split()) <= 3:
            next_i = i
            if next_i < len(lines) and re.fullmatch(r"\([^()]*\)", lines[next_i].strip()):
                cue = lines[next_i].strip()[1:-1].strip() or cue
                next_i += 1
            spoken = []
            while next_i < len(lines) and lines[next_i].strip():
                spoken.append(lines[next_i].strip())
                next_i += 1
            if spoken:
                if role.casefold() not in roles and not re.match(r"^(?:A|AN|THE)\b", role):
                    raise ValueError(f"Dialogue speaker {role!r} has no cast binding.")
                if role.casefold() not in roles:
                    action.append(line)
                    continue
                add_action()
                speech = " ".join(spoken)
                if len(speech) >= 2 and speech[0] in '\"“' and speech[-1] in '\"”':
                    speech = speech[1:-1]
                add_dialogue(role, cue, speech)
                i = next_i
                continue
        action.append(line)
    add_action()
    if not beats:
        raise ValueError("Screenplay has no action or dialogue beats.")
    relevant = [{key: known[item["character"]].get(key) for key in _PROFILE_FIELDS}
                for item in bindings]
    digest = hashlib.sha256(_json({"script": script, "cast": bindings, "profiles": relevant}).encode("utf-8")).hexdigest()
    return {"version": 1, "script": script, "cast": bindings, "characters": relevant,
            "beats": beats, "fade_out": fade_out, "source_sha256": digest}


def _line_text(beat: dict) -> str:
    cue = f" ({beat['cue']})" if beat["cue"] else ""
    return f'{beat["speaker"]}{cue} says, "{beat["text"]}".'


def _cast_context(source: dict) -> str:
    by_id = {profile["id"]: profile for profile in source["characters"]}

    def phrase(value: str) -> str:
        value = value.strip().rstrip(".")
        value = value[:1].lower() + value[1:]
        if not re.match(r"^(?:a|an|the)\s", value, re.I):
            value = ("an " if value[:1] in "aeiou" else "a ") + value
        return value

    parts = []
    for binding in source["cast"]:
        profile = by_id[binding["character"]]
        role = binding["role"]
        parts.append(f"{role} looks like {phrase(profile['description'])}")
        voice = (profile.get("voice_description") or "").strip()
        if voice:
            parts.append(f"{role} speaks with {phrase(voice)}")
    return ". ".join(parts).rstrip(". ") + ". " if parts else ""


def _token_count(prompt: str, token_counter) -> int:
    if token_counter is None:
        # The injected Gemma counter supplies the hard model-specific bound. This
        # conservative fallback also keeps offline validation useful.
        return max(len(_WORDS.findall(prompt)) * 2, math.ceil(len(prompt) / 3))
    count = token_counter(prompt)
    if not isinstance(count, int) or count < 0:
        raise ValueError("Token counter must return a nonnegative integer.")
    return count


def _timing_estimate(beats: list[dict]) -> dict:
    """Expose the same conservative speech/action floor to planner and validator.

    Source actions and delivery cues can require more time; this is deliberately
    not an upper bound inferred from paragraph count. A fade adds no idle time.
    """
    words = sum(len(_WORDS.findall(beat["text"])) for beat in beats
                if beat["kind"] == "dialogue")
    actions = sum(beat["kind"] == "action" for beat in beats)
    speech_seconds = words * 60 / 160
    return {"spoken_words": words, "speech_seconds_at_160_wpm": speech_seconds,
            "action_beats": actions,
            "minimum_seconds": max(4, math.ceil(speech_seconds + actions + 0.5))}


def validate_shooting_script(candidate: dict, request: dict, profiles: list[dict], *, token_counter=None) -> dict:
    """Reject invented/reordered content and fill dialogue from the source of truth."""
    source = parse_request(request, profiles)
    if not isinstance(candidate, dict) or not isinstance(candidate.get("shots"), list):
        raise ValueError("Codex response must contain a shots list.")
    if candidate.get("source_sha256", source["source_sha256"]) != source["source_sha256"]:
        raise ValueError("Shooting script was made for a different screenplay or cast profile.")
    proposed = candidate["shots"]
    if not 1 <= len(proposed) <= 64:
        raise ValueError("Shooting script needs 1 to 64 shots.")
    source_beats = source["beats"]
    by_id = {beat["id"]: beat for beat in source_beats}
    expected = 1
    shots = []
    for index, item in enumerate(proposed, 1):
        if not isinstance(item, dict):
            raise ValueError(f"Shot {index} must be an object.")
        seconds = item.get("seconds")
        if type(seconds) is not int or not 4 <= seconds <= 12:
            raise ValueError(f"Shot {index} must last 4 to 12 whole seconds.")
        ids = item.get("beat_ids")
        if not isinstance(ids, list) or not ids or any(type(n) is not int for n in ids):
            raise ValueError(f"Shot {index} needs beat_ids.")
        if ids != list(range(expected, expected + len(ids))) or ids[-1] > len(source_beats):
            raise ValueError(f"Shot {index} skips, duplicates, or reorders source beats.")
        expected = ids[-1] + 1
        scene_markers = [by_id[n]["scene"] for n in ids]
        scene_ids = [by_id[n]["scene_id"] for n in ids]
        if len(set(scene_ids)) > 1:
            raise ValueError(f"Shot {index} crosses a scene boundary.")
        continue_previous = item.get("continue_previous")
        if type(continue_previous) is not bool:
            raise ValueError(f"Shot {index} continue_previous must be true or false.")
        if index == 1 and continue_previous:
            raise ValueError("The first shot cannot continue a previous shot.")
        if index > 1 and scene_ids[0] != shots[-1]["scene_id"] and continue_previous:
            raise ValueError(f"Shot {index} cannot continue across scenes.")
        if index > 1 and scene_ids[0] == shots[-1]["scene_id"] and not continue_previous:
            raise ValueError(
                f"Shot {index} must continue the previous shot within the same scene: "
                "use continue_previous true and preserve its ending composition in the opening frame."
            )
        template = _bounded(item.get("prompt_template"), f"Shot {index} prompt template")
        start = _bounded(item.get("start_state"), f"Shot {index} start state", 1200)
        end = _bounded(item.get("end_state"), f"Shot {index} end state", 1200)
        if any(mark in template for mark in ('"', '“', '”')):
            raise ValueError(f"Shot {index} template contains quoted text outside source dialogue placeholders.")
        if re.search(r"\b(?:fade\s+(?:in|out)|subtitles?|captions?|title\s+card)\b", template, re.I):
            raise ValueError(f"Shot {index} includes text or a fade that the final assembly owns.")
        placeholder_ids = [int(n) for n in _PLACEHOLDER.findall(template)]
        required = [n for n in ids if by_id[n]["kind"] == "dialogue"]
        if placeholder_ids != required:
            raise ValueError(f"Shot {index} dialogue placeholders must match source lines exactly and in order.")
        if "{{" in _PLACEHOLDER.sub("", template) or "}}" in _PLACEHOLDER.sub("", template):
            raise ValueError(f"Shot {index} has an invalid placeholder.")
        for beat in source_beats:
            if (beat["kind"] == "dialogue" and
                    re.search(r"(?<!\w)" + re.escape(beat["text"]) + r"(?!\w)", template, re.I)):
                raise ValueError(f"Shot {index} contains raw dialogue; use placeholders.")
        if re.search(r"\b(?:says|speaks|talks|whispers|shouts|mutters|asks|replies|exclaims)\b", template, re.I):
            raise ValueError(f"Shot {index} contains unsourced speech direction; use dialogue placeholders.")
        action_prompt = _PLACEHOLDER.sub(lambda m: _line_text(by_id[int(m.group(1))]), template)
        action_prompt = action_prompt.rstrip()
        if action_prompt[-1] not in ".!?":
            action_prompt += "."
        speech_direction = (
            "The only spoken words are the quoted dialogue, delivered exactly once in order. "
            "No added words, repetitions, or unscripted vocalizations. Only the current speaker moves their mouth. "
            "After the final word, mouths rest unless the source action requires otherwise. "
            if required else "There is no spoken dialogue in this shot. "
        )
        prompt = (action_prompt + " " + _cast_context(source) + speech_direction
                  + "No subtitles, captions, or text overlays.")
        if _token_count(prompt, token_counter) > 1024:
            raise ValueError(f"Shot {index} prompt exceeds 1024 Gemma tokens.")
        minimum = _timing_estimate([by_id[n] for n in ids])["minimum_seconds"]
        if seconds < minimum:
            raise ValueError(f"Shot {index} needs at least {minimum} seconds for its dialogue and action.")
        shots.append({"seconds": seconds, "beat_ids": ids, "prompt_template": template,
                      "prompt": prompt, "start_state": start, "end_state": end,
                      "continue_previous": continue_previous, "scene": scene_markers[0],
                      "scene_id": scene_ids[0]})
    if expected != len(source_beats) + 1:
        raise ValueError("Shooting script omits source beats.")
    if isinstance(candidate.get("shots"), list):
        for proposed_shot, filled in zip(candidate["shots"], shots):
            if "prompt" in proposed_shot and proposed_shot["prompt"] != filled["prompt"]:
                raise ValueError("Saved shot prompt does not match the source dialogue.")
    return {"version": 1, "source_sha256": source["source_sha256"],
            "seconds": sum(shot["seconds"] for shot in shots), "fade_out": source["fade_out"],
            "source": source, "shots": shots}


def load_shooting_script(path: Path, request: dict, profiles: list[dict], *, token_counter=None) -> dict:
    """Read and revalidate a cached plan against the current source and profiles."""
    path = Path(path)
    try:
        saved = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"Cannot read shooting script: {path}") from exc
    artifact = validate_shooting_script(saved, request, profiles, token_counter=token_counter)
    if saved.get("version") != 1 or saved.get("seconds") != artifact["seconds"] or saved.get("fade_out") != artifact["fade_out"]:
        raise ValueError("Saved shooting-script metadata is invalid.")
    return artifact


def save_shooting_script(plan: dict, run_dir: Path) -> None:
    """Save an already validated plan as machine and readable artifacts."""
    if not isinstance(plan, dict) or plan.get("version") != 1 or not isinstance(plan.get("shots"), list):
        raise ValueError("Cannot save an invalid shooting-script plan.")
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "shooting-script.json").write_text(json.dumps(plan, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    display = []
    for index, shot in enumerate(plan["shots"], 1):
        display.append(f"SHOT {index} | {shot['seconds']}s | beats {', '.join(map(str, shot['beat_ids']))}")
        display.extend((f"Start: {shot['start_state']}", shot["prompt"], f"End: {shot['end_state']}", ""))
    if plan.get("fade_out"):
        display.append("Fade out at the end of the final shot.")
    (run_dir / "shooting-script.txt").write_text("\n".join(display).rstrip() + "\n", encoding="utf-8")


_SCHEMA = {"type": "object", "additionalProperties": False, "required": ["shots"],
           "properties": {"shots": {"type": "array",
             "items": {"type": "object", "additionalProperties": False,
               "required": ["seconds", "beat_ids", "prompt_template", "start_state", "end_state", "continue_previous"],
               "properties": {"seconds": {"type": "integer"},
                 "beat_ids": {"type": "array", "items": {"type": "integer"}},
                 "prompt_template": {"type": "string"}, "start_state": {"type": "string"},
                 "end_state": {"type": "string"}, "continue_previous": {"type": "boolean"}}}}}}


def _instruction(source: dict, correction: str | None) -> str:
    speech_estimates = []
    for beat in source["beats"]:
        if beat["kind"] == "dialogue":
            timing = _timing_estimate([beat])
            speech_estimates.append({
                "beat_id": beat["id"], "spoken_words": timing["spoken_words"],
                "speech_seconds_at_160_wpm": timing["speech_seconds_at_160_wpm"],
            })
    body = {
        "script": source["script"], "cast": source["cast"], "characters": source["characters"],
        "beats": source["beats"], "fade_out_at_end": source["fade_out"],
        "timing_guidance": {
            "words_per_minute": 160, "settling_seconds": 0.5,
            "speech_estimates": speech_estimates,
        },
    }
    direction = Path(__file__).with_name("shooting-script-instructions.txt").read_text(encoding="utf-8")
    return (direction.rstrip() + "\n\n" + _json(body)
            + ("\n\nCorrect this validation error: " + correction if correction else ""))


def _executable(executable) -> str:
    chosen = executable or os.environ.get("AIVS_CODEX_PATH") or shutil.which("codex")
    if not chosen:
        raise RuntimeError("Codex is unavailable. Install Codex or set AIVS_CODEX_PATH, then sign in.")
    resolved = shutil.which(str(chosen)) if not Path(str(chosen)).is_absolute() else str(chosen)
    if not resolved or not Path(resolved).is_file() or not os.access(resolved, os.X_OK):
        raise RuntimeError(f"Codex executable is unavailable: {chosen}")
    return resolved


def _stream_codex(cmd, instruction, *, cwd, deadline, events_path, errors_path, response_path,
                  on_activity, on_activity_delta):
    """Use app-server notifications; exec --json only exposes completed text items."""
    with events_path.open("wb") as events, \
            errors_path.open("wb") as errors, selectors.DefaultSelector() as selector:
        process = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, cwd=cwd)
        # A large screenplay must not block pipe draining while being sent.
        os.set_blocking(process.stdin.fileno(), False)
        outgoing = bytearray()
        pending = {True: b"", False: b""}
        streamed = set()
        messages = {}
        phases = {}
        finished = False

        def send(message):
            if not outgoing:
                selector.register(process.stdin, selectors.EVENT_WRITE)
            outgoing.extend((json.dumps(message, ensure_ascii=False) + "\n").encode("utf-8"))

        def stream(stream_id, text):
            if text:
                streamed.add(stream_id)
                if on_activity_delta is not None:
                    on_activity_delta(stream_id, text)
                else:
                    on_activity(f"Codex: {text}")

        def complete_item(item):
            item_id = item.get("id", "")
            kind = item.get("type")
            if kind == "agentMessage":
                text = item.get("text", "")
                messages[item_id] = text
                phases[item_id] = item.get("phase")
                if item_id not in streamed:
                    stream(item_id, text)
            elif kind == "reasoning":
                for field in ("summary", "content"):
                    for index, text in enumerate(item.get(field, [])):
                        stream_id = f"{item_id}:{field}:{index}"
                        if stream_id not in streamed:
                            stream(stream_id, text)
            elif kind == "commandExecution" and item_id not in streamed:
                stream(item_id, item.get("aggregatedOutput") or "")

        def fail(detail):
            raise RuntimeError(f"Codex shooting-script translation failed: {detail}")

        def handle(event):
            nonlocal finished
            if not isinstance(event, dict):
                return
            method = event.get("method")
            if "id" in event:
                if method:
                    send({"id": event["id"], "error": {
                        "code": -32601, "message": "This client does not support this request."}})
                    return
                error = event.get("error")
                if error:
                    fail(error.get("message", str(error)) if isinstance(error, dict) else error)
                result = event.get("result", {})
                if event["id"] == 1:
                    send({"method": "initialized", "params": {}})
                    send({"id": 2, "method": "thread/start", "params": {
                        "cwd": str(cwd), "approvalPolicy": "never", "sandbox": "read-only",
                        "ephemeral": True, "serviceName": "aivs"}})
                elif event["id"] == 2:
                    thread_id = result.get("thread", {}).get("id")
                    if not thread_id:
                        fail("Codex did not return a thread ID.")
                    on_activity("Codex connected.")
                    send({"id": 3, "method": "turn/start", "params": {
                        "threadId": thread_id, "input": [{"type": "text", "text": instruction}],
                        "cwd": str(cwd), "approvalPolicy": "never",
                        "sandboxPolicy": {"type": "readOnly"},
                        "summary": "detailed", "outputSchema": _SCHEMA}})
                return
            params = event.get("params", {})
            item_id = params.get("itemId", "")
            if method == "item/agentMessage/delta":
                delta = params.get("delta", "")
                messages[item_id] = messages.get(item_id, "") + delta
                stream(item_id, delta)
            elif method in ("item/reasoning/summaryTextDelta", "item/reasoning/textDelta"):
                summary = method == "item/reasoning/summaryTextDelta"
                field = "summary" if summary else "content"
                index = params.get("summaryIndex" if summary else "contentIndex", 0)
                stream(f"{item_id}:{field}:{index}", params.get("delta", ""))
            elif method in ("item/plan/delta", "item/commandExecution/outputDelta"):
                stream(item_id, params.get("delta", ""))
            elif method == "item/started":
                item = params.get("item", {})
                if item.get("type") == "agentMessage":
                    phases[item.get("id", "")] = item.get("phase")
                elif item.get("type") == "commandExecution":
                    on_activity(f"Codex running: {item.get('command', '')}")
            elif method == "item/completed":
                complete_item(params.get("item", {}))
            elif method == "turn/completed":
                turn = params.get("turn", {})
                for item in turn.get("items", []):
                    complete_item(item)
                if turn.get("status") != "completed":
                    error = turn.get("error") or {}
                    fail(error.get("message") or f"Turn {turn.get('status', 'failed')}.")
                finished = True
            elif method == "error":
                error = params.get("error") or {}
                detail = error.get("message") or "Request failed."
                if not params.get("willRetry"):
                    fail(detail)
                on_activity(f"Codex: {detail}")

        def publish(data, protocol):
            line = data.decode("utf-8", errors="replace").rstrip("\r")
            if protocol:
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    if line.strip():
                        on_activity(f"Codex: {line}")
                    return
                handle(event)
            elif line.strip():
                on_activity(f"Codex: {line}")

        try:
            selector.register(process.stdout, selectors.EVENT_READ, (events, True))
            selector.register(process.stderr, selectors.EVENT_READ, (errors, False))
            send({"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "aivs", "title": "AIVS", "version": "0.1.0"}}})
            while not finished and (selector.get_map() or process.poll() is None):
                now = time.monotonic()
                if now >= deadline:
                    raise subprocess.TimeoutExpired(cmd, _CODEX_TIMEOUT_SECONDS)
                for key, _ in selector.select(min(0.25, deadline - now)):
                    if key.fileobj is process.stdin:
                        try:
                            written = os.write(key.fd, outgoing[:65536])
                        except BrokenPipeError:
                            outgoing.clear()
                        else:
                            del outgoing[:written]
                        if not outgoing:
                            selector.unregister(process.stdin)
                        continue
                    log, protocol = key.data
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        if pending[protocol]:
                            publish(pending[protocol], protocol)
                            pending[protocol] = b""
                        continue
                    log.write(data)
                    log.flush()
                    lines = (pending[protocol] + data).split(b"\n")
                    pending[protocol] = lines.pop()
                    for line in lines:
                        publish(line, protocol)
            if not finished:
                detail = (errors_path.read_text(encoding="utf-8", errors="replace")
                          or f"App server stopped (exit {process.wait()}).").strip()[-1000:]
                fail(detail)
            on_activity("Codex response received.")
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()
            # Keep partial generated output even when the turn times out or stops.
            raw = next((text for item_id, text in reversed(messages.items())
                        if phases.get(item_id) != "commentary"), "")
            response_path.write_text(raw, encoding="utf-8")
        return raw


def create_shooting_script(request: dict, profiles: list[dict], run_dir: Path, *, executable=None,
                           token_counter=None, on_activity=None, on_activity_delta=None) -> dict:
    """Ask the signed-in local Codex CLI for staging, with one validation retry."""
    source = parse_request(request, profiles)
    codex = _executable(executable)
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + _CODEX_TIMEOUT_SECONDS
    activity = on_activity or (lambda message: None)
    correction = None
    with tempfile.TemporaryDirectory(prefix="aivs-shooting-") as temp:
        (run_dir / "shooting-script-schema.json").write_text(json.dumps(_SCHEMA, indent=2), encoding="utf-8")
        for attempt in (1, 2):
            instruction = _instruction(source, correction)
            (run_dir / f"shooting-script-request-{attempt}.txt").write_text(instruction, encoding="utf-8")
            response_path = run_dir / f"shooting-script-response-{attempt}.json"
            cmd = [codex, "app-server"]
            activity("Starting Codex to prepare the shooting script." if attempt == 1
                     else "Retrying shooting script with Codex (attempt 2/2).")
            events_path = run_dir / f"shooting-script-events-{attempt}.jsonl"
            errors_path = run_dir / f"shooting-script-errors-{attempt}.txt"
            try:
                raw = _stream_codex(cmd, instruction, cwd=temp, deadline=deadline,
                                    events_path=events_path, errors_path=errors_path,
                                    response_path=response_path, on_activity=activity,
                                    on_activity_delta=on_activity_delta)
            except subprocess.TimeoutExpired as exc:
                raise RuntimeError(f"Codex shooting-script translation timed out after {_CODEX_TIMEOUT_SECONDS:g} seconds.") from exc
            activity("Validating shooting script.")
            try:
                candidate = json.loads(raw)
                artifact = validate_shooting_script(candidate, request, profiles, token_counter=token_counter)
            except (json.JSONDecodeError, ValueError) as exc:
                correction = str(exc)
                if attempt == 2:
                    raise ValueError(f"Codex returned an invalid shooting script after one correction: {correction}") from exc
                activity(f"Shooting script needs a correction: {correction}")
                continue
            save_shooting_script(artifact, run_dir)
            activity(f"Shooting script ready: {len(artifact['shots'])} shots, {artifact['seconds']} seconds.")
            return artifact
    raise AssertionError("Unreachable shooting-script translation state")
