//! Provisional intervals for validating coverage that has not been planned yet.
use super::decode;
use crate::continuity::{Result, SceneState};
use serde_json::{Value, json};

fn fallback_hands(state: &Value) -> Result<Value> {
    let state: SceneState = decode(state)?;
    let mut hands: Vec<_> = state
        .characters
        .iter()
        .flat_map(|character| {
            ["left", "right"].map(|side| (character.role.as_str(), side, "unknown".to_owned()))
        })
        .collect();
    for relation in &state.relationships {
        if relation.value == "yes" {
            if let Some(side) = relation.relation.strip_prefix("holds-") {
                if let Some(hand) = hands
                    .iter_mut()
                    .find(|(role, hand, _)| *role == relation.subject && *hand == side)
                {
                    hand.2.clone_from(&relation.object);
                }
            }
        }
    }
    for prop in &state.props {
        for (role, side, object) in &mut hands {
            if prop.support == format!("{role}:{side}-hand") {
                object.clone_from(&prop.id);
            }
        }
    }
    let result = json!(
        hands
            .into_iter()
            .map(|(role, hand, object)| { json!({"role": role, "hand": hand, "object": object}) })
            .collect::<Vec<_>>()
    );
    Ok(result)
}

/// These frames only fill untouched intervals during incremental plan validation.
/// They are never published as generated motion direction.
pub fn fallback(shot: &Value, fps: u64) -> Result<Value> {
    let opening_state = &shot["continuity"]["opening"];
    let ending_state = &shot["continuity"]["ending"];
    let opening = json!({
        "frame": 0,
        "composition": shot["storyboard_prompt"],
        "action": "Opening",
        "state": opening_state,
        "transitions": [],
        "strength": 1,
        "hands": fallback_hands(opening_state)?,
    });
    if opening_state == ending_state {
        return Ok(json!([opening]));
    }
    let last_frame = shot["seconds"]
        .as_u64()
        .and_then(|seconds| seconds.checked_mul(fps))
        .and_then(|frames| frames.checked_sub(1))
        .filter(|frame| *frame > 1)
        .ok_or("Invalid motion duration or frame rate.")?;
    let mut middle = opening.clone();
    middle["frame"] = json!(1);
    let mut ending = opening.clone();
    ending["frame"] = json!(last_frame);
    ending["state"] = ending_state.clone();
    ending["transitions"] = json!(
        shot["continuity"]["transitions"]
            .as_array()
            .ok_or("Missing shot continuity transitions.")?
            .iter()
            .filter(|transition| transition["timing"] == "during-shot")
            .collect::<Vec<_>>()
    );
    ending["hands"] = fallback_hands(ending_state)?;
    Ok(json!([opening, middle, ending]))
}
