use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

mod messages;

use messages::handle_app_server_message;

const CODEX_PATH_ENV: &str = "AIVS_CODEX_PATH";
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(20);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGptAccount {
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexWorkStatus {
    Thinking,
    Editing,
    Checking,
    Working,
}

#[derive(Debug)]
pub enum CodexEvent {
    AccountStatus(Option<ChatGptAccount>),
    BrowserOpened,
    LoginCompleted(ChatGptAccount),
    ChatReady,
    WorkStatus(CodexWorkStatus),
    ActivityDelta(String),
    AssistantDelta(String),
    AssistantMessage(String),
    ChatTurnCompleted,
    ChatTurnCancelled,
    CancelError(String),
    ChatError(String),
    Error(String),
    Unavailable(String),
}

pub(crate) enum CodexCommand {
    BeginChatGptLogin,
    OpenChat,
    SendChatMessage {
        message: String,
        model: String,
        reasoning_effort: String,
    },
    CancelChatMessage,
    Shutdown,
}

enum AppServerOutput {
    Message(Value),
    Invalid(String),
    Closed,
}

#[derive(Clone, Copy)]
enum PendingRequest {
    Initialize,
    Account { completes_login: bool },
    Login,
    ThreadStart,
    TurnStart,
    TurnInterrupt,
}

struct ProtocolState {
    stdin: ChildStdin,
    next_id: u64,
    pending: BTreeMap<u64, PendingRequest>,
    project_root: PathBuf,
    network_access: bool,
    initialized: bool,
    queued_login: bool,
    login_active: bool,
    chat_requested: bool,
    queued_messages: Vec<QueuedChatMessage>,
    thread_id: Option<String>,
    thread_start_pending: bool,
    turn_active: bool,
    turn_id: Option<String>,
    turn_interrupt_pending: bool,
    cancel_requested: bool,
}

#[derive(Clone)]
struct QueuedChatMessage {
    message: String,
    model: String,
    reasoning_effort: String,
}

impl ProtocolState {
    fn new(stdin: ChildStdin, project_root: PathBuf) -> Self {
        Self {
            stdin,
            next_id: 1,
            pending: BTreeMap::new(),
            project_root,
            network_access: false,
            initialized: false,
            queued_login: false,
            login_active: false,
            chat_requested: false,
            queued_messages: Vec::new(),
            thread_id: None,
            thread_start_pending: false,
            turn_active: false,
            turn_id: None,
            turn_interrupt_pending: false,
            cancel_requested: false,
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        pending: PendingRequest,
    ) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(
            &mut self.stdin,
            &json!({ "method": method, "id": id, "params": params }),
        )?;
        self.pending.insert(id, pending);
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        write_message(
            &mut self.stdin,
            &json!({ "method": method, "params": params }),
        )
    }

    fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "aivs",
                    "title": "AIVS",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
            PendingRequest::Initialize,
        )
    }

    fn read_account(&mut self, completes_login: bool) -> Result<(), String> {
        self.request(
            "account/read",
            json!({ "refreshToken": false }),
            PendingRequest::Account { completes_login },
        )
    }

    fn begin_login(&mut self) -> Result<(), String> {
        if self.login_active {
            return Ok(());
        }
        self.login_active = true;
        if let Err(error) = self.request(
            "account/login/start",
            json!({
                "type": "chatgpt",
                "useHostedLoginSuccessPage": true,
                "appBrand": "chatgpt",
            }),
            PendingRequest::Login,
        ) {
            self.login_active = false;
            return Err(error);
        }
        Ok(())
    }

    fn open_chat(&mut self) -> Result<(), String> {
        self.chat_requested = true;
        self.ensure_thread()
    }

    fn queue_chat_message(
        &mut self,
        message: String,
        model: String,
        reasoning_effort: String,
    ) -> Result<(), String> {
        self.chat_requested = true;
        self.queued_messages.push(QueuedChatMessage {
            message,
            model,
            reasoning_effort,
        });
        self.ensure_thread()?;
        self.start_queued_turn()
    }

    fn ensure_thread(&mut self) -> Result<(), String> {
        if !self.chat_requested
            || !self.initialized
            || self.thread_id.is_some()
            || self.thread_start_pending
        {
            return Ok(());
        }
        self.thread_start_pending = true;
        let cwd = self.project_root.to_string_lossy().into_owned();
        if let Err(error) = self.request(
            "thread/start",
            json!({
                "cwd": cwd,
                "approvalPolicy": "never",
                "sandbox": "workspace-write",
                "serviceName": "aivs",
            }),
            PendingRequest::ThreadStart,
        ) {
            self.thread_start_pending = false;
            return Err(error);
        }
        Ok(())
    }

    fn start_queued_turn(&mut self) -> Result<(), String> {
        if !self.initialized || self.turn_active {
            return Ok(());
        }
        let Some(thread_id) = self.thread_id.clone() else {
            return Ok(());
        };
        let Some(message) = self.queued_messages.first().cloned() else {
            return Ok(());
        };
        self.turn_active = true;
        self.turn_id = None;
        self.turn_interrupt_pending = false;
        self.cancel_requested = false;
        let cwd = self.project_root.to_string_lossy().into_owned();
        if let Err(error) = self.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": message.message }],
                "model": (!message.model.is_empty()).then_some(message.model),
                "effort": (!message.reasoning_effort.is_empty()).then_some(message.reasoning_effort),
                "cwd": cwd,
                "approvalPolicy": "never",
                "sandboxPolicy": {
                    "type": "workspaceWrite",
                    "writableRoots": [cwd],
                    "networkAccess": self.network_access,
                },
                "summary": "concise",
            }),
            PendingRequest::TurnStart,
        ) {
            self.turn_active = false;
            return Err(error);
        }
        self.queued_messages.remove(0);
        Ok(())
    }

    fn interrupt_turn(&mut self) -> Result<(), String> {
        if !self.turn_active || self.turn_interrupt_pending {
            return Ok(());
        }
        let (Some(thread_id), Some(turn_id)) = (self.thread_id.clone(), self.turn_id.clone())
        else {
            self.cancel_requested = true;
            return Ok(());
        };
        self.turn_interrupt_pending = true;
        if let Err(error) = self.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
            PendingRequest::TurnInterrupt,
        ) {
            self.turn_interrupt_pending = false;
            return Err(error);
        }
        Ok(())
    }
}

pub struct CodexClient {
    commands: Sender<CodexCommand>,
    events: Receiver<CodexEvent>,
    worker: Option<thread::JoinHandle<()>>,
}

impl CodexClient {
    #[cfg(test)]
    pub(crate) fn test_channels() -> (Self, Receiver<CodexCommand>, Sender<CodexEvent>) {
        let (commands, requests) = mpsc::channel();
        let (responses, events) = mpsc::channel();
        (
            Self {
                commands,
                events,
                worker: None,
            },
            requests,
            responses,
        )
    }

    pub fn new(project_root: &Path, network_access: bool) -> Result<Self, String> {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let project_root = project_root.to_owned();
        let worker = thread::Builder::new()
            .name("aivs-codex".to_owned())
            .spawn(move || {
                run_worker(
                    &project_root,
                    command_receiver,
                    event_sender,
                    network_access,
                )
            })
            .map_err(|error| format!("could not start the ChatGPT connection worker: {error}"))?;

        Ok(Self {
            commands: command_sender,
            events: event_receiver,
            worker: Some(worker),
        })
    }

    pub fn begin_chatgpt_login(&self) -> Result<(), String> {
        self.commands
            .send(CodexCommand::BeginChatGptLogin)
            .map_err(|_| "ChatGPT connection is unavailable. Reconnect and try again.".to_owned())
    }

    pub fn open_chat(&self) -> Result<(), String> {
        self.commands
            .send(CodexCommand::OpenChat)
            .map_err(|_| "Codex chat is unavailable. Reconnect and try again.".to_owned())
    }

    pub fn send_chat_message(
        &self,
        message: String,
        model: String,
        reasoning_effort: String,
    ) -> Result<(), String> {
        self.commands
            .send(CodexCommand::SendChatMessage {
                message,
                model,
                reasoning_effort,
            })
            .map_err(|_| "Codex chat is unavailable. Reconnect and try again.".to_owned())
    }

    pub fn cancel_chat_message(&self) -> Result<(), String> {
        self.commands
            .send(CodexCommand::CancelChatMessage)
            .map_err(|_| "Codex chat is unavailable. Reconnect and try again.".to_owned())
    }

    pub fn try_recv(&self) -> Option<CodexEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for CodexClient {
    fn drop(&mut self) {
        let _ = self.commands.send(CodexCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    project_root: &Path,
    commands: Receiver<CodexCommand>,
    events: Sender<CodexEvent>,
    network_access: bool,
) {
    run_worker_with_executable(
        project_root,
        commands,
        events,
        codex_executable(),
        network_access,
    );
}

fn run_worker_with_executable(
    project_root: &Path,
    commands: Receiver<CodexCommand>,
    events: Sender<CodexEvent>,
    executable: PathBuf,
    network_access: bool,
) {
    let mut child = match start_app_server_at(project_root, &executable) {
        Ok(child) => child,
        Err(message) => {
            let _ = events.send(CodexEvent::Unavailable(message));
            return;
        }
    };
    let Some(stdin) = child.stdin.take() else {
        let _ = events.send(CodexEvent::Unavailable(
            "Codex App Server did not provide an input stream.".to_owned(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = events.send(CodexEvent::Unavailable(
            "Codex App Server did not provide an output stream.".to_owned(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        return;
    };

    let (output_sender, output_receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("aivs-codex-output".to_owned())
        .spawn(move || read_app_server_output(stdout, output_sender));
    if let Err(error) = reader {
        let _ = events.send(CodexEvent::Unavailable(format!(
            "Could not read Codex App Server output: {error}"
        )));
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    let mut protocol = ProtocolState::new(stdin, project_root.to_owned());
    protocol.network_access = network_access;
    if let Err(message) = protocol.initialize() {
        let _ = events.send(CodexEvent::Unavailable(message));
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    let started = Instant::now();
    loop {
        if !protocol.initialized && started.elapsed() > INITIALIZE_TIMEOUT {
            let _ = events.send(CodexEvent::Unavailable(
                "Codex did not initialize within 20 seconds. Reconnect and try again.".to_owned(),
            ));
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        loop {
            match commands.try_recv() {
                Ok(CodexCommand::BeginChatGptLogin) if protocol.initialized => {
                    if let Err(message) = protocol.begin_login() {
                        let _ = events.send(CodexEvent::Error(message));
                    }
                }
                Ok(CodexCommand::BeginChatGptLogin) => protocol.queued_login = true,
                Ok(CodexCommand::OpenChat) if protocol.initialized => {
                    if let Err(message) = protocol.open_chat() {
                        let _ = events.send(CodexEvent::ChatError(message));
                    }
                }
                Ok(CodexCommand::OpenChat) => protocol.chat_requested = true,
                Ok(CodexCommand::SendChatMessage {
                    message,
                    model,
                    reasoning_effort,
                }) if protocol.initialized => {
                    if let Err(message) =
                        protocol.queue_chat_message(message, model, reasoning_effort)
                    {
                        let _ = events.send(CodexEvent::ChatError(message));
                    }
                }
                Ok(CodexCommand::SendChatMessage {
                    message,
                    model,
                    reasoning_effort,
                }) => {
                    protocol.chat_requested = true;
                    protocol.queued_messages.push(QueuedChatMessage {
                        message,
                        model,
                        reasoning_effort,
                    });
                }
                Ok(CodexCommand::CancelChatMessage) if protocol.initialized => {
                    if let Err(message) = protocol.interrupt_turn() {
                        let _ = events.send(CodexEvent::CancelError(message));
                    }
                }
                Ok(CodexCommand::CancelChatMessage) => {}
                Ok(CodexCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        match output_receiver.recv_timeout(WORKER_POLL_INTERVAL) {
            Ok(AppServerOutput::Message(message)) => {
                if let Err(message) = handle_app_server_message(&mut protocol, message, &events) {
                    protocol.login_active = false;
                    if protocol.initialized {
                        let _ = events.send(CodexEvent::Error(message));
                    } else {
                        let _ = events.send(CodexEvent::Unavailable(message));
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                }
            }
            Ok(AppServerOutput::Invalid(message)) => {
                log::warn!("Codex App Server returned invalid JSON: {message}");
            }
            Ok(AppServerOutput::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let detail = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|status| format!(" ({status})"))
                    .unwrap_or_default();
                let _ = events.send(CodexEvent::Unavailable(format!(
                    "Codex App Server stopped{detail}. Reconnect and try again."
                )));
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn start_app_server_at(project_root: &Path, executable: &Path) -> Result<Child, String> {
    let mut command = ProcessCommand::new(executable);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(directory) = codex_bootstrap_directory(project_root) {
        command.current_dir(directory);
    }
    command.spawn().map_err(|error| {
        format!(
            "Could not start Codex App Server from {}: {error}. Bundle Codex with AIVS or set {CODEX_PATH_ENV}.",
            executable.display()
        )
    })
}

fn codex_bootstrap_directory(project_root: &Path) -> Option<PathBuf> {
    let launch_directory = env::current_dir().ok();
    codex_bootstrap_directory_from(project_root, launch_directory.as_deref())
}

fn codex_bootstrap_directory_from(
    project_root: &Path,
    launch_directory: Option<&Path>,
) -> Option<PathBuf> {
    find_codex_root(project_root)
        .or_else(|| launch_directory.and_then(find_codex_root))
        .or_else(|| project_root.is_dir().then(|| project_root.to_path_buf()))
}

fn find_codex_root(start: &Path) -> Option<PathBuf> {
    let mut directory = start.to_path_buf();
    loop {
        if directory.join(".codex-root").is_file() {
            return Some(directory);
        }
        let parent = directory.parent()?.to_path_buf();
        if parent == directory {
            return None;
        }
        directory = parent;
    }
}

pub(crate) fn codex_executable() -> PathBuf {
    if let Some(path) = env::var_os(CODEX_PATH_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    if let Ok(current_executable) = env::current_exe()
        && let Some(executable_dir) = current_executable.parent()
    {
        let filename = if cfg!(target_os = "windows") {
            "codex.exe"
        } else {
            "codex"
        };
        let sibling = executable_dir.join(filename);
        if sibling.is_file() {
            return sibling;
        }
        if let Some(contents_dir) = executable_dir.parent() {
            let macos_resource = contents_dir.join("Resources").join(filename);
            if macos_resource.is_file() {
                return macos_resource;
            }
        }
    }

    PathBuf::from(if cfg!(target_os = "windows") {
        "codex.exe"
    } else {
        "codex"
    })
}

fn read_app_server_output(stdout: std::process::ChildStdout, output: Sender<AppServerOutput>) {
    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => match serde_json::from_str(&line) {
                Ok(message) => {
                    if output.send(AppServerOutput::Message(message)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = output.send(AppServerOutput::Invalid(error.to_string()));
                }
            },
            Err(error) => {
                let _ = output.send(AppServerOutput::Invalid(error.to_string()));
                break;
            }
        }
    }
    let _ = output.send(AppServerOutput::Closed);
}

fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, message)
        .map_err(|error| format!("could not encode a Codex App Server request: {error}"))?;
    stdin
        .write_all(b"\n")
        .and_then(|_| stdin.flush())
        .map_err(|error| format!("could not send a request to Codex App Server: {error}"))
}

#[cfg(test)]
#[path = "codex/tests.rs"]
mod tests;
