use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod ui;

const REQUIREMENTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/python/requirements.txt");
const VOICE_SETUP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/experiments/character-consistency/setup_voice_conversion.py"
);

#[derive(Debug, PartialEq)]
enum Setup {
    Checking,
    NeedsToken,
    Installing,
    Failed,
    Ready,
}

enum Event {
    Installing,
    NeedsToken,
    Progress(String, Option<f32>),
    RenderProgress {
        label: String,
        fraction: f32,
        remaining_seconds: Option<f64>,
    },
    Log(String),
    LogDelta {
        id: String,
        delta: String,
    },
    Ready,
    Complete(PathBuf),
    Failed(String),
    Stopped,
}

enum Task {
    Setup,
    Render(String),
    Resume(PathBuf),
    Previsualize(PathBuf),
    FinalRender(PathBuf),
}

struct Job {
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    fn start(root: PathBuf, task: Task) -> Self {
        let (sender, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let thread = thread::spawn(move || {
            let worker = Worker {
                root,
                sender,
                cancel: worker_cancel,
            };
            let result = match task {
                Task::Setup => worker.setup(),
                Task::Render(request) => worker.render(&request),
                Task::Resume(directory) => worker.resume(&directory, false),
                Task::Previsualize(directory) => worker.previsualize(&directory),
                Task::FinalRender(directory) => worker.resume(&directory, true),
            };
            if let Err(error) = result {
                worker.send(if worker.cancel.load(Ordering::Relaxed) {
                    Event::Stopped
                } else {
                    Event::Failed(error)
                });
            }
        });
        Self {
            events,
            cancel,
            thread: Some(thread),
        }
    }

    fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    root: PathBuf,
    sender: Sender<Event>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Output {
    ready: bool,
    needs_token: bool,
    output: Option<PathBuf>,
    error: Option<String>,
}

impl Worker {
    fn send(&self, event: Event) {
        let _ = self.sender.send(event);
    }

    fn progress(&self, label: &str) {
        self.send(Event::Progress(label.into(), None));
    }

    fn python(&self, action: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current AIVS executable"));
        command.args(["--production", action]);
        if matches!(action, "check" | "download") {
            command.args(["--profile", "hq"]);
        }
        command
    }

    fn setup(&self) -> Result<(), String> {
        if !has_token(&self.root) {
            self.send(Event::NeedsToken);
            return Ok(());
        }
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err("LTX-2.5 requires an Apple Silicon Mac with Metal support.".into());
        }
        self.progress("Checking local Python environment, GPU, and model cache");
        if self.root.join(".venv/bin/python").is_file()
            && let Ok(output) = self.run(self.python("check"))
        {
            if output.needs_token {
                self.send(Event::NeedsToken);
                return Ok(());
            }
            if output.ready {
                self.send(Event::Ready);
                return Ok(());
            }
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err("Stopped".into());
        }
        self.send(Event::Installing);
        for (program, package) in [
            ("uv", "uv"),
            ("ffmpeg", "ffmpeg"),
            ("ffprobe", "ffmpeg"),
            ("git", "git"),
        ] {
            if !available(program) {
                if !available("brew") {
                    return Err(format!(
                        "Install Homebrew and run ‘brew install {package}’, then choose Retry setup."
                    ));
                }
                self.progress(&format!("Installing {package} with Homebrew"));
                let mut command = Command::new("brew");
                command.args(["install", package]);
                self.run(command)?;
            }
        }
        self.progress("Preparing Python 3.12 in .venv");
        let mut command = Command::new("uv");
        command.args(["venv", "--python", "3.12", "--allow-existing", ".venv"]);
        self.run(command)?;
        self.progress("Installing the pinned MLX video packages");
        let mut command = Command::new("uv");
        command.args(["pip", "sync", "--python", ".venv/bin/python", REQUIREMENTS]);
        self.run(command)?;
        self.progress("Preparing saved-character voices in the separate Seed-VC environment");
        let mut command = Command::new(self.root.join(".venv/bin/python"));
        command.args(["-u", VOICE_SETUP]);
        self.run(command)?;
        self.progress("Downloading the distilled and HQ video models");
        let output = self.run(self.python("download"))?;
        if output.needs_token {
            self.send(Event::NeedsToken);
        } else if output.ready {
            self.send(Event::Ready);
        } else {
            return Err("Setup did not pass its final readiness checks. Retry setup.".into());
        }
        Ok(())
    }

    fn render(&self, request: &str) -> Result<(), String> {
        self.progress("Preparing shooting script…");
        let value: serde_json::Value = serde_json::from_str(request)
            .map_err(|error| format!("Cannot read screenplay request: {error}"))?;
        let profile = value["render_profile"].as_str().unwrap_or("hq");
        let mut request_file = tempfile::NamedTempFile::new()
            .map_err(|error| format!("Cannot create screenplay request file: {error}"))?;
        request_file
            .write_all(request.as_bytes())
            .map_err(|error| format!("Cannot write screenplay request file: {error}"))?;
        let mut command = self.python("render");
        command.arg("--screenplay-file").arg(request_file.path());
        command.args(["--profile", profile]);
        let output = self.run(command)?;
        if let Some(path) = output.output.filter(|path| path.is_file()) {
            self.send(Event::Complete(path));
            Ok(())
        } else {
            Err("The renderer stopped without a validated video. See activity for details.".into())
        }
    }

    fn previsualize(&self, directory: &Path) -> Result<(), String> {
        self.progress("Preparing motion keyframes and animatic…");
        let mut command = self.python("previsualize");
        command.arg("--output-dir").arg(directory);
        let output = self.run(command)?;
        if let Some(path) = output.output.filter(|path| path.is_file()) {
            self.send(Event::Complete(path));
            Ok(())
        } else {
            Err("Previsualization stopped without an animatic. Saved keyframes are kept for Resume.".into())
        }
    }

    fn resume(&self, directory: &Path, motion_previs: bool) -> Result<(), String> {
        self.progress("Resuming video…");
        let mut planning_budget = crate::settings::load_planning_budget(&self.root);
        for (filename, key) in [
            ("screenplay-request.json", "planning_budget_seconds"),
            ("planning-budget.json", "limit_seconds"),
        ] {
            if let Some(saved) = std::fs::read(directory.join(filename))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| value[key].as_u64())
            {
                planning_budget = planning_budget.max(saved);
            }
        }
        let mut command = self.python("resume");
        command.arg("--output-dir").arg(directory);
        if motion_previs {
            command.arg("--motion-previs");
        }
        command
            .arg("--planning-budget-seconds")
            .arg(planning_budget.to_string());
        let output = self.run(command)?;
        if let Some(path) = output.output.filter(|path| path.is_file()) {
            self.send(Event::Complete(path));
            Ok(())
        } else {
            Err("The renderer stopped without a validated video. See activity for details.".into())
        }
    }

    fn run(&self, mut command: Command) -> Result<Output, String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("Stopped".into());
        }
        if let Ok(executable) = std::env::current_exe() {
            command.env("AIVS_ENGINE_PATH", executable);
        }
        command
            .current_dir(&self.root)
            .env("AIVS_WORKSPACE", &self.root)
            .env("AIVS_CODEX_PATH", crate::codex::codex_executable())
            .env("HF_HOME", self.root.join(".cache/huggingface"))
            .env("PYTHONUNBUFFERED", "1")
            .env("NO_COLOR", "1")
            .env("PATH", executable_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start setup/renderer: {error}"))?;
        let stdout = read_output(child.stdout.take().unwrap(), self.sender.clone(), true);
        let stderr = read_output(child.stderr.take().unwrap(), self.sender.clone(), false);
        let status = loop {
            if self.cancel.load(Ordering::Relaxed) {
                // Signal the group, including model workers and FFmpeg.
                // Give Rust time to save its interrupted production report.
                #[cfg(unix)]
                let _ = Command::new("/bin/kill")
                    .args(["-INT", "--", &format!("-{}", child.id())])
                    .status();
                for _ in 0..40 {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                #[cfg(unix)]
                let _ = Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
            }
        };
        let output = stdout
            .join()
            .map_err(|_| "The renderer output reader stopped.".to_owned())?;
        let _ = stderr.join();
        if self.cancel.load(Ordering::Relaxed) {
            return Err("Stopped".into());
        }
        let status = status.map_err(|error| error.to_string())?;
        if output.needs_token || status.success() {
            Ok(output)
        } else {
            Err(output.error.unwrap_or_else(|| {
                format!("Setup/renderer exited with {status}. See activity for details.")
            }))
        }
    }
}

fn executable_path() -> std::ffi::OsString {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    paths.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    std::env::join_paths(paths).unwrap_or_default()
}

fn available(program: &str) -> bool {
    Command::new(program)
        .arg(if matches!(program, "ffmpeg" | "ffprobe") {
            "-version"
        } else {
            "--version"
        })
        .env("PATH", executable_path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn has_token(root: &Path) -> bool {
    // Bootstrap check only; python-dotenv performs the authoritative parse once
    // installed. Credentials never enter command arguments, UI, or activity logs.
    std::fs::read_to_string(root.join(".env")).is_ok_and(|contents| {
        contents
            .lines()
            .filter_map(|line| {
                let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
                let (key, value) = line.split_once('=')?;
                (key.trim() == "HF_API").then_some(value.trim())
            })
            .next_back()
            .is_some_and(|value| {
                let value = match value.chars().next() {
                    Some(quote @ ('\'' | '"')) => {
                        value[1..].split_once(quote).map_or("", |(value, _)| value)
                    }
                    _ => value.split(" #").next().unwrap_or_default(),
                };
                !value.trim().is_empty() && !value.starts_with('#')
            })
    })
}

fn read_output(
    reader: impl Read + Send + 'static,
    sender: Sender<Event>,
    protocol: bool,
) -> JoinHandle<Output> {
    thread::spawn(move || {
        let mut output = Output::default();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if protocol
                && let Some(json) = line.strip_prefix("AIVS_EVENT ")
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(json)
            {
                match value["event"].as_str().unwrap_or_default() {
                    "progress" => {
                        let label = value["label"].as_str().unwrap_or_default().to_owned();
                        let fraction = value["fraction"]
                            .as_f64()
                            .map(|value| value.clamp(0.0, 1.0) as f32);
                        let _ = sender.send(Event::Progress(label, fraction));
                    }
                    "render_progress" => {
                        if let Some(fraction) =
                            value["fraction"].as_f64().filter(|value| value.is_finite())
                        {
                            let _ = sender.send(Event::RenderProgress {
                                label: value["label"].as_str().unwrap_or_default().into(),
                                fraction: fraction.clamp(0.0, 0.99) as f32,
                                remaining_seconds: value["remaining_seconds"]
                                    .as_f64()
                                    .filter(|value| value.is_finite() && *value >= 0.0),
                            });
                        }
                    }
                    "activity" => {
                        if let Some(message) = value["message"].as_str() {
                            let _ = sender.send(Event::Log(message.to_owned()));
                        }
                    }
                    "activity_delta" => {
                        if let (Some(id), Some(delta)) =
                            (value["id"].as_str(), value["delta"].as_str())
                        {
                            let _ = sender.send(Event::LogDelta {
                                id: id.to_owned(),
                                delta: delta.to_owned(),
                            });
                        }
                    }
                    "ready" => output.ready = true,
                    "needs_token" => output.needs_token = true,
                    "complete" => output.output = value["output"].as_str().map(PathBuf::from),
                    "error" => output.error = value["message"].as_str().map(str::to_owned),
                    _ => {}
                }
            } else if !line.trim().is_empty() {
                let _ = sender.send(Event::Log(line));
            }
        }
        output
    })
}

pub struct VideoWorkspace {
    root: PathBuf,
    pub render_format: crate::settings::RenderFormat,
    pub render_profile: crate::settings::RenderProfile,
    pub planning_budget_seconds: u64,
    setup: Setup,
    job: Option<Job>,
    selected_scene: Option<u64>,
    rendering: bool,
    stopping: bool,
    status: String,
    fraction: Option<f32>,
    remaining_seconds: Option<f64>,
    started_at: Instant,
    logs: VecDeque<String>,
    current_log_stream: Option<String>,
    error: Option<String>,
    output: Option<PathBuf>,
    output_aspect: f32,
    failure_alert: Option<String>,
    reveal_output: bool,
    pub playback_target: Option<crate::playback::PlaybackTarget>,
    pub playback_error: Option<String>,
}

impl VideoWorkspace {
    pub fn new(root: PathBuf) -> Self {
        let mut workspace = Self::idle(root);
        workspace.start(Task::Setup);
        workspace
    }

    fn idle(root: PathBuf) -> Self {
        Self {
            render_format: crate::settings::RenderFormat::load(&root),
            render_profile: crate::settings::RenderProfile::load(&root),
            planning_budget_seconds: crate::settings::load_planning_budget(&root),
            root,
            setup: Setup::Checking,
            job: None,
            selected_scene: None,
            rendering: false,
            stopping: false,
            status: "Checking local setup…".into(),
            fraction: None,
            remaining_seconds: None,
            started_at: Instant::now(),
            logs: VecDeque::new(),
            current_log_stream: None,
            error: None,
            output: None,
            output_aspect: 16.0 / 9.0,
            failure_alert: None,
            reveal_output: false,
            playback_target: None,
            playback_error: None,
        }
    }

    fn start(&mut self, task: Task) {
        if self.job.is_some() {
            return;
        }
        self.error = None;
        self.failure_alert = None;
        self.logs.clear();
        self.current_log_stream = None;
        self.fraction = None;
        self.remaining_seconds = None;
        self.started_at = Instant::now();
        self.stopping = false;
        self.rendering = matches!(task, Task::Render(_) | Task::Resume(_));
        if !self.rendering {
            self.setup = Setup::Checking;
        }
        self.status = if self.rendering {
            "Starting renderer…"
        } else {
            "Checking local setup…"
        }
        .into();
        self.job = Some(Job::start(self.root.clone(), task));
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Some(job) = &self.job {
            let event = match job.events.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if job.cancel.load(Ordering::Relaxed) {
                        Event::Stopped
                    } else {
                        Event::Failed(
                            "The video worker stopped unexpectedly. See Activity for details."
                                .into(),
                        )
                    }
                }
            };
            let finished = self.handle_event(event);
            ctx.request_repaint();
            if finished {
                self.job = None;
                break;
            }
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    pub fn has_job(&self) -> bool {
        self.job.is_some()
    }

    pub fn is_rendering(&self) -> bool {
        self.rendering
    }

    pub fn take_failure_alert(&mut self) -> Option<String> {
        self.failure_alert.take()
    }

    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::Installing => self.setup = Setup::Installing,
            Event::NeedsToken => {
                self.setup = Setup::NeedsToken;
                return true;
            }
            Event::Progress(label, fraction) => {
                self.status = label;
                self.fraction = fraction;
            }
            Event::RenderProgress {
                label,
                fraction,
                remaining_seconds,
            } => {
                self.status = label;
                self.fraction = Some(self.fraction.unwrap_or(0.0).max(fraction));
                self.remaining_seconds = remaining_seconds;
            }
            Event::Log(line) => {
                self.current_log_stream = None;
                self.logs.push_back(line);
                if self.logs.len() > 250 {
                    self.logs.pop_front();
                }
            }
            Event::LogDelta { id, delta } => {
                if self.current_log_stream.as_deref() == Some(id.as_str()) {
                    if let Some(line) = self.logs.back_mut() {
                        line.push_str(&delta);
                    }
                } else {
                    self.logs.push_back(format!("Codex: {delta}"));
                    self.current_log_stream = Some(id);
                    if self.logs.len() > 250 {
                        self.logs.pop_front();
                    }
                }
            }
            Event::Ready => {
                self.setup = Setup::Ready;
                self.status.clear();
                self.error = None;
                return true;
            }
            Event::Complete(path) => {
                self.rendering = false;
                self.fraction = Some(1.0);
                self.remaining_seconds = None;
                self.playback_error = None;
                let animatic = path.file_name().is_some_and(|name| name == "animatic.mp4");
                let report = path
                    .parent()
                    .and_then(|directory| {
                        if animatic {
                            directory.parent()
                        } else {
                            Some(directory)
                        }
                    })
                    .and_then(|directory| std::fs::read(directory.join("render.json")).ok())
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                self.output_aspect = report
                    .as_ref()
                    .and_then(|report| {
                        let width = report["width"].as_u64()?;
                        let height = report["height"].as_u64()?;
                        (width > 0 && height > 0).then_some(width as f32 / height as f32)
                    })
                    .unwrap_or(16.0 / 9.0);
                self.status = if animatic {
                    "Animatic ready. Final Render uses the accepted keyframes."
                } else {
                    match report.as_ref().and_then(|report| report["status"].as_str()) {
                        Some("complete") => "Video ready.",
                        Some("rendered-with-issues") => {
                            "Video rendered; visual issues need review."
                        }
                        _ => "Video rendered; visual review is incomplete.",
                    }
                }
                .into();
                self.output = Some(path);
                self.reveal_output = true;
                return true;
            }
            Event::Failed(error) => {
                if self.rendering && !self.stopping {
                    self.failure_alert = Some(error.clone());
                }
                if !self.is_ready() {
                    self.setup = Setup::Failed;
                }
                self.status = if self.rendering {
                    "Video generation failed."
                } else {
                    "Setup failed."
                }
                .into();
                self.rendering = false;
                self.remaining_seconds = None;
                self.error = Some(error);
                return true;
            }
            Event::Stopped => {
                self.rendering = false;
                self.status = if self.is_ready() {
                    "Stopped. Partial output is kept in outputs."
                } else {
                    self.setup = Setup::Failed;
                    "Setup stopped. Choose Retry setup to check again."
                }
                .into();
                return true;
            }
        }
        false
    }

    pub fn is_ready(&self) -> bool {
        self.setup == Setup::Ready
    }

    pub fn can_resume(&self) -> bool {
        self.is_ready() && self.job.is_none()
    }

    pub fn resume(&mut self, directory: PathBuf) {
        if self.can_resume() {
            self.start(Task::Resume(directory));
        }
    }

    pub fn previsualize(&mut self, directory: PathBuf) {
        if self.can_resume() {
            self.start(Task::Previsualize(directory));
        }
    }

    pub fn final_render(&mut self, directory: PathBuf) {
        if self.can_resume() {
            self.start(Task::FinalRender(directory));
        }
    }

    fn render_request(
        &self,
        screenplay: &crate::screenplay::Screenplay,
        scene_id: u64,
    ) -> Result<String, String> {
        let request = screenplay.render_request(scene_id)?;
        let mut value: serde_json::Value =
            serde_json::from_str(&request).map_err(|error| error.to_string())?;
        value["render_format"] = self.render_format.id().into();
        value["render_profile"] = self.render_profile.id().into();
        value["planning_budget_seconds"] = self.planning_budget_seconds.into();
        serde_json::to_string(&value).map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) fn output_for_test(&mut self, path: PathBuf) {
        self.handle_event(Event::Complete(path));
    }

    #[cfg(test)]
    pub(crate) fn ready_for_test(root: PathBuf) -> Self {
        let mut workspace = Self::idle(root);
        workspace.setup = Setup::Ready;
        workspace
    }
}

fn remaining_time(seconds: f64) -> String {
    let seconds = seconds.ceil() as u64;
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests;
