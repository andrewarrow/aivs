//! Existing command-line contract, executed by the Rust production engine.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn stop(_signal: i32) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
fn cancellation() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
        }
        unsafe {
            signal(2, stop);
            signal(15, stop);
        }
    }
    let copy = flag.clone();
    std::thread::spawn(move || {
        loop {
            if INTERRUPTED.load(Ordering::Relaxed) {
                copy.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    });
    flag
}
fn usage() -> &'static str {
    "AIVS production engine\nUsage: aivs-engine --production <check|download|render|resume|previsualize> [options]\n\nScreenplay: --screenplay-file PATH | --screenplay-json JSON | --script-file PATH --cast ROLE=CHARACTER\nRender: --seed N --profile hq|distilled --render-format landscape|portrait --voice-mode saved|native\nPlanning: --planning-budget-seconds N --fresh-preproduction --plan-only --storyboards-only --shooting-script PATH\nResume: --output-dir PATH [--storyboards-only | --motion-previs]\nPrompt: --prompt TEXT --character NAME --character-text-only --character-references-only"
}
fn parse(arguments: Vec<String>) -> Result<(String, Value)> {
    let mut args = arguments.into_iter();
    let command = args.next().ok_or_else(|| usage().to_owned())?;
    if matches!(command.as_str(), "-h" | "--help") {
        return Ok(("help".into(), Value::Null));
    }
    let switches = BTreeSet::from([
        "motion-previs",
        "fresh-preproduction",
        "character-text-only",
        "character-references-only",
        "plan-only",
        "storyboards-only",
    ]);
    let values = BTreeSet::from([
        "output-dir",
        "seed",
        "profile",
        "planning-budget-seconds",
        "prompt",
        "character",
        "screenplay-json",
        "screenplay-file",
        "script-file",
        "cast",
        "voice-mode",
        "shooting-script",
        "render-format",
    ]);
    let mut flags = BTreeMap::<String, Vec<String>>::new();
    while let Some(raw) = args.next() {
        if matches!(raw.as_str(), "-h" | "--help") {
            return Ok(("help".into(), Value::Null));
        }
        let (key, inline) = raw
            .strip_prefix("--")
            .ok_or_else(|| format!("Unexpected argument: {raw}"))?
            .split_once('=')
            .map_or((raw.trim_start_matches("--"), None), |(k, v)| (k, Some(v)));
        if switches.contains(key) {
            if inline.is_some() {
                return Err(format!("--{key} does not accept a value."));
            }
            flags.entry(key.into()).or_default();
        } else if values.contains(key) {
            let value = inline
                .map(str::to_owned)
                .or_else(|| args.next())
                .ok_or_else(|| format!("--{key} needs a value."))?;
            flags.entry(key.into()).or_default().push(value);
        } else {
            return Err(format!("Unknown option: --{key}"));
        }
    }
    let get = |key: &str| flags.get(key).and_then(|v| v.last()).cloned();
    let has = |key: &str| flags.contains_key(key);
    let mut payload = json!({});
    for key in switches {
        payload[key.replace('-', "_")] = json!(has(key));
    }
    for key in ["seed", "planning-budget-seconds"] {
        if let Some(value) = get(key) {
            payload[key.replace('-', "_")] = json!(
                value
                    .parse::<i64>()
                    .map_err(|_| format!("--{key} requires an integer."))?
            );
        }
    }
    for key in ["profile", "voice-mode", "render-format", "prompt"] {
        if let Some(value) = get(key) {
            payload[key.replace('-', "_")] = json!(value);
        }
    }
    if let Some(file) = get("shooting-script") {
        payload["shooting_script_path"] = json!(expand(&file)?);
    }
    payload["fresh"] = json!(has("fresh-preproduction"));
    payload["character"] = json!(flags.get("character").cloned().unwrap_or_default());
    if has("motion-previs") && (command != "resume" || has("storyboards-only")) {
        return Err("--motion-previs requires resume without --storyboards-only.".into());
    }
    if has("plan-only") && has("storyboards-only") {
        return Err("Choose either --plan-only or --storyboards-only.".into());
    }
    match command.as_str() {
        "check" | "download" => {
            if has("plan-only") || has("storyboards-only") || has("voice-mode") {
                return Err(
                    "Planning stop points and voice mode apply only to screenplay rendering."
                        .into(),
                );
            }
            if has("output-dir") {
                return Err("--output-dir is only used with resume.".into());
            }
        }
        "resume" => {
            if has("profile") || has("fresh-preproduction") {
                return Err("Resume uses saved shots and render profile; start a new render to change them.".into());
            }
            if has("render-format") {
                return Err(
                    "Resume uses the run's saved render format; omit --render-format.".into(),
                );
            }
            if has("voice-mode") || has("plan-only") {
                return Err("Resume keeps its saved voice mode; use --storyboards-only to stop before video.".into());
            }
            payload["run_dir"] = json!(expand(&get("output-dir").ok_or(
                "Resume requires --output-dir pointing to an existing AIVS output directory."
            )?)?);
        }
        "previsualize" => {
            payload["run_dir"]=json!(expand(&get("output-dir").ok_or("Previsualization requires --output-dir with completed shooting-script.json and storyboards.")?)?);
            payload["command"] = json!("prepare");
        }
        "render" => {
            if has("output-dir") {
                return Err("--output-dir is only used with resume.".into());
            }
            let sources = ["screenplay-json", "screenplay-file", "script-file"]
                .iter()
                .filter(|k| has(k))
                .count();
            if sources > 1 {
                return Err("Choose one screenplay input.".into());
            }
            let mut request = if let Some(raw) = get("screenplay-json") {
                serde_json::from_str(&raw).map_err(|e| e.to_string())?
            } else if let Some(file) = get("screenplay-file") {
                read(&expand(&file)?)?
            } else if let Some(file) = get("script-file") {
                let mut cast = Vec::new();
                for binding in flags.get("cast").into_iter().flatten() {
                    let (role, character) = binding
                        .split_once('=')
                        .filter(|(r, c)| !r.trim().is_empty() && !c.trim().is_empty())
                        .ok_or("Use --cast ROLE=CHARACTER for each role assignment.")?;
                    cast.push(json!({"role":role.trim(),"character":character.trim()}));
                }
                json!({"version":1,"script":fs::read_to_string(expand(&file)?).map_err(|e|e.to_string())?,"cast":cast,"render_format":"landscape"})
            } else {
                Value::Null
            };
            if !has("script-file") && has("cast") {
                return Err(
                    "--cast requires --script-file; JSON requests include their own cast.".into(),
                );
            }
            if !request.is_null() {
                if !request.is_object() {
                    return Err("Screenplay request must be an object.".into());
                }
                if let Some(format) = get("render-format") {
                    request["render_format"] = json!(format);
                }
                payload["request"] = request;
            } else {
                for key in [
                    "profile",
                    "planning-budget-seconds",
                    "fresh-preproduction",
                    "render-format",
                    "plan-only",
                    "storyboards-only",
                    "voice-mode",
                    "shooting-script",
                ] {
                    if has(key) {
                        return Err(format!("--{key} requires a screenplay request."));
                    }
                }
            }
        }
        _ => return Err(format!("Unknown command: {command}\n{}", usage())),
    }
    Ok((command, payload))
}
pub fn expand(value: &str) -> Result<PathBuf> {
    let path = if let Some(suffix) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or("Missing home directory.")?).join(suffix)
    } else {
        PathBuf::from(value)
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    })
}
fn emit(mut value: Value, token: &str) -> Result<()> {
    let kind = value["type"].as_str().unwrap_or("activity").to_owned();
    if kind == "shot" {
        return Ok(());
    }
    remove(&mut value, &["type"]);
    if matches!(kind.as_str(), "render_progress" | "inference_progress") {
        value = value["data"].clone();
        value["event"] = json!("render_progress");
    } else {
        value["event"] = json!(kind);
    }
    let serialized = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    let serialized = if token.is_empty() {
        serialized
    } else {
        serialized.replace(token, "[redacted]")
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "AIVS_EVENT {serialized}")
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}
pub fn main(arguments: Vec<String>) -> i32 {
    let root = std::env::var_os("AIVS_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let token = fs::read_to_string(root.join(".env"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| s.trim().strip_prefix("HF_API="))
        .last()
        .unwrap_or_default()
        .trim()
        .trim_matches(['\'', '"'])
        .to_owned();
    let flag = cancellation();
    let result = (|| -> Result<()> {
        let (command, mut args) = parse(arguments)?;
        if command == "help" {
            println!("{}", usage());
            return Ok(());
        }
        args["root"] = json!(root);
        args["python"] = json!(
            std::env::var_os("AIVS_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(".venv/bin/python"))
        );
        let operation = match command.as_str() {
            "render" => {
                if args["request"].is_null() {
                    "production_prompt"
                } else {
                    "production_run"
                }
            }
            "resume" => "production_resume",
            "previsualize" => "production_motion",
            "check" => "production_check",
            "download" => "production_download",
            _ => return Err("Invalid production command.".into()),
        };
        dispatch(operation, &args, flag.clone(), &mut |value| {
            emit(value, &token)
        })?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = emit(
                json!({"type":"error","message":if flag.load(Ordering::Relaxed){"Stopped. Partial output is kept in outputs."}else{&error}}),
                &token,
            );
            if flag.load(Ordering::Relaxed) { 130 } else { 1 }
        }
    }
}
