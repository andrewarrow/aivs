"""Standard-library helpers for estimating dialogue and screenplay action time."""

from __future__ import annotations

import math
import re


WORDS_PER_MINUTE = 160
TURN_GAP_SECONDS = 0.2
PADDING_SECONDS = 1.0
ACTION_BEAT_SECONDS = 3.0
_WORD_RE = re.compile(r"[\w]+(?:['’][\w]+)*", re.UNICODE)
_SPEAKER_LABEL_RE = re.compile(r"(?:^|\b)[A-Z][A-Z0-9_-]*(?:\s*\([^)]*\))?:\s*$")
_SCENE_HEADING_RE = re.compile(r"^(?:INT\./EXT\.|EXT\./INT\.|INT\.|EXT\.|I/E\.)[ \t]+\S.*$", re.MULTILINE)
_TRANSITION_RE = re.compile(r"^(?:FADE (?:IN|OUT|TO BLACK)|(?:CUT|DISSOLVE|SMASH CUT|MATCH CUT) TO)[.:]?$", re.IGNORECASE)
_SCRIPT_DIALOGUE_RE = re.compile(r'^[A-Z][A-Z0-9_ .\'-]*(?:\s*\([^)]*\))?:\s*["“]')
_NARRATION_BEFORE_SPEECH_RE = re.compile(
    r"\b(?:says?|speaks?|replies?|asks?|responds?|whispers?|shouts?|tells?)"
    r"(?:\s+to\s+[\w'-]+)?\s*$",
    re.IGNORECASE,
)


def _strip_outer_quote_wrapper(prompt: str) -> str:
    """Remove a clear whole-prompt wrapper without eating quoted dialogue lines."""
    trimmed = prompt.strip()
    if len(trimmed) < 2:
        return prompt

    opening = trimmed[0]
    closing = '”' if opening == '“' else opening
    if opening not in ('"', '“') or trimmed[-1] != closing:
        return prompt

    body = trimmed[1:-1]
    other_quote = '"' if opening == '“' else ('“' if opening == '"' else None)
    if other_quote is not None and other_quote in body:
        return body

    # Same-kind inner quotes make a wrapper ambiguous. Narration or a speaker
    # label immediately before an inner quote makes it clear the outer pair
    # wraps a scene prompt rather than two standalone quoted lines.
    inner_quote = opening
    first_inner_quote = body.find(inner_quote)
    prefix = body[:first_inner_quote] if first_inner_quote >= 0 else ""
    if (_SPEAKER_LABEL_RE.search(prefix) or
            _NARRATION_BEFORE_SPEECH_RE.search(prefix)):
        return body

    # Otherwise preserve already valid quoted lines. If the outer pair masks
    # them so completely that parsing yields no words, try the unwrapped body.
    raw_segments = _parse_quoted_segments(trimmed)
    if any(_WORD_RE.search(segment) for segment in raw_segments):
        return prompt
    return body


def _parse_quoted_segments(prompt: str) -> list[str]:
    """Return paired straight or curly double-quoted segments in source order."""
    segments: list[str] = []
    start: int | None = None
    quote_kind: str | None = None

    for index, char in enumerate(prompt):
        if quote_kind is None:
            if char == '"':
                quote_kind, start = 'straight', index + 1
            elif char == '“':
                quote_kind, start = 'curly', index + 1
        elif quote_kind == 'straight' and char == '"':
            assert start is not None
            segments.append(prompt[start:index])
            quote_kind, start = None, None
        elif quote_kind == 'curly' and char == '”':
            assert start is not None
            segments.append(prompt[start:index])
            quote_kind, start = None, None

    return segments


def _quoted_segments(prompt: str) -> list[str]:
    """Extract dialogue, retrying after removal of a clear outer wrapper."""
    trimmed = prompt.strip()
    unwrapped = _strip_outer_quote_wrapper(prompt)
    if unwrapped != prompt:
        segments = _parse_quoted_segments(unwrapped)
        if any(_WORD_RE.search(segment) for segment in segments):
            return segments
    return _parse_quoted_segments(trimmed)


def _screenplay_actions(prompt: str) -> list[str]:
    """Find action paragraphs in the workspace's labeled screenplay format.

    Only text after a scene heading is eligible, so ordinary prose prompts and
    cast/camera instructions keep their existing speech-only estimates.
    """
    prompt = _strip_outer_quote_wrapper(prompt)
    heading = _SCENE_HEADING_RE.search(prompt)
    if heading is None:
        return []
    actions = []
    for block in re.split(r"\n\s*\n", prompt[heading.end():]):
        text = block.strip()
        if not text or _SCENE_HEADING_RE.fullmatch(text) or _TRANSITION_RE.fullmatch(text):
            continue
        if _SCRIPT_DIALOGUE_RE.match(text) or text.startswith(('"', '“', '(')):
            continue
        actions.append(text)
    return actions


def duration_estimate(prompt: str) -> dict:
    """Estimate dialogue time plus room for screenplay action paragraphs.

    This intentionally uses quoted segments as a simple dialogue marker; it does
    not attempt to distinguish spoken dialogue from film or book quotations.
    """
    dialogue = _quoted_segments(prompt)
    word_count = sum(len(_WORD_RE.findall(segment)) for segment in dialogue)
    if word_count == 0:
        raise ValueError('No quoted dialogue found. Put spoken words in quotes or set --seconds.')

    turn_count = len([segment for segment in dialogue if _WORD_RE.search(segment)])
    speech_seconds = (
        word_count * 60 / WORDS_PER_MINUTE
        + TURN_GAP_SECONDS * (turn_count - 1)
        + PADDING_SECONDS
    )
    actions = _screenplay_actions(prompt)
    action_seconds = len(actions) * ACTION_BEAT_SECONDS
    estimate = {
        "seconds": max(2, math.ceil(speech_seconds + action_seconds)),
        "word_count": word_count,
        "turn_count": turn_count,
        "words_per_minute": WORDS_PER_MINUTE,
        "turn_gap_seconds": TURN_GAP_SECONDS,
        "padding_seconds": PADDING_SECONDS,
        "method": "quoted-dialogue-estimate",
        "dialogue": dialogue,
    }
    if actions:
        estimate.update(
            method="screenplay-dialogue-and-action-estimate",
            action_count=len(actions),
            action_beat_seconds=ACTION_BEAT_SECONDS,
            action_seconds=action_seconds,
        )
    return estimate
