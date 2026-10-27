//! Assigned set references and immutable, run-local snapshots.
use crate::{artifacts, continuity::Result, media, storyboard};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
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
    if set["image"]["file"] != format!("sets/{id}/reference.png")
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

pub(crate) fn resolve(root: &Path, selection: &Value) -> Result<Option<(Value, PathBuf)>> {
    if selection.is_null() {
        return Ok(None);
    }
    let id = selection
        .as_str()
        .or_else(|| selection["id"].as_str())
        .ok_or("Assigned set must identify a saved set.")?;
    let directory = if root.join("sets").is_dir() {
        root.join("sets")
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sets")
    };
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
        let profile = json!({"id":id,"name":saved["name"],"description":saved["description"],
            "image":{"file":format!("sets/{id}/reference.png"),"sha256":info["source_sha256"],
                "width":info["width"],"height":info["height"]}});
        validate(&profile)?;
        return Ok(Some((profile, image)));
    }
    Err(format!(
        "Assigned set {id:?} is unavailable. Choose another set before generating."
    ))
}

pub(crate) fn snapshot(set: &Value, image: &Path, run: &Path) -> Result<()> {
    validate(set)?;
    let bytes = fs::read(image).map_err(|e| e.to_string())?;
    if artifacts::hash(&bytes) != set["image"]["sha256"] {
        return Err("Assigned set image changed before production. Generate again.".into());
    }
    artifacts::exclusive_bytes(&run.join(text(&set["image"], "file")?), &bytes)?;
    verify(set, run)
}

pub(crate) fn verify(set: &Value, run: &Path) -> Result<()> {
    if !set.is_null() {
        validate(set)?;
        storyboard::resolve_image(&json!({"image":set["image"]}), run)?;
    }
    Ok(())
}
