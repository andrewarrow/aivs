use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/python/video.py");
const REQUIREMENTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/python/requirements.txt");
const DEFAULT_PROMPT: &str = include_str!("../python/default-prompt.txt");

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
    Log(String),
    Ready,
    Complete(PathBuf),
    Failed(String),
    Stopped,
}

#[derive(Clone, Copy)]
enum Task {
    Setup,
    Render,
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
                Task::Render => worker.render(),
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
        let mut command = Command::new(self.root.join(".venv/bin/python"));
        command.args(["-u", SCRIPT, action]);
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
        for (program, package) in [("uv", "uv"), ("ffmpeg", "ffmpeg"), ("ffprobe", "ffmpeg")] {
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
        self.progress("Downloading the video model, text encoder, and audio weights");
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

    fn render(&self) -> Result<(), String> {
        let output = self.run(self.python("render"))?;
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
        command
            .current_dir(&self.root)
            .env("AIVS_WORKSPACE", &self.root)
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
                // Signal the process group so Python, uv, and FFmpeg all stop.
                // Give Python time to save its interrupted render report.
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
    setup: Setup,
    job: Option<Job>,
    rendering: bool,
    stopping: bool,
    status: String,
    fraction: Option<f32>,
    logs: VecDeque<String>,
    error: Option<String>,
    output: Option<PathBuf>,
}

impl VideoWorkspace {
    pub fn new(root: PathBuf) -> Self {
        let mut workspace = Self::idle(root);
        workspace.start(Task::Setup);
        workspace
    }

    fn idle(root: PathBuf) -> Self {
        Self {
            root,
            setup: Setup::Checking,
            job: None,
            rendering: false,
            stopping: false,
            status: "Checking local setup…".into(),
            fraction: None,
            logs: VecDeque::new(),
            error: None,
            output: None,
        }
    }

    fn start(&mut self, task: Task) {
        if self.job.is_some() {
            return;
        }
        self.error = None;
        self.logs.clear();
        self.fraction = None;
        self.stopping = false;
        self.rendering = matches!(task, Task::Render);
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
        while let Some(event) = self.job.as_ref().and_then(|job| job.events.try_recv().ok()) {
            let finished = self.handle_event(event);
            if finished {
                self.job = None;
                break;
            }
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
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
            Event::Log(line) => {
                if self.rendering {
                    self.status.clone_from(&line);
                }
                self.logs.push_back(line);
                if self.logs.len() > 250 {
                    self.logs.pop_front();
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
                self.output = Some(path);
                self.status = "Video ready.".into();
                return true;
            }
            Event::Failed(error) => {
                if !self.is_ready() {
                    self.setup = Setup::Failed;
                }
                self.rendering = false;
                self.error = Some(error);
                return true;
            }
            Event::Stopped => {
                self.rendering = false;
                self.status = "Stopped. Partial output is kept in outputs.".into();
                return true;
            }
        }
        false
    }

    pub fn is_ready(&self) -> bool {
        self.setup == Setup::Ready
    }

    pub fn setup_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            if self.setup == Setup::Checking {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking local setup…");
                });
            }
        });
        // A ready install goes directly from the startup check to its workspace.
        if self.setup == Setup::Checking {
            return;
        }
        let ctx = ui.ctx().clone();
        let width = (ctx.content_rect().width() - 64.0).clamp(240.0, 480.0);
        egui::Modal::new(egui::Id::new("video_setup")).show(&ctx, |ui| {
            ui.set_width(width);
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 80.0).max(180.0))
                .show(ui, |ui| {
                    ui.heading("Set up AIVS");
                    ui.add_space(8.0);
                    if self.setup == Setup::NeedsToken {
                        ui.label("Get an access token from Hugging Face, then create a .env file in this workspace with HF_API set to your token.");
                        ui.hyperlink_to("Get a Hugging Face access token", "https://huggingface.co/settings/tokens");
                        ui.label(egui::RichText::new("HF_API=your_token").monospace());
                        ui.add(egui::Label::new(format!("File: {}", self.root.join(".env").display())).wrap());
                        if ui.add(egui::Button::new("Check again").min_size(egui::vec2(100.0, 44.0))).clicked() {
                            self.start(Task::Setup);
                        }
                    } else {
                        ui.label("Python, the Apple GPU renderer, and the model cache must be ready before you can continue.");
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(&self.status).wrap());
                        if self.job.is_some() {
                            ui.add(egui::ProgressBar::new(self.fraction.unwrap_or(0.0))
                                .animate(self.fraction.is_none())
                                .desired_width(width));
                            ui.label("First setup downloads about 43 GB and needs 50 GiB free. Keep AIVS open.");
                        }
                        self.error_ui(ui);
                        self.activity_ui(ui, 120.0);
                        if self.setup == Setup::Failed
                            && ui.add(egui::Button::new("Retry setup").min_size(egui::vec2(100.0, 44.0))).clicked()
                        {
                            self.start(Task::Setup);
                        }
                    }
                });
            // Escape and backdrop clicks deliberately do not unlock setup.
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(760.0));
                ui.heading("Talking video");
                ui.add(
                    egui::Label::new("LTX-2.5 · 1280 × 704 · 24 fps · generated speech and video")
                        .wrap(),
                );
                ui.add_space(16.0);
                ui.strong("Prompt");
                ui.add(
                    egui::Label::new(DEFAULT_PROMPT.trim())
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(16.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.job.is_none(),
                            egui::Button::new("Generate video").min_size(egui::vec2(128.0, 44.0)),
                        )
                        .clicked()
                    {
                        self.start(Task::Render);
                    }
                    if self.rendering
                        && ui
                            .add_enabled(
                                !self.stopping,
                                egui::Button::new("Stop").min_size(egui::vec2(64.0, 44.0)),
                            )
                            .clicked()
                    {
                        if let Some(job) = &self.job {
                            job.stop();
                        }
                        self.stopping = true;
                    }
                    if let Some(path) = self.output.clone() {
                        if ui
                            .add(egui::Button::new("Open video").min_size(egui::vec2(96.0, 44.0)))
                            .clicked()
                        {
                            self.open(&path);
                        }
                        if ui
                            .add(egui::Button::new("Show files").min_size(egui::vec2(96.0, 44.0)))
                            .clicked()
                            && let Some(parent) = path.parent()
                        {
                            self.open(parent);
                        }
                    }
                });
                ui.add_space(8.0);
                if self.rendering {
                    ui.add(egui::ProgressBar::new(0.0).animate(true));
                }
                if self.stopping && self.rendering {
                    ui.label("Stopping…");
                } else if !self.status.is_empty() {
                    ui.add(egui::Label::new(&self.status).wrap());
                }
                self.error_ui(ui);
                if let Some(path) = &self.output {
                    ui.add(
                        egui::Label::new(path.display().to_string())
                            .wrap()
                            .selectable(true),
                    );
                }
                self.activity_ui(ui, 240.0);
            });
    }

    fn error_ui(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.add(egui::Label::new(format!("Error: {error}")).wrap());
        }
    }

    fn activity_ui(&self, ui: &mut egui::Ui, max_height: f32) {
        if !self.logs.is_empty() {
            ui.collapsing("Activity", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("video_activity")
                    .max_height(max_height)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.logs {
                            ui.add(egui::Label::new(egui::RichText::new(line).monospace()).wrap());
                        }
                    });
            });
        }
    }

    fn open(&mut self, path: &Path) {
        if let Err(error) = Command::new("open").arg(path).spawn() {
            self.error = Some(format!("Could not open output: {error}"));
        }
    }

    #[cfg(test)]
    pub(crate) fn ready_for_test(root: PathBuf) -> Self {
        let mut workspace = Self::idle(root);
        workspace.setup = Setup::Ready;
        workspace
    }
}

#[cfg(test)]
mod tests;
