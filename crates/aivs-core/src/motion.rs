//! Timed keyframe contracts and hand/component constraints, without inference.
use crate::continuity::{self, FactKey, Result, SceneState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const POLICY: &str = "motion-previsualization-v1";

#[derive(Deserialize)]
struct Hand {
    role: String,
    hand: String,
    object: String,
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|error| format!("Invalid motion data: {error}"))
}

fn inventory(state: &SceneState) -> (BTreeSet<&str>, BTreeSet<&str>) {
    (
        state.characters.iter().map(|c| c.role.as_str()).collect(),
        state.props.iter().map(|p| p.id.as_str()).collect(),
    )
}

fn unknown(value: &str) -> bool {
    matches!(value, "unknown" | "not-visible")
}

fn physical_state(state: &SceneState, hand_states: &[Hand]) -> Result<()> {
    continuity::facts(state)?;
    let (roles, props) = inventory(state);
    let by_id: BTreeMap<_, _> = state.props.iter().map(|p| (p.id.as_str(), p)).collect();
    let expected: BTreeSet<_> = roles
        .iter()
        .flat_map(|role| [(*role, "left"), (*role, "right")])
        .collect();
    let observed: BTreeSet<_> = hand_states
        .iter()
        .map(|h| (h.role.as_str(), h.hand.as_str()))
        .collect();
    if hand_states.len() != expected.len() || observed != expected {
        return Err("Every character needs one left-hand and one right-hand object state.".into());
    }
    let mut hands = BTreeMap::new();
    let mut docked = BTreeSet::new();
    for hand in hand_states {
        if !props.contains(hand.object.as_str())
            && !matches!(hand.object.as_str(), "none" | "unknown" | "not-visible")
        {
            return Err(
                "Each hand holds one tracked component, none, or an explicit unknown.".into(),
            );
        }
        if let Some(prop) = by_id.get(hand.object.as_str()) {
            if prop.owner != hand.role {
                return Err("The hand's object owner contradicts the physical state.".into());
            }
            if prop.support != format!("{}:{}-hand", hand.role, hand.hand) {
                return Err("Every held component must be supported by the same tracked hand, not a cradle/base.".into());
            }
            hands.insert(
                (hand.role.as_str(), format!("holds-{}", hand.hand)),
                hand.object.as_str(),
            );
        }
    }
    for prop in &state.props {
        if !roles.contains(prop.owner.as_str()) && prop.owner != "none" && !unknown(&prop.owner) {
            return Err("A prop owner must be a tracked character or none/unknown.".into());
        }
    }
    for relation in &state.relationships {
        let subject = relation.subject.as_str();
        let object = relation.object.as_str();
        if !(roles.contains(subject) || props.contains(subject))
            || !(roles.contains(object) || props.contains(object))
        {
            return Err(
                "Motion relationships must reference tracked characters or prop components.".into(),
            );
        }
        if matches!(relation.relation.as_str(), "holds-left" | "holds-right") {
            if !roles.contains(subject)
                || !props.contains(object)
                || !matches!(relation.value.as_str(), "yes" | "no")
            {
                return Err(
                    "Hand relationships require a role, holds-left/right, component ID and yes/no."
                        .into(),
                );
            }
            let key = (subject, relation.relation.clone());
            if relation.value == "no" && hands.get(&key) == Some(&object) {
                return Err(
                    "A negative holding relationship contradicts its explicit hand state.".into(),
                );
            }
            if relation.value == "yes" {
                if hands.get(&key).is_some_and(|held| *held != object) {
                    return Err(
                        "Hand-object relationships contradict the explicit hand state.".into(),
                    );
                }
                if !hand_states.iter().any(|h| {
                    h.role == subject
                        && format!("holds-{}", h.hand) == relation.relation
                        && h.object == object
                }) {
                    return Err("A holding relationship must match its hand state.".into());
                }
                hands.insert(key, object);
                let prop = by_id[object];
                let side = if relation.relation == "holds-left" {
                    "left"
                } else {
                    "right"
                };
                if prop.owner != subject || prop.support != format!("{subject}:{side}-hand") {
                    return Err("Hand ownership and component support contradict the grip.".into());
                }
            }
        }
        if relation.relation == "docked-in" && relation.value == "yes" {
            if !props.contains(subject)
                || !props.contains(object)
                || by_id[subject].support != object
            {
                return Err(
                    "A docked component must be supported by its tracked cradle/base.".into(),
                );
            }
            docked.insert(subject);
        }
    }
    if hands.values().any(|held| docked.contains(held)) {
        return Err("A component cannot be both held and docked.".into());
    }
    for prop in &state.props {
        for (suffix, relation) in [(":left-hand", "holds-left"), (":right-hand", "holds-right")] {
            if let Some(role) = prop.support.strip_suffix(suffix) {
                if hands.get(&(role, relation.into())) != Some(&prop.id.as_str()) {
                    return Err("A hand-supported component needs a matching explicit grip.".into());
                }
            }
        }
    }
    Ok(())
}

fn fact_keys(state: &SceneState) -> Result<BTreeSet<FactKey>> {
    Ok(continuity::facts(state)?
        .into_iter()
        .map(|(key, _)| key)
        .collect())
}

/// The caller supplies the artifact's plan digest; Rust owns all state rules.
pub fn validate_timing(plan: &Value, motion: &Value, plan_sha256: &Value, fps: u64) -> Result<()> {
    if motion["version"].as_u64() != Some(1) || motion["policy"] != POLICY {
        return Err("Unsupported motion plan.".into());
    }
    if !plan_sha256.is_string() || motion["plan_sha256"] != *plan_sha256 {
        return Err("Motion keyframes belong to a different finalized shooting plan.".into());
    }
    let shots = plan["shots"]
        .as_array()
        .ok_or("Motion plans need shooting plan shots.")?;
    let rows = motion["shots"]
        .as_array()
        .ok_or("Motion plans must cover every shot exactly once in order.")?;
    if rows.len() != shots.len()
        || rows
            .iter()
            .enumerate()
            .any(|(i, row)| row["shot"].as_u64() != Some(i as u64 + 1))
    {
        return Err("Motion plans must cover every shot exactly once in order.".into());
    }
    let mut expanded = Vec::new();
    for (shot, row) in shots.iter().zip(rows) {
        let frames = row["keyframes"]
            .as_array()
            .ok_or("Use one static frame or three/four movement keyframes per shot.")?;
        if !matches!(frames.len(), 1 | 3 | 4) {
            return Err("Use one static frame or three/four movement keyframes per shot.".into());
        }
        let last_frame = shot["seconds"]
            .as_u64()
            .and_then(|seconds| seconds.checked_mul(fps))
            .and_then(|frames| frames.checked_sub(1))
            .ok_or("Invalid motion duration or frame rate.")?;
        let times: Vec<_> = frames.iter().map(|f| f["frame"].as_u64()).collect();
        if times.iter().any(Option::is_none)
            || times[0] != Some(0)
            || times.windows(2).any(|pair| pair[0] >= pair[1])
            || (frames.len() > 1 && times.last() != Some(&Some(last_frame)))
        {
            return Err("Motion timestamps must be ordered output frames, from zero to the last visible frame.".into());
        }
        if frames[0]["state"] != shot["continuity"]["opening"]
            || frames[0]["transitions"] != json!([])
        {
            return Err(
                "The opening must retain the approved state with no preceding motion transitions."
                    .into(),
            );
        }
        if frames.last().ok_or("Missing motion frame.")?["state"] != shot["continuity"]["ending"] {
            return Err("The final keyframe must reach the existing approved ending state.".into());
        }
        let opening: SceneState = decode(&frames[0]["state"])?;
        let keys = fact_keys(&opening)?;
        let initial_inventory = inventory(&opening);
        let shot_transitions = shot["continuity"]["transitions"]
            .as_array()
            .ok_or("Missing shot continuity transitions.")?;
        let authorized: BTreeSet<_> = shot_transitions
            .iter()
            .filter(|t| t["timing"] == "during-shot")
            .map(|t| (t["entity"].as_str(), t["field"].as_str()))
            .collect();
        for frame in frames {
            let state: SceneState = decode(&frame["state"])?;
            if fact_keys(&state)? != keys || inventory(&state) != initial_inventory {
                return Err("Keyframes must retain every component, character, attribute and relationship, including offscreen ones.".into());
            }
            physical_state(&state, &decode::<Vec<Hand>>(&frame["hands"])?)?;
            for event in frame["transitions"]
                .as_array()
                .ok_or("Keyframe transitions must be a list.")?
            {
                let field = event["field"]
                    .as_str()
                    .ok_or("Invalid motion transition field.")?;
                if ["morphology.", "physical.", "attached-to:", "connected-to:"]
                    .iter()
                    .any(|prefix| field.starts_with(prefix))
                    && !authorized.contains(&(event["entity"].as_str(), Some(field)))
                {
                    return Err("Motion refinement cannot invent physical transformations or detach canonical connections.".into());
                }
            }
            for field in ["composition", "action"] {
                continuity::text(
                    frame[field]
                        .as_str()
                        .ok_or("Keyframe directions must be text.")?,
                )?;
            }
            if !frame["strength"]
                .as_f64()
                .is_some_and(|n| n > 0.0 && n <= 1.0)
            {
                return Err("Keyframe strengths must be in (0, 1].".into());
            }
        }
        for index in 0..frames.len().saturating_sub(1).max(1) {
            let before = &frames[index];
            let after = &frames[if frames.len() == 1 { index } else { index + 1 }];
            let mut transitions = if frames.len() == 1 {
                Vec::new()
            } else {
                after["transitions"]
                    .as_array()
                    .ok_or("Keyframe transitions must be a list.")?
                    .clone()
            };
            if transitions.iter().any(|t| t["timing"] != "during-shot") {
                return Err("Keyframe interval transitions must occur during the shot.".into());
            }
            if index == 0 {
                let mut across: Vec<_> = shot_transitions
                    .iter()
                    .filter(|t| t["timing"] == "across-cut")
                    .cloned()
                    .collect();
                across.extend(transitions);
                transitions = across;
            }
            let visible: Vec<_> = before["state"]["characters"]
                .as_array()
                .ok_or("Missing motion characters.")?
                .iter()
                .filter(|c| c["presence"] == "visible")
                .map(|c| c["role"].clone())
                .collect();
            let mut interval = shot.clone();
            interval["continuity"] = json!({"opening": before["state"], "ending": after["state"], "transitions": transitions});
            interval["opening_visible_roles"] = json!(visible);
            expanded.push(interval);
        }
    }
    let mut expanded_plan = plan.clone();
    expanded_plan["shots"] = json!(expanded);
    continuity::validate_plan(&expanded_plan)
}
