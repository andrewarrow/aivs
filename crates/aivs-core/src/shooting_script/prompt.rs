//! Exact dialogue substitution and stable, source-backed render instructions.
use super::{PLANNER_VERSION, PLANNING_POLICY, array, regex};
use crate::continuity::Result;
use regex::RegexBuilder;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn word_count(text: &str) -> usize {
    regex(r"[\p{L}\p{N}_]+(?:['’][\p{L}\p{N}_]+)*")
        .find_iter(text)
        .count()
}

pub(super) fn estimated_tokens(text: &str) -> usize {
    (word_count(text) * 2).max(text.chars().count().div_ceil(3))
}

fn phrase(value: &str) -> String {
    let value = value.trim().trim_end_matches('.');
    let mut chars = value.chars();
    let value = chars
        .next()
        .map(|first| first.to_lowercase().collect::<String>() + chars.as_str())
        .unwrap_or_default();
    if regex(r"(?i)^(?:a|an|the)\s").is_match(&value) {
        value
    } else {
        format!(
            "{} {value}",
            if value.is_empty() || value.starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            }
        )
    }
}

fn cast_context(
    source: &Value,
    identities: &Value,
    visible: &Value,
    speaking: &[&str],
    storyboard: bool,
    legacy: bool,
) -> Result<String> {
    let mut parts = Vec::new();
    let modern = !identities.is_null();
    for binding in array(&source["cast"], "Source needs cast bindings.")? {
        let role = binding["role"].as_str().ok_or("Invalid cast role.")?;
        let profile = array(&source["characters"], "Source needs character profiles.")?
            .iter()
            .find(|profile| profile["id"] == binding["character"])
            .ok_or("Missing cast profile.")?;
        let identity = identities
            .as_array()
            .and_then(|items| items.iter().find(|item| item["role"] == role))
            .unwrap_or(profile);
        if !storyboard
            && (!modern
                || visible
                    .as_array()
                    .is_some_and(|roles| roles.iter().any(|r| r == role)))
        {
            parts.push(format!(
                "{role} looks like {}",
                phrase(
                    identity["description"]
                        .as_str()
                        .ok_or("Missing cast appearance.")?
                )
            ));
        }
        if legacy {
            let voice = identity["voice_description"]
                .as_str()
                .unwrap_or_default()
                .trim();
            if !voice.is_empty() && (!modern || speaking.contains(&role)) {
                parts.push(format!("{role} speaks with {}", phrase(voice)));
            }
        }
    }
    Ok(if parts.is_empty() {
        String::new()
    } else {
        parts.join(". ").trim_end_matches(['.', ' ']).to_owned() + ". "
    })
}

fn speaker_context(source: &Value, identities: &Value, role: &str) -> String {
    let identity = identities
        .as_array()
        .and_then(|items| items.iter().find(|item| item["role"] == role))
        .or_else(|| {
            let binding = source["cast"]
                .as_array()?
                .iter()
                .find(|item| item["role"] == role)?;
            source["characters"]
                .as_array()?
                .iter()
                .find(|profile| profile["id"] == binding["character"])
        });
    let Some(identity) = identity else {
        return role.to_owned();
    };
    // Bind the visible performer and voice at the utterance itself. A role
    // name alone, followed much later by a cast/voice list, can leave speaker
    // assignment ambiguous during joint picture-and-audio generation.
    let appearance = identity["description"].as_str().unwrap_or_default().trim();
    let voice = identity["voice_description"]
        .as_str()
        .unwrap_or_default()
        .trim();
    let mut speaker = role.to_owned();
    if !appearance.is_empty() {
        speaker.push_str(&format!(", {},", phrase(appearance)));
    }
    if !voice.is_empty() {
        speaker.push_str(&format!(" speaking with {},", phrase(voice)));
    }
    speaker
}

fn raw_dialogue(template: &str, dialogue: &str) -> Result<bool> {
    let pattern = RegexBuilder::new(&regex::escape(dialogue))
        .case_insensitive(true)
        .build()
        .map_err(|error| error.to_string())?;
    let word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut cursor = 0;
    while let Some(found) = pattern.find_at(template, cursor) {
        if !template[..found.start()]
            .chars()
            .next_back()
            .is_some_and(word)
            && !template[found.end()..].chars().next().is_some_and(word)
        {
            return Ok(true);
        }
        // An invalid boundary can hide a later overlapping literal match.
        let Some(next) = template[found.start()..].chars().next() else {
            break;
        };
        cursor = found.start() + next.len_utf8();
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile(
    template: &str,
    source: &Value,
    identities: &Value,
    visible: &Value,
    required: &[u64],
    beats: &BTreeMap<u64, &Value>,
    request: &Value,
    storyboard: bool,
    number: usize,
    legacy: bool,
) -> Result<String> {
    if regex(r"(?i)-{3,}>\s*done!").is_match(template) {
        return Err(format!(
            "Shot {number} template contains a chat completion footer. Remove the footer; keep only film direction and source dialogue placeholders."
        ));
    }
    if template.contains(['"', '“', '”']) {
        return Err(format!(
            "Shot {number} template contains quoted text outside source dialogue placeholders."
        ));
    }
    if regex(r"(?i)\b(?:fade\s+(?:in|out)|subtitles?|captions?|title\s+card)\b").is_match(template)
    {
        return Err(format!(
            "Shot {number} includes text or a fade that the final assembly owns."
        ));
    }
    let placeholders = regex(r"\{\{line_([0-9]+)\}\}");
    let ids: Vec<_> = placeholders
        .captures_iter(template)
        .map(|m| m[1].parse::<u64>())
        .collect();
    if ids.iter().any(|id| id.is_err())
        || ids
            .iter()
            .filter_map(|id| id.as_ref().ok().copied())
            .collect::<Vec<_>>()
            != required
    {
        return Err(format!(
            "Shot {number} dialogue placeholders must match source lines exactly and in order."
        ));
    }
    let stripped = placeholders.replace_all(template, "");
    if stripped.contains("{{") || stripped.contains("}}") {
        return Err(format!("Shot {number} has an invalid placeholder."));
    }
    for beat in beats.values().filter(|beat| beat["kind"] == "dialogue") {
        if raw_dialogue(
            template,
            beat["text"].as_str().ok_or("Invalid source dialogue.")?,
        )? {
            return Err(format!(
                "Shot {number} contains raw dialogue; use placeholders."
            ));
        }
    }
    if regex(r"(?i)\b(?:says|speaks|talks|whispers|shouts|mutters|asks|replies|exclaims)\b")
        .is_match(template)
    {
        return Err(format!(
            "Shot {number} contains unsourced speech direction; use dialogue placeholders."
        ));
    }
    let mut action = placeholders
        .replace_all(template, |capture: &regex::Captures<'_>| {
            let beat = beats[&capture[1].parse::<u64>().expect("validated placeholder")];
            let cue = beat["cue"]
                .as_str()
                .filter(|cue| !cue.is_empty())
                .map(|cue| format!(" ({cue})"))
                .unwrap_or_default();
            let role = beat["speaker"].as_str().unwrap_or_default();
            let speaker = if legacy {
                role.to_owned()
            } else {
                speaker_context(source, identities, role)
            };
            format!(
                "{}{cue} says, \"{}\".",
                speaker,
                beat["text"].as_str().unwrap_or_default()
            )
        })
        .trim_end()
        .to_owned();
    if !action.ends_with(['.', '!', '?']) {
        action.push('.');
    }
    let speech = match (!identities.is_null(), required.is_empty()) {
        (false, false) => {
            "The only spoken words are the quoted dialogue, delivered exactly once in order. No added words, repetitions, or unscripted vocalizations. Only the current speaker moves their mouth. After the final word, mouths rest unless the source action requires otherwise. "
        }
        (false, true) => "There is no spoken dialogue in this shot. ",
        (true, false) => {
            "The only spoken words are the quoted dialogue, delivered exactly once in order. During each line, listeners' lips rest. After the final line, mouths rest except for explicitly staged source actions. Nonverbal vocal sounds occur only at their specified source moments. "
        }
        (true, true) => {
            "No spoken words. Only the staged environmental sounds and source nonverbal vocal actions are audible. "
        }
    };
    let framing = request
        .get("render_format")
        .map(crate::render_formats::framing)
        .unwrap_or(Value::String(String::new()));
    let speaking = required
        .iter()
        .filter_map(|id| beats[id]["speaker"].as_str())
        .collect::<Vec<_>>();
    let cast = cast_context(source, identities, visible, &speaking, storyboard, legacy)?;
    let image = if storyboard {
        "Animate the supplied storyboard first frame, preserving its cast, composition, costumes and prop state as the action begins. "
    } else {
        ""
    };
    let voice = if source["voice_mode"] == "native" && !required.is_empty() {
        "Keep each speaker's described voice consistent throughout this shot. "
    } else {
        ""
    };
    Ok(format!(
        "{image}{action} {}{cast}{speech}{voice}",
        framing.as_str().unwrap_or_default()
    ))
}

pub(super) fn display(plan: &Value) -> Result<String> {
    let mut lines = Vec::new();
    if plan["planner_version"] == PLANNER_VERSION {
        lines.extend(["Planning: storyboard direction, image-based motion direction and source-fidelity check completed.".into(), String::new()]);
    }
    if plan.get("preproduction_review").is_some() {
        let stages = if plan["planning_policy"] == PLANNING_POLICY {
            "draft, intent revision, physical rehearsal, render feasibility, final approval"
        } else {
            "draft, intent revision, physical rehearsal, final approval"
        };
        lines.extend([format!("Preproduction: {stages} completed."), String::new()]);
    }
    if let Some(name) = plan.get("render_format") {
        let spec = crate::render_formats::resolve(name)?;
        lines.extend([
            format!(
                "Render: {} × {} | {} | {}",
                spec["width"],
                spec["height"],
                spec["aspect_ratio"].as_str().unwrap_or_default(),
                spec["name"].as_str().unwrap_or_default()
            ),
            String::new(),
        ]);
    }
    for (index, shot) in array(&plan["shots"], "Missing shooting-script shots.")?
        .iter()
        .enumerate()
    {
        let ids = array(&shot["beat_ids"], "Missing shot beat IDs.")?
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "SHOT {} | {}s | beats {ids}",
            index + 1,
            shot["seconds"]
        ));
        if let Some(still) = shot.get("storyboard_prompt") {
            lines.push(format!(
                "Storyboard: {}",
                still.as_str().unwrap_or_default()
            ));
        }
        if let Some(purpose) = shot.get("dramatic_purpose") {
            lines.push(format!("Purpose: {}", purpose.as_str().unwrap_or_default()));
            lines.push(format!(
                "Coverage: {}",
                shot["coverage"].as_str().unwrap_or_default()
            ));
            for requirement in array(&shot["visual_contract"], "Missing visual contract.")? {
                lines.push(format!(
                    "Must show: {}",
                    requirement.as_str().unwrap_or_default()
                ));
            }
            for part in array(&shot["action_coverage"], "Missing action coverage.")? {
                lines.push(format!(
                    "Action beat {}, part {}/{}: {}",
                    part["beat_id"],
                    part["part"],
                    part["parts"],
                    part["action"].as_str().unwrap_or_default()
                ));
            }
        }
        lines.extend([
            format!(
                "Start: {}",
                shot["start_state"].as_str().unwrap_or_default()
            ),
            shot["prompt"].as_str().unwrap_or_default().into(),
            format!("End: {}", shot["end_state"].as_str().unwrap_or_default()),
            String::new(),
        ]);
    }
    if plan["fade_out"] == true {
        lines.push("Fade out at the end of the final shot.".into());
    }
    Ok(lines.join("\n").trim_end().to_owned() + "\n")
}

pub(super) fn instruction_context(source: &Value) -> Value {
    let mut assets = serde_json::json!({
            "new_scene_opening": "When exactly one saved performer is visible, the actual first frame is their canonical portrait, provided as an image input. Preserve that face. Explicitly cut after this opening to the scripted location, costume, and framing before source action begins; budget the cut. A multi-person opening has no portrait conditioning.",
            "voice": "A segment with exactly one saved speaking performer has its native soundtrack converted to their saved voice sample after generation. Picture is unchanged. Multiple speakers retain generated voices. Do not split source-required overlap just to enable conversion. Whole-track conversion may affect ambience; prefer clean dialogue when source permits.",
            "limits": "Portrait anchoring and voice conversion do not guarantee identity or lip sync.",
            "portrait_inputs": "Attached portraits follow the unique source character order, omitting characters without portraits."
    });
    if source["storyboard_policy"] == super::STORYBOARD_POLICY {
        assets["new_scene_opening"] = serde_json::json!(
            "Every segment starts on its own locally generated storyboard image, including reverse angles and later segments in the same scene. Saved portraits guide actor identity during still-image generation; they are not themselves video opening frames. The storyboard_prompt establishes the intended opening composition, costumes, prop ownership and story state. Animate that composition directly; do not begin on a portrait or inherit the prior video's final frame."
        );
        assets["limits"] = serde_json::json!(
            "Generated storyboards and voice conversion do not guarantee identity, continuity or lip sync."
        );
    }
    if source["voice_mode"] == "native" {
        assets["voice"] = serde_json::json!(
            "Keep the model's native generated soundtrack for every shot. No saved-voice conversion runs. Use the saved performers' voice descriptions as prompt guidance; voice samples will not condition audio. Keep one speaking role per clip even in native mode. Do not split a source line or serialize required overlap to accommodate a voice converter."
        );
        assets["limits"] = serde_json::json!(
            "Portraits, generated storyboards and native voice descriptions do not guarantee identity, continuity or lip sync."
        );
    }
    let mut body = serde_json::json!({
        "script": source["script"], "cast": source["cast"], "characters": source["characters"],
        "storyboard_policy": super::STORYBOARD_POLICY, "beats": source["beats"],
        "fade_out_at_end": source["fade_out"],
        "generation_capabilities": {"planning_policy": PLANNING_POLICY,
            "max_speaking_roles_per_shot": 1,
            "independent_storyboard_first_frame_per_shot": true, "joint_picture_and_audio": true,
            "audio_moves_between_generations": false, "saved_character_assets": assets},
        "timing_guidance": {"words_per_minute": 160, "settling_seconds": 0.5},
    });
    super::optional_fields(&mut body, source, &["render_format", "set"]);
    if source.get("set").is_some() {
        body["assigned_set_guidance"] = serde_json::json!(
            "The final attachment after the performer portraits is the assigned set image. Use this exact location for the selected scene. Preserve its architecture, permanent furniture, materials and spatial layout in every angle. Stage source action within this set; do not invent a replacement location. Describe the assigned set in world.environments reference_prompt; production reuses its saved image as the environment plate. Only cast, story props and source-required changes are added to storyboard frames."
        );
    }
    body
}
