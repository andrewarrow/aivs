//! Production rules and Codex sessions without a desktop, GPU, or Python runtime.
pub mod codex;
pub mod continuity;

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub const PROTOCOL_VERSION: u64 = 1;

/// One request and response per line. Domain failures never terminate the worker.
/// Artifact bytes and fingerprints remain owned by the legacy codec during migration.
pub fn serve(reader: impl BufRead + Send + 'static, mut writer: impl Write) -> std::io::Result<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let parent_closed = cancelled.clone();
    let (input, requests) = mpsc::channel();
    std::thread::spawn(move || {
        for line in reader.lines() {
            if input.send(line).is_err() {
                break;
            }
        }
        parent_closed.store(true, Ordering::Relaxed);
    });
    for line in requests {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let mut error_kind = "validation";
                let result = if request["version"].as_u64() == Some(PROTOCOL_VERSION) {
                    if request["operation"] == "implementation" {
                        Ok(json!({
                            "crates/aivs-core/src/lib.rs": include_str!("lib.rs"),
                            "crates/aivs-core/src/continuity.rs": include_str!("continuity.rs"),
                            "crates/aivs-core/src/codex.rs": include_str!("codex.rs"),
                        }))
                    } else if request["operation"] == "codex" {
                        error_kind = "codex";
                        match serde_json::from_value(request["args"].clone()) {
                            Ok(args) => codex::run_production(args, cancelled.clone(), |event| {
                                let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                                serde_json::to_writer(&mut writer, &envelope).map_err(|e| e.to_string())?;
                                writer.write_all(b"\n").and_then(|_| writer.flush()).map_err(|e| e.to_string())
                            }).map_err(|failure| { error_kind = failure.kind; failure.message }),
                            Err(error) => Err(format!("Invalid Codex request: {error}")),
                        }
                    } else {
                        continuity::dispatch(&request)
                    }
                } else {
                    Err("Unsupported AIVS engine protocol version.".into())
                };
                match result {
                    Ok(result) => {
                        json!({"version": PROTOCOL_VERSION, "id": request["id"], "result": result})
                    }
                    Err(error) => {
                        json!({"version": PROTOCOL_VERSION, "id": request["id"], "error": error, "error_kind": error_kind})
                    }
                }
            }
            Err(error) => {
                json!({"version": PROTOCOL_VERSION, "id": null, "error": error.to_string()})
            }
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}
