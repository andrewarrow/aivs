//! Keyframe pixel observations are evidence; Rust computes acceptance.
use super::{Hand, decode, inventory, unknown};
use crate::continuity::{self, Result, SceneState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct ComponentCount {
    id: String,
    count: i64,
}

#[derive(Deserialize)]
struct Review {
    hand_findings: String,
    correction: String,
    repair_strategy: String,
    observed_hands: Vec<Hand>,
    component_counts: Vec<ComponentCount>,
}

pub fn validate_review(plan: &Value, number: usize, frame: &Value, value: &Value) -> Result<Value> {
    let review: Review = decode(value)?;
    continuity::text(&review.hand_findings)?;
    continuity::text(&review.correction)?;
    if !matches!(
        review.repair_strategy.as_str(),
        "targeted-edit" | "regenerate"
    ) {
        return Err("Keyframe review must select a supported repair strategy.".into());
    }
    let state: SceneState = decode(&frame["state"])?;
    let (roles, props) = inventory(&state);
    let expected_hands: BTreeSet<_> = roles
        .iter()
        .flat_map(|role| [(*role, "left"), (*role, "right")])
        .collect();
    let observed_hands: BTreeMap<_, _> = review
        .observed_hands
        .iter()
        .map(|hand| {
            (
                (hand.role.as_str(), hand.hand.as_str()),
                hand.object.as_str(),
            )
        })
        .collect();
    if review.observed_hands.len() != expected_hands.len()
        || observed_hands.keys().copied().collect::<BTreeSet<_>>() != expected_hands
        || observed_hands.values().any(|object| {
            !props.contains(object) && !matches!(*object, "none" | "unknown" | "not-visible")
        })
    {
        return Err(
            "Pixel review must observe each tracked character's two hand-object states.".into(),
        );
    }
    let mut shot = number
        .checked_sub(1)
        .and_then(|index| plan["shots"].as_array()?.get(index))
        .filter(|shot| shot.is_object())
        .ok_or("Invalid motion shot number.")?
        .clone();
    let visible_roles: BTreeSet<_> = state
        .characters
        .iter()
        .filter(|character| character.presence == "visible")
        .map(|character| character.role.as_str())
        .collect();
    shot["continuity"] =
        json!({"opening": frame["state"], "ending": frame["state"], "transitions": []});
    shot["opening_visible_roles"] = json!(
        state
            .characters
            .iter()
            .filter(|character| character.presence == "visible")
            .map(|character| &character.role)
            .collect::<Vec<_>>()
    );
    let mut single_frame_plan = plan.clone();
    single_frame_plan["shots"] = json!([shot]);
    let checked = continuity::review_shot(&single_frame_plan, 1, value)?;
    let mut findings: Vec<String> = decode(&checked["violations"])?;
    for hand in decode::<Vec<Hand>>(&frame["hands"])? {
        if visible_roles.contains(hand.role.as_str())
            && !unknown(&hand.object)
            && observed_hands
                .get(&(hand.role.as_str(), hand.hand.as_str()))
                .copied()
                != Some(hand.object.as_str())
        {
            findings.push(format!(
                "Unverified hand-object state: {} {}",
                hand.role, hand.hand
            ));
        }
    }
    let observed_state: SceneState = decode(&value["continuity_review"]["observed_opening_state"])?;
    let observations: BTreeMap<_, _> = continuity::facts(&observed_state)?.into_iter().collect();
    let visible_ids: BTreeSet<_> = state
        .props
        .iter()
        .filter(|prop| prop.visibility == "visible")
        .map(|prop| prop.id.as_str())
        .chain(visible_roles.iter().copied())
        .collect();
    for (key, wanted) in continuity::facts(&state)? {
        let actual = observations.get(&key).map(String::as_str);
        let entity = key.0.split_once(':').map_or(key.0.as_str(), |(_, id)| id);
        let position_consistent = checked["field_assessments"]
            .as_array()
            .is_some_and(|fields| {
                fields.iter().any(|finding| {
                    finding["entity"] == key.0
                        && finding["field"] == "location"
                        && finding["assessment"] == "consistent"
                })
            });
        if key.1 == "location"
            && !unknown(&wanted)
            && actual != Some(wanted.as_str())
            && !position_consistent
            && (visible_ids.contains(entity) || actual.is_some_and(|actual| !unknown(actual)))
        {
            findings.push(format!(
                "Position mismatch: {}",
                continuity::key_label(&key)
            ));
        }
        if key.0.starts_with("relationship:")
            && visible_ids.contains(entity)
            && visible_ids.contains(key.1.split_once(':').map_or(key.1.as_str(), |(_, id)| id))
            && !unknown(&wanted)
            && actual != Some(wanted.as_str())
        {
            findings.push(format!(
                "Unverified visible relationship: {}",
                continuity::key_label(&key)
            ));
        }
    }
    let ids: BTreeSet<_> = review
        .component_counts
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    if ids.len() != review.component_counts.len()
        || ids != roles.union(&props).copied().collect::<BTreeSet<_>>()
        || review.component_counts.iter().any(|row| row.count < -1)
    {
        return Err(
            "Pixel review must count every tracked component and character exactly once.".into(),
        );
    }
    if review
        .component_counts
        .iter()
        .any(|row| row.count > 1 || (visible_ids.contains(row.id.as_str()) && row.count != 1))
    {
        findings.push("Missing, uncertain or duplicated visible component/character.".into());
    }
    // Always recompute these fields, including for cached rejected observations.
    let mut result = value.clone();
    result["accepted"] = json!(checked["decision"] == "pass" && findings.is_empty());
    result["findings"] = json!(findings);
    Ok(result)
}
