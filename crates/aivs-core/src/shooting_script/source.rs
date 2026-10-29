//! Fountain-style source beats and the exact inputs to the legacy JSON digest.
use super::{
    STORYBOARD_POLICY, array, bounded, optional_fields, prompt, regex, same_keys, unicode,
};
use crate::continuity::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PROFILE_FIELDS: &[&str] = &[
    "id",
    "name",
    "description",
    "voice_description",
    "voice_id",
    "image",
    "scene_image",
    "voice_sample",
];

struct Parser {
    beats: Vec<Value>,
    action: Vec<String>,
    scene: Value,
    scene_id: u64,
    transition: Value,
    roles: BTreeMap<String, String>,
}

impl Parser {
    fn flush_action(&mut self) {
        if !self.action.is_empty() {
            let text = self.action.join(" ").trim().to_owned();
            if !text.is_empty() {
                self.beats.push(
                    json!({"id": self.beats.len() + 1, "kind": "action", "text": text,
                    "scene": self.scene, "scene_id": self.scene_id, "transition": self.transition}),
                );
                self.transition = Value::Null;
            }
            self.action.clear();
        }
    }

    fn dialogue(&mut self, role: &str, cue: Option<&str>, speech: &str) -> Result<()> {
        let canonical = self
            .roles
            .get(&unicode::casefold(role))
            .ok_or_else(|| format!("Dialogue speaker {role:?} has no cast binding."))?;
        let value = json!(speech);
        let speech = bounded(&value, &format!("Dialogue for {canonical}"), 4000)?;
        self.beats.push(json!({"id": self.beats.len() + 1, "kind": "dialogue", "speaker": canonical,
            "cue": cue, "text": speech, "scene": self.scene, "scene_id": self.scene_id, "transition": self.transition}));
        self.transition = Value::Null;
        Ok(())
    }
}

/// Split only speech that cannot fit in one generation. Keep exact substrings,
/// preferring sentence/clause boundaries near a balanced division of the line.
fn dialogue_parts(mut text: &str) -> Vec<&str> {
    let words = regex(prompt::WORD_PATTERN);
    let mut parts = Vec::new();
    loop {
        let matches = words.find_iter(text).collect::<Vec<_>>();
        if matches.len() <= prompt::MAX_DIALOGUE_WORDS {
            parts.push(text);
            return parts;
        }
        let count = matches.len().div_ceil(prompt::MAX_DIALOGUE_WORDS);
        let target = matches.len().div_ceil(count);
        let cut = (target..=prompt::MAX_DIALOGUE_WORDS)
            .map(|index| {
                let end = matches[index - 1].end();
                let next = matches[index].start();
                let gap = &text[end..next];
                let cut = gap
                    .rfind(char::is_whitespace)
                    .map_or(next, |offset| end + offset);
                let punctuation = text[..cut]
                    .trim_end()
                    .trim_end_matches(['\"', '”', '\'', '’', ')']);
                let rank = if punctuation.ends_with(['.', '?', '!']) {
                    0
                } else if punctuation.ends_with([',', ';', ':', '—', '–']) {
                    1
                } else {
                    2
                };
                ((rank, index.abs_diff(target)), cut)
            })
            .min_by_key(|(score, _)| *score)
            .unwrap()
            .1;
        parts.push(text[..cut].trim_end());
        text = text[cut..].trim_start();
    }
}

fn segment_dialogue(beats: &mut Vec<Value>) -> bool {
    let mut segmented = Vec::new();
    let mut changed = false;
    for beat in beats.iter() {
        let parts = if beat["kind"] == "dialogue" {
            dialogue_parts(beat["text"].as_str().unwrap())
        } else {
            vec![beat["text"].as_str().unwrap()]
        };
        changed |= parts.len() > 1;
        for (index, text) in parts.iter().enumerate() {
            let mut part = beat.clone();
            part["id"] = json!(segmented.len() + 1);
            part["text"] = json!(text);
            if parts.len() > 1 {
                part["dialogue_part"] = json!(index + 1);
                part["dialogue_parts"] = json!(parts.len());
                if let Some(key) = beat["stable_id"].as_str() {
                    part["stable_id"] = json!(format!("{key}:dialogue:{}", index + 1));
                }
                if index > 0 {
                    part["transition"] = Value::Null;
                }
            }
            segmented.push(part);
        }
    }
    *beats = segmented;
    changed
}

pub(super) fn parse(request: &Value, profiles: &Value) -> Result<Value> {
    if !request.is_object() || request["version"].as_f64() != Some(1.0) {
        return Err("Shooting-script request version must be 1.".into());
    }
    let format = request
        .get("render_format")
        .map(crate::render_formats::resolve)
        .transpose()?;
    if !request["character_conditioning"].is_null()
        && request["character_conditioning"] != "saved-character-assets-v1"
    {
        return Err("Unrecognized saved character conditioning policy.".into());
    }
    if !request["storyboard_policy"].is_null() && request["storyboard_policy"] != STORYBOARD_POLICY
    {
        return Err("Unrecognized storyboard planning policy.".into());
    }
    let voice_mode = request.get("voice_mode").unwrap_or(&Value::Null);
    if request.get("voice_mode").is_some()
        && !matches!(voice_mode.as_str(), Some("saved" | "native"))
    {
        return Err("Voice mode must be saved or native.".into());
    }
    bounded(&request["script"], "Screenplay", 100_000)?;
    let script = request["script"].as_str().unwrap();
    let cast = array(
        &request["cast"],
        "Cast must be a list of role and character bindings.",
    )?;
    let mut known = BTreeMap::new();
    for profile in array(profiles, "Character profiles must be a list.")? {
        if !profile.is_object() {
            return Err("Invalid character profile.".into());
        }
        let id = bounded(&profile["id"], "Character id", 128)?;
        if known.contains_key(id) {
            return Err(format!("Duplicate character profile: {id}."));
        }
        bounded(
            &profile["description"],
            &format!("Appearance for {id}"),
            4000,
        )?;
        if profile
            .get("voice_description")
            .is_some_and(|voice| !voice.is_string())
        {
            return Err(format!("Voice description for {id} must be text."));
        }
        known.insert(id, profile);
    }
    let mut bindings = Vec::new();
    let mut roles = BTreeMap::new();
    for binding in cast {
        if !same_keys(binding, &["role", "character"]) {
            return Err("Each cast binding needs role and character only.".into());
        }
        let role = bounded(&binding["role"], "Cast role", 128)?;
        let id = bounded(&binding["character"], "Cast character", 128)?;
        let folded = unicode::casefold(role);
        if roles.contains_key(&folded) {
            return Err(format!("Duplicate cast role: {role}."));
        }
        if !known.contains_key(id) {
            return Err(format!("Unknown cast character: {id}."));
        }
        roles.insert(folded, role.to_owned());
        bindings.push(json!({"role": role, "character": id}));
    }
    let mut parser = Parser {
        beats: Vec::new(),
        action: Vec::new(),
        scene: Value::Null,
        scene_id: 0,
        transition: Value::Null,
        roles,
    };
    let mut fade_out = false;
    // Match Python splitlines, including CRLF and Unicode paragraph separators.
    let normalized = script.replace("\r\n", "\n");
    let lines = normalized
        .split([
            '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
        ])
        .map(str::trim)
        .collect::<Vec<_>>();
    let heading = regex(
        r"(?i)^(?:INT\.?\s*/\s*EXT\.?|EXT\.?\s*/\s*INT\.?|I\s*/\s*E\.?|INT\.?|EXT\.?|EST\.?)\s+",
    );
    let transition = regex(
        r"(?i)^(?:FADE\s+(?:IN|OUT)\.?|CUT TO:|DISSOLVE TO:|SMASH CUT TO:|THE END\.?|[A-Z][A-Z\s]+ TO:)\s*$",
    );
    let inline = regex(
        r#"^(?P<role>[\p{L}\p{N}_][\p{L}\p{N}_ .'-]*?)(?:\s*\((?P<cue>[^\n()]*)\))?\s*:\s*["“](?P<speech>.*?)["”]\s*$"#,
    );
    let role_pattern = regex(r"^[\p{L}\p{N}_][\p{L}\p{N}_ .'-]*(?:\s*\([^\n()]*\))?$");
    let split_role = regex(r"^(?P<role>.+?)(?:\s*\((?P<cue>[^()]*)\))?$");
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.is_empty() {
            parser.flush_action();
            continue;
        }
        if heading.is_match(line) {
            parser.flush_action();
            parser.scene = json!(line);
            parser.scene_id += 1;
            parser.transition = Value::Null;
            continue;
        }
        if transition.is_match(line) {
            parser.flush_action();
            if regex(r"(?i)^FADE\s+OUT\.?$").is_match(line) {
                fade_out = true;
            } else {
                parser.transition = json!(line);
                if !regex(r"(?i)^FADE\s+IN\.?$").is_match(line) {
                    parser.scene_id += 1;
                }
            }
            continue;
        }
        if let Some(capture) = inline.captures(line) {
            parser.flush_action();
            parser.dialogue(
                capture["role"].trim(),
                capture.name("cue").map(|m| m.as_str()),
                &capture["speech"],
            )?;
            continue;
        }
        if role_pattern.is_match(line) {
            let capture = split_role
                .captures(line)
                .ok_or_else(|| format!("Invalid dialogue speaker: {line:?}."))?;
            let role = capture["role"].trim();
            let mut cue = capture
                .name("cue")
                .map(|m| m.as_str().trim())
                .filter(|cue| !cue.is_empty());
            if role.to_uppercase() == role
                && role.chars().count() <= 48
                && role.split_whitespace().count() <= 3
            {
                let mut next = i;
                if next < lines.len() && regex(r"^\([^()]*\)$").is_match(lines[next]) {
                    let value = lines[next][1..lines[next].len() - 1].trim();
                    if !value.is_empty() {
                        cue = Some(value);
                    }
                    next += 1;
                }
                let start = next;
                while next < lines.len() && !lines[next].is_empty() {
                    next += 1;
                }
                if next > start {
                    if !parser.roles.contains_key(&unicode::casefold(role)) {
                        if !regex(r"^(?:A|AN|THE)\b").is_match(role) {
                            return Err(format!("Dialogue speaker {role:?} has no cast binding."));
                        }
                        parser.action.push(line.to_owned());
                        continue;
                    }
                    parser.flush_action();
                    let speech = lines[start..next].join(" ");
                    let speech = if speech.chars().count() >= 2
                        && speech.starts_with(['"', '“'])
                        && speech.ends_with(['"', '”'])
                    {
                        let first = speech.chars().next().unwrap().len_utf8();
                        let last = speech.chars().next_back().unwrap().len_utf8();
                        &speech[first..speech.len() - last]
                    } else {
                        &speech
                    };
                    parser.dialogue(role, cue, speech)?;
                    i = next;
                    continue;
                }
            }
        }
        parser.action.push(line.to_owned());
    }
    parser.flush_action();
    if parser.beats.is_empty() {
        return Err("Screenplay has no action or dialogue beats.".into());
    }
    let relevant = bindings
        .iter()
        .map(|binding| {
            let profile = known[binding["character"].as_str().unwrap()];
            PROFILE_FIELDS
                .iter()
                .map(|key| ((*key).to_owned(), profile[*key].clone()))
                .collect::<serde_json::Map<_, _>>()
        })
        .collect::<Vec<_>>();
    let mut identity = json!({"script": script, "cast": bindings, "profiles": relevant});
    let mut source = json!({"version": 1, "script": script, "cast": bindings, "characters": relevant,
        "beats": parser.beats, "fade_out": fade_out});
    if let Some(keys) = request.get("beat_keys") {
        let keys = keys
            .as_array()
            .ok_or("Source beat keys must be an array.")?;
        let beats = source["beats"].as_array_mut().unwrap();
        if keys.len() != beats.len()
            || keys.iter().any(|key| {
                !key.as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 128)
            })
            || keys
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != keys.len()
        {
            return Err(
                "Stable source keys must identify every action and dialogue beat once.".into(),
            );
        }
        for (beat, key) in beats.iter_mut().zip(keys) {
            beat["stable_id"] = key.clone();
        }
        source["beat_keys"] = json!(keys);
        identity["beat_keys"] = json!(keys);
    }
    // Editor keys still address original blocks. Derive part keys only after
    // binding those blocks, and retain the original beat_keys for round trips.
    if segment_dialogue(source["beats"].as_array_mut().unwrap()) {
        // Old unsplit coverage must not share source hashes or cached revisions.
        identity["dialogue_segmentation"] = json!("timed-parts-v1");
    }
    if let Some(key) = request.get("scene_key") {
        bounded(key, "Stable scene key", 128)?;
        source["scene_key"] = key.clone();
        identity["scene_key"] = key.clone();
    }
    if let Some(set) = request.get("set").filter(|set| !set.is_null()) {
        crate::sets::validate(set)?;
        identity["set"] = set.clone();
        source["set"] = set.clone();
    }
    for key in ["character_conditioning", "storyboard_policy"] {
        if !request[key].is_null() {
            optional_fields(&mut identity, request, &[key]);
            optional_fields(&mut source, request, &[key]);
        }
    }
    if voice_mode == "native" {
        identity["voice_mode"] = voice_mode.clone();
        source["voice_mode"] = voice_mode.clone();
    }
    if let Some(format) = format {
        identity["render_format"] = format.clone();
        source["render_format"] = format;
    }
    // The adapter hashes this identity with the existing Python codec.
    Ok(json!({"source": source, "identity": identity}))
}
