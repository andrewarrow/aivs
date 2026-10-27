//! Assigned references from the shared set library.
use crate::{artifacts, continuity::Result, media, storyboard};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && !s.contains('\0'))
        .ok_or_else(|| format!("Assigned set needs {key}."))
}

pub(crate) fn validate(set: &Value) -> Result<()> {
    let id = text(set, "id")?;
    if id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err("Assigned set has an invalid ID.".into());
    }
    text(set, "name")?;
    text(set, "description")?;
    let image = Path::new(text(&set["image"], "file")?);
    if !image.starts_with("sets")
        || image.components().count() != 3
        || !image
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || !text(&set["image"], "sha256")?
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        || text(&set["image"], "sha256")?.len() != 64
        || set["image"]["width"].as_u64().unwrap_or(0) == 0
        || set["image"]["height"].as_u64().unwrap_or(0) == 0
    {
        return Err("Assigned set has an invalid reference image record.".into());
    }
    Ok(())
}

pub(crate) fn library_root(root: &Path) -> PathBuf {
    if root.join("sets").is_dir() {
        root.to_owned()
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }
}

pub(crate) fn resolve(root: &Path, selection: &Value) -> Result<Option<Value>> {
    if selection.is_null() {
        return Ok(None);
    }
    let id = selection
        .as_str()
        .or_else(|| selection["id"].as_str())
        .ok_or("Assigned set must identify a saved set.")?;
    let directory = library_root(root).join("sets");
    for entry in fs::read_dir(&directory).map_err(|e| format!("Cannot load assigned set: {e}"))? {
        let folder = entry.map_err(|e| e.to_string())?.path();
        if !folder.is_dir() {
            continue;
        }
        let file = folder.join("set.json");
        if !file.is_file() {
            continue;
        }
        let saved = artifacts::read_json(&file)?;
        if saved["id"] != id {
            continue;
        }
        let filename = text(&saved["image"], "file")?;
        if Path::new(filename).file_name().and_then(|s| s.to_str()) != Some(filename) {
            return Err("Assigned set image must be inside its set folder.".into());
        }
        let image = storyboard::resolve_image(&json!({"image":saved["image"]}), &folder)?;
        let info = media::source_image(&image)?;
        let folder_name = folder
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Assigned set has an invalid folder name.")?;
        let profile = json!({"id":id,"name":saved["name"],"description":saved["description"],
            "image":{"file":format!("sets/{folder_name}/{filename}"),"sha256":info["source_sha256"],
                "width":info["width"],"height":info["height"]}});
        validate(&profile)?;
        return Ok(Some(profile));
    }
    Err(format!(
        "Assigned set {id:?} is unavailable. Choose another set before generating."
    ))
}

pub(crate) fn verify(set: &Value, run: &Path) -> Result<()> {
    if !set.is_null() {
        validate(set)?;
        storyboard::resolve_image(&json!({"image":set["image"]}), run)?;
    }
    Ok(())
}
