//! Durable image receipts and compact completed-session diagnostics.
use crate::artifacts;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::Path,
};

pub fn keep_stream_logs() -> bool {
    std::env::var("AIVS_KEEP_STREAM_LOGS")
        .is_ok_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Persist the payload before logging or delivering its receipt. The image
/// checkpoint uses this same filename, including after an interrupted turn.
pub fn log_event(event: &Value, image_root: &Path) -> Result<Value, String> {
    let mut event = event.clone();
    fn sanitize(value: &mut Value, root: &Path) -> Result<(), String> {
        match value {
            Value::Object(fields) => {
                if fields.get("type").and_then(Value::as_str) == Some("imageGeneration") {
                    let completed =
                        fields.get("status").and_then(Value::as_str) == Some("completed");
                    let id_name = artifacts::hash(
                        fields
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("unidentified"),
                    )[..16]
                        .to_owned();
                    let canonical = root.join(format!("{id_name}.image"));
                    let bytes =
                        if let Some(encoded) = fields
                            .get("result")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            let encoded = if encoded.starts_with("data:") {
                                encoded.split_once(',').map_or(encoded, |(_, data)| data)
                            } else {
                                encoded
                            };
                            Some(STANDARD.decode(encoded).map_err(|e| {
                                format!("Invalid image payload in Codex event: {e}")
                            })?)
                        } else if completed && canonical.is_file() {
                            fields.insert("savedPath".into(), json!(canonical));
                            fields.insert(
                                "artifact_sha256".into(),
                                json!(artifacts::file_hash(&canonical)?),
                            );
                            fields.insert(
                                "result_bytes".into(),
                                json!(fs::metadata(&canonical).map_err(|e| e.to_string())?.len()),
                            );
                            None
                        } else if completed {
                            fields
                                .get("savedPath")
                                .and_then(Value::as_str)
                                .map(Path::new)
                                .filter(|p| p.is_file())
                                .map(|p| fs::read(p).map_err(|e| e.to_string()))
                                .transpose()?
                        } else {
                            None
                        };
                    if let Some(bytes) = bytes {
                        let digest = artifacts::hash(&bytes);
                        let name = if completed { id_name } else { digest.clone() };
                        let artifact = root.join(format!("{name}.image"));
                        if artifact.exists() {
                            if artifacts::file_hash(&artifact)? != digest {
                                return Err("Completed Codex image payload changed.".into());
                            }
                        } else {
                            artifacts::exclusive_bytes(&artifact, &bytes)?;
                        }
                        fields.insert("savedPath".into(), json!(artifact));
                        fields.insert("artifact_sha256".into(), json!(digest));
                        fields.insert("result_bytes".into(), json!(bytes.len()));
                    }
                    fields.remove("result");
                }
                for child in fields.values_mut() {
                    sanitize(child, root)?;
                }
            }
            Value::Array(values) => {
                for child in values {
                    sanitize(child, root)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    sanitize(&mut event, image_root)?;
    Ok(event)
}

pub(super) fn invalid_line(bytes: &[u8]) -> Value {
    json!({"kind":"invalid-protocol-line","bytes":bytes.len(),"sha256":artifacts::hash(bytes),
        "preview":String::from_utf8_lossy(bytes).chars().take(512).collect::<String>()})
}

#[derive(Default)]
struct Summary {
    count: u64,
    methods: BTreeMap<String, u64>,
    thread: Value,
    turn: Value,
    usage: Value,
    images: BTreeMap<String, Value>,
    notices: Vec<Value>,
}

impl Summary {
    fn observe(&mut self, event: &Value) {
        self.count += 1;
        let method = event["method"].as_str().unwrap_or("response");
        *self.methods.entry(method.to_owned()).or_default() += 1;
        let params = &event["params"];
        if let Some(id) = params.get("threadId") {
            self.thread = id.clone();
        }
        if method == "thread/tokenUsage/updated" {
            self.usage = params["tokenUsage"].clone();
        }
        if method == "turn/completed" {
            let turn = &params["turn"];
            self.turn = json!({"id":turn["id"],"status":turn["status"],"error":turn["error"],
                "startedAt":turn["startedAt"],"completedAt":turn["completedAt"],"durationMs":turn["durationMs"]});
        }
        if method == "error"
            || (event.get("error").is_some_and(|v| !v.is_null()))
            || event["kind"] == "invalid-protocol-line"
        {
            self.notices.push(event.clone());
        }
        fn images(value: &Value, output: &mut BTreeMap<String, Value>) {
            match value {
                Value::Object(fields) => {
                    if fields.get("type").and_then(Value::as_str) == Some("imageGeneration") {
                        let id = value["id"].as_str().unwrap_or("unidentified");
                        output.insert(id.to_owned(),json!({"id":id,"status":value["status"],"savedPath":value["savedPath"],
                            "artifact_sha256":value["artifact_sha256"],"result_bytes":value["result_bytes"],"failure":value["failure"]}));
                    }
                    for child in fields.values() {
                        images(child, output);
                    }
                }
                Value::Array(values) => {
                    for child in values {
                        images(child, output);
                    }
                }
                _ => {}
            }
        }
        images(event, &mut self.images);
    }

    fn value(&self) -> Value {
        json!({"kind":"aivs-codex-session","version":1,"status":"completed","event_count":self.count,
            "method_counts":self.methods,"thread_id":self.thread,"turn":self.turn,"token_usage":self.usage,
            "images":self.images.values().collect::<Vec<_>>(),"notices":self.notices})
    }
}

/// Successful sessions retain one summary; failed/interrupted sessions retain
/// detailed events. Image payloads are externalized in either case. The original
/// file is replaced only after its entire replacement has been written.
pub fn compact_event_log(path: &Path, image_root: &Path) -> Result<u64, String> {
    let original_bytes = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let parent = path.parent().ok_or("Missing event log directory.")?;
    let mut replacement = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut summary = Summary::default();
    for line in BufReader::new(fs::File::open(path).map_err(|e| e.to_string())?).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Value =
            serde_json::from_str(&line).unwrap_or_else(|_| invalid_line(line.as_bytes()));
        if event["kind"] == "aivs-codex-session" {
            return Ok(0);
        }
        let event = log_event(&event, image_root)?;
        summary.observe(&event);
        serde_json::to_writer(&mut replacement, &event).map_err(|e| e.to_string())?;
        replacement.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    if summary.turn["status"] == "completed" && !keep_stream_logs() {
        replacement
            .as_file_mut()
            .set_len(0)
            .map_err(|e| e.to_string())?;
        replacement
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut replacement, &summary.value()).map_err(|e| e.to_string())?;
        replacement.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    replacement.flush().map_err(|e| e.to_string())?;
    let new_bytes = replacement
        .as_file()
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    if artifacts::file_hash(path)? != artifacts::file_hash(replacement.path())? {
        replacement.persist(path).map_err(|e| e.to_string())?;
    }
    Ok(original_bytes.saturating_sub(new_bytes))
}
