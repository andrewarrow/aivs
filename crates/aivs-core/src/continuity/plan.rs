//! Validate source-backed state transitions within shots and across cuts.
use super::facts::{facts, get, key_label, persistent, physical_key, put, text, unknown};
use super::types::{Beat, Plan, Shot, Transition};
use super::world::{canonical_physical, validate_world};
use super::{FactKey, Facts, Result, decode};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn source_evidence(
    ids: &[i64],
    quote: &str,
    beats: &[Beat],
    allowed: &BTreeSet<i64>,
) -> Result<()> {
    if ids.is_empty()
        || ids.iter().any(|id| !allowed.contains(id))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || quote.trim().is_empty()
        || !ids
            .iter()
            .any(|id| beats.iter().any(|b| b.id == *id && b.text.contains(quote)))
    {
        return Err(
            "Continuity transition requires an exact quote from its authorized source beats."
                .into(),
        );
    }
    Ok(())
}

fn changes<'a>(before: &'a Facts, after: &'a Facts) -> Vec<(&'a FactKey, &'a str, &'a str)> {
    before
        .iter()
        .filter_map(|(key, a)| {
            get(after, key)
                .filter(|b| !unknown(a) && !unknown(b) && a.as_str() != *b)
                .map(|b| (key, a.as_str(), b))
        })
        .collect()
}

fn remember(known: &mut Facts, incoming: &Facts, retain_unknown: bool) {
    for (key, value) in incoming {
        if !unknown(value) || (retain_unknown && get(known, key).is_none()) {
            put(known, key.clone(), value.clone());
        }
    }
}

pub(super) fn validate_plan_data(plan: &Plan) -> Result<()> {
    let roles: BTreeSet<_> = plan.source.cast.iter().map(|r| r.role.as_str()).collect();
    let prop_ids = validate_world(plan)?;
    let mut physical = canonical_physical(&plan.world);
    let mut known = physical.clone();
    let mut previous: Option<&Shot> = None;
    for (index, shot) in plan.shots.iter().enumerate() {
        let number = index + 1;
        let contract = &shot.continuity;
        let opening = facts(&contract.opening)?;
        let ending = facts(&contract.ending)?;
        for state in [&opening, &ending] {
            for (key, value) in &physical {
                let Some(actual) = get(state, key) else {
                    return Err(format!(
                        "Shot {number} must retain canonical physical facts even when concealed."
                    ));
                };
                if unknown(actual) && !unknown(value) {
                    return Err(format!(
                        "Shot {number} must not forget known physical identity when it leaves the crop."
                    ));
                }
            }
        }
        if index == 0 && !changes(&physical, &opening).is_empty() {
            return Err("The first opening must match canonical physical definitions.".into());
        }
        for state in [&contract.opening, &contract.ending] {
            if state
                .characters
                .iter()
                .map(|r| r.role.as_str())
                .collect::<BTreeSet<_>>()
                != roles
            {
                return Err(format!(
                    "Shot {number} must track presence of every source role, including offscreen roles."
                ));
            }
            for prop in &state.props {
                if !prop_ids.contains(&prop.id) {
                    return Err(format!(
                        "Shot {number} invents a prop/component identity: {}",
                        prop.id
                    ));
                }
                if !roles.contains(prop.owner.as_str())
                    && !unknown(&prop.owner)
                    && prop.owner != "none"
                {
                    return Err(format!(
                        "Shot {number} prop owners must be source roles, none or unknown."
                    ));
                }
            }
        }
        let visible: BTreeSet<_> = contract
            .opening
            .characters
            .iter()
            .filter(|r| r.presence == "visible")
            .map(|r| &r.role)
            .collect();
        let declared: BTreeSet<_> = shot.opening_visible_roles.iter().collect();
        if declared != visible
            || declared.len() != shot.opening_visible_roles.len()
            || !visible.is_subset(&shot.visible_roles.iter().collect())
        {
            return Err(format!(
                "Shot {number} opening_visible_roles must match time-zero presence."
            ));
        }
        if contract.transitions.len() > 128 {
            return Err("Continuity transitions must be a compact array.".into());
        }
        let history: Facts = known
            .iter()
            .filter(|(key, _)| {
                previous.is_none_or(|p| p.scene_id == shot.scene_id) || persistent(key)
            })
            .cloned()
            .collect();
        let mut current = history.clone();
        remember(&mut current, &opening, true);
        let mut expected = Vec::new();
        if previous.is_some() {
            expected.extend(
                changes(&history, &opening)
                    .into_iter()
                    .map(|(k, a, b)| ("across-cut", k, a, b)),
            );
        }
        expected.extend(
            changes(&current, &ending)
                .into_iter()
                .map(|(k, a, b)| ("during-shot", k, a, b)),
        );
        let mut recorded = BTreeSet::new();
        let mut paths: BTreeMap<(&str, FactKey), Vec<&Transition>> = BTreeMap::new();
        for event in &contract.transitions {
            for field in [
                &event.entity,
                &event.field,
                &event.before,
                &event.after,
                &event.motivation,
                &event.story_time,
            ] {
                text(field)?;
            }
            if !matches!(event.timing.as_str(), "during-shot" | "across-cut")
                || !matches!(event.mode.as_str(), "on-screen" | "offscreen")
            {
                return Err("Invalid transition timing or visibility.".into());
            }
            if event.timing == "across-cut" && (previous.is_none() || event.mode != "offscreen") {
                return Err("An across-cut transition needs a preceding shot and motivated offscreen action.".into());
            }
            let mut allowed: BTreeSet<_> = if event.timing == "during-shot" {
                &shot.beat_ids
            } else {
                &previous.ok_or("Missing preceding shot.")?.beat_ids
            }
            .iter()
            .copied()
            .collect();
            if event.timing == "across-cut" && previous.is_some_and(|p| p.scene_id != shot.scene_id)
            {
                allowed.extend(&shot.beat_ids);
            }
            source_evidence(&event.beat_ids, &event.quote, &plan.source.beats, &allowed)?;
            if (event.field.starts_with("morphology.") || event.field.starts_with("physical."))
                && !event.physical_transformation
            {
                return Err("Physical identity/body changes require a source-backed transformation or sufficient story time.".into());
            }
            let key = (event.entity.clone(), event.field.clone());
            if !recorded.insert((&event.timing, key.clone(), &event.before, &event.after)) {
                return Err("Duplicate continuity transition.".into());
            }
            paths.entry((&event.timing, key)).or_default().push(event);
        }
        for (timing, key, before, after) in expected {
            let mut cursor = before;
            if let Some(events) = paths.get(&(timing, key.clone())) {
                for event in events {
                    if !unknown(cursor) && event.before != cursor {
                        return Err(format!(
                            "Shot {number} transition starts from the wrong known state: {}",
                            key_label(key)
                        ));
                    }
                    cursor = &event.after;
                }
            }
            if cursor != after {
                return Err(format!(
                    "Shot {number} unexplained {timing} change: {} {before} -> {after}",
                    key_label(key)
                ));
            }
        }
        for ((timing, key), events) in paths {
            let (start, end) = if timing == "across-cut" {
                (&history, &opening)
            } else {
                (&current, &ending)
            };
            let (Some(mut cursor), Some(endpoint)) = (get(start, &key), get(end, &key)) else {
                return Err(format!(
                    "Shot {number} {timing} transition must name a tracked entity and field: {}. Use the exact entity ID and field key present in both endpoint states.",
                    key_label(&key)
                ));
            };
            for event in events {
                if !unknown(cursor) && event.before != cursor {
                    return Err(format!(
                        "Shot {number} transition starts from the wrong known state: {}",
                        key_label(&key)
                    ));
                }
                cursor = &event.after;
            }
            if !unknown(endpoint) && cursor != endpoint {
                return Err(format!(
                    "Shot {number} transition does not reach its recorded endpoint: {}",
                    key_label(&key)
                ));
            }
        }
        previous = Some(shot);
        known = current;
        remember(&mut known, &ending, false);
        for (key, value) in &ending {
            if physical_key(key) && !unknown(value) {
                put(&mut physical, key.clone(), value.clone());
            }
        }
    }
    Ok(())
}

pub fn validate_plan(plan: &Value) -> Result<()> {
    validate_plan_data(&decode(plan)?)
}
