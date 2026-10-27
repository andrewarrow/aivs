//! Saved performer profiles, asset validation and immutable run snapshots.
mod prompt;
use crate::{artifacts::*, continuity::Result};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

const ASSETS: [(&str, &str); 3] = [
    ("image", "reference.png"),
    ("scene_image", "scene-reference.png"),
    ("voice_sample", "voice.wav"),
];
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]*$").unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*$").unwrap());
static VOICE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]{2}_[a-z0-9_]+$").unwrap());

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Invalid character {key}."))
}
fn path(args: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(args, key)?))
}
fn profiles(args: &Value) -> Result<&Vec<Value>> {
    args["profiles"]
        .as_array()
        .ok_or_else(|| "Invalid character profiles.".into())
}
fn slug(name: &Value) -> Result<(String, String)> {
    let display = name
        .as_str()
        .ok_or("Character name must be a slug such as Joe.")?
        .trim();
    if !NAME.is_match(display) {
        return Err("Character name must start with a letter and contain only letters, numbers, underscores, or hyphens.".into());
    }
    Ok((display.to_ascii_lowercase(), display.into()))
}

fn description(value: &Value, label: &str, required: bool) -> Result<()> {
    let value = value
        .as_str()
        .ok_or_else(|| format!("{label} must be text."))?
        .trim();
    if required && value.is_empty() {
        return Err(format!("{label} is required."));
    }
    let chars = value.chars().collect::<Vec<_>>();
    for (index, c) in chars.iter().copied().enumerate() {
        if matches!(c, '"' | '“' | '”')
            || (matches!(c, '\'' | '‘' | '’')
                && (index == 0
                    || index + 1 == chars.len()
                    || !chars[index - 1].is_alphanumeric()
                    || !chars[index + 1].is_alphanumeric()))
        {
            return Err(format!("{label} cannot contain quote characters."));
        }
    }
    Ok(())
}

pub fn validate_asset(
    record: &Value,
    expected: &str,
    directory: &Path,
    label: &str,
) -> Result<PathBuf> {
    if !record.as_object().is_some_and(|fields| {
        fields.len() == 2 && fields.contains_key("file") && fields.contains_key("sha256")
    }) {
        return Err(format!("Invalid {label} asset record."));
    }
    if record["file"] != expected
        || Path::new(expected).file_name().and_then(|s| s.to_str()) != Some(expected)
    {
        return Err(format!("Invalid {label} asset filename."));
    }
    let digest = record["sha256"]
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        .ok_or_else(|| format!("Invalid {label} asset hash."))?;
    let path = directory.join(expected);
    if !path.is_file() || file_hash(&path)? != digest {
        let mut chars = label.chars();
        let capitalized = chars
            .next()
            .map_or(String::new(), |c| c.to_uppercase().collect::<String>())
            + &chars.as_str().to_lowercase();
        return Err(format!(
            "{capitalized} asset is missing or has changed: {expected}."
        ));
    }
    Ok(path)
}

pub fn read_profile(path: &Path) -> Result<Value> {
    let profile = read_json(path)
        .map_err(|_| format!("Cannot read character profile: {}", path.display()))?;
    if !profile.is_object() || profile["schema_version"] != 1 {
        return Err(format!(
            "Unsupported or invalid character profile: {}",
            path.display()
        ));
    }
    let id = profile["id"]
        .as_str()
        .filter(|id| {
            ID.is_match(id)
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|p| p.to_str())
                    == Some(*id)
        })
        .ok_or_else(|| format!("Invalid character id in profile: {}", path.display()))?;
    let (slug, name) = slug(&profile["name"])?;
    if profile["name"] != name || id != slug {
        return Err(format!(
            "Character name and id do not match in profile: {}",
            path.display()
        ));
    }
    description(&profile["description"], "Character description", true)?;
    description(
        profile.get("voice_description").unwrap_or(&json!("")),
        "Voice description",
        false,
    )?;
    if !profile["voice_id"].is_null()
        && !profile["voice_id"]
            .as_str()
            .is_some_and(|s| VOICE.is_match(s))
    {
        return Err(format!("Invalid voice id in profile: {}", path.display()));
    }
    for (field, filename) in ASSETS {
        if !profile[field].is_null() {
            validate_asset(
                &profile[field],
                filename,
                path.parent().ok_or("Missing character directory.")?,
                &field.replace('_', " "),
            )?;
        }
    }
    Ok(profile)
}

pub fn verify(profiles: &[Value], run_dir: &Path) -> Result<()> {
    for supplied in profiles {
        if !supplied.is_object() {
            return Err("Invalid snapshot profile.".into());
        }
        let (id, _) = slug(&supplied["name"])?;
        let saved = read_profile(&run_dir.join("characters").join(&id).join("character.json"))?;
        if saved != *supplied {
            return Err(format!(
                "Character snapshot does not match run metadata: {id}"
            ));
        }
    }
    Ok(())
}

fn snapshot(profiles: &[Value], run_dir: &Path, root: &Path) -> Result<Value> {
    let directory = run_dir.join("characters");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut snapshots = Vec::new();
    for supplied in profiles {
        let (id, _) = slug(&supplied["name"])?;
        let source = root.join("characters").join(&id);
        let current = read_profile(&source.join("character.json"))?;
        if *supplied != current {
            return Err(format!("Character profile changed before snapshot: {id}"));
        }
        let target = directory.join(&id);
        if target.exists() {
            return Err(format!(
                "Character snapshot already exists: {}",
                target.display()
            ));
        }
        let staging = tempfile::Builder::new()
            .prefix(&format!(".{id}-"))
            .tempdir_in(&directory)
            .map_err(|e| e.to_string())?;
        for (field, filename) in ASSETS {
            if !current[field].is_null() {
                fs::copy(source.join(filename), staging.path().join(filename))
                    .map_err(|e| e.to_string())?;
                validate_asset(
                    &current[field],
                    filename,
                    staging.path(),
                    &field.replace('_', " "),
                )?;
            }
        }
        atomic_json(&staging.path().join("character.json"), &current)?;
        if target.exists() {
            return Err(format!(
                "Character snapshot already exists: {}",
                target.display()
            ));
        }
        fs::rename(staging.path(), &target).map_err(|e| e.to_string())?;
        snapshots.push(current);
    }
    Ok(json!(snapshots))
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "character_file_hash" => Ok(json!(file_hash(&path(args, "path")?)?)),
        "character_validate_asset" => Ok(json!(validate_asset(
            &args["record"],
            text(args, "expected_filename")?,
            &path(args, "directory")?,
            text(args, "label")?
        )?)),
        "character_read_profile" => read_profile(&path(args, "path")?),
        "character_load" => {
            let (id, _) = slug(&args["name"])?;
            read_profile(
                &path(args, "root")?
                    .join("characters")
                    .join(id)
                    .join("character.json"),
            )
        }
        "character_resolve" => prompt::resolve(args),
        "character_replace_references" => Ok(json!(prompt::replace(
            text(args, "prompt")?,
            profiles(args)?
        )?)),
        "character_expand_prompt" => prompt::expand(args),
        "character_snapshot" => snapshot(
            profiles(args)?,
            &path(args, "run_dir")?,
            &path(args, "root")?,
        ),
        "character_verify_snapshots" => {
            verify(profiles(args)?, &path(args, "run_dir")?)?;
            Ok(Value::Null)
        }
        "character_reference_image" => {
            let profiles = profiles(args)?
                .iter()
                .filter(|p| !p["scene_image"].is_null() || !p["image"].is_null())
                .cloned()
                .collect::<Vec<_>>();
            if profiles.len() > 1 {
                return Err("Multiple character reference images are available; provide a joint scene reference image.".into());
            }
            let Some(profile) = profiles.first() else {
                return Ok(Value::Null);
            };
            let run_dir = path(args, "run_dir")?;
            verify(&profiles, &run_dir)?;
            let image = if profile["scene_image"].is_null() {
                &profile["image"]
            } else {
                &profile["scene_image"]
            };
            Ok(json!(
                run_dir
                    .join("characters")
                    .join(text(profile, "id")?)
                    .join(text(image, "file")?)
            ))
        }
        _ => Err(format!("Unknown character operation: {operation}")),
    }
}
