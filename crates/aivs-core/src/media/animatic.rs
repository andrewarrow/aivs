//! Silent held-keyframe previews; no model runtime or weights are involved.
use super::*;
use crate::{artifacts, storyboard};

pub(super) fn assemble(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let root = path(args, "run_dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let plan = &args["plan"];
    let manifest = &args["manifest"];
    let shots = plan["shots"]
        .as_array()
        .ok_or("Animatic needs planned shots.")?;
    let rows = manifest["shots"]
        .as_array()
        .ok_or("Animatic needs accepted keyframes.")?;
    if shots.is_empty() || shots.len() != rows.len() {
        return Err("Animatic needs keyframes for every shot.".into());
    }
    let folder = root.join("motion");
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let target = folder.join("animatic.mp4");
    let checkpoint = folder.join("animatic.json");
    let binding =
        json!({"keyframes": args["manifest_sha256"], "fps": FPS, "style": "held-keyframes-v1"});
    if target.is_file() && checkpoint.is_file() {
        let saved = artifacts::read_json(&checkpoint)?;
        if saved["binding"] == binding && saved["sha256"] == artifacts::file_hash(&target)? {
            return Ok(json!(target));
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix("animatic-")
        .tempdir_in(&folder)
        .map_err(|e| e.to_string())?;
    let temp = temporary.path();
    let mut concat = String::new();
    let mut index = 0;
    let mut total_frames = 0_u64;
    for (shot, row) in shots.iter().zip(rows) {
        let end = shot["seconds"]
            .as_u64()
            .and_then(|s| s.checked_mul(FPS))
            .ok_or("Invalid animatic duration.")?;
        total_frames = total_frames
            .checked_add(end)
            .ok_or("Animatic frame count overflow.")?;
        let frames = row["keyframes"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("Missing animatic keyframes.")?;
        for (offset, frame) in frames.iter().enumerate() {
            index += 1;
            let name = format!("{index:05}.png");
            fs::copy(storyboard::resolve_image(frame, &root)?, temp.join(&name))
                .map_err(|e| e.to_string())?;
            let current = frame["frame"]
                .as_u64()
                .ok_or("Invalid animatic frame number.")?;
            let next = if let Some(next) = frames.get(offset + 1) {
                next["frame"]
                    .as_u64()
                    .ok_or("Invalid next keyframe number.")?
            } else {
                end
            };
            let duration = next
                .checked_sub(current)
                .filter(|n| *n > 0)
                .ok_or("Animatic keyframes must have increasing times.")?
                as f64
                / FPS as f64;
            concat.push_str(&format!(
                "file '{name}'\noption framerate {FPS}\nduration {duration:.12}\n"
            ));
        }
    }
    concat.push_str(&format!("file '{index:05}.png'\noption framerate {FPS}\n"));
    fs::write(temp.join("frames.txt"), concat).map_err(|e| e.to_string())?;
    let partial = temp.join("animatic.mp4");
    run(
        ffmpeg()
            .args(["-f", "concat", "-safe", "0", "-i"])
            .arg(temp.join("frames.txt"))
            .args([
                "-r",
                &FPS.to_string(),
                "-frames:v",
                &total_frames.to_string(),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&partial),
        cancelled,
    )?;
    fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    artifacts::atomic_json(
        &checkpoint,
        &json!({"binding": binding, "sha256": artifacts::file_hash(&target)?,
        "audio": "silent", "preview": "held keyframes; no inferred human motion"}),
    )?;
    Ok(json!(target))
}
