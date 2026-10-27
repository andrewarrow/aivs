use super::*;
use crate::media;
use std::{
    fs::{File, TryLockError},
    process::Command,
    sync::atomic::Ordering,
    thread,
    time::Duration,
};

fn executable() -> Result<PathBuf> {
    let chosen = env::var_os("AIVS_CODEX_PATH").unwrap_or_else(|| "codex".into());
    let path = PathBuf::from(&chosen);
    let found = if path.is_absolute() {
        Some(path)
    } else {
        env::var_os("PATH").and_then(|paths| {
            env::split_paths(&paths)
                .map(|directory| directory.join(&chosen))
                .find(|candidate| candidate.is_file())
        })
    };
    found.filter(|path| path.is_file()).ok_or_else(|| {
        format!(
            "Codex executable is unavailable: {}. {SETUP}",
            chosen.to_string_lossy()
        )
    })
}

pub(super) fn identity(root: &Path, cancelled: &AtomicBool) -> Result<Value> {
    let executable = executable()?;
    let version = media::run_timeout(
        Command::new(&executable).arg("--version"),
        cancelled,
        Some(Duration::from_secs(20)),
    )
    .map_err(|error| format!("Cannot start Codex for storyboard images: {error}. {SETUP}"))?;
    let home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or("Cannot locate the Codex home directory.")?;
    let mut files = serde_json::Map::new();
    for path in [
        home.join("auth.json"),
        home.join("config.toml"),
        root.join(".codex/config.toml"),
        bootstrap(root).join(".codex/config.toml"),
    ] {
        files.insert(
            path.to_string_lossy().into_owned(),
            if path.is_file() {
                json!(file_hash(&path)?)
            } else {
                Value::Null
            },
        );
    }
    let mut identity = configuration();
    identity["codex_version"] = json!(String::from_utf8_lossy(&version).trim());
    identity["executable"] = json!(executable);
    identity["configuration_sha256"] = json!(fingerprint(&Value::Object(files))?);
    Ok(identity)
}

fn valid_receipt(receipt: &Value, identity: &Value) -> Result<bool> {
    let age = now()
        - receipt["verified_at"]
            .as_f64()
            .ok_or("Missing preflight verification time.")?;
    let images = receipt["images"]
        .as_array()
        .ok_or("Missing preflight images.")?;
    if receipt["identity"] != *identity || !(0.0..86400.0).contains(&age) || images.len() != 2 {
        return Ok(false);
    }
    for image in images {
        if file_hash(Path::new(text(image, "file")?))? != text(image, "sha256")? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn check(
    root: &Path,
    identity: &Value,
    refresh: bool,
    cancelled: Arc<AtomicBool>,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let directory = root
        .join(".cache/imagegen-preflight")
        .join(fingerprint(identity)?);
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock = File::options()
        .create(true)
        .append(true)
        .open(directory.join(".lock"))
        .map_err(|e| e.to_string())?;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Image generation preflight cancelled.".into());
        }
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("Cannot lock image generation preflight: {error}")),
        }
    }
    let receipt_path = directory.join("verified.json");
    let mut checked = identity.clone();
    checked["preflight"] = json!(receipt_path);
    if !refresh
        && read_json(&receipt_path)
            .and_then(|saved| valid_receipt(&saved, identity))
            .unwrap_or(false)
    {
        return Ok(checked);
    }
    let pending = directory.join("pending.json");
    let attempt = if pending.is_file() && !refresh {
        let saved = read_json(&pending)?;
        let path = PathBuf::from(text(&saved, "directory")?)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !path.starts_with(directory.canonicalize().map_err(|e| e.to_string())?) {
            return Err("Invalid preflight attempt directory.".into());
        }
        path
    } else {
        let path = directory.join(format!("attempt-{}", timestamp()));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        atomic_json(&pending, &json!({"directory": path}))?;
        path
    };
    let mut images = Vec::new();
    for name in ["generate", "edit"] {
        let output = attempt.join(format!("{name}.png"));
        let metadata = attempt.join(format!("{name}.json"));
        if !metadata.is_file() {
            if output.exists() {
                archive(&output, &format!("{name}.interrupted"), "png")?;
            }
            activity(emit, format!("Checking real Codex image {name} capability"))?;
            let mut args = json!({"prompt": format!("A single ceramic cup centered on a plain white table, {}.", if name == "edit" { "blue" } else { "red" }),
                "references": [], "output": output, "seed": 0, "width": 256, "height": 256});
            if name == "edit" {
                args["edit_target"] = json!(attempt.join("generate.png"));
                args["correction"] = json!("Change only the cup color from red to blue.");
                args["preserve"] = json!(["cup shape", "framing", "table", "lighting"]);
            }
            let runtime = session::generate(&args, root, identity, cancelled.clone(), emit)
                .map_err(|error| format!("Codex ImageGen preflight {name} failed with {}: {error}. Saved diagnostics: {}", identity["codex_version"].as_str().unwrap_or("unknown version"), attempt.display()))?;
            atomic_json(&metadata, &runtime)?;
        }
        let runtime = read_json(&metadata)?;
        let sha256 = file_hash(&output)?;
        if runtime["sha256"] != sha256 {
            return Err("Saved preflight image changed; run check --refresh.".into());
        }
        images.push(json!({"file": output, "sha256": sha256}));
    }
    let original_pixels = image::open(attempt.join("generate.png"))
        .map_err(|e| e.to_string())?
        .to_rgb8();
    let edited_pixels = image::open(attempt.join("edit.png"))
        .map_err(|e| e.to_string())?
        .to_rgb8();
    if original_pixels == edited_pixels {
        return Err("Codex returned unchanged pixels for the preflight edit; inspect the saved attempt and run storyboard_backend.py check --refresh.".into());
    }
    atomic_json(
        &receipt_path,
        &json!({"identity": identity, "verified_at": now(), "images": images}),
    )?;
    fs::remove_file(pending).map_err(|e| e.to_string())?;
    Ok(checked)
}
