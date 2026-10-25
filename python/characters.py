"""Saved character lookup, prompt expansion, and render snapshots adapted from Hunter.

Names and @name references resolve outside quoted dialogue. Profiles and assets
use the same schema as Hunter; character management remains outside this module.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tempfile

from dialogue_helpers import _strip_outer_quote_wrapper


SCHEMA_VERSION = 1
_ID_RE = re.compile(r"[a-z][a-z0-9_-]*\Z")
_NAME_RE = re.compile(r"[A-Za-z][A-Za-z0-9_-]*\Z")
_VOICE_ID_RE = re.compile(r"[a-z]{2}_[a-z0-9_]+\Z")
_ASSET_NAMES = {"image": "reference.png", "voice_sample": "voice.wav"}
_DOUBLE_QUOTE_CHARS = frozenset('"“”')
_SINGLE_QUOTE_CHARS = frozenset("'‘’")
_AT_NAME_RE = re.compile(r"@([A-Za-z][A-Za-z0-9_-]*)")
_ALTERNATE_VISUAL_STYLE_RE = re.compile(
    r"\b(?:cartoons?|anime|manga|animation|animated|illustrations?|stylized|"
    r"stylised|cgi|3d|three[- ]dimensional|claymation|stop[- ]motion|"
    r"comic[- ]book|graphic[- ]novel|watercolou?r|oil painting|pixel art|"
    r"storybook|fantasy art)\b",
    re.IGNORECASE,
)


_STYLE_NEGATION_RE = re.compile(
    r"\b(?:not|no|without|avoid|never|isn't|is not|aren't|are not)\s+"
    r"(?:(?:a|an|the)\s+)?(?:(?:in|as)\s+)?$",
    re.IGNORECASE,
)


def _requests_alternate_visual_style(text: str) -> bool:
    """Detect affirmative alternate styles without treating quoted speech or negation as requests."""
    quote_mask = _quoted_mask(text)
    for match in _ALTERNATE_VISUAL_STYLE_RE.finditer(text):
        if any(quote_mask[match.start():match.end()]):
            continue
        prefix = text[max(0, match.start() - 32):match.start()]
        if _STYLE_NEGATION_RE.search(prefix):
            continue
        return True
    return False


def _library_dir(root: str | os.PathLike[str] | None) -> Path:
    base = Path(root) if root is not None else Path(__file__).resolve().parents[1]
    return base / "characters"


def _slug(name: str) -> tuple[str, str]:
    if not isinstance(name, str):
        raise ValueError("Character name must be a slug such as Joe.")
    display = name.strip()
    if not _NAME_RE.fullmatch(display):
        raise ValueError("Character name must start with a letter and contain only letters, numbers, underscores, or hyphens.")
    return display.lower(), display


def _text(value: str, label: str, *, required: bool = False) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be text.")
    value = value.strip()
    if required and not value:
        raise ValueError(f"{label} is required.")
    if any(char in _DOUBLE_QUOTE_CHARS for char in value):
        raise ValueError(f"{label} cannot contain quote characters.")
    for index, char in enumerate(value):
        if char not in _SINGLE_QUOTE_CHARS:
            continue
        if index == 0 or index == len(value) - 1 or not (value[index - 1].isalnum() and value[index + 1].isalnum()):
            raise ValueError(f"{label} cannot contain quote characters.")
    return value


def _digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def _validate_asset_record(record: object, expected_filename: str, directory: Path, label: str) -> Path:
    if not isinstance(record, dict) or set(record) != {"file", "sha256"}:
        raise ValueError(f"Invalid {label} asset record.")
    filename, expected_hash = record["file"], record["sha256"]
    if filename != expected_filename or Path(filename).name != filename:
        raise ValueError(f"Invalid {label} asset filename.")
    if not isinstance(expected_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", expected_hash):
        raise ValueError(f"Invalid {label} asset hash.")
    path = directory / filename
    if not path.is_file() or _digest(path) != expected_hash:
        raise ValueError(f"{label.capitalize()} asset is missing or has changed: {filename}.")
    return path


def _read_profile(path: Path) -> dict:
    try:
        profile = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"Cannot read character profile: {path}") from exc
    if not isinstance(profile, dict) or profile.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"Unsupported or invalid character profile: {path}")
    char_id = profile.get("id")
    if not isinstance(char_id, str) or not _ID_RE.fullmatch(char_id) or path.parent.name != char_id:
        raise ValueError(f"Invalid character id in profile: {path}")
    _, name = _slug(profile.get("name"))
    if name != profile["name"] or char_id != name.lower():
        raise ValueError(f"Character name and id do not match in profile: {path}")
    _text(profile.get("description"), "Character description", required=True)
    _text(profile.get("voice_description", ""), "Voice description")
    voice_id = profile.get("voice_id")
    if voice_id is not None and (not isinstance(voice_id, str) or not _VOICE_ID_RE.fullmatch(voice_id)):
        raise ValueError(f"Invalid voice id in profile: {path}")
    for key, filename in _ASSET_NAMES.items():
        record = profile.get(key)
        if record is not None:
            _validate_asset_record(record, filename, path.parent, key.replace("_", " "))
    return profile


def load_character(name: str, root: str | os.PathLike[str] | None = None) -> dict:
    """Read a saved character profile and validate its reference assets."""
    char_id, _ = _slug(name)
    path = _library_dir(root) / char_id / "character.json"
    return _read_profile(path)


def _basic_quoted_mask(prompt: str) -> list[bool]:
    masked = [False] * len(prompt)
    quote = None
    quote_start = None
    for index, char in enumerate(prompt):
        if quote is None:
            if char in ('"', '“', '‘'):
                quote = {'“': '”', '‘': '’'}.get(char, char)
                quote_start = index
                masked[index] = True
            elif char == "'" and not (index > 0 and prompt[index - 1].isalnum()):
                quote = "'"
                quote_start = index
                masked[index] = True
        else:
            masked[index] = True
            if char == quote:
                if quote in ("'", "’"):
                    # Apostrophes inside dialogue do not close the dialogue.
                    if index + 1 < len(prompt) and prompt[index + 1].isalnum() and (
                        prompt[index - 1].isalnum() or index == quote_start + 1
                    ):
                        continue
                    remainder = prompt[index + 1:].split("\n", 1)[0]
                    next_close = remainder.find(quote)
                    next_open = remainder.find("‘") if quote == "’" else remainder.find("'")
                    if (index > 0 and prompt[index - 1].isalnum()
                            and re.match(r"\s+\w", remainder)
                            and next_close >= 0 and (next_open < 0 or next_close < next_open)):
                        continue
                quote = None
                quote_start = None
    return masked


def _quoted_mask(prompt: str) -> list[bool]:
    """Mask dialogue, accounting for an obvious wrapper around a whole prompt."""
    unwrapped = _strip_outer_quote_wrapper(prompt)
    if unwrapped == prompt:
        return _basic_quoted_mask(prompt)
    left = len(prompt) - len(prompt.lstrip())
    body_start = left + 1
    body_mask = _basic_quoted_mask(unwrapped)
    masked = [False] * len(prompt)
    for offset, is_quoted in enumerate(body_mask):
        if body_start + offset < len(masked):
            masked[body_start + offset] = is_quoted
    return masked


def resolve_characters(prompt: str, explicit=(), root: str | os.PathLike[str] | None = None) -> list[dict]:
    """Resolve explicit selectors plus saved names mentioned outside dialogue."""
    if not isinstance(prompt, str):
        raise ValueError("Prompt must be text.")
    library = _library_dir(root)
    known_ids = {
        path.name for path in library.iterdir()
        if library.exists() and not path.name.startswith(".") and path.is_dir() and _ID_RE.fullmatch(path.name)
    } if library.exists() else set()
    selected: list[str] = []
    seen: set[str] = set()

    def add(char_id: str) -> None:
        if char_id not in seen:
            selected.append(char_id)
            seen.add(char_id)

    if explicit is None:
        explicit = ()
    if isinstance(explicit, str):
        explicit = (explicit,)
    for selector in explicit:
        if isinstance(selector, dict):
            selector = selector.get("id") or selector.get("name")
        if not isinstance(selector, str):
            raise ValueError(f"Invalid character selector: {selector!r}")
        char_id = selector.strip().lower()
        if char_id not in known_ids:
            raise ValueError(f"Unknown character: {selector}")
        add(char_id)

    mask = _quoted_mask(prompt)
    mentions: list[tuple[int, dict]] = []
    for match in _AT_NAME_RE.finditer(prompt):
        # Ignore @names spoken inside quoted dialogue; --character is explicit.
        if any(mask[match.start():match.end()]):
            continue
        char_id = match.group(1).lower()
        if char_id not in known_ids:
            raise ValueError(f"Unknown character reference: @{match.group(1)}")
        mentions.append((match.start(), char_id))

    for char_id in known_ids:
        pattern = re.compile(
            rf"(?<![A-Za-z0-9_@-]){re.escape(char_id)}(?![A-Za-z0-9_-])",
            re.IGNORECASE,
        )
        for match in pattern.finditer(prompt):
            if not any(mask[match.start():match.end()]):
                mentions.append((match.start(), char_id))
    for _, char_id in sorted(mentions, key=lambda item: item[0]):
        add(char_id)
    return [load_character(char_id, root) for char_id in selected]


def replace_references(prompt: str, profiles) -> str:
    """Replace known @id references outside quoted dialogue, preserving speech."""
    profiles = list(profiles)
    by_id = {profile["id"].lower(): profile for profile in profiles}
    mask = _quoted_mask(prompt)
    pieces = []
    cursor = 0
    for match in _AT_NAME_RE.finditer(prompt):
        if any(mask[match.start():match.end()]):
            continue
        profile = by_id.get(match.group(1).lower())
        if profile is None:
            continue
        pieces.append(prompt[cursor:match.start()])
        pieces.append(profile["name"])
        cursor = match.end()
    pieces.append(prompt[cursor:])
    return "".join(pieces)


def expand_prompt(prompt: str, profiles, *, image_conditioned: bool = False) -> str:
    """Add neutral appearance/voice cues and expand @id references."""
    profiles = list(profiles)
    result = replace_references(prompt, profiles)
    if not profiles:
        return result
    definitions = []
    for profile in profiles:
        details = [f"{profile['name']} has a consistent appearance: {profile['description']}"]
        if profile.get("voice_description"):
            details.append(f"Voice quality: {profile['voice_description']}")
        definitions.append(". ".join(details) + ".")
    continuity = "Character continuity: " + " ".join(definitions)
    if (not image_conditioned
            and not _requests_alternate_visual_style(prompt)
            and not any(_requests_alternate_visual_style(profile["description"]) for profile in profiles)):
        continuity += (
            " Visual style: live-action footage with natural lighting, realistic textures, "
            "and lifelike proportions."
        )
    return continuity + "\n\n" + result


def snapshot_characters(profiles, run_dir: str | os.PathLike[str], root: str | os.PathLike[str] | None = None) -> list[dict]:
    """Copy profiles and verified assets into a run-local immutable snapshot."""
    run_characters = Path(run_dir) / "characters"
    run_characters.mkdir(parents=True, exist_ok=True)
    library = _library_dir(root)
    snapshots = []
    for supplied in profiles:
        char_id, _ = _slug(supplied.get("name") if isinstance(supplied, dict) else "")
        current = _read_profile(library / char_id / "character.json")
        if supplied != current:
            raise ValueError(f"Character profile changed before snapshot: {char_id}")
        target = run_characters / char_id
        if target.exists():
            raise FileExistsError(f"Character snapshot already exists: {target}")
        staging = Path(tempfile.mkdtemp(prefix=f".{char_id}-", dir=run_characters))
        try:
            for key, filename in _ASSET_NAMES.items():
                record = current.get(key)
                if record:
                    shutil.copyfile(library / char_id / filename, staging / filename)
            (staging / "character.json").write_text(json.dumps(current, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            staging.rename(target)
        except BaseException:
            shutil.rmtree(staging, ignore_errors=True)
            raise
        snapshots.append(current)
    return snapshots


def verify_snapshots(profiles, run_dir: str | os.PathLike[str]) -> None:
    """Validate snapshot profile metadata and asset hashes without live registry access."""
    base = Path(run_dir) / "characters"
    for supplied in profiles:
        if not isinstance(supplied, dict):
            raise ValueError("Invalid snapshot profile.")
        char_id, _ = _slug(supplied.get("name"))
        path = base / char_id / "character.json"
        saved = _read_profile(path)
        if saved != supplied:
            raise ValueError(f"Character snapshot does not match run metadata: {char_id}")


def reference_image(profiles, run_dir: str | os.PathLike[str]) -> Path | None:
    """Return the sole saved reference image, if exactly one character has one."""
    with_images = [profile for profile in profiles if profile.get("image")]
    if len(with_images) > 1:
        raise ValueError("Multiple character reference images are available; provide a joint scene reference image.")
    if not with_images:
        return None
    verify_snapshots(with_images, run_dir)
    profile = with_images[0]
    return Path(run_dir) / "characters" / profile["id"] / profile["image"]["file"]

