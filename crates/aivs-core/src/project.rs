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
    run.ancestors()
        .find(|root| root.join("project.json").is_file())
        .map(Path::to_owned)
}

/// A run pins one creative revision. Legacy runs keep their local files.
pub fn creative_root(run: &Path) -> Result<PathBuf> {
    let receipt = run.join("preproduction.json");
    if receipt.is_file() {
        let receipt = artifacts::read_json(&receipt)?;
        if receipt["kind"] == "aivs-revision-binding" {
            let project = for_run(run).ok_or("Missing project for creative revision.")?;
            let relative = Path::new(
                receipt["directory"]
                    .as_str()
                    .ok_or("Missing revision directory.")?,
            );
            if !relative.starts_with("preproduction/revisions")
                || !relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
            {
                return Err("Creative revisions must stay inside project preproduction/.".into());
            }
            let directory = project
                .join(relative)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !directory.starts_with(project.join("preproduction/revisions")) {
                return Err("Creative revision escapes the project.".into());
            }
            return Ok(directory);
        }
    }
    Ok(run.to_owned())
}

pub fn creative_file(run: &Path, name: &str) -> Result<PathBuf> {
    Ok(creative_root(run)?.join(name))
}

/// New runs record shared library locations instead of copying their assets.
/// Runs created before this metadata existed retain their local asset paths.
pub fn library_root(run: &Path, library: &str) -> Result<PathBuf> {
    let report = if run.join("render.json").is_file() {
        run.join("render.json")
    } else {
        run.join("input.json")
    };
    if report.is_file() {
        let report = artifacts::read_json(&report)?;
        if let Some(root) = report["libraries"].get(library) {
            let root = PathBuf::from(root.as_str().ok_or("Invalid shared library path.")?);
            if !root.is_absolute() {
                return Err("Shared library roots must be absolute paths.".into());
            }
            return Ok(root);
        }
    }
    Ok(run.to_owned())
}

/// Project references stay under references/; character and set paths resolve
/// against the shared libraries recorded by the run.
pub fn image_root(asset: &Value, run: &Path) -> Result<PathBuf> {
    match asset["scope"].as_str() {
        None => {
            let file = Path::new(asset["file"].as_str().unwrap_or_default());
            for library in ["characters", "sets"] {
                if file.starts_with(library) {
                    return library_root(run, library);
                }
            }
            if file.starts_with("storyboards") && !run.join(file).is_file() {
                return creative_root(run);
            }
            Ok(run.to_owned())
        }
        Some("run") => Ok(run.to_owned()),
        Some("project") => {
            let file = Path::new(
                asset["file"]
                    .as_str()
                    .ok_or("Missing project reference path.")?,
            );
            if !(file.starts_with("references") || file.starts_with("preproduction"))
                || !file
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(
                    "Project image paths must remain inside references/ or preproduction/.".into(),
                );
            }
            // A standalone exported run may carry its own references/ directory.
            Ok(for_run(run).unwrap_or_else(|| run.to_owned()))
        }
        Some(_) => Err("Unknown image storage scope.".into()),
    }
}
