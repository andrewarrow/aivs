//! Structured planning/inspection contracts, shared by all production clients.
use super::Result;
use serde_json::{Value, json};

fn object(properties: &[(&str, Value)]) -> Value {
    let fields: serde_json::Map<String, Value> = properties
        .iter()
        .map(|(name, schema)| ((*name).into(), schema.clone()))
        .collect();
    json!({"type": "object", "additionalProperties": false,
        "required": properties.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "properties": fields})
}

fn array(items: Value) -> Value {
    json!({"type": "array", "items": items})
}

fn choices(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

pub fn schema(name: &str) -> Result<Value> {
    let text = json!({"type": "string"});
    let number = json!({"type": "number"});
    let integer = json!({"type": "integer"});
    let strings = array(text.clone());
    let ids = array(integer.clone());
    let attributes = array(object(&[("key", text.clone()), ("value", text.clone())]));
    let relationships = array(object(&[
        ("subject", text.clone()),
        ("relation", text.clone()),
        ("object", text.clone()),
        ("value", text.clone()),
    ]));
    let decision = choices(&["pass", "repair-required", "uncertain"]);
    let hands = array(object(&[
        ("role", text.clone()),
        ("hand", choices(&["left", "right"])),
        ("object", text.clone()),
    ]));
    Ok(match name {
        "continuity.world" => object(&[
            (
                "props",
                array(object(&[
                    ("id", text.clone()),
                    ("scene_ids", ids),
                    ("type", text.clone()),
                    ("shape", text.clone()),
                    ("size", text.clone()),
                    ("color", text.clone()),
                    ("features", strings.clone()),
                    (
                        "components",
                        array(object(&[
                            ("id", text.clone()),
                            ("type", text.clone()),
                            ("shape", text.clone()),
                            ("size", text.clone()),
                            ("color", text.clone()),
                            ("features", strings),
                        ])),
                    ),
                    ("relationships", relationships),
                    ("reference_prompt", text.clone()),
                ])),
            ),
            (
                "characters",
                array(object(&[
                    ("role", text.clone()),
                    ("morphology", attributes),
                    ("reference_prompt", text.clone()),
                ])),
            ),
            (
                "environments",
                array(object(&[("scene_id", integer), ("reference_prompt", text)])),
            ),
        ]),
        "continuity.state" => object(&[
            (
                "characters",
                array(object(&[
                    ("role", text.clone()),
                    (
                        "presence",
                        choices(&["visible", "offscreen", "absent", "unknown", "not-visible"]),
                    ),
                    ("location", text.clone()),
                    ("posture", text.clone()),
                    ("costume", text.clone()),
                    ("appearance", attributes.clone()),
                    ("morphology", attributes.clone()),
                    ("visibility", attributes.clone()),
                ])),
            ),
            (
                "props",
                array(object(&[
                    ("id", text.clone()),
                    ("owner", text.clone()),
                    ("support", text.clone()),
                    ("location", text.clone()),
                    ("state", text.clone()),
                    ("visibility", text.clone()),
                    ("physical", attributes),
                ])),
            ),
            (
                "reveals",
                array(object(&[("id", text.clone()), ("value", text)])),
            ),
            ("relationships", relationships),
        ]),
        "continuity.transition" => object(&[
            ("entity", text.clone()),
            ("field", text.clone()),
            ("before", text.clone()),
            ("after", text.clone()),
            ("timing", choices(&["during-shot", "across-cut"])),
            ("mode", choices(&["on-screen", "offscreen"])),
            ("beat_ids", ids),
            ("quote", text.clone()),
            ("motivation", text.clone()),
            ("physical_transformation", json!({"type": "boolean"})),
            ("story_time", text),
        ]),
        "continuity.contract" => object(&[
            ("opening", schema("continuity.state")?),
            ("ending", schema("continuity.state")?),
            ("transitions", array(schema("continuity.transition")?)),
        ]),
        "continuity.review" => object(&[
            ("observed_opening_state", schema("continuity.state")?),
            (
                "source_transition_evidence",
                array(object(&[
                    ("beat_ids", ids),
                    ("quote", text.clone()),
                    ("explanation", text.clone()),
                ])),
            ),
            ("violations", strings),
            ("evidence", text.clone()),
            ("neighbor_comparison", text.clone()),
            ("confidence", number.clone()),
            ("decision", decision.clone()),
            (
                "failure_kind",
                choices(&["none", "image", "plan", "uncertain"]),
            ),
            (
                "reference_findings",
                array(object(&[
                    ("id", text.clone()),
                    ("decision", decision),
                    ("evidence", text),
                    ("confidence", number),
                ])),
            ),
        ]),
        "motion.frame" => object(&[
            ("frame", integer),
            ("composition", text.clone()),
            ("action", text),
            ("state", schema("continuity.state")?),
            ("transitions", array(schema("continuity.transition")?)),
            ("strength", number),
            ("hands", hands),
        ]),
        "motion.shot" => object(&[
            ("shot", integer),
            ("keyframes", array(schema("motion.frame")?)),
        ]),
        "motion.review" => object(&[
            ("continuity_review", schema("continuity.review")?),
            (
                "component_counts",
                array(object(&[("id", text.clone()), ("count", integer)])),
            ),
            ("hand_findings", text.clone()),
            ("observed_hands", hands),
            ("repair_strategy", choices(&["targeted-edit", "regenerate"])),
            ("correction", text),
        ]),
        _ => return Err(format!("Unknown production schema: {name}")),
    })
}
