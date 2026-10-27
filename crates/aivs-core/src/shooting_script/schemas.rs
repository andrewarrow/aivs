//! Planner response contracts, including continuity and repair proposals.
use super::{PLANNING_POLICY, STORYBOARD_POLICY};
use crate::continuity::Result;
use serde_json::{Value, json};

fn object(fields: &[(&str, Value)]) -> Value {
    json!({"type": "object", "additionalProperties": false,
        "required": fields.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "properties": fields.iter().map(|(name, value)| ((*name).to_owned(), value.clone())).collect::<serde_json::Map<_, _>>()})
}

fn array(items: Value) -> Value {
    json!({"type": "array", "items": items})
}

fn continuity(name: &str) -> Result<Value> {
    crate::continuity::dispatch(&json!({"operation": "schema", "args": {"name": name}}))
}

fn add(object: &mut Value, name: &str, schema: Value) {
    object["properties"][name] = schema;
    object["required"].as_array_mut().unwrap().push(json!(name));
}

pub(super) fn schema(name: &str) -> Result<Value> {
    let text = json!({"type": "string"});
    let integer = json!({"type": "integer"});
    let boolean = json!({"type": "boolean"});
    let strings = array(text.clone());
    match name {
        "plan" | "director" => {
            let mut shot = object(&[
                ("seconds", integer.clone()),
                ("beat_ids", array(integer.clone())),
                ("visible_roles", strings.clone()),
                ("prompt_template", text.clone()),
                ("start_state", text.clone()),
                ("end_state", text.clone()),
                ("continue_previous", boolean),
                (
                    "coverage",
                    json!({"type": "string", "enum": ["continuous", "edited"]}),
                ),
                ("dramatic_purpose", text.clone()),
                ("visual_contract", strings.clone()),
                (
                    "action_coverage",
                    array(object(&[
                        ("beat_id", integer.clone()),
                        ("part", integer.clone()),
                        ("parts", integer),
                        ("action", text.clone()),
                    ])),
                ),
            ]);
            if name == "director" {
                add(&mut shot, "storyboard_prompt", text.clone());
                add(&mut shot, "opening_visible_roles", strings.clone());
                add(&mut shot, "continuity", continuity("continuity.contract")?);
            }
            let mut plan = object(&[
                (
                    "dialogue_policy",
                    json!({"type": "string", "enum": [super::DIALOGUE_POLICY]}),
                ),
                (
                    "planning_policy",
                    json!({"type": "string", "enum": [PLANNING_POLICY]}),
                ),
                (
                    "cast_identity",
                    array(object(&[
                        ("role", text.clone()),
                        ("description", text.clone()),
                        ("voice_description", text),
                    ])),
                ),
                ("revision_notes", strings),
                ("shots", array(shot)),
            ]);
            if name == "director" {
                add(
                    &mut plan,
                    "storyboard_policy",
                    json!({"type": "string", "enum": [STORYBOARD_POLICY]}),
                );
                add(&mut plan, "world", continuity("continuity.world")?);
            }
            Ok(plan)
        }
        "motion" => {
            let mut shot = object(&[
                ("shot", integer),
                ("prompt_template", text.clone()),
                ("image_observations", text.clone()),
                ("blocking_issue", text.clone()),
                ("storyboard_prompt", text),
            ]);
            add(
                &mut shot,
                "continuity_review",
                continuity("continuity.review")?,
            );
            add(
                &mut shot,
                "plan_repair",
                json!({"type": "array", "maxItems": 1,
                "items": schema("director")?["properties"]["shots"]["items"]}),
            );
            add(
                &mut shot,
                "world_repair",
                json!({"type": "array", "maxItems": 1, "items": continuity("continuity.world")?}),
            );
            Ok(object(&[("shots", array(shot))]))
        }
        "fidelity" => Ok(object(&[(
            "checks",
            array(object(&[
                ("id", text.clone()),
                ("preserved", boolean),
                (
                    "evidence",
                    array(object(&[("shot", integer), ("quote", text.clone())])),
                ),
                ("reason", text),
            ])),
        )])),
        "fidelity_repair" => Ok(object(&[(
            "shots",
            array(object(&[("shot", integer), ("prompt_template", text)])),
        )])),
        _ => Err(format!("Unknown shooting-script schema: {name}")),
    }
}
