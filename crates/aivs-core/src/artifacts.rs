//! Artifact bytes and the legacy Python sorted, ASCII JSON fingerprint codec.
use crate::continuity::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

pub fn file_hash(path: &Path) -> Result<String> {
    let mut stream = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 1024 * 64];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Matches json.dumps(value, sort_keys=True), including spaces and UTF-16 escapes.
/// Image-generation fingerprint inputs contain strings, integers, booleans and null;
/// the other production fingerprints continue to use their existing Python codec.
pub fn legacy_json(value: &Value) -> Result<String> {
    fn string(text: &str) -> String {
        let quoted = serde_json::to_string(text).expect("string serialization");
        let mut output = String::new();
        for c in quoted.chars() {
            if c as u32 >= 0x7f {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    output.push_str(&format!("\\u{unit:04x}"));
                }
            } else {
                output.push(c);
            }
        }
        output
    }
    Ok(match value {
        Value::String(text) => string(text),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(legacy_json)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let rows = keys
                .into_iter()
                .map(|key| Ok(format!("{}: {}", string(key), legacy_json(&values[key])?)))
                .collect::<Result<Vec<_>>>()?;
            format!("{{{}}}", rows.join(", "))
        }
        Value::Number(number) if number.is_f64() => {
            return Err("Legacy image fingerprints require integer numbers.".into());
        }
        _ => value.to_string(),
    })
}

pub fn fingerprint(value: &Value) -> Result<String> {
    Ok(hash(legacy_json(value)?))
}

pub fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().ok_or("Missing artifact directory.")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut temporary, value).map_err(|e| e.to_string())?;
    temporary.write_all(b"\n").map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn exclusive_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("Missing artifact directory.")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(bytes).map_err(|e| e.to_string())?;
    temporary
        .persist_noclobber(path)
        .map_err(|e| format!("Refusing to replace {}: {e}", path.display()))?;
    Ok(())
}

pub fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
pub fn now() -> f64 {
    timestamp() as f64 / 1_000_000_000.0
}
