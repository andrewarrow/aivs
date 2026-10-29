//! Approval receipts and publication of final source-bound shooting scripts.
use super::{PLANNER_VERSION, approval, prompt};
use crate::{artifacts, continuity::Result};
use serde_json::Value;
use std::{fs, path::PathBuf};

fn fidelity(receipt: &Value, plan: &Value, fingerprint: &Value) -> Result<()> {
    if !receipt.is_object()
        || receipt["version"].as_u64() != Some(1)
        || receipt["plan_sha256"] != *fingerprint
    {
        return Err("This motion plan has no matching source-fidelity record.".into());
    }
    if receipt["policy"] == "motion-review-v1" {
        match receipt["status"].as_str() {
            Some("not-run") if receipt.get("report").is_none() => return Ok(()),
            Some("advisory") => {
                approval::fidelity(&receipt["report"], plan, false)?;
                return Ok(());
            }
            _ => return Err("Invalid advisory source-fidelity record.".into()),
        }
    }
    approval::fidelity(&receipt["report"], plan, true)?;
    Ok(())
}

pub(super) fn validate_saved(args: &Value) -> Result<Value> {
    let saved = &args["saved"];
    let mut artifact = args["artifact"].clone();
    let fingerprint = &args["fingerprint"];
    if saved["version"].as_u64() != Some(1)
        || saved["seconds"] != artifact["seconds"]
        || saved["fade_out"] != artifact["fade_out"]
    {
        return Err("Saved shooting-script metadata is invalid.".into());
    }
    if artifact["planner_version"] == PLANNER_VERSION {
        let motion = &saved["motion_plan"];
        if artifact["planning_stage"] != "final"
            || !motion.is_object()
            || motion["version"].as_u64() != Some(1)
            || motion["plan_sha256"] != *fingerprint
        {
            return Err(
                "This storyboard draft has no matching image-based motion plan yet.".into(),
            );
        }
        artifact["motion_plan"] = motion.clone();
        fidelity(&saved["source_fidelity"], &artifact, fingerprint)?;
        artifact["source_fidelity"] = saved["source_fidelity"].clone();
    } else if saved.get("cast_identity").is_some() && saved.get("preproduction_review").is_none() {
        return Err("This preproduction draft has no final approval; generate an approved shooting script before rendering.".into());
    }
    if let Some(review) = saved.get("preproduction_review") {
        if !review.is_object()
            || review["version"].as_u64() != Some(1)
            || review["plan_sha256"] != *fingerprint
        {
            return Err("Saved preproduction approval does not match this shooting script.".into());
        }
        approval::review(&review["report"], &artifact, true)?;
        artifact["preproduction_review"] = review.clone();
    }
    Ok(artifact)
}

pub(super) fn publish(args: &Value) -> Result<Value> {
    let plan = &args["plan"];
    if !plan.is_object() || plan["version"].as_u64() != Some(1) || !plan["shots"].is_array() {
        return Err("Cannot save an invalid shooting-script plan.".into());
    }
    let fingerprint = &args["fingerprint"];
    if plan["planner_version"] == PLANNER_VERSION {
        let motion = &plan["motion_plan"];
        if plan["planning_stage"] != "final"
            || !motion.is_object()
            || motion["plan_sha256"] != *fingerprint
        {
            return Err(
                "Complete the image-based motion pass before publishing the shooting script."
                    .into(),
            );
        }
        fidelity(&plan["source_fidelity"], plan, fingerprint)?;
    } else if plan.get("cast_identity").is_some() {
        let review = &plan["preproduction_review"];
        if !review.is_object()
            || review["version"].as_u64() != Some(1)
            || review["plan_sha256"] != *fingerprint
        {
            return Err(
                "Cannot publish a preproduction draft without matching final approval.".into(),
            );
        }
        approval::review(&review["report"], plan, true)?;
    }
    // Render the readable view before committing the machine artifact.
    let display = prompt::display(plan)?;
    let directory = PathBuf::from(
        args["run_dir"]
            .as_str()
            .ok_or("Missing shooting-script directory.")?,
    );
    artifacts::atomic_json(&directory.join("shooting-script.json"), plan)?;
    fs::write(directory.join("shooting-script.txt"), display).map_err(|e| e.to_string())?;
    Ok(Value::Null)
}
