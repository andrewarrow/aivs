//! Saved-cast production policy and isolated Seed-VC orchestration.
use crate::{artifacts::*, characters, continuity::Result, media};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock, atomic::AtomicBool},
};

pub const POLICY: &str = "saved-character-assets-v1";
static VOICE_PREFLIGHTS: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Invalid character conditioning {key}."))
}
fn list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value[key]
        .as_array()
        .ok_or_else(|| format!("Invalid character conditioning {key}."))
}
fn path(value: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(value, key)?))
}
fn mode(source: &Value) -> Result<&str> {
    match source.get("voice_mode") {
        None => Ok("saved"),
        Some(Value::String(value)) if value == "saved" => Ok("saved"),
        Some(Value::String(value)) if value == "native" => Ok("native"),
        _ => Err("Voice mode must be saved or native.".into()),
    }
}

fn verify<'a>(
    source: &Value,
    profiles: &'a [Value],
    run_dir: &Path,
) -> Result<BTreeMap<String, &'a Value>> {
    if source["character_conditioning"] != POLICY {
        return Err("Regenerate the shooting plan for saved character asset conditioning.".into());
    }
    characters::verify(profiles, run_dir)?;
    let by_id = profiles
        .iter()
        .map(|p| Ok((text(p, "id")?.to_owned(), p)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for profile in list(source, "characters")? {
        let saved = by_id
            .get(text(profile, "id")?)
            .ok_or("Shooting plan and saved character assets do not match.")?;
        if !profile
            .as_object()
            .is_some_and(|fields| fields.iter().all(|(k, v)| saved.get(k) == Some(v)))
        {
            return Err("Shooting plan and saved character assets do not match.".into());
        }
    }
    Ok(by_id)
}

fn worker(root: &Path, arguments: &[&std::ffi::OsStr], cancelled: &AtomicBool) -> Result<Value> {
    let python = root.join(".cache/seed-vc-env/bin/python");
    if !python.is_file() {
        return Err("Saved-character voices require the separate Seed-VC environment. Restart AIVS to finish automatic setup, then Resume this run. For CLI setup, see README.md's Saved character rendering setup. For a new run with generated voices, choose --voice-mode native.".into());
    }
    let bytes = media::run(
        Command::new(python)
            .arg("-u")
            .arg(root.join("experiments/character-consistency/run_voice_conversion.py"))
            .arg("--code-dir")
            .arg(root.join(".cache/seed-vc"))
            .arg("--models-dir")
            .arg(root.join(".cache/seed-vc-models"))
            .args(arguments),
        cancelled,
    )
    .map_err(|e| {
        format!(
            "Saved voice conversion failed: {e}. See README.md's Saved character rendering setup."
        )
    })?;
    for line in String::from_utf8_lossy(&bytes).lines().rev() {
        if let Some(record) = line.strip_prefix("AIVS_VOICE ") {
            let value: Value = serde_json::from_str(record).map_err(|e| e.to_string())?;
            if !value.is_object() {
                return Err("Saved voice converter returned invalid provenance.".into());
            }
            return Ok(value);
        }
    }
    Err("Saved voice converter returned no provenance record.".into())
}

fn identity(
    source: &Value,
    run_dir: &Path,
    root: &Path,
    cancelled: &AtomicBool,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let key = serde_json::to_string(&json!([run_dir, source["source_sha256"], root]))
        .map_err(|e| e.to_string())?;
    let cache = VOICE_PREFLIGHTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(value) = cache
        .lock()
        .map_err(|_| "Voice preflight cache unavailable.")?
        .get(&key)
    {
        return Ok(value.clone());
    }
    emit(json!({"activity": "Checking saved-character voice setup before planning"}))?;
    let result = worker(root, &["--check".as_ref()], cancelled)?;
    cache
        .lock()
        .map_err(|_| "Voice preflight cache unavailable.")?
        .insert(key, result.clone());
    Ok(result)
}

fn preflight(
    args: &Value,
    cancelled: &AtomicBool,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let source = &args["source"];
    let voice_mode = mode(source)?;
    let run_dir = path(args, "run_dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let by_id = verify(source, list(args, "profiles")?, &run_dir)?;
    let require_voice = args["require_voice"].as_bool().unwrap_or(true);
    let speakers = list(source, "beats")?
        .iter()
        .filter(|b| b["kind"] == "dialogue")
        .map(|b| text(b, "speaker"))
        .collect::<Result<BTreeSet<_>>>()?;
    for binding in list(source, "cast")? {
        let role = text(binding, "role")?;
        let profile = by_id
            .get(text(binding, "character")?)
            .ok_or_else(|| format!("No saved character snapshot for {role}."))?;
        let mut assets = vec![("image", "reference.png")];
        if require_voice && voice_mode == "saved" && speakers.contains(role) {
            assets.push(("voice_sample", "voice.wav"));
        }
        for (field, filename) in assets {
            if profile[field].is_null() {
                return Err(format!(
                    "{} needs a saved {} before production can start.",
                    text(profile, "name")?,
                    field.replace('_', " ")
                ));
            }
            let asset = characters::validate_asset(
                &profile[field],
                filename,
                &run_dir.join("characters").join(text(profile, "id")?),
                &field.replace('_', " "),
            )?;
            if !asset
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&run_dir)
            {
                return Err("Saved character assets must remain inside the run directory.".into());
            }
            if field == "image" {
                media::source_image(&asset)?;
            }
        }
    }
    let converter = if require_voice && voice_mode == "saved" && !speakers.is_empty() {
        identity(source, &run_dir, &path(args, "root")?, cancelled, emit)?
    } else {
        Value::Null
    };
    Ok(json!({"voice_mode": voice_mode, "converter": converter}))
}

fn prepare(
    args: &Value,
    cancelled: &AtomicBool,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let source = &args["plan"]["source"];
    let run_dir = path(args, "run_dir")?;
    let by_id = verify(source, list(args, "profiles")?, &run_dir)?;
    let storyboard = args["storyboard_conditioned"] == true;
    let ready = if storyboard {
        let mut request = args.clone();
        request["source"] = source.clone();
        request["require_voice"] = json!(true);
        preflight(&request, cancelled, emit)?
    } else {
        json!({"voice_mode": mode(source)?, "converter": null})
    };
    let cast = list(source, "cast")?
        .iter()
        .map(|b| {
            let id = text(b, "character")?;
            Ok((
                text(b, "role")?,
                *by_id
                    .get(id)
                    .ok_or_else(|| format!("No saved character snapshot for {}.", b["role"]))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let beats = list(source, "beats")?
        .iter()
        .map(|b| Ok((b["id"].as_u64().ok_or("Invalid beat id.")?, b)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut prepared = Vec::new();
    for (index, shot) in list(&args["plan"], "shots")?.iter().enumerate() {
        let roles = if let Some(roles) = shot.get("visible_roles") {
            roles
                .as_array()
                .ok_or("Invalid visible roles.")?
                .iter()
                .map(|r| r.as_str().ok_or_else(|| "Invalid visible role.".to_owned()))
                .collect::<Result<Vec<_>>>()?
        } else {
            cast.keys().copied().collect()
        };
        let visible = roles
            .into_iter()
            .map(|r| {
                cast.get(r)
                    .ok_or_else(|| format!("Unknown visible role: {r}."))
                    .and_then(|p| text(p, "id"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let mut speakers = BTreeSet::new();
        for id in list(shot, "beat_ids")? {
            let beat = beats
                .get(&id.as_u64().ok_or("Invalid beat id.")?)
                .ok_or("Unknown shot beat.")?;
            if beat["kind"] == "dialogue" {
                let role = text(beat, "speaker")?;
                speakers.insert(text(
                    cast.get(role).ok_or("Unknown dialogue speaker.")?,
                    "id",
                )?);
            }
        }
        let mut binding = json!({"policy": POLICY});
        for (kind, selected, field) in [
            ("image", visible, "image"),
            ("voice", speakers, "voice_sample"),
        ] {
            binding[kind] = if kind == "voice" && ready["voice_mode"] == "native" {
                json!({"status": "native", "characters": selected})
            } else if kind == "image" && storyboard {
                json!({"status": "storyboard", "characters": selected})
            } else if selected.len() == 1 {
                let id = *selected.first().unwrap();
                let profile = by_id[id];
                let asset = &profile[field];
                if asset.is_null() {
                    return Err(format!(
                        "{} needs a saved {} for character rendering.",
                        text(profile, "name")?,
                        field.replace('_', " ")
                    ));
                }
                json!({"character": id, "sha256": asset["sha256"],
                    "file": Path::new("characters").join(id).join(text(asset, "file")?)})
            } else if !selected.is_empty() {
                let reason = if kind == "image" {
                    "Multiple visible performers need a joint scene reference; using descriptions and frame continuity."
                } else {
                    "Multiple speakers need isolated dialogue tracks; retaining generated voices."
                };
                emit(json!({"activity": format!("Shot {}: {reason}", index + 1)}))?;
                json!({"status": "unsupported", "reason": reason})
            } else {
                json!({"status": "not-needed"})
            };
        }
        prepared.push(binding);
    }
    if prepared.iter().any(|b| b["voice"].get("file").is_some()) {
        let converter = if ready["converter"].is_null() {
            identity(source, &run_dir, &path(args, "root")?, cancelled, emit)?
        } else {
            ready["converter"].clone()
        };
        for binding in &mut prepared {
            if binding["voice"].get("file").is_some() {
                binding["voice"]["converter"] = converter.clone();
            }
        }
    }
    Ok(json!(prepared))
}

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    command
}

fn apply_voice(args: &Value, cancelled: &AtomicBool) -> Result<Value> {
    let source = path(args, "source")?;
    let clip = path(args, "clip")?;
    let run_dir = path(args, "run_dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let voice = &args["voice"];
    let reference = run_dir
        .join(text(voice, "file")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !reference.starts_with(&run_dir) || json!(file_hash(&reference)?) != voice["sha256"] {
        return Err("Saved character voice changed before conversion.".into());
    }
    let seconds = args["seconds"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("Invalid voice duration.")?;
    let directory = clip
        .parent()
        .ok_or("Missing clip directory.")?
        .join("voice");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let audio = directory.join("source.wav");
    media::run(
        ffmpeg()
            .arg("-i")
            .arg(&source)
            .args([
                "-vn",
                "-map",
                "0:a:0",
                "-ar",
                "24000",
                "-ac",
                "1",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&audio),
        cancelled,
    )?;
    let inputs = json!({"source_audio": file_hash(&audio)?, "reference": voice["sha256"]});
    let converted = directory.join("converted.wav");
    let mut record = read_json(&directory.join("conversion.json")).unwrap_or(Value::Null);
    if record["inputs"] != inputs
        || record["identity"] != voice["converter"]
        || !converted.is_file()
        || record["sha256"] != file_hash(&converted)?
    {
        record = worker(
            &path(args, "root")?,
            &[
                "--source-audio".as_ref(),
                audio.as_os_str(),
                "--reference".as_ref(),
                reference.as_os_str(),
                "--output-dir".as_ref(),
                directory.as_os_str(),
            ],
            cancelled,
        )?;
    }
    if record["inputs"] != inputs
        || record["identity"] != voice["converter"]
        || record["sha256"] != file_hash(&converted)?
        || voice["sha256"] != file_hash(&reference)?
    {
        return Err(
            "Converted soundtrack does not match the saved character and native take.".into(),
        );
    }
    let partial = clip.with_file_name("video.voice.partial.mp4");
    let soundtrack = directory.join("soundtrack.wav");
    media::run(
        ffmpeg()
            .arg("-i")
            .arg(&converted)
            .args([
                "-af",
                &format!("apad,atrim=duration={seconds},asetpts=PTS-STARTPTS"),
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&soundtrack),
        cancelled,
    )?;
    media::run(
        ffmpeg()
            .arg("-i")
            .arg(&source)
            .arg("-i")
            .arg(&soundtrack)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-t",
                &seconds.to_string(),
                "-movflags",
                "+faststart",
            ])
            .arg(&partial),
        cancelled,
    )?;
    media::dispatch(
        "validate_video",
        &json!({"path": partial, "frames": seconds.checked_mul(24).ok_or("Voice duration overflow.")?,
        "width": args["width"], "height": args["height"], "expect_audio": true}),
        cancelled,
    )?;
    let validation = media::dispatch(
        "validate_supplied_audio",
        &json!({"video": partial, "source": soundtrack, "duration": seconds}),
        cancelled,
    )?;
    fs::rename(&partial, &clip).map_err(|e| e.to_string())?;
    record["character"] = voice["character"].clone();
    record["native_picture_unchanged"] = json!(true);
    record["soundtrack_sha256"] = json!(file_hash(&soundtrack)?);
    record["audio_validation"] = validation;
    record["review"] = json!("pending: voice identity, spoken words, and lip sync");
    Ok(record)
}

pub fn dispatch(
    operation: &str,
    args: &Value,
    cancelled: &AtomicBool,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    match operation {
        "conditioning_check_voice" => {
            worker(&path(args, "root")?, &["--check".as_ref()], cancelled)
        }
        "conditioning_preflight" => preflight(args, cancelled, &mut emit),
        "conditioning_prepare" => prepare(args, cancelled, &mut emit),
        "conditioning_apply_voice" => apply_voice(args, cancelled),
        _ => Err("Unknown character conditioning operation.".into()),
    }
}
