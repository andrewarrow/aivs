//! Source-bound inspection checkpoints and bounded Codex repairs.
use super::assess;
use crate::{artifacts, continuity::Result, planning};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

pub fn review(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let checkpoint = PathBuf::from(
        args["checkpoint"]
            .as_str()
            .ok_or("Missing prop inspection checkpoint.")?,
    );
    let spec = &args["spec"];
    let expected = &args["binding"];
    if checkpoint.is_file() {
        let mut report = artifacts::read_json(&checkpoint)?;
        if report["binding"] == *expected {
            let failures = assess(&spec["definition"], &report["observation"])?;
            report["approved"] = json!(failures.is_empty());
            report["failures"] = json!(failures);
            return Ok(report);
        }
    }
    emit(
        json!({"type": "activity", "message": format!("Inspecting canonical {} components and connections", spec["id"].as_str().unwrap_or_default())}),
    )?;
    let schema: Value =
        serde_json::from_str(include_str!("schema.json")).map_err(|e| e.to_string())?;
    let instruction = format!(
        "{}{}",
        include_str!("instructions.txt"),
        serde_json::to_string(&spec["definition"]).map_err(|e| e.to_string())?
    );
    let observation = planning::validated(
        &json!({"name": args["name"], "instruction": instruction, "schema": schema,
        "run_dir": args["run_dir"], "source_sha256": args["source_sha256"], "executable": args["executable"],
        "image_paths": [args["image_path"]], "limit": args["limit"]}),
        cancelled,
        &mut emit,
        |value| {
            assess(&spec["definition"], &value)?;
            Ok(value)
        },
    )?;
    let failures = assess(&spec["definition"], &observation)?;
    let report = json!({"binding": expected, "observation": observation, "approved": failures.is_empty(), "failures": failures});
    artifacts::atomic_json(&checkpoint, &report)?;
    Ok(report)
}

pub(super) fn failure_summary(report: &Value) -> Result<String> {
    let failures = report["failures"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Prop inspection has no failures.")?;
    let mut summary = failures[0]
        .as_str()
        .ok_or("Invalid prop inspection failure.")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if summary.chars().count() > 240 {
        summary = summary.chars().take(240).collect();
        if let Some((head, _)) = summary.rsplit_once(' ') {
            summary = head.into();
        }
        summary.push('…');
    }
    if failures.len() > 1 {
        summary.push_str(&format!(
            " ({} more findings in the saved inspection.)",
            failures.len() - 1
        ));
    }
    Ok(summary)
}
