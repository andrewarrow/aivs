//! Image-grounded motion batches and concrete repairs, including resumed rows.
use super::{array, bounded};
use crate::continuity::{self, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn review(plan: &Value, number: usize, row: &Value) -> Result<Value> {
    continuity::dispatch(
        &json!({"operation": "review_shot", "args": {"plan": plan, "number": number, "row": row}}),
    )
}

pub(super) fn validate(args: &Value) -> Result<Value> {
    let plan = &args["plan"];
    let numbers: Vec<usize> =
        serde_json::from_value(args["numbers"].clone()).map_err(|e| e.to_string())?;
    let supplied = array(
        &args["candidate"]["shots"],
        "Return exactly the requested sequence shot numbers.",
    )?;
    if supplied.len() != numbers.len() {
        return Err("Return exactly the requested sequence shot numbers.".into());
    }
    let reference_ids: BTreeSet<String> =
        serde_json::from_value(args["reference_ids"].clone()).map_err(|e| e.to_string())?;
    let mut trial = plan.clone();
    for (&number, row) in numbers.iter().zip(supplied) {
        if number == 0
            || trial["shots"]
                .as_array()
                .is_none_or(|shots| number > shots.len())
            || row["shot"].as_u64() != Some(number as u64)
        {
            return Err("Sequence shot numbers are missing or out of order.".into());
        }
        bounded(&row["image_observations"], "Pixel observations", 1200)?;
        bounded(&row["prompt_template"], "Motion direction", 1600)?;
        let report = review(plan, number, row)?;
        let fixes = array(
            &row["plan_repair"],
            "A plan repair must be empty or contain the complete replacement shot.",
        )?;
        if fixes.len() > 1 {
            return Err(
                "A plan repair must be empty or contain the complete replacement shot.".into(),
            );
        }
        if report["failure_kind"] == "plan" && fixes.is_empty() {
            return Err("A structurally wrong plan requires a concrete replacement state/coverage contract.".into());
        }
        if !fixes.is_empty() && report["failure_kind"] != "plan" {
            return Err("Only a diagnosed plan failure may revise a shot contract.".into());
        }
        let worlds = array(
            &row["world_repair"],
            "Canonical world repair must be empty or one complete replacement world.",
        )?;
        if worlds.len() > 1 {
            return Err(
                "Canonical world repair must be empty or one complete replacement world.".into(),
            );
        }
        if !worlds.is_empty() && report["failure_kind"] != "plan" {
            return Err(
                "Only a diagnosed plan failure may revise canonical physical definitions.".into(),
            );
        }
        let findings = array(
            &report["reference_findings"],
            "Inspect every supplied canonical reference image exactly once per shot review.",
        )?;
        let found: BTreeSet<_> = findings
            .iter()
            .filter_map(|f| f["id"].as_str().map(str::to_owned))
            .collect();
        if findings.len() != reference_ids.len() || found != reference_ids {
            return Err(
                "Inspect every supplied canonical reference image exactly once per shot review."
                    .into(),
            );
        }
        let mut bad_references = Vec::new();
        for finding in findings {
            bounded(&finding["evidence"], "Reference pixel evidence", 1200)?;
            let confidence = finding["confidence"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=1.0).contains(n));
            if !matches!(
                finding["decision"].as_str(),
                Some("pass" | "repair-required" | "uncertain")
            ) || confidence.is_none()
            {
                return Err("Reference findings need a valid decision and confidence.".into());
            }
            if finding["decision"] != "pass" || confidence.unwrap_or(0.0) < 0.8 {
                bad_references.push(finding);
            }
        }
        if report["decision"] != "pass" || !bad_references.is_empty() {
            if bounded(
                &row["storyboard_prompt"],
                "Concrete corrected opening composition",
                1200,
            )
            .is_err()
            {
                let mut reasons: Vec<String> = serde_json::from_value(report["violations"].clone())
                    .map_err(|e| e.to_string())?;
                for finding in bad_references {
                    reasons.push(format!(
                        "Reference {}: {} (confidence {}): {}",
                        finding["id"].as_str().unwrap_or_default(),
                        finding["decision"].as_str().unwrap_or_default(),
                        finding["confidence"],
                        finding["evidence"].as_str().unwrap_or_default()
                    ));
                }
                let detail = if reasons.is_empty() {
                    report["evidence"].as_str().unwrap_or_default().to_owned()
                } else {
                    reasons.join("; ")
                };
                return Err(format!(
                    "Shot {number} deterministic review is {}: {detail}. If the observations used different attribute keys from the opening contract, correct those keys using actual pixel evidence. Otherwise supply a complete corrected storyboard_prompt (1–1200 characters) for the required repair. Do not mark hidden or uncertain facts visible merely to pass this check.",
                    report["decision"].as_str().unwrap_or_default()
                ));
            }
        } else if row["storyboard_prompt"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            || !fixes.is_empty()
            || !worlds.is_empty()
        {
            return Err("Accepted frames must not include repairs.".into());
        }
        let blocking = row["blocking_issue"].as_str();
        let composition = row["storyboard_prompt"].as_str();
        if blocking.is_none_or(|s| s.chars().count() > 1200 || s.contains('\0'))
            || composition.is_none_or(|s| {
                s.chars().count() > 1200 || s.contains('\0') || s.contains("{{") || s.contains("}}")
            })
        {
            return Err(
                "Storyboard repairs need bounded static compositions and concrete blocking text."
                    .into(),
            );
        }
        if report["decision"] == "pass" {
            trial["shots"][number - 1]["prompt_template"] = row["prompt_template"].clone();
            trial["shots"][number - 1]
                .as_object_mut()
                .unwrap()
                .remove("prompt");
        }
    }
    let mut combined: BTreeMap<usize, Value> =
        serde_json::from_value(args["rows"].clone()).map_err(|e| e.to_string())?;
    for (&number, row) in numbers.iter().zip(supplied) {
        combined.insert(number, row.clone());
    }
    let sequence = continuity::dispatch(
        &json!({"operation": "review_sequence", "args": {"plan": plan, "rows": combined}}),
    )?;
    for number in &numbers {
        let report = &sequence[number.to_string()];
        if report["decision"] != "pass"
            && combined[number]["storyboard_prompt"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(format!(
                "Shot {number} sequence contradiction requires a corrected composition: {}",
                report["violations"]
            ));
        }
    }
    let mut trials = vec![trial];
    if combined.len() == plan["shots"].as_array().map_or(0, Vec::len)
        && combined.values().any(|row| {
            row["plan_repair"].as_array().is_some_and(|a| !a.is_empty())
                || row["world_repair"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
        })
    {
        let mut corrected = plan.clone();
        let mut world: Option<Value> = None;
        for (number, row) in &combined {
            let index = number
                .checked_sub(1)
                .filter(|i| *i < corrected["shots"].as_array().map_or(0, Vec::len))
                .ok_or("Invalid saved motion shot number.")?;
            if let Some(replacement) = row["plan_repair"].as_array().and_then(|a| a.first()) {
                corrected["shots"][index] = replacement.clone();
                corrected["shots"][index]
                    .as_object_mut()
                    .ok_or("Invalid replacement shot.")?
                    .remove("prompt");
            }
            for replacement in array(&row["world_repair"], "Invalid saved world repair.")? {
                if world.as_ref().is_some_and(|w| w != replacement) {
                    return Err("Canonical world corrections conflict across shots.".into());
                }
                world = Some(replacement.clone());
            }
        }
        if let Some(world) = world {
            corrected["world"] = world;
        }
        trials.push(corrected);
    }
    // The adapter checks these compiled prompts with the Python model tokenizer.
    Ok(json!({"rows": supplied, "trial_plans": trials}))
}
