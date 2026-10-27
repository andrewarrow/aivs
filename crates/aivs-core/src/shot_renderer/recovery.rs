//! Decide reuse, native-picture recovery, and take allocation before inference.
use super::{reports, storage, take_seed};
use crate::{artifacts, continuity::Result, storyboard};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) fn candidates(args: &Value) -> Result<Value> {
    let shot = reports::path(args, "shot_dir")?;
    let archive = reports::path(args, "archive_dir")?;
    let mut directories = Vec::new();
    if archive.is_dir() {
        for entry in fs::read_dir(archive).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().is_dir() {
                if let Some(take) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    directories.push((take, entry.path()));
                }
            }
        }
    }
    directories.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut rows = Vec::new();
    for directory in std::iter::once(shot).chain(directories.into_iter().map(|(_, path)| path)) {
        rows.push(json!({"directory": directory, "report": storage::saved(&directory)?}));
    }
    Ok(json!(rows))
}

fn matches(
    saved: &Value,
    report: &Value,
    clip: &Path,
    args: &Value,
    cancelled: &AtomicBool,
) -> Result<bool> {
    storage::matches(
        &json!({"saved": saved, "report": report, "clip": clip,
        "previous_clip": args["previous_clip"]}),
        cancelled,
    )
    .map(|value| value == true)
}

fn restore_storyboard(
    args: &Value,
    candidate: &Value,
    report: &mut Value,
    shot: &mut Value,
) -> Result<()> {
    let binding = &candidate["storyboard"];
    if binding.is_null() || *binding == report["storyboard"] {
        return Ok(());
    }
    if !report["storyboard_acceptance"].is_null() {
        return Err("Saved adapted storyboard has no acceptance for this exact sequence.".into());
    }
    // Python supplies only the legacy JSON fingerprint. Rust checks the exact
    // candidate and image bytes before applying directions or moving any files.
    let fingerprint = args["adapted_fingerprints"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["report"] == *candidate))
        .and_then(|row| row["fingerprint"].as_str())
        .ok_or("Saved adapted storyboard has no matching execution fingerprint.")?;
    if binding["planned_shot_sha256"] != fingerprint {
        return Err("Saved adapted storyboard belongs to different execution directions.".into());
    }
    let image = storyboard::resolve_image(binding, &reports::path(args, "run_dir")?)?;
    shot["storyboard_prompt"] = candidate["storyboard_prompt"].clone();
    report["storyboard"] = binding.clone();
    report["storyboard_prompt"] = candidate["storyboard_prompt"].clone();
    report["continuity"]["source_image"] = json!(image);
    report["continuity"]["source_image_sha256"] = binding["image"]["sha256"].clone();
    Ok(())
}

fn cancelled(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Relaxed) {
        Err("Shot recovery cancelled because its parent closed.".into())
    } else {
        Ok(())
    }
}

pub(super) fn recover(args: &Value, flag: &AtomicBool) -> Result<Value> {
    let shot_dir = reports::path(args, "shot_dir")?;
    let archive = reports::path(args, "archive_dir")?;
    let clip = shot_dir.join("video.mp4");
    let native = reports::path(args, "native_clip")?;
    let saved = storage::saved(&shot_dir)?;
    let mut report = args["report"].clone();
    let mut shot = args["shot"].clone();
    let automatic = args["automatic_review"] == true;
    let resume = args["resume"] == true;
    let voice = args["convert_voice"] == true;
    let mut reused = false;
    if resume {
        let rows = candidates(args)?;
        let mut failure = None;
        for row in rows.as_array().ok_or("Invalid saved take candidates.")? {
            cancelled(flag)?;
            let directory = reports::path(row, "directory")?;
            let candidate_clip = directory.join("video.mp4");
            if !candidate_clip.is_file() {
                continue;
            }
            let candidate = &row["report"];
            let mut trial_report = report.clone();
            let mut trial_shot = shot.clone();
            let checked = (|| -> Result<bool> {
                if candidate.is_null() {
                    return Err(
                        "Saved video has no valid render report; its provenance needs review."
                            .into(),
                    );
                }
                if !matches(candidate, &report, &candidate_clip, args, flag)? {
                    return Ok(false);
                }
                reports::validate_clip(&candidate_clip, &report, flag)?;
                restore_storyboard(args, candidate, &mut trial_report, &mut trial_shot)?;
                Ok(true)
            })();
            match checked {
                Ok(false) => continue,
                Err(error) => {
                    cancelled(flag)?;
                    if failure.is_none() {
                        failure = Some((candidate_clip, error));
                    }
                    continue;
                }
                Ok(true) => {}
            }
            cancelled(flag)?;
            if directory != shot_dir {
                storage::restore(&json!({"source": directory, "shot_dir": shot_dir,
                    "archive_dir": archive, "saved": candidate}))?;
                trial_report["restored_from"] = json!(directory);
            }
            report = trial_report;
            shot = trial_shot;
            reports::restore_fields(&mut report, &mut shot, candidate, automatic);
            report["runtime"] = candidate["runtime"].clone();
            report["reused"] = json!(true);
            for key in ["native_sha256", "voice_conversion"] {
                if let Some(value) = candidate.get(key) {
                    report[key] = value.clone();
                }
            }
            reused = true;
            break;
        }
        if !reused {
            if let Some((file, error)) = failure {
                let number = args["index"].as_u64().ok_or("Invalid shot index.")? + 1;
                return Err(format!(
                    "Shot {number} needs review: {error} Saved take: {}. No automatic retake was made. Resume rechecks saved footage.",
                    file.display()
                ));
            }
        }
    }
    let mut reuse_native = false;
    let mut pending = false;
    if !reused {
        cancelled(flag)?;
        if resume && voice && !saved.is_null() && native.is_file() {
            let digest = artifacts::file_hash(&native)?;
            if !saved["native_sha256"].is_null() && saved["native_sha256"] != digest {
                return Err(format!(
                    "Saved native video changed: {}. Review it before resuming.",
                    native.display()
                ));
            }
            if !saved["runtime"].is_null() && saved["native_sha256"] == digest {
                let mut native_report = saved.clone();
                native_report["sha256"] = Value::Null;
                reuse_native = matches(&native_report, &report, &native, args, flag)?;
                if reuse_native {
                    reports::validate_clip(&native, &report, flag)?;
                }
            }
        }
        pending = reuse_native
            || (resume
                && !saved.is_null()
                && !clip.exists()
                && saved["runtime"].is_null()
                && saved["sha256"].is_null()
                && matches(&saved, &report, &clip, args, flag)?);
        if pending {
            restore_storyboard(args, &saved, &mut report, &mut shot)?;
            reports::restore_fields(&mut report, &mut shot, &saved, automatic);
            if reuse_native {
                report["runtime"] = saved["runtime"].clone();
                report["native_sha256"] = saved["native_sha256"].clone();
            }
        } else if fs::read_dir(&shot_dir)
            .map_err(|e| e.to_string())?
            .next()
            .transpose()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            let take = saved["take"].as_u64().filter(|n| *n > 0).unwrap_or(1);
            let take = storage::archive(&shot_dir, &archive, take)?
                .checked_add(1)
                .ok_or("Take number overflow.")?;
            report["take"] = json!(take);
            report["seed"] = json!(take_seed(
                report["base_seed"]
                    .as_i64()
                    .ok_or("Invalid take base seed.")?,
                take
            ));
        }
    }
    cancelled(flag)?;
    artifacts::atomic_json(&shot_dir.join("render.json"), &report)?;
    Ok(json!({"report": report, "shot": shot, "reused": reused,
        "reuse_native": reuse_native, "pending": pending}))
}
