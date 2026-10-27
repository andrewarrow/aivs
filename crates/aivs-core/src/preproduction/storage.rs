//! Reuse approved preproduction without replacing different run-local assets.
use crate::{artifacts, continuity::Result, shooting_script, storyboard};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

pub(super) fn direction(args: &Value) -> Result<Value> {
    let director = &args["director"];
    let final_plan = &args["final"];
    let before = director["shots"]
        .as_array()
        .ok_or("Missing director shots.")?;
    let after = final_plan["shots"]
        .as_array()
        .ok_or("Missing final shots.")?;
    if director["source_sha256"] != final_plan["source_sha256"] || before.len() != after.len() {
        return Err("Shot direction changed the storyboard source or coverage.".into());
    }
    for (before, after) in before.iter().zip(after) {
        let mut before = before.as_object().ok_or("Invalid director shot.")?.clone();
        let mut after = after.as_object().ok_or("Invalid final shot.")?.clone();
        for key in ["prompt", "prompt_template"] {
            before.remove(key);
            after.remove(key);
        }
        if before != after {
            return Err(
                "Shot direction changed a storyboard composition; regenerate preproduction.".into(),
            );
        }
    }
    Ok(Value::Null)
}

pub(super) fn copy_final(args: &Value) -> Result<Value> {
    let source = PathBuf::from(
        args["source"]
            .as_str()
            .ok_or("Missing preproduction source.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let destination = PathBuf::from(
        args["destination"]
            .as_str()
            .ok_or("Missing preproduction destination.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let plan = &args["plan"];
    let boards = &args["boards"];
    let shots = boards["shots"]
        .as_array()
        .ok_or("Missing storyboard shots.")?;
    let references = boards["references"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for entry in shots.iter().chain(references) {
        let original = storyboard::resolve_image(entry, &source)?;
        let relative = entry["image"]["file"]
            .as_str()
            .ok_or("Missing storyboard image path.")?;
        let asset_root = crate::project::image_root(&entry["image"], &destination)?;
        let target = asset_root.join(relative);
        let parent = target
            .parent()
            .ok_or("Missing storyboard destination directory.")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        if !parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&asset_root)
        {
            return Err("Storyboard destination escapes its run directory.".into());
        }
        if target.exists() {
            if artifacts::file_hash(&target)?
                != entry["image"]["sha256"]
                    .as_str()
                    .ok_or("Missing storyboard image hash.")?
            {
                return Err(format!(
                    "A different storyboard already exists: {}",
                    relative
                ));
            }
        } else {
            artifacts::exclusive_bytes(&target, &fs::read(&original).map_err(|e| e.to_string())?)?;
        }
    }
    artifacts::atomic_json(&destination.join("storyboard-plan.json"), &args["director"])?;
    if source.join("storyboard-plan.txt").is_file() {
        fs::copy(
            source.join("storyboard-plan.txt"),
            destination.join("storyboard-plan.txt"),
        )
        .map_err(|e| e.to_string())?;
    }
    artifacts::atomic_json(&destination.join("storyboards.json"), boards)?;
    artifacts::atomic_json(
        &destination.join("storyboard-references.json"),
        &json!({"version": 1,
        "source_sha256": plan["source_sha256"], "references": references}),
    )?;
    artifacts::atomic_json(
        &destination.join("storyboard-review.json"),
        &artifacts::read_json(&source.join("storyboard-review.json"))?,
    )?;
    shooting_script::dispatch(
        "shooting_publish",
        &json!({"plan": plan, "run_dir": destination, "fingerprint": args["fingerprint"]}),
    )
}
