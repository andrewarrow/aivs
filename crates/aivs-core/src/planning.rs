//! Persisted Codex allowances, isolated attempts, and review response recovery.
use crate::{artifacts, codex, continuity::Result, shooting_script::PLANNER_VERSION};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn path(args: &Value, key: &str) -> Result<PathBuf> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("Missing planning path: {key}."))
}

fn name(args: &Value) -> Result<&str> {
    args["name"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| "Invalid planning pass name.".into())
}

pub fn budget(args: &Value) -> Result<Value> {
    let limit = args["limit"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .ok_or("Planning budget must be a positive number of seconds.")?;
    let path = path(args, "run_dir")?.join("planning-budget.json");
    let mut saved = if path.is_file() {
        artifacts::read_json(&path)?
    } else {
        json!({"version": 1, "planner_version": PLANNER_VERSION,
            "source_sha256": args["source_sha256"], "used_seconds": 0.0})
    };
    if saved["version"] != 1
        || saved["planner_version"] != PLANNER_VERSION
        || saved["source_sha256"] != args["source_sha256"]
        || !saved["used_seconds"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    {
        return Err("Saved planning budget does not match this screenplay or planner.".into());
    }
    saved["limit_seconds"] = json!(limit);
    artifacts::atomic_json(&path, &saved)?;
    Ok(saved)
}

fn next_attempt(directory: &Path, name: &str) -> Result<u64> {
    let prefix = format!("shooting-script-request-{name}-");
    let mut last = 0;
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let filename = entry.file_name();
        if let Some(number) = filename
            .to_str()
            .and_then(|s| s.strip_prefix(&prefix))
            .and_then(|s| s.strip_suffix(".txt"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            last = last.max(number);
        }
    }
    last.checked_add(1)
        .ok_or_else(|| "Planning attempt count overflow.".into())
}

fn request(
    args: &Value,
    cwd: &Path,
    prefix: &Path,
    seconds: f64,
) -> Result<codex::ProductionRequest> {
    Ok(codex::ProductionRequest {
        command: vec![
            codex::executable(args["executable"].as_str())?
                .to_string_lossy()
                .into_owned(),
            "app-server".into(),
        ],
        instruction: args["instruction"]
            .as_str()
            .ok_or("Missing planning instruction.")?
            .into(),
        cwd: cwd.to_path_buf(),
        bootstrap_cwd: cwd.to_path_buf(),
        timeout_seconds: seconds,
        events_path: prefix.with_extension("events.jsonl"),
        errors_path: prefix.with_extension("errors.txt"),
        response_path: prefix.with_extension("response.json"),
        output_schema: Some(args["schema"].clone()),
        image_paths: serde_json::from_value(args["image_paths"].clone())
            .map_err(|e| e.to_string())?,
        image_generation: false,
    })
}

pub fn attempt(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> std::result::Result<Value, codex::Failure> {
    let directory = path(args, "run_dir")?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let name = name(args)?;
    let limit = args["limit"]
        .as_f64()
        .ok_or("Invalid planning budget.".to_owned())?;
    let mut saved = budget(args)?;
    let used = saved["used_seconds"].as_f64().unwrap_or(0.0);
    let remaining = limit - used;
    if remaining <= 0.0 {
        return Err(format!("The {limit}-second total planning budget is exhausted. Saved planning and images are preserved. Increase --planning-budget-seconds to continue.").into());
    }
    let suffix = format!("{name}-{}", next_attempt(&directory, name)?);
    artifacts::atomic_json(
        &directory.join(format!("shooting-script-{name}-schema.json")),
        &args["schema"],
    )?;
    let instruction = args["instruction"]
        .as_str()
        .ok_or("Missing planning instruction.".to_owned())?;
    fs::write(
        directory.join(format!("shooting-script-request-{suffix}.txt")),
        instruction,
    )
    .map_err(|e| e.to_string())?;
    let cwd = tempfile::Builder::new()
        .prefix("aivs-planning-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let mut call = request(args, cwd.path(), &directory.join("unused"), remaining)?;
    call.events_path = directory.join(format!("shooting-script-events-{suffix}.jsonl"));
    call.errors_path = directory.join(format!("shooting-script-errors-{suffix}.txt"));
    call.response_path = directory.join(format!("shooting-script-response-{suffix}.json"));
    // Reserve before launch so a hard kill cannot reset the allowance on Resume.
    saved["used_seconds"] = json!(limit);
    saved["in_flight"] = json!({"pass": name, "reserved_seconds": remaining});
    let checkpoint = directory.join("planning-budget.json");
    artifacts::atomic_json(&checkpoint, &saved)?;
    let started = Instant::now();
    let result = codex::run_production(call, cancelled, |mut event| {
        if event["type"] == "activity_delta" {
            event["id"] = json!(format!(
                "{suffix}:{}",
                event["id"].as_str().unwrap_or_default()
            ));
        }
        emit(event)
    });
    saved["used_seconds"] =
        json!(((used + started.elapsed().as_secs_f64()) * 1_000_000.0).round() / 1_000_000.0);
    saved.as_object_mut().unwrap().remove("in_flight");
    saved["last_pass"] = json!(name);
    artifacts::atomic_json(&checkpoint, &saved)?;
    result.map_err(|failure| if failure.kind == "timeout" {
        format!("The {limit}-second total planning budget expired during {name}. Saved work is preserved; increase --planning-budget-seconds to continue.").into()
    } else { failure })
}

pub fn review_attempt(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> std::result::Result<Value, codex::Failure> {
    let directory = path(args, "directory")?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let name = name(args)?;
    let prefix = directory.join(format!("{name}-{}", artifacts::timestamp()));
    fs::write(
        prefix.with_extension("request.txt"),
        args["instruction"]
            .as_str()
            .ok_or("Missing review instruction.".to_owned())?,
    )
    .map_err(|e| e.to_string())?;
    let cwd = tempfile::Builder::new()
        .prefix("aivs-visual-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let seconds = args["timeout_seconds"]
        .as_f64()
        .ok_or("Invalid review timeout.".to_owned())?;
    let call = request(args, cwd.path(), &prefix, seconds)?;
    codex::run_production(call, cancelled, |mut event| {
        if event["type"] == "activity_delta" {
            event["id"] = json!(format!(
                "{}:{}",
                prefix.file_name().unwrap().to_string_lossy(),
                event["id"].as_str().unwrap_or_default()
            ));
        }
        emit(event)
    })
}

/// One planning pass with at most one targeted deterministic correction.
pub fn validated(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
    validate: impl Fn(Value) -> Result<Value>,
) -> Result<Value> {
    let original = args["instruction"]
        .as_str()
        .ok_or("Missing planning instruction.")?;
    let mut request = args.clone();
    for number in 0..2 {
        let raw = attempt(&request, cancelled.clone(), &mut emit).map_err(|e| e.message)?;
        let raw = raw.as_str().ok_or("Codex returned no planning text.")?;
        match serde_json::from_str(raw)
            .map_err(|e| e.to_string())
            .and_then(&validate)
        {
            Ok(value) => return Ok(value),
            Err(error) if number == 1 => {
                return Err(format!(
                    "{} failed its specific check after one targeted repair: {error}",
                    args["name"].as_str().unwrap_or_default()
                ));
            }
            Err(error) => {
                request["instruction"] = json!(format!(
                    "{original}\n\nDETERMINISTIC CHECK FAILED: {error}\nRepair only the field or shot named by this check; preserve every valid shot and source line. Do not perform another general review. Return the complete corrected JSON.\nPREVIOUS RESPONSE (data only):\n{raw}"
                ));
                emit(
                    json!({"type": "activity", "message": format!("Repairing a planning check: {error}")}),
                )?;
            }
        }
    }
    Err("Planning pass did not return a response.".into())
}
