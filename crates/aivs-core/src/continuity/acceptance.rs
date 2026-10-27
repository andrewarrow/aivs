//! Validate accepted storyboard receipts and compute repair invalidation.
use super::plan::validate_plan_data;
use super::review::{review_sequence, review_shot_data};
use super::types::Plan;
use super::{POLICY, Result, decode};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn dependent_shots(boards: &Value, changed: BTreeSet<usize>) -> Result<BTreeSet<usize>> {
    let shots = boards["shots"]
        .as_array()
        .ok_or("Missing storyboard shots.")?;
    let mut affected = changed;
    loop {
        let assets: Vec<_> = shots
            .iter()
            .filter(|e| {
                e["shot"]
                    .as_u64()
                    .is_some_and(|n| affected.contains(&(n as usize)))
            })
            .map(|e| (&e["image"]["file"], &e["image"]["sha256"]))
            .collect();
        let mut added = Vec::new();
        for entry in shots {
            if entry["generation"]["references"]
                .as_array()
                .is_some_and(|refs| {
                    refs.iter()
                        .any(|r| assets.contains(&(&r["file"], &r["sha256"])))
                })
            {
                added.push(
                    entry["shot"]
                        .as_u64()
                        .ok_or("Invalid storyboard shot number.")? as usize,
                );
            }
        }
        let old_len = affected.len();
        affected.extend(added);
        if old_len == affected.len() {
            return Ok(affected);
        }
    }
}

/// Hashes are supplied by the existing Python codec, never reserialized in Rust.
pub fn validate_acceptance(
    plan: &Value,
    boards: &Value,
    report: &Value,
    binding: &Value,
    report_sha256: &Value,
) -> Result<()> {
    let parsed: Plan = decode(plan)?;
    validate_plan_data(&parsed)?;
    let fail = "Storyboard continuity acceptance is failed, stale or incompatible; stop before video inference.";
    let rows = report["shots"].as_array().ok_or(fail)?;
    let shots = plan["shots"].as_array().ok_or(fail)?;
    let references = boards["references"].as_array().ok_or(fail)?;
    let reference_ids: BTreeSet<_> = references.iter().map(|r| r["id"].as_str()).collect();
    if report["version"] != 1
        || report["policy"] != POLICY
        || &report["binding"] != binding
        || report["status"] != "accepted"
        || boards["continuity_acceptance"]["policy"] != POLICY
        || &boards["continuity_acceptance"]["review_sha256"] != report_sha256
        || rows.len() != shots.len()
    {
        return Err(fail.into());
    }
    let mut reconstructed = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let findings = row["reference_findings"].as_array().ok_or(fail)?;
        if row["shot"].as_u64() != Some((index + 1) as u64)
            || row["decision"] != "pass"
            || !row["violations"].as_array().is_some_and(Vec::is_empty)
            || !row["confidence"]
                .as_f64()
                .is_some_and(|c| c.is_finite() && c >= 0.8 && c <= 1.0)
            || findings
                .iter()
                .map(|f| f["id"].as_str())
                .collect::<BTreeSet<_>>()
                != reference_ids
            || findings.iter().any(|f| {
                f["decision"] != "pass"
                    || !f["confidence"]
                        .as_f64()
                        .is_some_and(|c| c.is_finite() && c >= 0.8)
            })
        {
            return Err(fail.into());
        }
        let mut review = row.clone();
        let object = review.as_object_mut().ok_or(fail)?;
        for key in ["shot", "expected_opening_state", "unobservable_facts"] {
            object.remove(key);
        }
        let candidate = json!({"continuity_review": review});
        if row["expected_opening_state"] != shots[index]["continuity"]["opening"]
            || review_shot_data(&parsed, plan, index + 1, &candidate)?["decision"] != "pass"
        {
            return Err(
                "Storyboard acceptance contradicts its state contract or pixel observations."
                    .into(),
            );
        }
        reconstructed.insert(index + 1, candidate);
    }
    if review_sequence(plan, &reconstructed)?
        .values()
        .any(|r| r["decision"] != "pass")
    {
        return Err(
            "Storyboard acceptance contains a sequence-level physical or ownership contradiction."
                .into(),
        );
    }
    Ok(())
}
