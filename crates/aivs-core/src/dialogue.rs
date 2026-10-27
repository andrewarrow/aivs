//! Dialogue and labeled screenplay action timing, preserving the CLI estimate contract.
use crate::continuity::Result;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_]+(?:['’][\p{L}\p{N}_]+)*").unwrap());
static SPEAKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\b)[A-Z][A-Z0-9_-]*(?:\s*\([^)]*\))?:\s*$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(?:INT\./EXT\.|EXT\./INT\.|INT\.|EXT\.|I/E\.)[ \t]+\S.*$").unwrap()
});
static TRANSITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:FADE (?:IN|OUT|TO BLACK)|(?:CUT|DISSOLVE|SMASH CUT|MATCH CUT) TO)[.:]?$")
        .unwrap()
});
static SCRIPT_DIALOGUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[A-Z][A-Z0-9_ .'-]*(?:\s*\([^)]*\))?:\s*["“]"#).unwrap());
static NARRATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:says?|speaks?|replies?|asks?|responds?|whispers?|shouts?|tells?)(?:\s+to\s+[\w'-]+)?\s*$").unwrap()
});
static BLOCKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());

fn quoted(prompt: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut opened = None;
    for (index, c) in prompt.char_indices() {
        match opened {
            None if c == '"' || c == '“' => {
                opened = Some((index + c.len_utf8(), if c == '“' { '”' } else { '"' }))
            }
            Some((start, close)) if c == close => {
                segments.push(&prompt[start..index]);
                opened = None;
            }
            _ => {}
        }
    }
    segments
}

pub fn unwrap_prompt(prompt: &str) -> &str {
    let trimmed = prompt.trim();
    let Some(opening) = trimmed.chars().next() else {
        return prompt;
    };
    let closing = if opening == '“' { '”' } else { opening };
    if !matches!(opening, '"' | '“') || trimmed.chars().count() < 2 || !trimmed.ends_with(closing)
    {
        return prompt;
    }
    let body = &trimmed[opening.len_utf8()..trimmed.len() - closing.len_utf8()];
    let other = if opening == '“' { '"' } else { '“' };
    if body.contains(other) {
        return body;
    }
    let prefix = body.find(opening).map_or("", |index| &body[..index]);
    if SPEAKER.is_match(prefix) || NARRATION.is_match(prefix) {
        return body;
    }
    if quoted(trimmed).iter().any(|segment| WORD.is_match(segment)) {
        prompt
    } else {
        body
    }
}

pub fn estimate(prompt: &str) -> Result<Value> {
    let unwrapped = unwrap_prompt(prompt);
    let mut dialogue = quoted(unwrapped);
    if unwrapped == prompt || !dialogue.iter().any(|segment| WORD.is_match(segment)) {
        dialogue = quoted(prompt.trim());
    }
    let words = dialogue
        .iter()
        .map(|segment| WORD.find_iter(segment).count())
        .sum::<usize>();
    if words == 0 {
        return Err(
            "No quoted dialogue found. Put spoken words in quotes or set --seconds.".into(),
        );
    }
    let turns = dialogue
        .iter()
        .filter(|segment| WORD.is_match(segment))
        .count();
    let actions = HEADING.find(unwrapped).map_or(0, |heading| {
        BLOCKS
            .split(&unwrapped[heading.end()..])
            .filter(|block| {
                let text = block.trim();
                !text.is_empty()
                    && !HEADING
                        .find(text)
                        .is_some_and(|m| m.start() == 0 && m.end() == text.len())
                    && !TRANSITION.is_match(text)
                    && !SCRIPT_DIALOGUE.is_match(text)
                    && !text.starts_with(['"', '“', '('])
            })
            .count()
    });
    let speech = words as f64 * 60.0 / 160.0 + 0.2 * (turns - 1) as f64 + 1.0;
    let mut estimate = json!({"seconds": ((speech+actions as f64*3.0).ceil() as u64).max(2), "word_count": words,
        "turn_count": turns, "words_per_minute": 160, "turn_gap_seconds": 0.2, "padding_seconds": 1.0,
        "method": "quoted-dialogue-estimate", "dialogue": dialogue});
    if actions > 0 {
        estimate["method"] = json!("screenplay-dialogue-and-action-estimate");
        estimate["action_count"] = json!(actions);
        estimate["action_beat_seconds"] = json!(3.0);
        estimate["action_seconds"] = json!(actions as f64 * 3.0);
    }
    Ok(estimate)
}
