//! Run-local shot contracts and publication. JSON fingerprints stay adapter-owned.
use super::{storage, take_seed, uses_previous_frame};
use crate::{artifacts, continuity::Result, media, storyboard};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

pub(super) fn path(args: &Value, field: &str) -> Result<PathBuf> {
    args[field]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("Missing shot path: {field}."))
}

fn copy_fields(target: &mut Value, source: &Value, fields: &[&str]) {
    for key in fields {
        if let Some(value) = source.get(*key) {
            target[*key] = value.clone();
        }
    }
}

pub(super) fn prepare(args: &Value) -> Result<Value> {
    let root = path(args, "run_dir")?;
    let index = args["index"].as_u64().ok_or("Invalid shot index.")?;
    let planned = args["plan"]["shots"]
        .get(index as usize)
        .ok_or("Invalid shot index.")?;
    let shot = &args["shot"];
    let base_seed = args["base_seed"]
        .as_i64()
        .ok_or("Invalid take base seed.")?;
    let shot_dir = root.join("shots").join(format!("{:03}", index + 1));
    let archive_dir = root.join("takes").join(format!("{:03}", index + 1));
    fs::create_dir_all(&shot_dir).map_err(|e| e.to_string())?;
    let take = storage::last(&archive_dir)?
        .checked_add(1)
        .ok_or("Take number overflow.")?;
    let binding = &args["frame_binding"];
    let assets = &args["assets"];
    let previous =
        binding.is_null() && uses_previous_frame(&args["plan"]["shots"], &args["index"])?;
    let image = if !binding.is_null() {
        Some(storyboard::resolve_image(binding, &root)?)
    } else if previous {
        Some(shot_dir.join("start-frame.png"))
    } else {
        assets["image"]["file"].as_str().map(|file| root.join(file))
    };
    let clip = shot_dir.join("video.mp4");
    let convert_voice = assets["voice"].get("file").is_some();
    let native = if convert_voice {
        shot_dir.join("video.source.mp4")
    } else {
        clip.clone()
    };
    let source_frame = if previous {
        Some(
            args["plan"]["shots"][index as usize - 1]["seconds"]
                .as_u64()
                .and_then(|n| n.checked_mul(24))
                .and_then(|n| n.checked_sub(1))
                .ok_or("Invalid predecessor duration.")?,
        )
    } else {
        None
    };
    let mut continuity = json!({
        "conditioning": if !binding.is_null() { "storyboard-first-frame" }
            else if previous { "previous-shot-last-frame" }
            else if image.is_some() { "saved-character-portrait" } else { "none" },
        "source_shot": if previous { Some(index) } else { None },
        "source_frame": source_frame, "source_image": image,
        "source_clip_sha256": if previous { &args["previous_summary"]["sha256"] } else { &Value::Null },
    });
    if !binding.is_null() {
        continuity["source_image_sha256"] = binding["image"]["sha256"].clone();
    }
    let mut report = json!({"prompt": shot["prompt"], "base_seed": base_seed,
        "seed": take_seed(base_seed, take), "take": take, "seconds": shot["seconds"],
        "width": args["width"], "height": args["height"], "status": "running", "runtime": null,
        "start_state": shot["start_state"], "end_state": shot["end_state"],
        "beat_ids": shot["beat_ids"], "scene_id": shot["scene_id"], "continuity": continuity});
    if !assets.is_null() {
        report["character_assets"] = assets.clone();
    }
    if !binding.is_null() {
        report["storyboard"] = binding.clone();
        report["planned_storyboard"] = binding.clone();
        report["storyboard_prompt"] = shot["storyboard_prompt"].clone();
    }
    for (arg, field) in [
        ("render_config", "render_config"),
        ("motion_row", "motion_keyframes"),
        ("storyboard_acceptance", "storyboard_acceptance"),
    ] {
        if !args[arg].is_null() {
            report[field] = args[arg].clone();
        }
    }
    if args["automatic_review"] == true {
        report["planned_prompt"] = planned["prompt"].clone();
        report["planned_shot_sha256"] = args["planned_shot_sha256"].clone();
        report["prompt_template"] = shot["prompt_template"].clone();
        report["observed_source_clip_sha256"] = args["previous_summary"]["sha256"].clone();
    }
    Ok(
        json!({"report": report, "shot_dir": shot_dir, "archive_dir": archive_dir,
        "clip": clip, "native_clip": native, "convert_voice": convert_voice,
        "use_previous_frame": previous, "image_file": image, "source_frame": source_frame}),
    )
}

pub(super) fn save(args: &Value) -> Result<Value> {
    artifacts::atomic_json(
        &path(args, "shot_dir")?.join("render.json"),
        &args["report"],
    )?;
    Ok(Value::Null)
}

pub(super) fn failure(args: &Value) -> Result<Value> {
    let shot = path(args, "shot_dir")?;
    let checkpoint = if args["committed"] == true {
        args["report"].clone()
    } else {
        storage::saved(&shot)?
    };
    let mut failed = if checkpoint.is_null() {
        args["report"].clone()
    } else {
        checkpoint.clone()
    };
    failed["status"] = args["status"].clone();
    if !args["error"].is_null() {
        failed["error"] = args["error"].clone();
    }
    // Never invent provenance for footage whose original report is unavailable.
    if !checkpoint.is_null() || !shot.join("video.mp4").is_file() {
        artifacts::atomic_json(&shot.join("render.json"), &failed)?;
    }
    Ok(failed)
}

pub(super) fn validate_clip(
    clip: &std::path::Path,
    report: &Value,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let frames = report["seconds"]
        .as_u64()
        .and_then(|n| n.checked_mul(24))
        .ok_or("Invalid shot frame count.")?;
    media::dispatch(
        "validate_video",
        &json!({"path": clip, "frames": frames,
        "width": report["width"], "height": report["height"], "expect_audio": true}),
        cancelled,
    )
}

pub(super) fn complete(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let shot_dir = path(args, "shot_dir")?;
    let clip = shot_dir.join("video.mp4");
    let mut report = args["report"].clone();
    // Preserve the completed inference receipt even if hashing is interrupted.
    artifacts::atomic_json(&shot_dir.join("render.json"), &report)?;
    report["sha256"] = json!(artifacts::file_hash(&clip)?);
    report["status"] = json!("verifying");
    artifacts::atomic_json(&shot_dir.join("render.json"), &report)?;
    if args["reused"] != true {
        validate_clip(&clip, &report, cancelled)?;
    }
    Ok(report)
}

pub(super) fn assemble(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let assembled = media::dispatch("assemble_shots", args, cancelled)?;
    let partial = path(&assembled, "partial")?;
    let info = media::dispatch(
        "validate_video",
        &json!({"path": partial,
        "frames": assembled["frames"], "width": args["width"], "height": args["height"],
        "expect_audio": true}),
        cancelled,
    )?;
    fs::rename(partial, path(args, "output")?).map_err(|e| e.to_string())?;
    Ok(info)
}

pub(super) fn restore_fields(report: &mut Value, shot: &mut Value, saved: &Value, automatic: bool) {
    report["take"] = saved.get("take").cloned().unwrap_or(json!(1));
    report["seed"] = saved["seed"].clone();
    if automatic {
        shot["prompt"] = saved["prompt"].clone();
        if let Some(template) = saved.get("prompt_template") {
            shot["prompt_template"] = template.clone();
        }
        if saved["adaptation"].is_object() {
            shot["adaptation"] = saved["adaptation"].clone();
        }
        copy_fields(report, shot, &["prompt", "prompt_template", "adaptation"]);
    }
}
