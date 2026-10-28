//! Maintenance of completed creative revisions; callers hold the render lock.
use crate::{artifacts, codex, continuity::Result, project, storyboard_images};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

fn files_under(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("Artifact maintenance cannot traverse symlinks.".into());
        }
        if kind.is_dir() {
            files_under(&entry.path(), files)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

/// Only remove raster files that duplicate a verified checkpoint master. Never
/// remove tool inputs or the master referenced by images.json/runtime receipts.
pub(crate) fn remove_duplicate_images(workspace: &Path) -> Result<(usize, u64)> {
    let checkpoint = workspace.join("images.json");
    if !checkpoint.is_file() {
        return Ok((0, 0));
    }
    let saved = artifacts::read_json(&checkpoint)?;
    let expected = saved["images"]
        .as_array()
        .ok_or("Missing image checkpoint records.")?
        .iter()
        .filter_map(|v| v["artifact_sha256"].as_str())
        .collect::<BTreeSet<_>>();
    let mut protected = saved["binding"]["inputs"]
        .as_object()
        .into_iter()
        .flat_map(|v| v.keys())
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    for source in saved["binding"]["source_inputs"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(path) = source["path"].as_str() {
            protected.insert(PathBuf::from(path));
        }
    }
    for image in saved["images"].as_array().into_iter().flatten() {
        if let Some(path) = image["savedPath"].as_str() {
            protected.insert(PathBuf::from(path));
        }
    }
    let mut masters = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut files = Vec::new();
    files_under(workspace, &mut files)?;
    for path in files {
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension == "image" {
            let digest = artifacts::file_hash(&path)?;
            if expected.contains(digest.as_str()) {
                masters.insert(digest);
            }
        } else if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp")
            && !protected.contains(&path)
        {
            candidates.push(path);
        }
    }
    let (mut count, mut bytes) = (0, 0);
    for path in candidates {
        if masters.contains(&artifacts::file_hash(&path)?) {
            let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            count += 1;
            bytes += size;
        }
    }
    Ok((count, bytes))
}

fn sheet_numbers(sheet: &Path, checkpoints: &[Value]) -> Result<Option<Vec<u64>>> {
    let saved = artifacts::read_json(&sheet.with_extension("json"))?;
    let group = saved["sources"]
        .as_array()
        .ok_or("Missing sheet source records.")?;
    if group.is_empty() {
        return Ok(None);
    }
    for checkpoint in checkpoints {
        if let Some(sources) = checkpoint["binding"]["source_inputs"].as_array() {
            for (offset, window) in sources.windows(group.len()).enumerate() {
                if window
                    .iter()
                    .zip(group)
                    .all(|(a, b)| a["path"] == b["path"] && a["sha256"] == b["sha256"])
                {
                    return Ok(Some(
                        (offset + 1..offset + 1 + group.len())
                            .map(|n| n as u64)
                            .collect(),
                    ));
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn compact_project(root: &Path, revision: Option<&str>) -> Result<Value> {
    let root = project::validate(root)?;
    let revisions = root.join("preproduction/revisions");
    if let Some(id) = revision {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Invalid revision ID for maintenance.".into());
        }
    }
    let mut totals = BTreeMap::<&str, u64>::from([
        ("revisions", 0),
        ("already_compacted", 0),
        ("files_removed", 0),
        ("bytes_removed", 0),
        ("cache_bytes_added", 0),
    ]);
    if !revisions.is_dir() {
        return Ok(json!(totals));
    }
    let directories = if let Some(id) = revision {
        vec![revisions.join(id)]
    } else {
        fs::read_dir(&revisions)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>>>()?
    };
    for directory in directories {
        if !directory.is_dir() {
            if revision.is_some() {
                return Err("Revision directory is missing.".into());
            }
            continue;
        }
        let canonical = directory.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(&revisions) {
            return Err("Revision maintenance escapes project storage.".into());
        }
        let manifest = directory.join("revision.json");
        if !manifest.is_file() {
            continue;
        }
        let manifest = artifacts::read_json(&manifest)?;
        if manifest["kind"] != "aivs-creative-revision" || manifest["status"] != "complete" {
            continue;
        }
        let marker = root.join("preproduction/cache/compaction").join(format!(
            "{}.json",
            directory
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("Invalid revision directory.")?
        ));
        let compacted = if marker.is_file() {
            Some(artifacts::read_json(&marker)?)
        } else {
            None
        };
        if compacted.as_ref().is_some_and(|record| {
            record["policy"] == "completed-revision-storage-v1"
                && record["keep_stream_logs"] == codex::keep_stream_logs()
        }) {
            *totals.get_mut("already_compacted").unwrap() += 1;
            continue;
        }
        let mut files = Vec::new();
        files_under(&directory, &mut files)?;
        let checkpoint_paths = files
            .iter()
            .filter(|p| p.file_name().is_some_and(|n| n == "images.json"))
            .collect::<Vec<_>>();
        let checkpoints = checkpoint_paths
            .iter()
            .map(|p| artifacts::read_json(p))
            .collect::<Result<Vec<_>>>()?;
        for path in &files {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if path.extension().is_some_and(|s| s == "jsonl")
                && (name == "events.jsonl" || name.starts_with("shooting-script-events-"))
            {
                let parent = path.parent().ok_or("Missing event directory.")?;
                let image_root = if parent
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("attempt-"))
                {
                    parent
                        .parent()
                        .ok_or("Missing image workspace.")?
                        .to_owned()
                } else {
                    parent.join("images")
                };
                *totals.get_mut("bytes_removed").unwrap() +=
                    codex::compact_event_log(path, &image_root)?;
            }
        }
        for checkpoint in &checkpoint_paths {
            let (count, bytes) =
                remove_duplicate_images(checkpoint.parent().ok_or("Missing checkpoint parent.")?)?;
            *totals.get_mut("files_removed").unwrap() += count as u64;
            *totals.get_mut("bytes_removed").unwrap() += bytes;
        }
        for path in &files {
            if path.extension().is_some_and(|s| s == "png")
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "reference-sheets")
                && path.with_extension("json").is_file()
            {
                let numbers = sheet_numbers(path, &checkpoints)?;
                let added =
                    storyboard_images::cache_existing_sheet(&root, path, numbers.as_deref())?;
                let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
                fs::remove_file(path).map_err(|e| e.to_string())?;
                *totals.get_mut("files_removed").unwrap() += 1;
                *totals.get_mut("bytes_removed").unwrap() += size;
                *totals.get_mut("cache_bytes_added").unwrap() += added;
            }
        }
        *totals.get_mut("revisions").unwrap() += 1;
        artifacts::atomic_json(
            &marker,
            &json!({"policy":"completed-revision-storage-v1","revision":manifest["id"],"completed_at":artifacts::now(),"keep_stream_logs":codex::keep_stream_logs()}),
        )?;
    }
    Ok(json!(totals))
}
