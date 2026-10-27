//! Deterministic source coverage, timing, prompt compilation and continuity gates.
use super::{
    DIALOGUE_POLICY, PLANNER_VERSION, PLANNING_POLICY, STORYBOARD_POLICY, array, bounded,
    optional_fields, prompt, same_keys,
};
use crate::continuity::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn canonicalize_features(shots: &mut [Value], world: &Value) {
    let canonical: BTreeMap<String, String> = world["props"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|prop| {
            std::iter::once(prop).chain(prop["components"].as_array().into_iter().flatten())
        })
        .filter_map(|entity| {
            let id = entity["id"].as_str()?;
            let features = entity["features"]
                .as_array()?
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()?;
            (!features.is_empty()).then(|| (id.to_owned(), features.join("; ")))
        })
        .collect();
    let normalize = |value: &mut Value, expected: &str| {
        // Feature lists are prose, so capitalization and separator spacing do
        // not describe a transformation. Preserve all wording and list order.
        if value.as_str().is_some_and(|actual| {
            actual
                .split(';')
                .map(|feature| feature.trim().to_ascii_lowercase())
                .eq(expected
                    .split(';')
                    .map(|feature| feature.trim().to_ascii_lowercase()))
        }) {
            *value = json!(expected);
        }
    };
    for shot in shots {
        let contract = &mut shot["continuity"];
        for endpoint in ["opening", "ending"] {
            for prop in contract[endpoint]["props"]
                .as_array_mut()
                .into_iter()
                .flatten()
            {
                let Some(expected) = prop["id"].as_str().and_then(|id| canonical.get(id)) else {
                    continue;
                };
                for attribute in prop["physical"].as_array_mut().into_iter().flatten() {
                    if attribute["key"] == "distinguishing_features" {
                        normalize(&mut attribute["value"], expected);
                    }
                }
            }
        }
        for event in contract["transitions"].as_array_mut().into_iter().flatten() {
            if event["field"] == "physical.distinguishing_features" {
                if let Some(expected) = event["entity"]
                    .as_str()
                    .and_then(|entity| entity.strip_prefix("prop:"))
                    .and_then(|id| canonical.get(id))
                {
                    normalize(&mut event["before"], expected);
                    normalize(&mut event["after"], expected);
                }
            }
        }
    }
}

pub(super) fn compile(
    candidate: &Value,
    source: &Value,
    request: &Value,
    model_tokenizer: bool,
) -> Result<Value> {
    let proposed = array(
        &candidate["shots"],
        "Codex response must contain a shots list.",
    )?;
    let storyboard = source["storyboard_policy"] == STORYBOARD_POLICY;
    if !candidate["dialogue_policy"].is_null() && candidate["dialogue_policy"] != DIALOGUE_POLICY {
        return Err("Unrecognized dialogue planning policy.".into());
    }
    // Existing saved runs keep their original approved coverage on Resume.
    // New storyboard plans must isolate each speaking role in its own clip.
    let single_speaker = candidate["dialogue_policy"] == DIALOGUE_POLICY
        || (storyboard && candidate["source_sha256"].is_null());
    if candidate
        .get("source_sha256")
        .is_some_and(|hash| hash != &source["source_sha256"])
    {
        return Err("Shooting script was made for a different screenplay or cast profile, or render format.".into());
    }
    if candidate["render_format"] != request["render_format"]
        && candidate.get("source_sha256").is_some()
    {
        return Err(
            "Saved shooting script has a different render format; generate a new plan.".into(),
        );
    }
    if candidate["storyboard_policy"] != source["storyboard_policy"] {
        return Err(
            "Shooting script has a different storyboard policy; generate a new plan.".into(),
        );
    }
    if !candidate["planner_version"].is_null() && candidate["planner_version"] != PLANNER_VERSION {
        return Err("Shooting script uses a different planner version.".into());
    }
    if !candidate["planning_stage"].is_null()
        && !matches!(
            candidate["planning_stage"].as_str(),
            Some("storyboard" | "final")
        )
    {
        return Err("Shooting script has an invalid planning stage.".into());
    }
    let modern = candidate["planning_policy"] == PLANNING_POLICY;
    if !modern && !candidate["planning_policy"].is_null() {
        return Err("Unrecognized shooting-script planning policy.".into());
    }
    if storyboard && !modern {
        return Err("Storyboard planning requires the first-render production contract.".into());
    }
    let cast = array(&source["cast"], "Source needs cast bindings.")?;
    let identities = &candidate["cast_identity"];
    if modern && !identities.is_array() {
        return Err("First-render planning requires stable cast_identity.".into());
    }
    if !identities.is_null() {
        let identities = array(
            identities,
            "Cast identity must describe every bound role exactly once.",
        )?;
        if identities.len() != cast.len() {
            return Err("Cast identity must describe every bound role exactly once.".into());
        }
        for (binding, identity) in cast.iter().zip(identities) {
            if !same_keys(identity, &["role", "description", "voice_description"])
                || identity["role"] != binding["role"]
            {
                return Err("Cast identity must use source roles in cast order.".into());
            }
            let role = binding["role"].as_str().unwrap_or_default();
            bounded(
                &identity["description"],
                &format!("Stable identity for {role}"),
                800,
            )?;
            if !identity["voice_description"]
                .as_str()
                .is_some_and(|voice| voice.chars().count() <= 800 && !voice.contains('\0'))
            {
                return Err(format!(
                    "Stable voice for {role} must be text of at most 800 characters."
                ));
            }
        }
    }
    if !(1..=64).contains(&proposed.len()) {
        return Err("Shooting script needs 1 to 64 shots.".into());
    }
    let beats = array(&source["beats"], "Source needs ordered beats.")?;
    let by_id: BTreeMap<u64, &Value> = beats
        .iter()
        .map(|beat| {
            beat["id"]
                .as_u64()
                .map(|id| (id, beat))
                .ok_or_else(|| "Invalid source beat ID.".to_owned())
        })
        .collect::<Result<_>>()?;
    let mut expected = 1_u64;
    let mut shots: Vec<Value> = Vec::new();
    let mut action_parts: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for (offset, item) in proposed.iter().enumerate() {
        let index = offset + 1;
        if !item.is_object() {
            return Err(format!("Shot {index} must be an object."));
        }
        let seconds = item["seconds"]
            .as_u64()
            .filter(|n| (4..=12).contains(n))
            .ok_or_else(|| format!("Shot {index} must last 4 to 12 whole seconds."))?;
        let raw_ids = array(&item["beat_ids"], &format!("Shot {index} needs beat_ids."))?;
        if raw_ids.is_empty() || raw_ids.iter().any(|id| !id.is_i64() && !id.is_u64()) {
            return Err(format!("Shot {index} needs beat_ids."));
        }
        let ids = raw_ids
            .iter()
            .map(|id| {
                id.as_u64()
                    .filter(|id| by_id.contains_key(id))
                    .ok_or_else(|| format!("Shot {index} needs unique source beat IDs in order."))
            })
            .collect::<Result<Vec<_>>>()?;
        if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(format!(
                "Shot {index} needs unique source beat IDs in order."
            ));
        }
        for id in &ids {
            if *id == expected {
                expected += 1;
            } else if !(modern && *id == expected - 1 && by_id[id]["kind"] == "action") {
                return Err(format!(
                    "Shot {index} skips, duplicates, or reorders source beats."
                ));
            }
        }
        let scene_id = &by_id[&ids[0]]["scene_id"];
        if ids.iter().any(|id| &by_id[id]["scene_id"] != scene_id) {
            return Err(format!("Shot {index} crosses a scene boundary."));
        }
        let continuation = item["continue_previous"]
            .as_bool()
            .ok_or_else(|| format!("Shot {index} continue_previous must be true or false."))?;
        if storyboard && continuation {
            return Err(format!(
                "Shot {index} must use its own storyboard first frame, with continue_previous false."
            ));
        }
        if offset == 0 && continuation {
            return Err("The first shot cannot continue a previous shot.".into());
        }
        if offset > 0 {
            if scene_id != &shots[offset - 1]["scene_id"] && continuation {
                return Err(format!("Shot {index} cannot continue across scenes."));
            }
            if !storyboard && scene_id == &shots[offset - 1]["scene_id"] && !continuation {
                return Err(format!(
                    "Shot {index} must continue the previous shot within the same scene: use continue_previous true and preserve its ending composition in the opening frame."
                ));
            }
        }
        let visible = &item["visible_roles"];
        if !identities.is_null() {
            let roles = array(
                visible,
                &format!(
                    "Shot {index} visible_roles must list its visible source roles exactly once."
                ),
            )?;
            if roles.iter().any(|role| {
                !role.is_string() || !cast.iter().any(|binding| &binding["role"] == role)
            }) || roles
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
                .len()
                != roles.len()
            {
                return Err(format!(
                    "Shot {index} visible_roles must list its visible source roles exactly once."
                ));
            }
        }
        let template = bounded(
            &item["prompt_template"],
            &format!("Shot {index} prompt template"),
            4000,
        )?;
        let start = bounded(
            &item["start_state"],
            &format!("Shot {index} start state"),
            1200,
        )?;
        let end = bounded(&item["end_state"], &format!("Shot {index} end state"), 1200)?;
        let mut filled = json!({"seconds": seconds, "beat_ids": raw_ids, "prompt_template": template,
            "start_state": start, "end_state": end, "continue_previous": continuation,
            "scene": by_id[&ids[0]]["scene"], "scene_id": scene_id});
        if storyboard {
            let still = bounded(
                &item["storyboard_prompt"],
                &format!("Shot {index} storyboard prompt"),
                1200,
            )?;
            if still.contains("{{") || still.contains("}}") {
                return Err(format!(
                    "Shot {index} storyboard must describe a still image without dialogue placeholders."
                ));
            }
            filled["storyboard_prompt"] = json!(still);
            filled["opening_visible_roles"] = item["opening_visible_roles"].clone();
            filled["continuity"] = item["continuity"].clone();
        }
        if !identities.is_null() {
            filled["visible_roles"] = visible.clone();
        }
        let mut action_weight = ids
            .iter()
            .filter(|id| by_id[*id]["kind"] == "action")
            .count() as f64;
        if modern {
            if !matches!(item["coverage"].as_str(), Some("continuous" | "edited")) {
                return Err(format!(
                    "Shot {index} coverage must be continuous or edited."
                ));
            }
            let purpose = bounded(
                &item["dramatic_purpose"],
                &format!("Shot {index} dramatic purpose"),
                800,
            )?;
            let contract = array(
                &item["visual_contract"],
                &format!("Shot {index} needs 1-8 observable visual contract requirements."),
            )?;
            if !(1..=8).contains(&contract.len()) {
                return Err(format!(
                    "Shot {index} needs 1-8 observable visual contract requirements."
                ));
            }
            let contract = contract
                .iter()
                .map(|value| bounded(value, &format!("Shot {index} visual requirement"), 800))
                .collect::<Result<Vec<_>>>()?;
            let parts = array(
                &item["action_coverage"],
                &format!("Shot {index} must describe its part of every assigned action beat."),
            )?;
            let action_ids = ids
                .iter()
                .filter(|id| by_id[*id]["kind"] == "action")
                .collect::<Vec<_>>();
            if parts.len() != action_ids.len() || parts.iter().any(|part| !part.is_object()) {
                return Err(format!(
                    "Shot {index} must describe its part of every assigned action beat."
                ));
            }
            action_weight = 0.0;
            for (id, part) in action_ids.iter().zip(parts) {
                let valid = part["part"]
                    .as_u64()
                    .zip(part["parts"].as_u64())
                    .is_some_and(|(part, parts)| part >= 1 && part <= parts && parts <= 64);
                if !same_keys(part, &["beat_id", "part", "parts", "action"])
                    || part["beat_id"].as_u64() != Some(**id)
                    || !valid
                {
                    return Err(format!(
                        "Shot {index} has invalid or reordered action coverage."
                    ));
                }
                bounded(
                    &part["action"],
                    &format!("Shot {index} action coverage"),
                    1200,
                )?;
                action_parts.entry(**id).or_default().push(part);
                action_weight += 1.0 / part["parts"].as_u64().unwrap() as f64;
            }
            filled["coverage"] = item["coverage"].clone();
            filled["dramatic_purpose"] = json!(purpose);
            filled["visual_contract"] = json!(contract);
            filled["action_coverage"] = json!(parts);
        }
        let required = ids
            .iter()
            .filter(|id| by_id[*id]["kind"] == "dialogue")
            .copied()
            .collect::<Vec<_>>();
        if single_speaker {
            let speakers = required
                .iter()
                .filter_map(|id| by_id[id]["speaker"].as_str())
                .collect::<BTreeSet<_>>();
            if speakers.len() > 1 {
                return Err(format!(
                    "Shot {index} gives dialogue to multiple roles ({}). Split at the speaker change into separate shots, each with its own storyboard and only that role's dialogue. Preserve source order and required actions; do not serialize source-required simultaneous speech.",
                    speakers.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        let compile_prompt = |legacy| {
            prompt::compile(
                template, source, identities, visible, &required, &by_id, request, storyboard,
                index, legacy,
            )
        };
        let mut compiled = compile_prompt(false)?;
        if item["prompt"]
            .as_str()
            .is_some_and(|saved| saved != compiled)
        {
            // Resume must preserve the exact approved prompt and its hashes.
            // Accept the previous compiler format only when it reproduces the
            // saved prompt exactly from the same source and template. New
            // shots always use the inline performer/voice bindings.
            let legacy = compile_prompt(true)?;
            if item["prompt"].as_str() == Some(legacy.as_str()) {
                compiled = legacy;
            }
        }
        if !model_tokenizer && prompt::estimated_tokens(&compiled) > 1024 {
            return Err(format!("Shot {index} prompt exceeds 1024 Gemma tokens."));
        }
        let words: usize = required
            .iter()
            .map(|id| prompt::word_count(by_id[id]["text"].as_str().unwrap_or_default()))
            .sum();
        let minimum = (words as f64 * 60.0 / 160.0 + action_weight + 0.5)
            .ceil()
            .max(4.0) as u64;
        if seconds < minimum {
            return Err(format!(
                "Shot {index} needs at least {minimum} seconds for its dialogue and action."
            ));
        }
        filled["prompt"] = json!(compiled);
        shots.push(filled);
    }
    if expected != beats.len() as u64 + 1 {
        return Err("Shooting script omits source beats.".into());
    }
    if storyboard {
        canonicalize_features(&mut shots, &candidate["world"]);
        crate::continuity::dispatch(&json!({"operation": "validate_plan", "args": {"plan": {
            "source": source, "shots": shots, "world": candidate["world"]}}}))?;
    }
    for (id, parts) in action_parts {
        if parts.iter().enumerate().any(|(index, part)| {
            part["part"].as_u64() != Some(index as u64 + 1)
                || part["parts"].as_u64() != Some(parts.len() as u64)
        }) {
            return Err(format!(
                "Action beat {id} must cover consecutive parts 1 through its actual part count."
            ));
        }
    }
    for (proposed, filled) in proposed.iter().zip(&shots) {
        if proposed
            .get("prompt")
            .is_some_and(|prompt| prompt != &filled["prompt"])
        {
            return Err("Saved shot prompt does not match the source dialogue.".into());
        }
    }
    let seconds: u64 = shots
        .iter()
        .filter_map(|shot| shot["seconds"].as_u64())
        .sum();
    let mut plan = json!({"version": 1, "source_sha256": source["source_sha256"],
        "seconds": seconds, "fade_out": source["fade_out"], "source": source, "shots": shots});
    if single_speaker {
        plan["dialogue_policy"] = json!(DIALOGUE_POLICY);
    }
    for key in ["planning_policy", "cast_identity"] {
        if !candidate[key].is_null() {
            plan[key] = candidate[key].clone();
        }
    }
    if storyboard {
        optional_fields(&mut plan, candidate, &["storyboard_policy", "world"]);
    }
    optional_fields(&mut plan, candidate, &["planner_version", "planning_stage"]);
    optional_fields(&mut plan, request, &["render_format"]);
    Ok(plan)
}
