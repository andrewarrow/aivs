//! Atomic take archiving/restoration and byte evidence for Resume.
use super::{match_saved_take, valid_saved_report};
use crate::{artifacts, continuity::Result, media};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn path(args: &Value, field: &str) -> Result<PathBuf> {
    args[field]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("Missing take path: {field}"))
}

fn read(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn saved(directory: &Path) -> Result<Value> {
    Ok(read(&directory.join("render.json"))?
        .filter(valid_saved_report)
        .unwrap_or(Value::Null))
}

fn last(directory: &Path) -> Result<u64> {
    if !directory.is_dir() {
        return Ok(0);
    }
    let mut take = 0;
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path().is_dir() {
            if let Some(number) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u64>().ok())
            {
                take = take.max(number);
            }
        }
    }
    Ok(take)
}

fn archive(shot: &Path, archive: &Path, take: u64) -> Result<u64> {
    let take = take.max(
        last(archive)?
            .checked_add(1)
            .ok_or("Take number overflow.")?,
    );
    fs::create_dir_all(archive).map_err(|e| e.to_string())?;
    let destination = archive.join(format!("{take:03}"));
    if destination.exists() {
        return Err("Refusing to overwrite an archived take.".into());
    }
    artifacts::atomic_json(
        &shot.join("archive.json"),
        &json!({"original_directory": shot,
        "directory": destination, "clip": destination.join("video.mp4")}),
    )?;
    fs::rename(shot, &destination).map_err(|e| e.to_string())?;
    for filename in ["speech-validation.json", "render.json"] {
        let file = destination.join(filename);
        let Some(mut data) = read(&file)?.filter(Value::is_object) else {
            continue;
        };
        let speech = if filename == "render.json" {
            &mut data["speech_validation"]
        } else {
            &mut data
        };
        if speech.is_object() && speech.get("clip").is_some() {
            speech["original_clip"] = speech["clip"].clone();
            speech["clip"] = json!(destination.join("video.mp4"));
            artifacts::atomic_json(&file, &data)?;
        }
    }
    fs::create_dir(shot).map_err(|e| e.to_string())?;
    Ok(take)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn restore(args: &Value) -> Result<Value> {
    let source = path(args, "source")?;
    let shot = path(args, "shot_dir")?;
    let archive_dir = path(args, "archive_dir")?;
    let temporary = tempfile::Builder::new()
        .prefix(".restore-take-")
        .tempdir_in(shot.parent().ok_or("Missing shot parent.")?)
        .map_err(|e| e.to_string())?;
    let staged = temporary.path().join("take");
    copy_tree(&source, &staged)?;
    match fs::remove_file(staged.join("archive.json")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut restored = args["saved"].clone();
    restored["status"] = json!("verifying");
    restored["restored_from"] = json!(source);
    restored["reused"] = json!(true);
    for key in ["error", "speech_validation", "saved_take_review"] {
        restored
            .as_object_mut()
            .ok_or("Invalid saved take.")?
            .remove(key);
    }
    artifacts::atomic_json(&staged.join("render.json"), &restored)?;
    if fs::read_dir(&shot)
        .map_err(|e| e.to_string())?
        .next()
        .transpose()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        let take = saved(&shot)?["take"]
            .as_u64()
            .filter(|n| *n > 0)
            .unwrap_or(1);
        archive(&shot, &archive_dir, take)?;
    }
    fs::remove_dir(&shot).map_err(|e| e.to_string())?;
    fs::rename(staged, shot).map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

fn matches(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let clip = path(args, "clip")?;
    let mut evidence = json!({"saved": args["saved"], "report": args["report"], "clip_path": clip,
        "is_final_clip": clip.file_name().is_some_and(|s| s == "video.mp4") && clip.is_file(),
        "has_previous_clip": !args["previous_clip"].is_null()});
    let mut decision = match_saved_take(&evidence)?;
    if decision["status"] == "needs_previous_frame" {
        let temporary = tempfile::Builder::new()
            .prefix(".resume-frame-")
            .tempdir_in(clip.parent().ok_or("Missing clip parent.")?)
            .map_err(|e| e.to_string())?;
        let frame = temporary.path().join("frame.png");
        media::dispatch(
            "extract_last_frame",
            &json!({"clip": args["previous_clip"], "image": frame, "frame_index": decision["source_frame"]}),
            cancelled,
        )?;
        evidence["legacy_frame_sha256"] = json!(artifacts::file_hash(&frame)?);
        decision = match_saved_take(&evidence)?;
    }
    if decision["status"] == "needs_clip_hash" {
        evidence["clip_sha256"] = json!(artifacts::file_hash(&clip)?);
        decision = match_saved_take(&evidence)?;
    }
    match decision["status"].as_str() {
        Some("matched") => Ok(json!(true)),
        Some("mismatch") => Ok(json!(false)),
        _ => Err("The Rust engine returned an incomplete take verification.".into()),
    }
}

pub(super) fn dispatch(operation: &str, args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    match operation {
        "take_saved_report" => saved(&path(args, "shot_dir")?),
        "take_last_archive" => last(&path(args, "archive_dir")?).map(|n| json!(n)),
        "take_archive" => archive(
            &path(args, "shot_dir")?,
            &path(args, "archive_dir")?,
            args["take"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("Invalid take number.")?,
        )
        .map(|n| json!(n)),
        "take_restore" => restore(args),
        "take_matches" => matches(args, cancelled),
        _ => Err("Unknown take storage operation.".into()),
    }
}
