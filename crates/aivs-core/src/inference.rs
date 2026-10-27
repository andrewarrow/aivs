//! One versioned request per disposable Python model process.
use crate::continuity::Result;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const VERSION: u64 = 1;
const PREFIX: &str = "AIVS_INFERENCE ";

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn tail(path: &std::path::Path) -> String {
    let read = || -> std::io::Result<String> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::End(-1800_i64.min(file.metadata()?.len() as i64)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    read().unwrap_or_default()
}

pub fn run(
    args: &Value,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let root = PathBuf::from(
        args["root"]
            .as_str()
            .ok_or("Missing inference workspace.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let operation = args["worker_operation"]
        .as_str()
        .ok_or("Missing inference operation.")?;
    let python = match operation {
        "ltx_render" | "json_digest" | "token_count" | "model_check" | "model_doctor"
        | "model_download" => PathBuf::from(
            args["python"]
                .as_str()
                .ok_or("Missing Python model interpreter.")?,
        ),
        "mflux_check" | "mflux_generate" => root.join(".venv-storyboard/bin/python"),
        _ => return Err("Unknown Python inference operation.".into()),
    };
    if !python.is_file() {
        return Err(format!(
            "The model interpreter is unavailable: {}. Retry setup for this model.",
            python.display()
        ));
    }
    let worker = root.join("python/inference_worker.py");
    if !worker.is_file() {
        return Err("The Python inference worker is missing from this workspace.".into());
    }
    let temporary_log = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let log_path = args["log_file"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary_log.path().to_path_buf());
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let log = File::create(&log_path).map_err(|e| e.to_string())?;
    let mut command = Command::new(python);
    command
        .arg("-u")
        .arg(worker)
        .current_dir(&root)
        .env("AIVS_WORKSPACE", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if operation != "model_download" {
        command
            .env("HF_HOME", root.join(".cache/huggingface"))
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("TOKENIZERS_PARALLELISM", "false");
    }
    let mut child = Worker(
        command
            .spawn()
            .map_err(|e| format!("Cannot start Python inference: {e}"))?,
    );
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or("Missing inference input stream.")?;
    serde_json::to_writer(
        &mut input,
        &json!({"version": VERSION, "operation": operation, "args": args["payload"]}),
    )
    .map_err(|e| e.to_string())?;
    input.write_all(b"\n").map_err(|e| e.to_string())?;
    drop(input);
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("Missing inference output stream.")?;
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or("Missing inference error stream.")?;
    let (send, receive) = mpsc::channel();
    let errors_send = send.clone();
    let mut errors_log = log.try_clone().map_err(|e| e.to_string())?;
    let errors_reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    let _ = errors_send.send(Err(e.to_string()));
                    return;
                }
            };
            let _ = writeln!(errors_log, "{line}");
            if errors_send
                .send(Ok(
                    json!({"version": VERSION, "event": {"type": "activity", "message": line}}),
                ))
                .is_err()
            {
                return;
            }
        }
    });
    let reader = thread::spawn(move || {
        let mut log = log;
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    let _ = send.send(Err(e.to_string()));
                    return;
                }
            };
            if let Some(payload) = line.strip_prefix(PREFIX) {
                if send
                    .send(serde_json::from_str::<Value>(payload).map_err(|e| e.to_string()))
                    .is_err()
                {
                    return;
                }
            } else {
                let _ = writeln!(log, "{line}");
                let _ = send.send(Ok(
                    json!({"version": VERSION, "event": {"type": "activity", "message": line}}),
                ));
            }
        }
    });
    let mut result = None;
    let mut failure = None;
    let mut heartbeat = Instant::now();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Python inference cancelled because its parent closed.".into());
        }
        match receive.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(message)) => {
                if message["version"].as_u64() != Some(VERSION) {
                    return Err(
                        "Python inference returned an incompatible protocol version.".into(),
                    );
                }
                if let Some(event) = message.get("event") {
                    emit(event.clone())?;
                } else if let Some(error) = message["error"].as_str() {
                    failure = Some(error.to_owned());
                } else if let Some(value) = message.get("result") {
                    if result.replace(value.clone()).is_some() {
                        return Err("Python inference returned multiple completion records.".into());
                    }
                } else {
                    return Err("Python inference returned an invalid record.".into());
                }
            }
            Ok(Err(error)) => return Err(format!("Cannot read Python inference result: {error}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if heartbeat.elapsed() >= Duration::from_secs(20) {
            emit(
                json!({"type": "activity", "message": if operation.starts_with("mflux_") { "Generating the storyboard image locally" } else { "Running local LTX inference" }}),
            )?;
            heartbeat = Instant::now();
        }
    }
    let _ = reader.join();
    let _ = errors_reader.join();
    // A completion record is valid only once the worker has exited and released MLX.
    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Python inference cancelled because its parent closed.".into());
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        thread::sleep(Duration::from_millis(25));
    };
    if let Some(error) = failure {
        return Err(error);
    }
    if !status.success() {
        return Err(format!(
            "Python inference failed ({status}): {}",
            tail(&log_path)
        ));
    }
    result.ok_or_else(|| {
        format!(
            "Python inference returned no completion record: {}",
            tail(&log_path)
        )
    })
}
