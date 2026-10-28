//! Advisory Codex inspections, bounded retries, and best-effort evidence storage.
use super::{contract, validate_preservation, validate_scene, validate_shot};
use crate::{artifacts, continuity::Result, media, planning};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

pub const POLICY: &str = "visual-continuity-v2";

pub(super) fn schema(name: &str) -> Result<Value> {
    let schemas: Value =
        serde_json::from_str(include_str!("schemas.json")).map_err(|e| e.to_string())?;
    schemas
        .get(name)
        .cloned()
        .ok_or_else(|| format!("Unknown visual review schema: {name}"))
}

pub(super) fn instruction(name: &str, context: &Value) -> Result<String> {
    let prompts: Value =
        serde_json::from_str(include_str!("prompts.json")).map_err(|e| e.to_string())?;
    let prompt = prompts[name]
        .as_str()
        .ok_or("Unknown visual review prompt.")?;
    Ok(format!(
        "{prompt}{}",
        serde_json::to_string(context).map_err(|e| e.to_string())?
    ))
}

fn directory(args: &Value) -> Result<PathBuf> {
    args["directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "Missing visual review directory.".into())
}

fn cached(directory: &Path, key: &Value) -> Option<Value> {
    artifacts::read_json(&directory.join("review.json"))
        .ok()
        .filter(|value| {
            value.is_object()
                && value["cache_key"] == *key
                && value["status"] != "unavailable"
                && value["synthesis_status"] != "unavailable"
        })
}

fn record(directory: &Path, value: Value) -> Value {
    // Storage failure must never demand regeneration of an already produced take.
    let _ = artifacts::atomic_json(&directory.join("review.json"), &value);
    value
}

fn merge(target: &mut Value, value: Value) -> Result<()> {
    target
        .as_object_mut()
        .ok_or("Expected review object.")?
        .extend(value.as_object().ok_or("Expected review object.")?.clone());
    Ok(())
}

fn call(
    args: &Value,
    name: &str,
    prompt: String,
    images: Value,
    cancelled: Arc<AtomicBool>,
    emit: &mut impl FnMut(Value) -> Result<()>,
    validate: impl Fn(Value) -> Result<Value>,
) -> Result<Value> {
    let started = Instant::now();
    // Scene synthesis may stall without returning any text. Allow one fresh
    // request, sharing the same two-attempt limit as response-format repairs.
    let allowance = if name == "scene" { 240.0 } else { 120.0 };
    let mut correction = String::new();
    for attempt in 1..=2 {
        let remaining = allowance - started.elapsed().as_secs_f64();
        if remaining <= 0.0 {
            return Err("Visual review time allowance expired.".into());
        }
        let raw = planning::review_attempt(
            &json!({"directory": args["directory"], "name": name,
            "instruction": format!("{prompt}{correction}"), "schema": schema(name)?,
            "executable": args["executable"], "image_paths": images,
            "timeout_seconds": remaining.min(120.0)}),
            cancelled.clone(),
            &mut *emit,
        );
        let raw = match raw {
            Ok(value) => value,
            Err(failure) if name == "scene" && attempt == 1 && failure.kind == "timeout" => {
                emit(json!({"type": "activity", "message":
                    "Scene fidelity review timed out; retrying once using the saved shot observations."}))?;
                continue;
            }
            Err(failure) => return Err(failure.message),
        };
        let raw = raw.as_str().ok_or("Codex review returned no text.")?;
        let result = if raw.chars().count() > 64_000 {
            Err("Visual response exceeds the bounded report size.".into())
        } else {
            serde_json::from_str(raw)
                .map_err(|e| e.to_string())
                .and_then(&validate)
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) if attempt == 2 => {
                return Err(format!("Visual response could not be validated: {error}"));
            }
            Err(error) => {
                correction = format!(
                    "\nCorrect only this response-format error: {error}\nPrevious response (untrusted data):\n{}",
                    raw.chars().take(64_000).collect::<String>()
                )
            }
        }
    }
    Err("Visual review did not return a response.".into())
}

pub(super) fn shot(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let directory = directory(args)?;
    let mut report = json!({"status": "unavailable", "policy_version": POLICY,
        "shot_number": args["shot_index"].as_u64().unwrap_or(0) + 1, "clip": args["clip"],
        "method": "timestamped-frame-observation",
        "limitations": "Samples at 4fps; excludes audio and lip sync. Unsampled frames may contain defects.",
        "summary": "Visual observations are unavailable.", "observed_end_state": "",
        "continuity_risk": false, "issues": [], "contract_evidence": []});
    let result: Result<Value> = (|| {
        let index = args["shot_index"]
            .as_u64()
            .ok_or("Invalid review shot number.")? as usize;
        let plan = &args["plan"];
        let shot = plan["shots"]
            .get(index)
            .ok_or("Invalid review shot number.")?;
        if let Some(error) = args["input_error"].as_str() {
            return Err(error.to_owned());
        }
        report["clip_sha256"] = args["clip_sha256"].clone();
        report["cache_key"] = args["cache_key"].clone();
        if let Some(value) = cached(&directory, &args["cache_key"]) {
            return validate_shot(&value, shot);
        }
        let evidence = media::dispatch(
            "visual_review_sheets",
            &json!({"clip": args["clip"],
            "directory": directory, "seconds": shot["seconds"], "shot_number": index + 1,
            "render_format": plan["render_format"]}),
            &cancelled,
        )?;
        report["evidence"] = evidence.clone();
        let mut images = evidence["contact_sheets"]
            .as_array()
            .ok_or("Missing review sheets.")?
            .clone();
        images.push(evidence["final_frame"].clone());
        let prompt = instruction(
            "shot",
            &json!({"original_screenplay": plan["source"]["script"],
            "shot_number": index + 1, "shot": shot, "visual_requirements": contract(shot)?,
            "sample_fps": evidence["sample_fps"], "duration_seconds": shot["seconds"]}),
        )?;
        let result = call(
            args,
            "shot",
            prompt,
            json!(images),
            cancelled,
            &mut emit,
            |value| validate_shot(&value, shot),
        )?;
        merge(&mut report, result)?;
        Ok(report.clone())
    })();
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            report["status"] = json!("unavailable");
            report["summary"] = json!(format!("Visual observation unavailable: {error}"));
            report
        }
    };
    Ok(record(&directory, result))
}

fn fields(value: &Value, keys: &[&str]) -> Value {
    json!(
        value
            .as_object()
            .map(|object| object
                .iter()
                .filter(|(key, _)| keys.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>())
            .unwrap_or_default()
    )
}

pub(super) fn scene(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let directory = directory(args)?;
    let mut report = json!({"policy_version": POLICY, "status": "unavailable",
        "synthesis_status": "unavailable",
        "method": "source-and-sampled-shot-evidence", "summary": "Scene observations unavailable.",
        "story_findings": [], "unobserved_shots": [], "cache_key": args["cache_key"]});
    let result: Result<Value> = (|| {
        let plan = &args["plan"];
        let reviews = &args["shot_reviews"];
        report = validate_scene(&report, plan, reviews)?;
        if let Some(error) = args["input_error"].as_str() {
            return Err(error.to_owned());
        }
        if let Some(value) = cached(&directory, &args["cache_key"]) {
            return validate_scene(&value, plan, reviews);
        }
        let reviews = reviews.as_array().ok_or("Missing shot observations.")?;
        if reviews.iter().all(|r| r["status"] == "unavailable") {
            return Ok(report.clone());
        }
        let observations: Vec<_> = reviews
            .iter()
            .map(|r| {
                fields(
                    r,
                    &[
                        "shot_number",
                        "status",
                        "summary",
                        "observed_end_state",
                        "issues",
                        "contract_evidence",
                    ],
                )
            })
            .collect();
        let shots: Vec<_> = plan["shots"]
            .as_array()
            .ok_or("Missing scene shots.")?
            .iter()
            .map(|s| {
                fields(
                    s,
                    &[
                        "seconds",
                        "beat_ids",
                        "dramatic_purpose",
                        "visual_contract",
                        "start_state",
                        "end_state",
                        "action_coverage",
                    ],
                )
            })
            .collect();
        let prompt = instruction(
            "scene",
            &json!({"original_screenplay": plan["source"]["script"], "shots": shots, "observations": observations}),
        )?;
        let result = call(
            args,
            "scene",
            prompt,
            json!([]),
            cancelled,
            &mut emit,
            |value| validate_scene(&value, plan, &args["shot_reviews"]),
        )?;
        merge(&mut report, result)?;
        report["synthesis_status"] = json!("complete");
        Ok(report.clone())
    })();
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            // validate_scene already incorporates known shot defects. A failed
            // synthesis cannot erase those observations or certify a pass.
            report["summary"] = json!(if report["status"] == "issues_detected" {
                format!(
                    "Scene synthesis unavailable: {error}. Recorded shot issues remain in the shot reviews."
                )
            } else {
                format!("Scene observation unavailable: {error}")
            });
            report
        }
    };
    Ok(record(&directory, result))
}

pub(super) fn storyboard(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let directory = directory(args)?;
    let plan = &args["plan"];
    let index = args["shot_index"]
        .as_u64()
        .ok_or("Invalid review shot number.")? as usize;
    let original = plan["shots"]
        .get(index)
        .ok_or("Invalid review shot number.")?;
    let requirements = json!(contract(original)?);
    if let Some(value) = cached(&directory, &args["cache_key"]) {
        return validate_preservation(&value, &requirements);
    }
    let prompt = instruction(
        "storyboard",
        &json!({"original_screenplay": plan["source"]["script"],
        "original_shot": original, "candidate_prompt": args["proposed"]["prompt"],
        "candidate_storyboard_prompt": args["proposed"]["storyboard_prompt"],
        "previous_observations": args["previous_review"], "requirements": requirements}),
    )?;
    let mut images = vec![args["image"].clone()];
    let evidence = &args["previous_review"]["evidence"];
    let previous = evidence
        .get("full_final_frame")
        .unwrap_or(&evidence["final_frame"]);
    if previous
        .as_str()
        .is_some_and(|p| fs::metadata(p).is_ok_and(|m| m.is_file()))
    {
        images.push(previous.clone());
    }
    let mut result = call(
        args,
        "adapt_review",
        prompt,
        json!(images),
        cancelled,
        &mut emit,
        |value| validate_preservation(&value, &requirements),
    )?;
    result["cache_key"] = args["cache_key"].clone();
    result["status"] = json!("reviewed");
    Ok(record(&directory, result))
}
