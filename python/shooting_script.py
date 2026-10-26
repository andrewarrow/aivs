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
import shutil
import subprocess
import tempfile
import time


_HEADING = re.compile(r"^(?:INT\.?|EXT\.?|INT/EXT\.?|EXT/INT\.?|I/E\.?|EST\.?)\s+", re.I)
_TRANSITION = re.compile(r"^(?:FADE\s+(?:IN|OUT)\.?|CUT TO:|DISSOLVE TO:|SMASH CUT TO:|THE END\.?|[A-Z][A-Z\s]+ TO:)\s*$", re.I)
_INLINE = re.compile(r"^(?P<role>[\w][\w .'-]*?)(?:\s*\((?P<cue>[^\n()]*)\))?\s*:\s*[\"“](?P<speech>.*?)[\"”]\s*$")
_ROLE = re.compile(r"^[\w][\w .'-]*(?:\s*\([^\n()]*\))?$")
_PLACEHOLDER = re.compile(r"\{\{line_(\d+)\}\}")
_WORDS = re.compile(r"\b[\w]+(?:['’][\w]+)*\b", re.UNICODE)
_MAX_SCRIPT = 100_000
_MAX_TEXT = 4_000
_PROFILE_FIELDS = ("id", "name", "description", "voice_description", "voice_id")


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
    script = _bounded(request.get("script"), "Screenplay", _MAX_SCRIPT)
    cast = request.get("cast")
    if not isinstance(cast, list):
        raise ValueError("Cast must be a list of role and character bindings.")
    known = _profile_map(profiles)
    bindings: list[dict] = []
    roles: dict[str, str] = {}
    used_characters: set[str] = set()
    for item in cast:
        if not isinstance(item, dict) or set(item) != {"role", "character"}:
            raise ValueError("Each cast binding needs role and character only.")
        role = _bounded(item["role"], "Cast role", 128)
        char_id = _bounded(item["character"], "Cast character", 128)
        if role.casefold() in roles or char_id in used_characters:
            raise ValueError(f"Duplicate cast role or character: {role}.")
        if char_id not in known:
            raise ValueError(f"Unknown cast character: {char_id}.")
        roles[role.casefold()] = role
        used_characters.add(char_id)
        bindings.append({"role": role, "character": char_id})

    beats: list[dict] = []
    action: list[str] = []
    scene: str | None = None
    fade_out = False
    lines = script.splitlines()

    def add_action() -> None:
        if action:
            text = " ".join(part.strip() for part in action).strip()
            if text:
                beats.append({"id": len(beats) + 1, "kind": "action", "text": text, "scene": scene})
            action.clear()

    def add_dialogue(role: str, cue: str | None, speech: str) -> None:
        canonical = roles.get(role.casefold())
        if canonical is None:
            raise ValueError(f"Dialogue speaker {role!r} has no cast binding.")
        speech = _bounded(speech, f"Dialogue for {canonical}")
        beats.append({"id": len(beats) + 1, "kind": "dialogue", "speaker": canonical,
                      "cue": cue, "text": speech, "scene": scene})

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
            continue
        if _TRANSITION.fullmatch(line):
            add_action()
            if re.fullmatch(r"FADE\s+OUT\.?", line, re.I):
                fade_out = True
            continue
        inline = _INLINE.fullmatch(line)
        if inline:
            add_action()
            add_dialogue(inline.group("role").strip(), inline.group("cue"), inline.group("speech"))
            continue
        # Fountain-style cue: uppercase cast role, optional parenthetical on cue
        # or following line, followed by unquoted dialogue until a blank line.
        role, cue = _split_role_cue(line) if _ROLE.fullmatch(line) else ("", None)
        if role and role.casefold() in roles and role.upper() == role:
            next_i = i
            if next_i < len(lines) and re.fullmatch(r"\([^()]*\)", lines[next_i].strip()):
                cue = lines[next_i].strip()[1:-1].strip() or cue
                next_i += 1
            spoken = []
            while next_i < len(lines) and lines[next_i].strip():
                spoken.append(lines[next_i].strip())
                next_i += 1
            if spoken:
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
    cue = f", {beat['cue']}" if beat["cue"] else ""
    return f'{beat["speaker"]}{cue}, says, "{beat["text"]}".'


def _cast_context(source: dict) -> str:
    by_id = {profile["id"]: profile for profile in source["characters"]}
    parts = []
    for binding in source["cast"]:
        profile = by_id[binding["character"]]
        role = binding["role"]
        parts.append(f'{role}: {profile["description"].strip()}')
        voice = (profile.get("voice_description") or "").strip()
        if voice:
            parts.append(f'{role} voice: {voice}')
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
        if len(set(scene_markers)) > 1:
            raise ValueError(f"Shot {index} crosses a scene boundary.")
        continue_previous = item.get("continue_previous")
        if type(continue_previous) is not bool:
            raise ValueError(f"Shot {index} continue_previous must be true or false.")
        if index == 1 and continue_previous:
            raise ValueError("The first shot cannot continue a previous shot.")
        if index > 1 and scene_markers[0] != shots[-1]["scene"] and continue_previous:
            raise ValueError(f"Shot {index} cannot continue across scenes.")
        template = _bounded(item.get("prompt_template"), f"Shot {index} prompt template")
        start = _bounded(item.get("start_state"), f"Shot {index} start state", 1200)
        end = _bounded(item.get("end_state"), f"Shot {index} end state", 1200)
        if any(mark in template for mark in ('"', '“', '”')):
            raise ValueError(f"Shot {index} template contains quoted text outside source dialogue placeholders.")
        placeholder_ids = [int(n) for n in _PLACEHOLDER.findall(template)]
        required = [n for n in ids if by_id[n]["kind"] == "dialogue"]
        if placeholder_ids != required:
            raise ValueError(f"Shot {index} dialogue placeholders must match source lines exactly and in order.")
        if "{{" in _PLACEHOLDER.sub("", template) or "}}" in _PLACEHOLDER.sub("", template):
            raise ValueError(f"Shot {index} has an invalid placeholder.")
        for beat in source_beats:
            if beat["kind"] == "dialogue" and beat["text"].casefold() in template.casefold():
                raise ValueError(f"Shot {index} contains raw dialogue; use placeholders.")
        if re.search(r"\b(?:says|speaks|talks|whispers|shouts|mutters|asks|replies|exclaims)\b", template, re.I):
            raise ValueError(f"Shot {index} contains unsourced speech direction; use dialogue placeholders.")
        prompt = _cast_context(source) + _PLACEHOLDER.sub(lambda m: _line_text(by_id[int(m.group(1))]), template)
        if _token_count(prompt, token_counter) > 1024:
            raise ValueError(f"Shot {index} prompt exceeds 1024 Gemma tokens.")
        spoken_words = sum(len(_WORDS.findall(by_id[n]["text"])) for n in required)
        action_count = sum(by_id[n]["kind"] == "action" for n in ids)
        minimum = math.ceil(spoken_words * 60 / 160 + action_count + 0.5)
        if seconds < max(4, minimum):
            raise ValueError(f"Shot {index} needs at least {max(4, minimum)} seconds for its dialogue and action.")
        shots.append({"seconds": seconds, "beat_ids": ids, "prompt_template": template,
                      "prompt": prompt, "start_state": start, "end_state": end,
                      "continue_previous": continue_previous, "scene": scene_markers[0]})
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


_SCHEMA = {"type": "object", "additionalProperties": False, "required": ["shots"],
           "properties": {"shots": {"type": "array", "minItems": 1, "maxItems": 64,
             "items": {"type": "object", "additionalProperties": False,
               "required": ["seconds", "beat_ids", "prompt_template", "start_state", "end_state", "continue_previous"],
               "properties": {"seconds": {"type": "integer", "minimum": 4, "maximum": 12},
                 "beat_ids": {"type": "array", "minItems": 1, "items": {"type": "integer", "minimum": 1}},
                 "prompt_template": {"type": "string"}, "start_state": {"type": "string"},
                 "end_state": {"type": "string"}, "continue_previous": {"type": "boolean"}}}}}}


def _instruction(source: dict, correction: str | None) -> str:
    body = {
        "script": source["script"], "cast": source["cast"], "characters": source["characters"],
        "beats": source["beats"], "fade_out_after_final_action": source["fade_out"],
    }
    return ("Create a shooting plan for this arbitrary human screenplay. Return only the JSON object matching the schema. "
            "Do not call tools. Use every beat ID once, in contiguous chronological order. Keep different scenes in separate shots. "
            "Use 4-12 seconds per shot. Cover dialogue time at 160 words/minute, plus at least one second for each action beat. "
            "Write short, self-contained, chronological present-tense visual prompts with explicit body movement and object interaction. "
            "Map each ROLE directly to its cast character, without introducing a second performer identity. "
            "The application prepends the exact saved appearance and voice details; do not repeat those details in your templates. "
            "Carry positions, object possession and state, wardrobe, and camera through start_state and end_state. "
            "No titles, captions, still posing, new plot, new speech, or raw quoted dialogue. "
            "Put exactly one {{line_BEATID}} placeholder for each dialogue beat in its shot, in order; the application fills the exact source words. "
            "Complete closing physical movement before any fade; fade is applied after assembly. "
            "Infer only minimal movement needed for continuity. Keep each prompt under about 200 words.\n\n"
            + _json(body) + ("\n\nCorrect this validation error: " + correction if correction else ""))


def _executable(executable) -> str:
    chosen = executable or os.environ.get("AIVS_CODEX_PATH") or shutil.which("codex")
    if not chosen:
        raise RuntimeError("Codex is unavailable. Install Codex or set AIVS_CODEX_PATH, then sign in.")
    resolved = shutil.which(str(chosen)) if not Path(str(chosen)).is_absolute() else str(chosen)
    if not resolved or not Path(resolved).is_file() or not os.access(resolved, os.X_OK):
        raise RuntimeError(f"Codex executable is unavailable: {chosen}")
    return resolved


def create_shooting_script(request: dict, profiles: list[dict], run_dir: Path, *, executable=None, token_counter=None) -> dict:
    """Ask the signed-in local Codex CLI for staging, with one validation retry."""
    source = parse_request(request, profiles)
    codex = _executable(executable)
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + 180
    correction = None
    with tempfile.TemporaryDirectory(prefix="aivs-shooting-") as temp:
        temp_path = Path(temp)
        schema_path = temp_path / "schema.json"
        schema_path.write_text(json.dumps(_SCHEMA), encoding="utf-8")
        (run_dir / "shooting-script-schema.json").write_text(json.dumps(_SCHEMA, indent=2), encoding="utf-8")
        for attempt in (1, 2):
            instruction = _instruction(source, correction)
            (run_dir / f"shooting-script-request-{attempt}.txt").write_text(instruction, encoding="utf-8")
            response_path = temp_path / f"response-{attempt}.json"
            cmd = [codex, "exec", "--ephemeral", "--skip-git-repo-check", "--sandbox", "read-only",
                   "-c", "approval_policy=never", "-C", temp, "--output-schema", str(schema_path),
                   "--output-last-message", str(response_path), "--json", "-"]
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("Codex shooting-script translation timed out after 180 seconds.")
            try:
                result = subprocess.run(cmd, input=instruction, text=True, capture_output=True,
                                        cwd=temp, timeout=remaining, check=False)
            except subprocess.TimeoutExpired as exc:
                raise RuntimeError("Codex shooting-script translation timed out after 180 seconds.") from exc
            (run_dir / f"shooting-script-events-{attempt}.jsonl").write_text(result.stdout, encoding="utf-8")
            (run_dir / f"shooting-script-errors-{attempt}.txt").write_text(result.stderr, encoding="utf-8")
            raw = response_path.read_text(encoding="utf-8") if response_path.is_file() else ""
            (run_dir / f"shooting-script-response-{attempt}.json").write_text(raw, encoding="utf-8")
            if result.returncode:
                detail = (result.stderr or result.stdout or "Codex produced no details.").strip()[-1000:]
                raise RuntimeError(f"Codex shooting-script translation failed (exit {result.returncode}). Check login and CLI setup. {detail}")
            try:
                candidate = json.loads(raw)
                artifact = validate_shooting_script(candidate, request, profiles, token_counter=token_counter)
            except (json.JSONDecodeError, ValueError) as exc:
                correction = str(exc)
                if attempt == 2:
                    raise ValueError(f"Codex returned an invalid shooting script after one correction: {correction}") from exc
                continue
            (run_dir / "shooting-script.json").write_text(json.dumps(artifact, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            display = []
            for index, shot in enumerate(artifact["shots"], 1):
                display.append(f"SHOT {index} | {shot['seconds']}s | beats {', '.join(map(str, shot['beat_ids']))}")
                display.extend((f"Start: {shot['start_state']}", shot["prompt"], f"End: {shot['end_state']}", ""))
            if artifact["fade_out"]:
                display.append("Fade out after the final action.")
            (run_dir / "shooting-script.txt").write_text("\n".join(display).rstrip() + "\n", encoding="utf-8")
            return artifact
    raise AssertionError("Unreachable shooting-script translation state")
