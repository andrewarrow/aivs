//! Screenplay parsing, compiled shot contracts, and independent approval rules.
//! Clients retain the historical artifact JSON digest codec and model tokenizer.
mod approval;
mod compile;
mod prompt;
mod schemas;
mod source;
mod unicode;

use crate::continuity::Result;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::{LazyLock, Mutex};

pub const PLANNER_VERSION: &str = "storyboard-two-pass-v2";
pub const PLANNING_POLICY: &str = "single-render-v1";
pub const STORYBOARD_POLICY: &str = "per-shot-first-frame-v1";

// Expressions are fixed by this module, never supplied by production inputs.
fn regex(pattern: &'static str) -> Regex {
    static CACHE: LazyLock<Mutex<std::collections::BTreeMap<&'static str, Regex>>> =
        LazyLock::new(|| Mutex::new(std::collections::BTreeMap::new()));
    CACHE
        .lock()
        .expect("pattern cache")
        .entry(pattern)
        .or_insert_with(|| Regex::new(pattern).expect("screenplay expression"))
        .clone()
}

fn bounded<'a>(value: &'a Value, label: &str, limit: usize) -> Result<&'a str> {
    value
        .as_str()
        .filter(|text| {
            !text.trim().is_empty() && text.chars().count() <= limit && !text.contains('\0')
        })
        .map(str::trim)
        .ok_or_else(|| format!("{label} must be nonempty text of at most {limit} characters."))
}

fn array<'a>(value: &'a Value, error: &str) -> Result<&'a Vec<Value>> {
    value.as_array().ok_or_else(|| error.to_owned())
}

fn same_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}

fn optional_fields(target: &mut Value, source: &Value, keys: &[&str]) {
    for key in keys {
        if let Some(value) = source.get(*key) {
            target[*key] = value.clone();
        }
    }
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "parse_shooting_request" => source::parse(&args["request"], &args["profiles"]),
        "shooting_schema" => schemas::schema(args["name"].as_str().unwrap_or_default()),
        "compile_shooting_script" => compile::compile(
            &args["candidate"],
            &args["source"],
            &args["request"],
            args["model_tokenizer"] == true,
        ),
        "shooting_fingerprint_content" => {
            let plan = &args["plan"];
            let mut content = json!({"source_sha256": plan["source_sha256"],
                "cast_identity": plan["cast_identity"], "shots": plan["shots"], "fade_out": plan["fade_out"]});
            optional_fields(
                &mut content,
                plan,
                &[
                    "world",
                    "planning_policy",
                    "storyboard_policy",
                    "planner_version",
                ],
            );
            Ok(content)
        }
        "shooting_fidelity_requirements" => approval::requirements(&args["plan"]),
        "validate_shooting_fidelity" => {
            approval::fidelity(&args["report"], &args["plan"], args["require_pass"] == true)
        }
        "validate_shooting_review" => {
            approval::review(
                &args["report"],
                &args["plan"],
                args["require_approval"] != false,
            )?;
            Ok(Value::Null)
        }
        "shooting_instruction_context" => Ok(prompt::instruction_context(&args["source"])),
        "shooting_script_display" => prompt::display(&args["plan"]).map(Value::String),
        _ => Err("Unknown shooting-script operation.".into()),
    }
}
