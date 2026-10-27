//! Rust controller for Codex image generation, capability probes and recovery.
mod preflight;
mod session;

use crate::{artifacts::*, continuity::Result, storyboard_images};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

pub(super) const SETUP: &str =
    "Update Codex CLI and sign in through Codex → Sign in with ChatGPT, then Resume.";

pub fn configuration() -> Value {
    json!({"backend": "codex", "policy": "codex-imagegen-v2", "transport": "app-server",
        "image_generation": "built-in", "model": "codex-configured",
        "seed_supported": false, "normalization": "lanczos-contain-v1"})
}

pub(super) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing image-generation {key}."))
}

pub(super) fn bootstrap(root: &Path) -> PathBuf {
    for start in [
        root.to_path_buf(),
        env::current_dir().unwrap_or_else(|_| root.into()),
    ] {
        for directory in start.ancestors() {
            if directory.join(".codex-root").is_file() {
                return directory.into();
            }
        }
    }
    root.into()
}

pub(super) fn instruction(
    args: &Value,
    prompt: &str,
    paths: &[PathBuf],
    width: u32,
    height: u32,
) -> Result<String> {
    let edit = !args["edit_target"].is_null();
    let mut description = prompt.to_owned();
    if let Some(composition) = args["composition"].as_str().filter(|s| !s.is_empty()) {
        description.push_str(&format!("\nComposition: {composition}"));
    }
    let action = if edit {
        let preserve = args["preserve"]
            .as_array()
            .ok_or("Image edits need preservation constraints.")?
            .iter()
            .map(|v| v.as_str().ok_or("Invalid image preservation constraint."))
            .collect::<Result<Vec<_>>>()?;
        let correction = text(args, "correction")?;
        if correction.trim().is_empty() || preserve.is_empty() {
            return Err(
                "Image edits need a specific correction and preservation constraints.".into(),
            );
        }
        format!(
            "Edit the FIRST attached image using your built-in image generation tool. Correction: {correction}\nPreserve unchanged: {}. Keep every correct region intact; other inputs are identity/geometry references. ",
            preserve.join(", ")
        )
    } else {
        "Create exactly one original raster photograph using your built-in image generation tool. "
            .into()
    };
    let images = paths
        .iter()
        .enumerate()
        .map(|(i, path)| format!("Tool input {}: {}", i + 1, path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "$imagegen\n{action}Produce a single opaque image, not a contact sheet or a description. Compose for a {width} × {height}px frame, with the same aspect ratio and orientation. Keep important faces, hands, full bodies when requested, and complete props clear of the edges so the image can be fitted to the exact video dimensions. Do not use an API key, another image service, SVG, or programmatic placeholder artwork. If image generation is unavailable, report that explicitly.\n\n{}Image brief:\n{description}",
        if paths.is_empty() {
            String::new()
        } else {
            format!(
                "Use ALL of these attached local reference images in this order as actual image-tool inputs. Preserve their identities and follow the role mapping in the brief. References establish appearance, not current pose, clothing, visibility or placement.\n{images}\n\n"
            )
        }
    ))
}

pub fn dispatch(
    operation: &str,
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    if operation == "imagegen_configuration" {
        return Ok(configuration());
    }
    let root = PathBuf::from(text(args, "root")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if operation == "prepare_image_references" {
        let output = PathBuf::from(text(args, "output")?);
        let paths = reference_paths(args)?;
        let inputs = storyboard_images::prepare(&paths, &output, args["keep_first"] == true)?;
        return Ok(
            json!({"paths": inputs.paths, "sources": inputs.sources, "mapping": inputs.mapping}),
        );
    }
    let identity = preflight::identity(&root, &cancelled)?;
    if operation == "imagegen_check_local" {
        return Ok(identity);
    }
    let checked = preflight::check(
        &root,
        &identity,
        args["refresh"] == true,
        cancelled.clone(),
        &mut emit,
    )?;
    if operation == "imagegen_check" {
        return Ok(checked);
    }
    if operation != "imagegen_generate" {
        return Err(format!("Unknown image generation operation: {operation}"));
    }
    // The receipt path is a check result, not part of generation identity.
    session::generate(args, &root, &checked, cancelled, &mut emit)
}

pub(super) fn reference_paths(args: &Value) -> Result<Vec<PathBuf>> {
    args["references"]
        .as_array()
        .ok_or("Image references must be an array.")?
        .iter()
        .map(|value| {
            let path = value.as_str().ok_or("Invalid image reference path.")?;
            Path::new(path)
                .canonicalize()
                .map_err(|e| format!("{path}: {e}"))
        })
        .collect()
}

pub(super) fn activity(
    emit: &mut impl FnMut(Value) -> Result<()>,
    message: impl Into<String>,
) -> Result<()> {
    emit(json!({"type": "activity", "message": message.into()}))
}

pub(super) fn validate_inputs(sources: &[Value]) -> Result<()> {
    for source in sources {
        if file_hash(Path::new(text(source, "path")?))? != text(source, "sha256")? {
            return Err("A storyboard reference changed during image generation.".into());
        }
    }
    Ok(())
}

pub(super) fn archive(path: &Path, name: &str, extension: &str) -> Result<()> {
    fs::rename(
        path,
        path.with_file_name(format!("{name}-{}.{extension}", timestamp())),
    )
    .map_err(|e| e.to_string())
}
