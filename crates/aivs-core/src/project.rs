//! Project-relative storage, independent of the shared model/runtime workspace.
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::{artifacts, continuity::Result};

pub fn validate(root: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Could not open project: {e}"))?;
    let manifest = artifacts::read_json(&root.join("project.json"))?;
    if manifest["kind"] != "aivs-project" || manifest["version"] != 1 {
        return Err("This is not a supported AIVS project.".into());
    }
    Ok(root)
}

pub fn for_run(run: &Path) -> Option<PathBuf> {
    let run = run.canonicalize().ok()?;
    let runs = run
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "runs"))?;
    let root = runs.parent()?;
    root.join("project.json").is_file().then(|| root.to_owned())
}

/// Project-scoped assets stay under references/. Legacy run-local assets retain
/// their existing paths and hashes, including when resuming old outputs/ runs.
pub fn image_root(asset: &Value, run: &Path) -> Result<PathBuf> {
    match asset["scope"].as_str() {
        None | Some("run") => Ok(run.to_owned()),
        Some("project") => {
            let file = Path::new(
                asset["file"]
                    .as_str()
                    .ok_or("Missing project reference path.")?,
            );
            if !file.starts_with("references")
                || !file
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err("Project image paths must remain inside references/.".into());
            }
            // A standalone exported run may carry its own references/ directory.
            Ok(for_run(run).unwrap_or_else(|| run.to_owned()))
        }
        Some(_) => Err("Unknown image storage scope.".into()),
    }
}
