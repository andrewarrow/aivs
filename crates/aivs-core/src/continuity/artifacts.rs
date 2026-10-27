//! Acceptance artifact contracts. The client retains the legacy JSON digest codec.
use super::{POLICY, Result, validate_plan};
use serde_json::{Value, json};

pub fn manifest(plan: &Value, boards: &Value) -> Result<()> {
    validate_plan(plan)?;
    if boards["policy"] != "per-shot-first-frame-v2"
        || boards["status"] != "complete"
        || boards["source_sha256"] != plan["source_sha256"]
    {
        return Err(
            "Storyboard acceptance requires a complete compatible manifest for this exact source."
                .into(),
        );
    }
    Ok(())
}

/// Compare independently read snapshots, after the adapter validates profile assets.
pub fn snapshots(plan: &Value, screenplay: &Value, profiles: &Value) -> Result<()> {
    if !screenplay.is_null() && screenplay != &plan["source"]["script"] {
        return Err("The screenplay snapshot changed after storyboard acceptance.".into());
    }
    let profiles = profiles.as_array().ok_or("Missing saved cast snapshots.")?;
    let supplied = match plan["source"].get("characters") {
        None => &[][..],
        Some(value) => value
            .as_array()
            .ok_or("Invalid source cast snapshots.")?
            .as_slice(),
    };
    if profiles.len() != supplied.len() {
        return Err("The cast snapshot changed after storyboard acceptance.".into());
    }
    for (expected, saved) in supplied.iter().zip(profiles) {
        let fields = expected
            .as_object()
            .ok_or("Invalid source cast snapshot.")?;
        if !saved.is_object() || fields.iter().any(|(key, value)| &saved[key] != value) {
            return Err("The cast snapshot changed after storyboard acceptance.".into());
        }
    }
    Ok(())
}

pub fn report_binding(args: &Value) -> Result<Value> {
    let plan = &args["plan"];
    let boards = &args["boards"];
    for field in [
        "review_policy_sha256",
        "source_snapshot_sha256",
        "plan_sha256",
    ] {
        if !args[field].as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }) {
            return Err(format!("Missing or invalid storyboard binding {field}."));
        }
    }
    let shots = boards["shots"]
        .as_array()
        .ok_or("Missing storyboard shots.")?;
    if shots
        .iter()
        .any(|entry| entry["shot"].as_u64().is_none() || !entry["image"].is_object())
    {
        return Err("Invalid storyboard image binding.".into());
    }
    Ok(json!({
        "policy": POLICY, "source_sha256": plan["source_sha256"],
        "review_policy_sha256": args["review_policy_sha256"],
        "source_snapshot_sha256": args["source_snapshot_sha256"],
        "plan_sha256": args["plan_sha256"],
        "references": boards.get("references").cloned().unwrap_or_else(|| json!([])),
        "images": shots.iter().map(|entry| json!({"shot": entry["shot"], "image": entry["image"]})).collect::<Vec<_>>(),
    }))
}
