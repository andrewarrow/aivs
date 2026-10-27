//! Deterministic LTX input preparation and publication around model-only work.
use super::*;

pub(super) fn prepare(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let profile = args["profile"]
        .as_str()
        .filter(|s| matches!(*s, "hq" | "distilled"))
        .ok_or("Local render profile must be distilled or hq.")?;
    if profile == "hq" && !args["audio_file"].is_null() {
        return Err("The HQ profile generates native audio; fixed audio is unsupported.".into());
    }
    let frames = integer(args, "frames")?;
    let width = integer(args, "width")?;
    let height = integer(args, "height")?;
    if [width, height].iter().any(|n| *n < 64 || n % 64 != 0) {
        return Err(
            "Render frames must be positive and dimensions must be positive multiples of 64."
                .into(),
        );
    }
    let mut image = if args["image_file"].is_null() {
        Value::Null
    } else {
        source::image(&path(args, "image_file")?)?
    };
    let anchors = if args["image_anchors"].is_null() {
        Value::Null
    } else {
        source::anchors(
            &json!({"anchors": args["image_anchors"], "frames": frames, "width": width, "height": height,
                "allow_resize": profile == "distilled"}),
        )?
    };
    let hold = args["hold_image_framing"] == true;
    if !anchors.is_null() && (hold || !args["audio_file"].is_null()) {
        return Err("Timed image anchors require the text/image video pipeline without held framing or fixed audio.".into());
    }
    if !anchors.is_null()
        && !image.is_null()
        && image["source_sha256"] != anchors[0]["source_sha256"]
    {
        return Err("The opening anchor must match the supplied starting image.".into());
    }
    let audio = if args["audio_file"].is_null() {
        Value::Null
    } else {
        source::audio(
            &json!({"source": args["audio_file"], "minimum_duration": frames as f64 / FPS as f64}),
        )?
    };
    if !image.is_null() && hold {
        image["conditioning"] = json!("whole-first-and-last-frame");
        image["anchor_frames"] = if frames <= 1 {
            json!([0])
        } else {
            json!([0, frames - 1])
        };
    }
    let output = path(args, "output")?;
    let native = output.with_file_name("video.native.mp4");
    let partial = output.with_file_name("video.partial.mp4");
    remove_existing(&partial)?;
    let inference_frames = (frames - 1)
        .checked_add(7)
        .and_then(|n| (n / 8).checked_mul(8))
        .and_then(|n| n.checked_add(1))
        .ok_or("LTX inference frame count overflow.")?;
    let conditioning_audio = if audio.is_null() {
        Value::Null
    } else {
        let target = output.with_file_name("audio.conditioning.wav");
        run(
            ffmpeg()
                .arg("-i")
                .arg(path(&audio, "source_file")?)
                .args([
                    "-af",
                    &format!(
                        "apad,atrim=duration={}",
                        inference_frames as f64 / FPS as f64
                    ),
                    "-c:a",
                    "pcm_s16le",
                ])
                .arg(&target),
            cancelled,
        )?;
        json!(target)
    };
    Ok(
        json!({"profile": profile, "image": image, "anchors": anchors, "audio": audio, "native": native,
        "partial": partial, "inference_frames": inference_frames, "conditioning_audio": conditioning_audio}),
    )
}

pub(super) fn finish(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let prepared = &args["prepared"];
    let preview = prepared["profile"] == "distilled";
    let frames = integer(args, "frames")?;
    let width = integer(args, "width")?;
    let height = integer(args, "height")?;
    let duration = frames as f64 / FPS as f64;
    let native = path(prepared, "native")?;
    let partial = path(prepared, "partial")?;
    let output = path(args, "output")?;
    let audio = &prepared["audio"];
    let mut command = Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i"])
        .arg(&native);
    if !audio.is_null() {
        command.arg("-i").arg(path(audio, "source_file")?);
    }
    command
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            &frames.to_string(),
            "-map",
            if audio.is_null() { "0:a:0" } else { "1:a:0" },
            "-af",
            &format!("apad,atrim=duration={duration},asetpts=PTS-STARTPTS"),
            "-c:a",
            "aac",
            "-b:a",
            if preview { "96k" } else { "192k" },
            "-ar",
            "48000",
            "-ac",
            "2",
            "-t",
            &duration.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            if preview { "veryfast" } else { "fast" },
            "-crf",
            if preview { "28" } else { "18" },
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&partial);
    run(&mut command, cancelled)?;
    let mut info = validate(
        &json!({"path": partial, "frames": frames, "width": width, "height": height, "expect_audio": true}),
        cancelled,
    )?;
    if !audio.is_null() {
        info["supplied_audio"] = audio::validate(
            &json!({"video": partial, "source": audio["source_file"], "duration": duration}),
            cancelled,
        )?;
    }
    fs::rename(&partial, &output).map_err(|e| e.to_string())?;
    if args["keep_native"] != true {
        fs::remove_file(&native).map_err(|e| e.to_string())?;
    }
    Ok(info)
}
