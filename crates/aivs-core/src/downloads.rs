//! Pinned-model cache integrity and combined download-space accounting.
use crate::continuity::Result;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

const GIB: u64 = 1024 * 1024 * 1024;

fn valid(path: &Path, filename: &str, expected_size: Option<u64>) -> Result<bool> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let size = metadata.len();
    if !metadata.is_file() || size == 0 || expected_size.is_some_and(|n| n != size) {
        return Ok(false);
    }
    if filename.ends_with(".safetensors") {
        let mut stream = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut length = [0; 8];
        stream.read_exact(&mut length).map_err(|e| e.to_string())?;
        let length = u64::from_le_bytes(length);
        if length == 0 || length > (100 * 1024 * 1024).min(size.saturating_sub(8)) {
            return Ok(false);
        }
        let mut bytes = vec![0; length as usize];
        stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let header: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let header = header.as_object().ok_or("Invalid safetensors header.")?;
        let mut largest: Option<u64> = None;
        for (name, entry) in header {
            if name == "__metadata__" {
                continue;
            }
            let offsets = entry["data_offsets"]
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or("Invalid tensor offsets.")?;
            let start = offsets[0].as_u64().ok_or("Invalid tensor offset.")?;
            let end = offsets[1]
                .as_u64()
                .filter(|n| *n >= start)
                .ok_or("Invalid tensor offset.")?;
            largest = Some(largest.unwrap_or(0).max(end));
        }
        return Ok(largest.and_then(|n| n.checked_add(length)?.checked_add(8)) == Some(size));
    }
    if filename.ends_with(".json") {
        let _: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(true)
}

pub fn cached_file(args: &Value) -> Value {
    let result: Result<bool> = (|| {
        let path = PathBuf::from(args["path"].as_str().ok_or("Missing cached-file path.")?);
        let filename = args["filename"]
            .as_str()
            .or_else(|| path.file_name().and_then(|s| s.to_str()))
            .ok_or("Invalid cached filename.")?;
        let expected = if args["expected_size"].is_null() {
            None
        } else {
            Some(
                args["expected_size"]
                    .as_u64()
                    .ok_or("Invalid expected file size.")?,
            )
        };
        valid(&path, filename, expected)
    })();
    json!(result.unwrap_or(false))
}

pub fn preflight(args: &Value) -> Result<Value> {
    let cache = PathBuf::from(
        args["cache_dir"]
            .as_str()
            .ok_or("Missing model cache directory.")?,
    );
    let metadata = args["models"]
        .as_array()
        .ok_or("Missing pinned model metadata.")?;
    let mut models = Vec::new();
    let (mut total_missing, mut total_cached) = (0_u64, 0_u64);
    for model in metadata {
        let repo = model["repo_id"]
            .as_str()
            .ok_or("Invalid model repository.")?;
        let revision = model["revision"]
            .as_str()
            .ok_or("Invalid pinned revision.")?;
        if model["sha"] != revision {
            return Err(format!(
                "Download metadata for {repo} does not match its pinned revision."
            ));
        }
        let files = model["files"]
            .as_array()
            .ok_or("Missing model file metadata.")?;
        let names: BTreeSet<_> = files.iter().filter_map(|f| f["name"].as_str()).collect();
        if model["required_files"]
            .as_array()
            .ok_or("Missing required model files.")?
            .iter()
            .any(|f| f.as_str().is_none_or(|s| !names.contains(s)))
        {
            return Err(format!(
                "Pinned download metadata for {repo} is missing required model files."
            ));
        }
        let storage = cache.join(format!("models--{}", repo.replace('/', "--")));
        let snapshot = storage.join("snapshots").join(revision);
        let (mut missing, mut cached) = (0_u64, 0_u64);
        let mut missing_files = Vec::new();
        for file in files {
            let name = file["name"].as_str().ok_or("Invalid model filename.")?;
            let size = file["size"].as_u64().ok_or_else(|| {
                format!("Download size is unavailable for {repo}/{name}; retry setup.")
            })?;
            if Path::new(name).is_absolute()
                || Path::new(name)
                    .components()
                    .any(|c| c == Component::ParentDir)
            {
                return Err(format!(
                    "Invalid model filename in download metadata for {repo}."
                ));
            }
            let asset = snapshot.join(name);
            let mut complete = valid(&asset, name, Some(size)).unwrap_or(false);
            if !complete && !asset.exists() {
                if let Some(blob) = file["blob"].as_str().filter(|s| {
                    matches!(s.len(), 40 | 64)
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }) {
                    complete =
                        valid(&storage.join("blobs").join(blob), name, Some(size)).unwrap_or(false);
                }
            }
            if complete {
                cached = cached
                    .checked_add(size)
                    .ok_or("Cached model size overflow.")?;
            } else {
                missing = missing
                    .checked_add(size)
                    .ok_or("Missing model size overflow.")?;
                missing_files.push(name);
            }
        }
        total_missing = total_missing
            .checked_add(missing)
            .ok_or("Combined model size overflow.")?;
        total_cached = total_cached
            .checked_add(cached)
            .ok_or("Combined cached size overflow.")?;
        models.push(json!({"repo_id": repo, "revision": revision, "missing_bytes": missing, "cached_bytes": cached, "missing_files": missing_files}));
    }
    let temporary = if total_missing > 0 { 2 * GIB } else { 0 };
    let mut volumes: Vec<(Value, Value)> = Vec::new();
    for volume in args["volumes"]
        .as_array()
        .ok_or("Missing filesystem observations.")?
    {
        let required = if volume["role"] == "cache" {
            total_missing
                .checked_add(temporary)
                .ok_or("Download reserve overflow.")?
        } else if volume["role"] == "output" {
            5 * GIB
        } else {
            return Err("Invalid download volume role.".into());
        };
        let index = volumes
            .iter()
            .position(|(device, _)| *device == volume["device"]);
        if let Some(index) = index {
            let value = &mut volumes[index].1;
            value["required_bytes"] = json!(
                value["required_bytes"]
                    .as_u64()
                    .and_then(|n| n.checked_add(required))
                    .ok_or("Filesystem reserve overflow.")?
            );
        } else {
            volumes.push((volume["device"].clone(), json!({"path": volume["path"], "required_bytes": required, "free_bytes": volume["free_bytes"]})));
        }
    }
    for (_, volume) in &volumes {
        let required = volume["required_bytes"]
            .as_u64()
            .ok_or("Invalid filesystem requirement.")?;
        let free = volume["free_bytes"]
            .as_u64()
            .ok_or("Invalid filesystem free space.")?;
        if free < required {
            return Err(format!(
                "Model setup needs {:.1} GiB free on {}; only {:.1} GiB is available. The combined download has {:.1} GiB of missing or damaged model files, plus {:.0} GiB for temporary files and 5 GiB for generated outputs across their storage volumes. Cached complete files are reused. Free space and retry. This check did not start any model downloads.",
                required as f64 / GIB as f64,
                volume["path"].as_str().unwrap_or_default(),
                free as f64 / GIB as f64,
                total_missing as f64 / GIB as f64,
                temporary as f64 / GIB as f64
            ));
        }
    }
    Ok(
        json!({"missing_bytes": total_missing, "cached_bytes": total_cached, "temporary_reserve_bytes": temporary,
        "output_reserve_bytes": 5 * GIB, "required_bytes": total_missing.checked_add(temporary).and_then(|n| n.checked_add(5 * GIB)).ok_or("Total download reserve overflow.")?,
        "models": models, "filesystems": volumes.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}),
    )
}
