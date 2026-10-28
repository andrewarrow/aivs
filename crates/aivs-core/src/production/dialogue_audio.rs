//! Project-owned, immutable PCM performances, reused independently of render quality.
use super::*;
use crate::media;
use std::process::Command;

const POLICY: &str = "fixed-dialogue-v1";

fn key(ctx: &mut Context<'_>, plan: &Value, index: usize, voice: &Value) -> Result<String> {
    let ids = rows(&plan["shots"][index]["beat_ids"])?;
    let beats = rows(&plan["source"]["beats"])?
        .iter()
        .filter(|beat| ids.contains(&beat["id"]))
        .map(|beat| {
            let mut beat = beat.clone();
            remove(&mut beat, &["id", "stable_id", "scene_id", "transition"]);
            beat
        })
        .collect::<Vec<_>>();
    let turns = sync::turns(plan, index)?;
    let cast = rows(&plan["source"]["cast"])?
        .iter()
        .filter(|binding| turns.iter().any(|turn| turn["speaker"] == binding["role"]))
        .map(|binding| {
            let profile = plan["source"]["characters"]
                .as_array()
                .and_then(|profiles| profiles.iter().find(|p| p["id"] == binding["character"]));
            json!({"cast":binding,"voice":profile.map(|p| json!({"voice":p["voice"],
                "voice_description":p["voice_description"],"voice_sample":p["voice_sample"]}))})
        })
        .collect::<Vec<_>>();
    ctx.digest(
        &json!({"policy":POLICY,"beats":beats,"cast":cast,"voice":voice,
        "voice_mode":plan["source"]["voice_mode"],"seconds":plan["shots"][index]["seconds"],
        "direction":plan["shots"][index]["prompt_template"]}),
    )
}

fn verify(project: &Path, value: &Value) -> Result<PathBuf> {
    let digest = text(value, "sha256")?;
    if value["policy"] != POLICY
        || value["review_status"] != "passed"
        || digest.len() != 64
        || !digest.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err("Reusable dialogue has an invalid receipt.".into());
    }
    let relative = PathBuf::from(format!("preproduction/cache/dialogue/audio/{digest}.wav"));
    if value["file"] != json!(relative) {
        return Err("Reusable dialogue has an unexpected audio path.".into());
    }
    let file = project.join(relative);
    if artifacts::file_hash(&file)? != digest {
        return Err(
            "Reusable dialogue audio changed. Preserve or restore it before rendering.".into(),
        );
    }
    Ok(file)
}

pub fn select(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    voice: &Value,
    run: &Path,
    resume: bool,
) -> Result<Value> {
    let Some(project) = ctx.project_root() else {
        return Ok(Value::Null);
    };
    let value = if resume {
        // Never introduce fixed audio into an existing native-audio take on Resume.
        let saved = run
            .join("shots")
            .join(format!("{:03}/render.json", index + 1));
        read(&saved)
            .ok()
            .and_then(|report| report.get("dialogue_audio").cloned())
            .unwrap_or(Value::Null)
    } else if ctx.options["fresh_dialogue"] == true || sync::turns(plan, index)?.is_empty() {
        Value::Null
    } else {
        let contract = key(ctx, plan, index, voice)?;
        let file = project
            .join("preproduction/cache/dialogue")
            .join(format!("{contract}.json"));
        if !file.exists() {
            return Ok(Value::Null);
        }
        let saved = read(&file)?;
        if saved["contract_sha256"] != contract || saved["review_status"] != "passed" {
            return Err("Reusable dialogue does not match its source contract.".into());
        }
        saved
    };
    if !value.is_null() {
        verify(&project, &value)?;
        if value["contract_sha256"] != key(ctx, plan, index, voice)? {
            return Err(
                "Saved dialogue belongs to different words, casting, timing or direction.".into(),
            );
        }
    }
    Ok(value)
}

pub fn file(ctx: &Context<'_>, value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    Ok(json!(verify(
        &ctx.project_root()
            .ok_or("Fixed dialogue needs its project.")?,
        value
    )?))
}

pub fn publish(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    voice: &Value,
    run: &Path,
    report: &Value,
    review: &Value,
) -> Result<Value> {
    let Some(project) = ctx.project_root() else {
        return Ok(Value::Null);
    };
    let turns = review["turns"].as_array();
    if review["status"] != "passed"
        || review["recognized_speech_coverage"].as_f64() != Some(1.0)
        || turns.is_none_or(|turns| {
            turns.is_empty()
                || turns
                    .iter()
                    .any(|turn| turn["text_coverage"].as_f64() != Some(1.0))
        })
    {
        return Ok(Value::Null);
    }
    // An already fixed performance remains the reusable original, avoiding
    // successive AAC decode/encode generations on every preview -> final run.
    if report["dialogue_audio"].is_object() {
        return Ok(report["dialogue_audio"].clone());
    }
    let contract = key(ctx, plan, index, voice)?;
    let directory = project.join("preproduction/cache/dialogue");
    let binding = directory.join(format!("{contract}.json"));
    if binding.is_file() && ctx.options["fresh_dialogue"] != true {
        let saved = read(&binding)?;
        verify(&project, &saved)?;
        if saved["contract_sha256"] != contract {
            return Err("Reusable dialogue belongs to a different source contract.".into());
        }
        return Ok(saved);
    }
    fs::create_dir_all(directory.join("audio")).map_err(|e| e.to_string())?;
    let temporary = tempfile::tempdir_in(&directory).map_err(|e| e.to_string())?;
    let staged = temporary.path().join("audio.wav");
    let clip = run
        .join("shots")
        .join(format!("{:03}/video.mp4", index + 1));
    if artifacts::file_hash(&clip)? != report["sha256"].as_str().ok_or("Missing shot hash.")? {
        return Err("Shot changed before publishing its dialogue.".into());
    }
    media::run(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
            .arg(&clip)
            .args([
                "-vn",
                "-map",
                "0:a:0",
                "-af",
                &format!(
                    "apad,atrim=duration={},asetpts=PTS-STARTPTS",
                    plan["shots"][index]["seconds"]
                ),
                "-ar",
                "48000",
                "-ac",
                "2",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&staged),
        &ctx.cancelled,
    )?;
    let digest = artifacts::file_hash(&staged)?;
    let relative = PathBuf::from(format!("preproduction/cache/dialogue/audio/{digest}.wav"));
    let destination = project.join(&relative);
    if destination.exists() {
        if artifacts::file_hash(&destination)? != digest {
            return Err("Reusable audio object changed.".into());
        }
    } else {
        fs::rename(&staged, &destination).map_err(|e| e.to_string())?;
    }
    let value = json!({"policy":POLICY,"contract_sha256":contract,"sha256":digest,"file":relative,
        "review_status":"passed","source_run":run.strip_prefix(&project).map_err(|e| e.to_string())?,
        "source_shot":index+1,"source_clip_sha256":report["sha256"],
        "reviewed_delivery_sha256":review["delivery_sha256"]});
    save(&binding, &value)?;
    Ok(value)
}
