//! Local FFmpeg execution, decoded-media validation and shot assembly.
//! The caller publishes the assembled partial only after validation succeeds.
mod animatic;
mod audio;
mod inference;
mod review;
mod source;
use crate::continuity::Result;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const FPS: u64 = 24;

pub(crate) use source::image as source_image;

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

pub(crate) fn run(command: &mut Command, cancelled: &AtomicBool) -> Result<Vec<u8>> {
    run_timeout(command, cancelled, None)
}

pub(crate) fn run_timeout(
    command: &mut Command,
    cancelled: &AtomicBool,
    timeout: Option<Duration>,
) -> Result<Vec<u8>> {
    let started = Instant::now();
    let program = command.get_program().to_string_lossy().into_owned();
    let mut process = Process(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Cannot launch {program}: {error}"))?,
    );
    let stdout = drain(process.0.stdout.take().ok_or("Missing media stdout.")?);
    let stderr = drain(process.0.stderr.take().ok_or("Missing media stderr.")?);
    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(format!("{program} cancelled because its parent closed."));
        }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Err(format!("{program} timed out."));
        }
        if let Some(status) = process.0.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        thread::sleep(Duration::from_millis(25));
    };
    let output = stdout
        .join()
        .map_err(|_| "Media stdout reader failed.")?
        .map_err(|error| error.to_string())?;
    let errors = stderr
        .join()
        .map_err(|_| "Media stderr reader failed.")?
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!(
            "{program} failed ({status}): {}",
            String::from_utf8_lossy(&errors).trim()
        ));
    }
    Ok(output)
}

fn path(args: &Value, name: &str) -> Result<PathBuf> {
    args[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("Missing media path: {name}."))
}

fn integer(args: &Value, name: &str) -> Result<u64> {
    args[name]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("Invalid media {name}."))
}

fn remove_existing(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Cannot remove {}: {error}", path.display())),
    }
}

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    command
}

fn extract(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let clip = path(args, "clip")?;
    let image = path(args, "image")?;
    let frame = args["frame_index"]
        .as_u64()
        .ok_or("Invalid source frame index.")?;
    fs::create_dir_all(image.parent().ok_or("Missing frame output directory.")?)
        .map_err(|e| e.to_string())?;
    remove_existing(&image)?;
    run(
        ffmpeg()
            .arg("-i")
            .arg(&clip)
            .args([
                "-vf",
                &format!("select=eq(n\\,{frame})"),
                "-frames:v",
                "1",
                "-fps_mode",
                "passthrough",
            ])
            .arg(&image),
        cancelled,
    )?;
    if !fs::metadata(&image).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0) {
        return Err(format!(
            "FFmpeg did not extract the last frame from {}.",
            clip.display()
        ));
    }
    Ok(Value::Null)
}

fn assemble(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let output = path(args, "output")?;
    if !output.is_absolute() {
        return Err("Assembly output path must be absolute.".into());
    }
    let directory = output.parent().ok_or("Missing assembly directory.")?;
    let count = integer(args, "clip_count")?;
    let seconds = integer(args, "seconds")?;
    let width = integer(args, "width")?;
    let height = integer(args, "height")?;
    let frames = seconds
        .checked_mul(FPS)
        .ok_or("Assembled frame count overflow.")?;
    let concat = directory.join("shots.concat.txt");
    let entries = (1..=count)
        .map(|index| format!("file 'shots/{index:03}/video.mp4'\n"))
        .collect::<String>();
    fs::write(&concat, entries).map_err(|error| error.to_string())?;
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("Invalid assembly filename.")?;
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("mp4");
    let partial = output.with_file_name(format!("{stem}.partial.{extension}"));
    remove_existing(&partial)?;
    let mut video = vec![
        if args["crop"] == true {
            format!("crop={width}:{height}")
        } else {
            format!("scale={width}:{height}:flags=lanczos")
        },
        "setsar=1".into(),
        format!("fps={FPS}"),
        "format=yuv420p".into(),
        "setpts=PTS-STARTPTS".into(),
    ];
    let mut audio = vec![
        "aresample=48000".to_owned(),
        "apad".into(),
        format!("atrim=duration={seconds}"),
        "asetpts=PTS-STARTPTS".into(),
    ];
    if args["fade_out"] == true {
        let start = (seconds as f64 - 0.5).max(0.0);
        video.push(format!("fade=t=out:st={start}:d=0.5"));
        audio.push(format!("afade=t=out:st={start}:d=0.5"));
    }
    run(
        ffmpeg()
            .current_dir(directory)
            .args(["-f", "concat", "-safe", "1", "-i"])
            .arg(&concat)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-vf",
                &video.join(","),
                "-af",
                &audio.join(","),
                "-frames:v",
                &frames.to_string(),
                "-t",
                &seconds.to_string(),
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-movflags",
                "+faststart",
            ])
            .arg(&partial),
        cancelled,
    )?;
    Ok(json!({"partial": partial, "frames": frames}))
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
        .filter(|n| n.is_finite())
}

fn probe_integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn validate(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let file = path(args, "path")?;
    let frames = integer(args, "frames")?;
    let width = integer(args, "width")?;
    let height = integer(args, "height")?;
    let bytes = run(Command::new("ffprobe").args(["-v", "error", "-count_frames", "-show_entries",
        "stream=codec_type,width,height,sample_aspect_ratio,display_aspect_ratio,nb_read_frames,r_frame_rate,duration,codec_name,pix_fmt,start_time,sample_rate,channels",
        "-of", "json"]).arg(&file), cancelled)?;
    let probe: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid FFprobe response: {error}"))?;
    let streams = probe["streams"]
        .as_array()
        .ok_or("Video validation failed: missing streams.")?;
    let mut info = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .cloned()
        .ok_or("Video validation failed: no video stream.")?;
    if probe_integer(&info["nb_read_frames"]) != Some(frames)
        || info["width"].as_u64() != Some(width)
        || info["height"].as_u64() != Some(height)
    {
        return Err(format!("Video validation failed: {info}"));
    }
    let duration = frames as f64 / FPS as f64;
    if info["r_frame_rate"] != format!("{FPS}/1")
        || !number(&info["duration"]).is_some_and(|n| (n - duration).abs() <= 0.001)
    {
        return Err(format!("Unexpected video timing: {info}"));
    }
    if args["expect_audio"] == true {
        let audios = streams
            .iter()
            .filter(|stream| stream["codec_type"] == "audio")
            .collect::<Vec<_>>();
        if audios.len() != 1 {
            return Err("Audio validation failed: expected one audio stream.".into());
        }
        let sound = audios[0];
        let tolerance = 1.0 / FPS as f64 + 0.01;
        let valid = probe_integer(&sound["sample_rate"]).is_some_and(|n| n > 0)
            && probe_integer(&sound["channels"]).is_some_and(|n| n > 0)
            && number(&sound["duration"]).is_some_and(|n| (n - duration).abs() <= tolerance)
            && number(sound.get("start_time").unwrap_or(&json!(0)))
                .zip(number(info.get("start_time").unwrap_or(&json!(0))))
                .is_some_and(|(audio, video)| (audio - video).abs() <= tolerance);
        if !valid {
            return Err(format!("Unexpected audio timing or format: {sound}"));
        }
        info["audio"] = sound.clone();
    }
    run(
        Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-xerror",
                "-nostdin",
                "-i",
            ])
            .arg(&file)
            .args(["-map", "0:v:0", "-map", "0:a?", "-f", "null", "-"]),
        cancelled,
    )?;
    Ok(info)
}

pub fn dispatch(operation: &str, args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    match operation {
        "prepare_ltx_render" => inference::prepare(args, cancelled),
        "finish_ltx_render" => inference::finish(args, cancelled),
        "assemble_animatic" => animatic::assemble(args, cancelled),
        "prepare_source_image" => source::image(&path(args, "source")?),
        "prepare_source_audio" => source::audio(args),
        "prepare_image_anchors" => source::anchors(args),
        "extract_last_frame" => extract(args, cancelled),
        "assemble_shots" => assemble(args, cancelled),
        "validate_video" => validate(args, cancelled),
        "validate_supplied_audio" => audio::validate(args, cancelled),
        "visual_review_sheets" => review::sheets(args, cancelled),
        _ => Err("Unknown media operation.".into()),
    }
}
