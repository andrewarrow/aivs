//! Map engine operations onto continuity domain rules.
use super::acceptance::{dependent_shots, validate_acceptance};
use super::facts::facts;
use super::plan::validate_plan;
use super::review::{review_sequence, review_shot};
use super::world::{canonical_physical, validate_world};
use super::{Result, decode};
use serde_json::{Value, json};

pub fn dispatch(request: &Value) -> Result<Value> {
    let args = &request["args"];
    match request["operation"].as_str().unwrap_or("") {
        "schema" => super::schemas::schema(args["name"].as_str().ok_or("Missing schema name.")?),
        "continuity_settings" => Ok(json!({"policy": super::POLICY, "batch_size": 6})),
        "storyboard_review_binding" => super::artifacts::report_binding(args),
        "validate_storyboard_manifest" => {
            super::artifacts::manifest(&args["plan"], &args["boards"])?;
            Ok(Value::Null)
        }
        "validate_storyboard_snapshots" => {
            super::artifacts::snapshots(&args["plan"], &args["screenplay"], &args["profiles"])?;
            Ok(Value::Null)
        }
        "facts" => Ok(json!(
            facts(&decode(&args["state"])?)?
                .into_iter()
                .map(|(key, value)| (key.0, key.1, value))
                .collect::<Vec<_>>()
        )),
        "validate_world" => Ok(json!(validate_world(&decode(&args["plan"])?)?)),
        "canonical_physical" => Ok(json!(
            canonical_physical(&decode(&args["world"])?)
                .into_iter()
                .map(|(key, value)| (key.0, key.1, value))
                .collect::<Vec<_>>()
        )),
        "validate_plan" => {
            validate_plan(&args["plan"])?;
            Ok(Value::Null)
        }
        "review_shot" => review_shot(
            &args["plan"],
            args["number"].as_u64().ok_or("Invalid shot number.")? as usize,
            &args["row"],
        ),
        "review_sequence" => Ok(json!(review_sequence(
            &args["plan"],
            &decode(&args["rows"])?
        )?)),
        "dependent_shots" => Ok(json!(dependent_shots(
            &args["boards"],
            decode(&args["changed"])?
        )?)),
        "validate_acceptance" => {
            validate_acceptance(
                &args["plan"],
                &args["boards"],
                &args["report"],
                &args["binding"],
                &args["report_sha256"],
            )?;
            Ok(Value::Null)
        }
        _ => Err("Unknown AIVS engine operation.".into()),
    }
}
