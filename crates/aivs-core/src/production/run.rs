use super::*;
use crate::{character_conditioning, characters, dialogue, media, render_formats};
use std::{fs::File, io::Read, process::Command, time::Instant};
fn lock(root: &Path, name: &str) -> Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(name))
        .map_err(|e| e.to_string())?;
    file.try_lock().map_err(
        |_| "Another AIVS operation is using this workspace. Wait for it to finish and retry.",
    )?;
    Ok(file)
}
fn character_root(ctx: &Context<'_>) -> PathBuf {
    if ctx.root.join("characters").is_dir() {
        ctx.root.clone()
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }
}
fn seed(value: &Value) -> Result<i64> {
    if value.is_null() {
        let mut bytes = [0; 4];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| e.to_string())?;
        Ok(i64::from(u32::from_ne_bytes(bytes) & 0x7fffffff))
    } else {
        value.as_i64().ok_or("seed must be an integer.".into())
    }
}
fn new_run(root: &Path, seed: i64) -> Result<PathBuf> {
    let date = Command::new("date")
        .arg("+%Y-%m-%d_%H-%M-%S")
        .output()
        .map_err(|e| e.to_string())?;
    if !date.status.success() {
        return Err("Cannot determine production timestamp.".into());
    }
    let date = String::from_utf8_lossy(&date.stdout).trim().to_owned();
    let path = root.join("outputs").join(format!(
        "{date}-{:06}_{seed}",
        (artifacts::timestamp() / 1000) % 1_000_000
    ));
    fs::create_dir_all(path.parent().ok_or("Missing output root.")?).map_err(|e| e.to_string())?;
    fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}
fn budget(options: &Value, request: &Value) -> Result<u64> {
    let value = options
        .get("planning_budget_seconds")
        .filter(|v| !v.is_null())
        .or_else(|| request.get("planning_budget_seconds"))
        .cloned()
        .unwrap_or(json!(600));
    value
        .as_u64()
        .filter(|n| (60..=86400).contains(n))
        .ok_or("Planning budget must be an integer between 60 and 86400 seconds.".into())
}
fn environment(ctx: &mut Context<'_>) -> Result<()> {
    let env = fs::read_to_string(ctx.root.join(".env")).unwrap_or_default();
    let token = env
        .lines()
        .filter_map(|s| s.trim().strip_prefix("HF_API="))
        .last()
        .unwrap_or("")
        .trim()
        .trim_matches(['\'', '"']);
    if token.is_empty() {
        ctx.event(json!({"type":"needs_token"}))?;
        return Err(
            "Get an access token from Hugging Face, then create .env with HF_API=your_token."
                .into(),
        );
    }
    for program in ["ffmpeg", "ffprobe"] {
        Command::new(program)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| format!("{program} is missing. Install FFmpeg and retry setup."))?;
    }
    Ok(())
}
fn preflight(
    ctx: &mut Context<'_>,
    source: &Value,
    profiles: &Value,
    run: &Path,
    require_voice: bool,
) -> Result<Value> {
    character_conditioning::dispatch(
        "conditioning_preflight",
        &json!({"root":ctx.root,"source":source,"profiles":profiles,"run_dir":run,"require_voice":require_voice}),
        &ctx.cancelled,
        |e| {
            let event = if let Some(activity) = e.get("activity") {
                json!({"type":"activity","message":activity})
            } else {
                e
            };
            (ctx.emit)(event)
        },
    )
}
fn pending(plan: &Value) -> Result<Value> {
    let mut shots = rows(&plan["shots"])?.clone();
    for shot in &mut shots {
        shot["status"] = json!("pending");
    }
    Ok(json!(shots))
}
fn finish(
    ctx: &mut Context<'_>,
    run: &Path,
    mut report: Value,
    result: Result<Value>,
) -> Result<Value> {
    match result {
        Ok(runtime) => {
            let latest = read(&run.join("render.json"))?;
            report["shots"] = latest["shots"].clone();
            report["status"] = runtime
                .get("status")
                .cloned()
                .unwrap_or(json!("rendered-needs-review"));
            report["runtime"] = runtime;
            report["sha256"] = json!(artifacts::file_hash(&run.join("video.mp4"))?);
            remove(&mut report, &["error"]);
            save(&run.join("render.json"), &report)?;
            ctx.event(
                json!({"type":"complete","output":run.join("video.mp4"),"status":report["status"]}),
            )?;
            Ok(report)
        }
        Err(error) => {
            if let Ok(latest) = read(&run.join("render.json")) {
                report["shots"] = latest["shots"].clone();
            }
            report["status"] = json!(if ctx.check().is_err() {
                "interrupted"
            } else {
                "failed"
            });
            report["error"] = json!(error);
            save(&run.join("render.json"), &report)?;
            Err(error)
        }
    }
}
fn generated(ctx: &mut Context<'_>, args: &Value) -> Result<Value> {
    let mut request = args["request"].clone();
    if !request.is_object() || request["version"] != 1 {
        return Err("Screenplay requests must use version 1.".into());
    }
    if request
        .get("character_conditioning")
        .is_some_and(|v| *v != character_conditioning::POLICY)
    {
        return Err(
            "New screenplay renders require saved-character-assets-v1 conditioning.".into(),
        );
    }
    let plan_only = args["plan_only"] == true;
    let storyboards_only = args["storyboards_only"] == true;
    if plan_only && storyboards_only {
        return Err("Choose either --plan-only or --storyboards-only.".into());
    }
    let voice = args["voice_mode"]
        .as_str()
        .or(request["voice_mode"].as_str())
        .unwrap_or("saved")
        .to_owned();
    if !matches!(voice.as_str(), "saved" | "native") {
        return Err("Voice mode must be saved or native.".into());
    }
    let profile = args["profile"]
        .as_str()
        .or(request["render_profile"].as_str())
        .unwrap_or("hq")
        .to_owned();
    let config = model_config(&profile)?;
    let allowance = budget(args, &request)?;
    merge(
        &mut request,
        &json!({"character_conditioning":character_conditioning::POLICY,"storyboard_policy":STORYBOARD_POLICY,"voice_mode":voice,"render_profile":profile,"planning_budget_seconds":allowance}),
    )?;
    let cast = rows(&request["cast"])?
        .iter()
        .map(|r| {
            r["character"]
                .as_str()
                .map(|s| json!(s))
                .ok_or("Screenplay cast must list role and saved character bindings.".into())
        })
        .collect::<Result<Vec<_>>>()?;
    let root = character_root(ctx);
    let profiles = characters::dispatch(
        "character_resolve",
        &json!({"prompt":"","explicit":cast,"root":root,"references_only":true}),
    )?;
    let assigned_set = crate::sets::resolve(&ctx.root, &request["set"])?;
    if let Some((set, _)) = &assigned_set {
        request["set"] = set.clone();
    }
    ctx.source(&request, &profiles)?;
    if !plan_only {
        environment(ctx)?;
    }
    let seed = seed(&args["seed"])?;
    let _lock = lock(&ctx.root, ".render.lock")?;
    let run = new_run(&ctx.root, seed)?;
    if let Some((set, image)) = &assigned_set {
        crate::sets::snapshot(set, image, &run)?;
    }
    let profiles = characters::dispatch(
        "character_snapshot",
        &json!({"profiles":profiles,"run_dir":run,"root":root}),
    )?;
    fs::write(run.join("screenplay.txt"), text(&request, "script")?).map_err(|e| e.to_string())?;
    save(&run.join("screenplay-request.json"), &request)?;
    let spec = if request.get("render_format").is_some() {
        render_formats::for_profile(&request["render_format"], &profile)?
    } else {
        Value::Null
    };
    let mut report = json!({"status":"planning","original_script":request["script"],"cast":request["cast"],"seed":seed,"fps":24,"width":spec["width"].as_u64().unwrap_or(1280),"height":spec["height"].as_u64().unwrap_or(704),"audio":if voice=="native"{"native-generated"}else{"native-with-saved-voice-conversion"},"voice_mode":voice,"characters":profiles,"shooting_script":"shooting-script.json","character_conditioning":character_conditioning::POLICY,"render_profile":profile,"render_config":config,"storyboard_policy":STORYBOARD_POLICY,"storyboards":"storyboards.json","planning_budget_seconds":allowance});
    if !spec.is_null() {
        for (from, to) in [
            ("name", "render_format"),
            ("model_width", "model_width"),
            ("model_height", "model_height"),
        ] {
            report[to] = spec[from].clone();
        }
    }
    save(&run.join("render.json"), &report)?;
    ctx.options["planning_budget_seconds"] = json!(allowance);
    ctx.options["update_run_report"] = json!(true);
    let outcome = (|| -> Result<Value> {
        let started = Instant::now();
        let source = ctx.source(&request, &profiles)?;
        preflight(
            ctx,
            &source,
            &profiles,
            &run,
            !(plan_only || storyboards_only),
        )?;
        if !(plan_only || storyboards_only) {
            ctx.worker("model_check", json!({"profile":profile}))?;
        }
        if !plan_only {
            boards::image_call(ctx, "imagegen_check", json!({}))?;
        }
        ctx.event(json!({"type":"progress","label":"Preparing shots and storyboards with Codex"}))?;
        let prepared = preparation::prepare(ctx, &request, &profiles, &run, seed)?;
        let plan = &prepared["plan"];
        let boards = &prepared["boards"];
        report["seconds"] = plan["seconds"].clone();
        report["planning_seconds"] =
            json!((started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0);
        report["shots"] = pending(plan)?;
        if plan_only || storyboards_only {
            report["status"] = json!(if plan_only {
                "planned"
            } else {
                "storyboards-ready"
            });
            save(&run.join("render.json"), &report)?;
            let plan_file = run.join(if boards.is_null() {
                "storyboard-plan.json"
            } else {
                "shooting-script.json"
            });
            ctx.event(json!({"type":if plan_only{"planned"}else{"storyboards_ready"},"output":if plan_only{plan_file.clone()}else{run.join("storyboards.json")},"shooting_script":plan_file}))?;
            return Ok(Value::Null);
        }
        report["status"] = json!("rendering");
        save(&run.join("render.json"), &report)?;
        ctx.event(json!({"type":"progress","label":format!("Filming {} shots ({} seconds)",rows(&plan["shots"] )?.len(),plan["seconds"])}))?;
        rendering::render(
            ctx,
            &json!({"plan":plan,"seed":seed,"output":run.join("video.mp4"),"character_profiles":profiles,"storyboard":boards,"render_config":config,"profile":profile}),
        )
    })();
    if outcome.as_ref().is_ok_and(Value::is_null) {
        return Ok(report);
    }
    finish(ctx, &run, report, outcome)
}
fn resumed(ctx: &mut Context<'_>, args: &Value) -> Result<Value> {
    let run = path(args, "run_dir")?
        .canonicalize()
        .map_err(|_| "Select an existing AIVS output directory to resume.")?;
    if !run.is_dir() {
        return Err("Select an existing AIVS output directory to resume.".into());
    }
    let _lock = lock(&ctx.root, ".render.lock")?;
    let mut report = read(&run.join("render.json"))?;
    let mut request = read(&run.join("screenplay-request.json"))?;
    let profiles = report["characters"].clone();
    rows(&profiles)?;
    let seed = report["seed"]
        .as_i64()
        .ok_or("Cannot resume this directory: the saved cast or seed is missing.")?;
    let profile = report["render_profile"]
        .as_str()
        .unwrap_or("distilled")
        .to_owned();
    let spec = if request.get("render_format").is_some() {
        render_formats::for_profile(&request["render_format"], &profile)?
    } else {
        Value::Null
    };
    if report["fps"] != 24
        || report["width"].as_u64() != Some(spec["width"].as_u64().unwrap_or(1280))
        || report["height"].as_u64() != Some(spec["height"].as_u64().unwrap_or(704))
        || report["render_format"] != request["render_format"]
    {
        return Err("Cannot resume a run with different video dimensions or frame rate.".into());
    }
    let source = ctx.source(&request, &profiles)?;
    crate::sets::verify(&source["set"], &run)?;
    if report["character_conditioning"] != request["character_conditioning"] {
        return Err("Cannot resume a run with a different character conditioning policy.".into());
    }
    if report["voice_mode"].as_str().unwrap_or("saved")
        != request["voice_mode"].as_str().unwrap_or("saved")
    {
        return Err("Cannot change the saved voice mode on Resume; start a new render.".into());
    }
    let config = model_config(&profile)?;
    if request["render_profile"].as_str().unwrap_or("distilled") != profile {
        return Err("Cannot resume a run with a different render profile.".into());
    }
    if report.get("render_config").is_some_and(|v| *v != config) {
        return Err("This run uses a different renderer configuration; start a new render.".into());
    }
    let allowance = budget(args, &request)?;
    ctx.options["planning_budget_seconds"] = json!(allowance);
    let uses = request["storyboard_policy"] == STORYBOARD_POLICY;
    let stop = args["storyboards_only"] == true;
    if stop && !uses {
        return Err("Storyboard-only Resume requires a run with per-shot storyboards.".into());
    }
    let saved_motion = report["motion_previsualization"].clone();
    let mut motion = Value::Null;
    let mut motion_hash = None;
    if args["motion_previs"] == true || !saved_motion.is_null() {
        motion = read(&run.join("motion/keyframes.json"))?;
        motion::accepted(
            ctx,
            &read(&run.join("shooting-script.json"))?,
            &read(&run.join("storyboards.json"))?,
            &motion,
            &run,
        )?;
        let hash = ctx.digest(&motion)?;
        if !saved_motion.is_null() && saved_motion != hash {
            return Err("Saved motion rendering belongs to different keyframes; preserve this run before changing them.".into());
        }
        motion_hash = Some(hash);
    }
    let output = run.join("video.mp4");
    if !stop
        && (motion.is_null() || !saved_motion.is_null())
        && report["status"] == "complete"
        && report["runtime"]["status"] == "complete"
        && output.is_file()
        && report["sha256"] == artifacts::file_hash(&output)?
    {
        ctx.event(json!({"type":"complete","output":output,"status":report["status"]}))?;
        return Ok(report);
    }
    environment(ctx)?;
    if uses && report["character_conditioning"] == character_conditioning::POLICY {
        preflight(ctx, &source, &profiles, &run, !stop)?;
    }
    let has_footage = output.is_file()
        || if run.join("shots").is_dir() {
            fs::read_dir(run.join("shots"))
                .map_err(|e| e.to_string())?
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.path().join("video.mp4").is_file()
                        || e.path().join("video.source.mp4").is_file()
                })
        } else {
            false
        };
    if stop && has_footage {
        preparation::prepare(ctx, &request, &profiles, &run, seed)?;
        ctx.event(json!({"type":"storyboards_ready","output":run.join("storyboards.json"),"shooting_script":run.join("shooting-script.json")}))?;
        return Ok(report);
    }
    let old = if !uses && run.join("shooting-script.json").is_file() {
        Some(ctx.load_plan(&run.join("shooting-script.json"), &request, &profiles)?)
    } else {
        None
    };
    remove(&mut report, &["error", "sha256", "runtime"]);
    report["status"] = json!(if old.is_none() {
        "planning"
    } else {
        "rendering"
    });
    report["planning_budget_seconds"] = json!(allowance);
    if let Some(hash) = motion_hash {
        report["motion_previsualization"] = json!(hash);
    }
    if !args["planning_budget_seconds"].is_null() {
        request["planning_budget_seconds"] = json!(allowance);
        save(&run.join("screenplay-request.json"), &request)?;
    }
    save(&run.join("render.json"), &report)?;
    ctx.options["update_run_report"] = json!(true);
    let outcome = (|| -> Result<Value> {
        let (plan, boards) = if uses {
            if !stop {
                ctx.worker("model_check", json!({"profile":profile}))?;
            }
            boards::image_call(ctx, "imagegen_check", json!({}))?;
            ctx.activity("Resuming shots and storyboards")?;
            let started = Instant::now();
            let prepared = preparation::prepare(ctx, &request, &profiles, &run, seed)?;
            report["planning_seconds"] =
                json!((started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0);
            (prepared["plan"].clone(), prepared["boards"].clone())
        } else {
            (old.ok_or("The old planner has been removed. Start Generate video again to create storyboards for this scene.")?,Value::Null)
        };
        report["seconds"] = plan["seconds"].clone();
        report["shots"] = pending(&plan)?;
        report["status"] = json!(if stop {
            "storyboards-ready"
        } else {
            "rendering"
        });
        save(&run.join("render.json"), &report)?;
        if stop {
            ctx.event(json!({"type":"storyboards_ready","output":run.join("storyboards.json"),"shooting_script":run.join("shooting-script.json")}))?;
            return Ok(Value::Null);
        }
        ctx.event(json!({"type":"progress","label":format!("Resuming {} shots; reusing saved takes",rows(&plan["shots"] )?.len())}))?;
        rendering::render(
            ctx,
            &json!({"plan":plan,"seed":seed,"output":output,"resume":true,"character_profiles":if report["character_conditioning"]==character_conditioning::POLICY{&profiles}else{&Value::Null},"storyboard":boards,"motion_manifest":motion,"render_config":if uses{&config}else{&Value::Null},"profile":profile}),
        )
    })();
    if outcome.as_ref().is_ok_and(Value::is_null) {
        return Ok(report);
    }
    finish(ctx, &run, report, outcome)
}
fn prompt(ctx: &mut Context<'_>, args: &Value) -> Result<Value> {
    environment(ctx)?;
    let prompt = args["prompt"]
        .as_str()
        .unwrap_or(include_str!("../../../../python/default-prompt.txt"))
        .trim();
    let root = character_root(ctx);
    let profiles = characters::dispatch(
        "character_resolve",
        &json!({"prompt":prompt,"explicit":args.get("character").unwrap_or(&json!([])),"root":root,"references_only":args["character_references_only"]==true}),
    )?;
    if args["character_text_only"] != true
        && rows(&profiles)?
            .iter()
            .filter(|p| p["scene_image"].is_object() || p["image"].is_object())
            .count()
            > 1
    {
        return Err("Multiple saved characters have reference images. Use --character-text-only to use descriptions without image guidance.".into());
    }
    let timing = dialogue::estimate(prompt)?;
    let seed = seed(&args["seed"])?;
    let _lock = lock(&ctx.root, ".render.lock")?;
    let run = new_run(&ctx.root, seed)?;
    let profiles = characters::dispatch(
        "character_snapshot",
        &json!({"profiles":profiles,"run_dir":run,"root":root}),
    )?;
    let image = if args["character_text_only"] == true || rows(&profiles)?.is_empty() {
        Value::Null
    } else {
        characters::dispatch(
            "character_reference_image",
            &json!({"profiles":profiles,"run_dir":run}),
        )?
    };
    let hold = !image.is_null()
        && rows(&profiles)?
            .iter()
            .any(|p| p["scene_image"].is_object());
    let expanded = characters::dispatch(
        "character_expand_prompt",
        &json!({"prompt":prompt,"profiles":profiles,"image_conditioned":!image.is_null()}),
    )?;
    let mut report = json!({"status":"rendering","original_prompt":prompt,"prompt":expanded,"seed":seed,"seconds":timing["seconds"],"fps":24,"width":1280,"height":704,"audio":"generated","dialogue_timing":timing,"characters":profiles,"character_text_only":args["character_text_only"]==true,"character_references_only":args["character_references_only"]==true});
    if let Some(file) = image.as_str() {
        let file = Path::new(file);
        report["image_source"] = json!({"file":file.strip_prefix(&run).map_err(|e|e.to_string())?,"conditioning":if hold{"whole-first-and-last-frame"}else{"whole-first-frame"}});
        if hold {
            let frames = timing["seconds"].as_u64().ok_or("Invalid duration.")? * 24;
            report["image_source"]["anchor_frames"] = if frames <= 1 {
                json!([0])
            } else {
                json!([0, frames - 1])
            };
        }
    }
    save(&run.join("render.json"), &report)?;
    let runtime=ctx.worker("ltx_render",json!({"prompt":expanded,"seed":seed,"frames":timing["seconds"].as_u64().ok_or("Invalid duration.")?*24,"output":run.join("video.mp4"),"image_file":image,"hold_image_framing":hold}));
    match runtime {
        Ok(runtime) => {
            report["runtime"] = runtime;
            report["status"] = json!("complete");
            report["sha256"] = json!(artifacts::file_hash(&run.join("video.mp4"))?);
            save(&run.join("render.json"), &report)?;
            ctx.event(json!({"type":"complete","output":run.join("video.mp4")}))?;
            Ok(report)
        }
        Err(error) => {
            report["status"] = json!(if ctx.check().is_err() {
                "interrupted"
            } else {
                "failed"
            });
            report["error"] = json!(error);
            save(&run.join("render.json"), &report)?;
            Err(error)
        }
    }
}
pub fn dispatch(ctx: &mut Context<'_>, operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "production_run" => generated(ctx, args),
        "production_resume" => resumed(ctx, args),
        "production_prompt" => prompt(ctx, args),
        _ => Err("Unknown production lifecycle operation.".into()),
    }
}
pub fn setup(ctx: &mut Context<'_>, download: bool, profile: &str) -> Result<Value> {
    environment(ctx)?;
    if download {
        let _lock = lock(&ctx.root, ".setup.lock")?;
        boards::image_call(ctx, "imagegen_check_local", json!({}))?;
        ctx.activity("Downloading pinned video models; cached files are reused")?;
        ctx.worker("model_download", json!({"profile":profile}))?;
    }
    ctx.event(json!({"type":"progress","label":"Checking Python packages and Apple GPU"}))?;
    ctx.worker("model_doctor", json!({}))?;
    ctx.activity("Checking cached video, text encoder, and audio weights")?;
    ctx.worker("model_check", json!({"profile":profile}))?;
    boards::image_call(ctx, "imagegen_check_local", json!({}))?;
    character_conditioning::dispatch(
        "conditioning_check_voice",
        &json!({"root":ctx.root}),
        &ctx.cancelled,
        &mut ctx.emit,
    )?;
    ctx.event(json!({"type":"ready"}))?;
    Ok(Value::Null)
}
