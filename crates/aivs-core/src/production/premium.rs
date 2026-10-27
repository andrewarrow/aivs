//! Optional paid comparison jobs. Submission is checkpointed and never retried.
use super::*;
use crate::media;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::Write,
    process::Command,
    time::{Duration, Instant},
};
const API: &str = "https://api.ltx.io/v2/image-to-video";
pub fn configuration(model: &str) -> Result<Value> {
    if model != "ltx-2-5-pro" {
        return Err("This comparison adapter supports the documented model ltx-2-5-pro.".into());
    }
    Ok(
        json!({"backend":"ltx-cloud","model":model,"api_version":"v2","endpoint":API,"fps":24,"generate_audio":true,"seed_supported":false,"model_revision":null}),
    )
}
pub fn validate(args: &Value) -> Result<Value> {
    configuration(args["model"].as_str().unwrap_or("ltx-2-5-pro"))?;
    let prompt = text(args, "prompt")?;
    if prompt.trim().is_empty() || prompt.chars().count() > 5000 {
        return Err("LTX Pro needs a nonempty prompt of at most 5000 characters.".into());
    }
    if !matches!(args["frames"].as_u64(), Some(144 | 192 | 240)) {
        return Err("LTX-2.5 Pro comparison shots must be exactly 6, 8, or 10 seconds.".into());
    }
    let width = args["width"]
        .as_u64()
        .ok_or("Cloud output dimensions must be integers.")?;
    let height = args["height"]
        .as_u64()
        .ok_or("Cloud output dimensions must be integers.")?;
    let (w, h) = match (width, height) {
        (1280, 768) => (1280, 720),
        (1088, 1920) => (1080, 1920),
        other => other,
    };
    if !matches!(
        (w, h),
        (1280, 720) | (1920, 1080) | (720, 1280) | (1080, 1920)
    ) {
        return Err(
            "LTX Pro comparison supports 720p/1080p landscape or portrait delivery.".into(),
        );
    }
    Ok(json!({"width":w,"height":h,"pad_left":(width-w)/2,"pad_top":(height-h)/2}))
}
fn pause(seconds: u64, flag: &AtomicBool) -> Result<()> {
    for _ in 0..seconds * 10 {
        if flag.load(Ordering::Relaxed) {
            return Err("Cloud rendering interrupted; its job remains saved.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
fn api(url: &str, key: &str, payload: Option<&Value>, flag: &AtomicBool) -> Result<Value> {
    let mut config = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    // curl config files keep credentials out of process arguments and logs.
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    if escaped.contains(['\r', '\n']) {
        return Err("Invalid LTX API credential.".into());
    }
    writeln!(
        config,
        "header = \"Authorization: Bearer {escaped}\"\nheader = \"Content-Type: application/json\""
    )
    .map_err(|e| e.to_string())?;
    let mut data = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    if let Some(payload) = payload {
        serde_json::to_writer(&mut data, payload).map_err(|e| e.to_string())?;
        data.flush().map_err(|e| e.to_string())?;
    }
    let response = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let attempts = if payload.is_some() { 1 } else { 4 };
    for attempt in 0..attempts {
        let mut command = Command::new("curl");
        command
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "60",
                "--proto",
                "=https",
                "--config",
            ])
            .arg(config.path())
            .arg("--output")
            .arg(response.path())
            .args(["--write-out", "%{http_code}"]);
        if payload.is_some() {
            command
                .args(["--request", "POST", "--data-binary"])
                .arg(format!("@{}", data.path().display()));
        }
        command.arg(url);
        let result = media::run(&mut command, flag)
            .map_err(|_| "LTX API request could not complete; resume the saved job.".to_owned());
        match result {
            Ok(bytes) => {
                let code = String::from_utf8_lossy(&bytes)
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "LTX returned an invalid HTTP response.")?;
                if (200..300).contains(&code) {
                    return read(response.path())
                        .map_err(|_| "LTX returned an invalid JSON object.".into());
                }
                if attempt + 1 < attempts && (code >= 500 || code == 429) {
                    pause(5 * (1 << attempt), flag)?;
                    continue;
                }
                return Err(format!("LTX API returned HTTP {code}."));
            }
            Err(error) => {
                if attempt + 1 == attempts {
                    return Err(error);
                }
                pause(5 * (1 << attempt), flag)?;
            }
        }
    }
    Err("LTX API request could not complete; resume the saved job.".into())
}
fn download(url: &str, destination: &Path, flag: &AtomicBool) -> Result<()> {
    if !url.starts_with("https://") {
        return Err("LTX result did not contain an HTTPS download URL.".into());
    }
    let partial = destination.with_extension("download.partial");
    for attempt in 0..4 {
        let result = media::run(
            Command::new("curl")
                .args([
                    "--silent",
                    "--show-error",
                    "--fail",
                    "--location",
                    "--max-time",
                    "60",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--output",
                ])
                .arg(&partial)
                .arg(url),
            flag,
        );
        if result.is_ok() {
            if fs::metadata(&partial).map_err(|e| e.to_string())?.len() == 0 {
                return Err("LTX downloaded an empty video.".into());
            }
            fs::rename(&partial, destination).map_err(|e| e.to_string())?;
            return Ok(());
        }
        if attempt < 3 {
            pause(5 * (1 << attempt), flag)?;
        }
    }
    Err("LTX video download failed; the saved job can be resumed.".into())
}
fn encoded_job(id: &str) -> String {
    id.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
pub fn render(
    _root: &Path,
    args: &Value,
    flag: &AtomicBool,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    if !args["audio_file"].is_null() {
        return Err(
            "Premium comparison generates native audio; supplied audio is unsupported.".into(),
        );
    }
    if args["hold_image_framing"] == true {
        return Err(
            "Premium comparison uses a first-frame storyboard, not a repeated last frame.".into(),
        );
    }
    let dimensions = validate(args)?;
    let image_path = path(args, "image_file")
        .map_err(|_| "Premium comparison requires a storyboard starting image.")?;
    let image = media::dispatch("prepare_source_image", &json!({"source":image_path}), flag)?;
    let bytes = fs::read(path(&image, "source_file")?).map_err(|e| e.to_string())?;
    let mime = match image::guess_format(&bytes).map_err(|e| e.to_string())? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("LTX storyboard must be PNG, JPEG, or WEBP.".into()),
    };
    if artifacts::hash(&bytes) != text(&image, "source_sha256")? {
        return Err("Storyboard image changed while preparing the cloud request.".into());
    }
    let encoded = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
    if encoded.len() > 7_000_000 {
        return Err("LTX storyboard exceeds the 7 MB encoded data URI limit.".into());
    }
    let key = std::env::var("LTX_API_KEY").unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        return Err("Set LTX_API_KEY before running the premium comparison.".into());
    }
    let output = path(args, "output")?;
    let folder = output.parent().ok_or("Missing cloud output directory.")?;
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let checkpoint = folder.join("cloud-job.json");
    let native = folder.join("video.cloud.mp4");
    let config = configuration(args["model"].as_str().unwrap_or("ltx-2-5-pro"))?;
    let frames = args["frames"].as_u64().ok_or("Invalid cloud duration.")?;
    let seconds = frames / 24;
    let width = args["width"].as_u64().ok_or("Invalid cloud width.")?;
    let height = args["height"].as_u64().ok_or("Invalid cloud height.")?;
    let request = json!({"prompt":args["prompt"],"model":config["model"],"duration":seconds,"resolution":format!("{}x{}",dimensions["width"],dimensions["height"]),"fps":24,"generate_audio":true});
    let identity = json!({"config":config,"request":request,"source_image_sha256":image["source_sha256"],"requested_seed":args["seed"],"output_width":width,"output_height":height});
    let hash = artifacts::fingerprint(&identity)?;
    let mut state = if checkpoint.exists() {
        let saved = read(&checkpoint)?;
        if saved["fingerprint"] != hash {
            return Err("Cloud candidate inputs changed; use a new candidate directory.".into());
        }
        if saved["job_id"].as_str().is_none_or(str::is_empty) {
            return Err("Cloud submission has no recorded job ID and may have been charged. Check the LTX console before starting a new candidate; automatic resubmission is disabled.".into());
        }
        if saved["status"] == "failed" {
            return Err(
                "The saved LTX job failed; use a new candidate to request another generation."
                    .into(),
            );
        }
        saved
    } else {
        let mut state = json!({"version":1,"fingerprint":hash,"inputs":identity,"status":"submitting","job_id":null});
        save(&checkpoint, &state)?;
        let mut payload = request;
        payload["image_uri"] = json!(encoded);
        let job = api(API, key, Some(&payload), flag)?;
        if job["id"].as_str().is_none_or(str::is_empty) {
            return Err(
                "LTX submission did not return a job ID; check the provider console.".into(),
            );
        }
        state["status"] = json!("submitted");
        state["job_id"] = job["id"].clone();
        state["created_at"] = job["created_at"].clone();
        save(&checkpoint, &state)?;
        state
    };
    let started = Instant::now();
    let native_hash = if native.is_file() {
        Some(artifacts::file_hash(&native)?)
    } else {
        None
    };
    if let Some(hash) = &native_hash {
        if !state["download_sha256"].is_null() && state["download_sha256"] != *hash {
            return Err("Saved cloud source video changed; use a new candidate directory.".into());
        }
    }
    let timeout = args["timeout_seconds"].as_f64().unwrap_or(3600.0);
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err("Cloud timeout must be positive.".into());
    }
    if native_hash
        .as_ref()
        .is_none_or(|h| state["download_sha256"] != *h)
    {
        let url = format!("{API}/{}", encoded_job(text(&state, "job_id")?));
        loop {
            if started.elapsed().as_secs_f64() > timeout {
                return Err(
                    "LTX job is still saved; resume this candidate to continue polling.".into(),
                );
            }
            let result = api(&url, key, None, flag)?;
            let status = text(&result, "status")?;
            if !matches!(status, "pending" | "processing" | "completed" | "failed") {
                return Err(
                    "LTX returned an unrecognized job status; the job remains saved.".into(),
                );
            }
            state["status"] = json!(status);
            save(&checkpoint, &state)?;
            if status == "failed" {
                return Err("LTX could not generate this candidate; inspect the job in the provider console.".into());
            }
            if status == "completed" {
                download(text(&result["result"], "video_url")?, &native, flag)?;
                state["download_sha256"] = json!(artifacts::file_hash(&native)?);
                save(&checkpoint, &state)?;
                break;
            }
            emit(
                json!({"type":"render_progress","data":{"label":format!("LTX Pro: {status}"),"fraction":0.25,"remaining_seconds":60}}),
            )?;
            pause(5, flag)?;
        }
    }
    let native_info = media::dispatch(
        "validate_video",
        &json!({"path":native,"frames":frames,"width":dimensions["width"],"height":dimensions["height"],"expect_audio":true}),
        flag,
    )?;
    let partial = output.with_file_name("video.partial.mp4");
    let filter = format!(
        "pad={width}:{height}:{}:{}:black,setsar=1,setpts=PTS-STARTPTS",
        dimensions["pad_left"], dimensions["pad_top"]
    );
    media::run(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
            .arg(&native)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-vf",
                &filter,
                "-frames:v",
                &frames.to_string(),
                "-af",
                &format!("apad,atrim=duration={seconds},asetpts=PTS-STARTPTS"),
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
        flag,
    )?;
    let info = media::dispatch(
        "validate_video",
        &json!({"path":partial,"frames":frames,"width":width,"height":height,"expect_audio":true}),
        flag,
    )?;
    fs::rename(&partial, &output).map_err(|e| e.to_string())?;
    state["status"] = json!("complete");
    state["output_sha256"] = json!(artifacts::file_hash(&output)?);
    save(&checkpoint, &state)?;
    let mut result = config;
    merge(
        &mut result,
        &json!({"pipeline":"cloud-image-to-video","audio":"generated","requested_seed":args["seed"],"applied_seed":null,"job_id":state["job_id"],"output_frames":frames,"source_image":image,"source_audio":null,"provider_dimensions":dimensions,"native_video":native_info,"native_file":"video.cloud.mp4","native_sha256":state["download_sha256"],"elapsed_seconds":(started.elapsed().as_secs_f64()*100.0).round()/100.0,"video":info}),
    )?;
    Ok(result)
}
