use serde_json::json;

use super::messages::{
    is_allowed_auth_url, parse_account_result, reasoning_summary_delta,
    work_status_for_started_item,
};
use super::*;
use std::fs;
use std::io::BufRead;

#[cfg(unix)]
struct ProtocolHarness {
    protocol: ProtocolState,
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    events: Receiver<CodexEvent>,
    events_tx: Sender<CodexEvent>,
}

#[cfg(unix)]
impl ProtocolHarness {
    fn new() -> Self {
        let mut harness = Self::new_uninitialized();
        let initialize = harness.read_message();
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["clientInfo"]["name"], "aivs");
        assert_eq!(initialize["params"]["clientInfo"]["title"], "AIVS");
        handle_app_server_message(
            &mut harness.protocol,
            json!({"id": initialize["id"], "result": {}}),
            &harness.events_tx,
        )
        .unwrap();
        assert_eq!(harness.read_message()["method"], "initialized");
        assert_eq!(harness.read_message()["method"], "account/read");
        harness
    }

    fn new_uninitialized() -> Self {
        let mut child = ProcessCommand::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (events_tx, events) = mpsc::channel();
        let mut protocol = ProtocolState::new(stdin, std::env::temp_dir());
        protocol.initialize().unwrap();
        Self {
            protocol,
            child,
            output: BufReader::new(stdout),
            events,
            events_tx,
        }
    }

    fn read_message(&mut self) -> Value {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

#[cfg(unix)]
impl Drop for ProtocolHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn parses_chatgpt_account_status() {
    let account = parse_account_result(&json!({
        "account": {
            "type": "chatgpt",
            "email": " player@example.com ",
            "planType": "plus"
        },
        "requiresOpenaiAuth": true
    }))
    .unwrap()
    .unwrap();

    assert_eq!(account.email.as_deref(), Some("player@example.com"));
    assert_eq!(account.plan_type.as_deref(), Some("plus"));
}

#[test]
fn ignores_non_chatgpt_accounts() {
    let account = parse_account_result(&json!({
        "account": { "type": "apiKey" },
        "requiresOpenaiAuth": true
    }))
    .unwrap();

    assert_eq!(account, None);
}

#[test]
fn accepts_signed_out_account_status() {
    let account = parse_account_result(&json!({
        "account": null,
        "requiresOpenaiAuth": true
    }))
    .unwrap();

    assert_eq!(account, None);
}

#[test]
fn only_opens_secure_openai_auth_urls() {
    assert!(is_allowed_auth_url("https://chatgpt.com/auth/login"));
    assert!(is_allowed_auth_url("https://auth.openai.com/codex/device"));
    assert!(!is_allowed_auth_url("http://chatgpt.com/auth/login"));
    assert!(!is_allowed_auth_url(
        "https://chatgpt.com.example.com/login"
    ));
    assert!(!is_allowed_auth_url("file:///tmp/login"));
}

#[test]
fn maps_started_items_to_safe_human_readable_work_statuses() {
    for (item_type, expected) in [
        ("reasoning", CodexWorkStatus::Thinking),
        ("fileChange", CodexWorkStatus::Editing),
        ("commandExecution", CodexWorkStatus::Checking),
        ("agentMessage", CodexWorkStatus::Working),
    ] {
        let message = json!({
            "method": "item/started",
            "params": { "item": { "type": item_type } }
        });
        assert_eq!(work_status_for_started_item(&message), Some(expected));
    }

    assert_eq!(
        work_status_for_started_item(&json!({
            "method": "item/started",
            "params": { "item": { "type": "unknown" } }
        })),
        None
    );
}

#[test]
fn reads_user_facing_reasoning_summary_deltas() {
    let message = json!({
        "method": "item/reasoning/summaryTextDelta",
        "params": { "delta": "Reviewing the game layout." }
    });
    assert_eq!(
        reasoning_summary_delta(&message),
        Some("Reviewing the game layout.")
    );
}

#[test]
fn prefers_the_project_checkout_root_for_codex_bootstrap() {
    let root = std::env::temp_dir().join(format!(
        "deployercoaster-aivs-codex-root-{}",
        std::process::id()
    ));
    let checkout = root.join("deployercoaster");
    let project = checkout.join("examples/game");
    let launch_directory = checkout.join("studio");
    let outside_project = root.join("external-game");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&launch_directory).unwrap();
    fs::create_dir_all(&outside_project).unwrap();
    fs::write(checkout.join(".codex-root"), "").unwrap();

    assert_eq!(
        codex_bootstrap_directory_from(&project, Some(&launch_directory)),
        Some(checkout.clone())
    );
    assert_eq!(
        codex_bootstrap_directory_from(&outside_project, Some(&launch_directory)),
        Some(checkout),
        "the launch checkout should be used for an external project"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn initialization_starts_queued_chat_and_defers_cancel_until_turn_id_arrives() {
    let mut harness = ProtocolHarness::new_uninitialized();
    harness
        .protocol
        .queue_chat_message("hello".to_owned(), "gpt-5".to_owned(), "high".to_owned())
        .unwrap();

    let initialize = harness.read_message();
    let init_request = json!({"id": initialize["id"], "result": {}});
    handle_app_server_message(&mut harness.protocol, init_request, &harness.events_tx).unwrap();
    assert_eq!(harness.read_message()["method"], "initialized");
    assert_eq!(harness.read_message()["method"], "account/read");
    let thread_start = harness.read_message();
    assert_eq!(thread_start["method"], "thread/start");
    assert_eq!(thread_start["params"]["serviceName"], "aivs");
    assert_eq!(
        thread_start["params"]["cwd"],
        std::env::temp_dir().to_string_lossy().as_ref()
    );

    let (events_tx, _) = mpsc::channel();
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": thread_start["id"], "result": {"thread": {"id": "thread-1"}}}),
        &events_tx,
    )
    .unwrap();
    let turn_start = harness.read_message();
    assert_eq!(turn_start["method"], "turn/start");
    assert_eq!(turn_start["params"]["input"][0]["text"], "hello");
    assert_eq!(
        turn_start["params"]["sandboxPolicy"]["writableRoots"][0],
        std::env::temp_dir().to_string_lossy().as_ref()
    );
    assert_eq!(
        turn_start["params"]["sandboxPolicy"]["networkAccess"],
        false
    );

    harness.protocol.interrupt_turn().unwrap();
    assert!(harness.protocol.cancel_requested);
    let (events_tx, _) = mpsc::channel();
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": turn_start["id"], "result": {"turn": {"id": "turn-1"}}}),
        &events_tx,
    )
    .unwrap();
    let interrupt = harness.read_message();
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");
}

#[cfg(unix)]
#[test]
fn turn_scopes_writes_to_workspace_and_respects_network_setting() {
    let mut harness = ProtocolHarness::new();
    harness.protocol.project_root = PathBuf::from("/sites/New Name");
    harness.protocol.network_access = true;
    harness.protocol.thread_id = Some("website-thread".into());
    harness
        .protocol
        .queue_chat_message("Create website".into(), "gpt-5".into(), "high".into())
        .unwrap();
    let request = harness.read_message();
    assert_eq!(request["method"], "turn/start");
    assert_eq!(request["params"]["cwd"], "/sites/New Name");
    assert_eq!(
        request["params"]["sandboxPolicy"]["writableRoots"],
        json!(["/sites/New Name"])
    );
    assert_eq!(request["params"]["sandboxPolicy"]["networkAccess"], true);
    assert_eq!(request["params"]["sandboxPolicy"]["type"], "workspaceWrite");
}

#[cfg(unix)]
#[test]
fn completed_turn_statuses_emit_success_cancel_failure_or_unknown_error() {
    for (status, expected) in [
        ("completed", "success"),
        ("interrupted", "cancelled"),
        ("failed", "failure"),
        ("queued", "unknown"),
    ] {
        let mut harness = ProtocolHarness::new();
        harness.protocol.initialized = true;
        harness.protocol.turn_active = true;
        let (events_tx, events) = mpsc::channel();
        handle_app_server_message(
            &mut harness.protocol,
            json!({"method": "turn/completed", "params": {"turn": {
                "id": "turn-1", "status": status, "error": {"message": "failed detail"}
            }}}),
            &events_tx,
        )
        .unwrap();
        let event = events.try_recv().unwrap();
        match (expected, event) {
            ("success", CodexEvent::ChatTurnCompleted)
            | ("cancelled", CodexEvent::ChatTurnCancelled) => {}
            ("failure", CodexEvent::ChatError(message)) => {
                assert_eq!(message, "failed detail");
            }
            ("unknown", CodexEvent::ChatError(message)) => {
                assert!(message.contains("unknown turn status: queued"));
            }
            (_, other) => panic!("unexpected event for {status}: {other:?}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn retryable_error_notifications_do_not_fail_the_active_turn() {
    let mut harness = ProtocolHarness::new();
    let (events_tx, events) = mpsc::channel();
    handle_app_server_message(
        &mut harness.protocol,
        json!({"method": "error", "params": {
            "error": {"message": "temporary transport failure"}, "willRetry": true
        }}),
        &events_tx,
    )
    .unwrap();
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
}

#[cfg(unix)]
#[test]
fn interrupt_request_failure_keeps_turn_active_and_emits_cancel_error() {
    let mut harness = ProtocolHarness::new();
    harness.protocol.thread_id = Some("thread-1".to_owned());
    harness.protocol.turn_active = true;
    harness.protocol.turn_id = Some("turn-1".to_owned());

    harness.protocol.interrupt_turn().unwrap();
    let interrupt = harness.read_message();
    assert_eq!(interrupt["method"], "turn/interrupt");
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": interrupt["id"], "error": {"message": "server still working"}}),
        &harness.events_tx,
    )
    .unwrap();

    assert!(harness.protocol.turn_active);
    assert_eq!(harness.protocol.turn_id.as_deref(), Some("turn-1"));
    assert!(!harness.protocol.turn_interrupt_pending);
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::CancelError(message) if message.contains("server still working")
    ));
}

#[cfg(unix)]
#[test]
fn failed_thread_start_discards_the_queued_prompt() {
    let mut harness = ProtocolHarness::new();
    harness
        .protocol
        .queue_chat_message(
            "do not replay".to_owned(),
            "gpt-5".to_owned(),
            "high".to_owned(),
        )
        .unwrap();
    let thread_start = harness.read_message();
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": thread_start["id"], "error": {"message": "thread unavailable"}}),
        &harness.events_tx,
    )
    .unwrap();

    assert!(!harness.protocol.thread_start_pending);
    assert!(harness.protocol.queued_messages.is_empty());
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::ChatError(message) if message.contains("thread unavailable")
    ));
}

#[cfg(unix)]
#[test]
fn streamed_codex_notifications_preserve_deltas_status_and_final_message() {
    let mut harness = ProtocolHarness::new();
    let tx = &harness.events_tx;
    for message in [
        json!({"method": "item/agentMessage/delta", "params": {"delta": "Hello"}}),
        json!({"method": "item/reasoning/summaryTextDelta", "params": {"delta": "Working"}}),
        json!({"method": "item/started", "params": {"item": {"type": "fileChange"}}}),
        json!({"method": "item/completed", "params": {"item": {
            "type": "agentMessage", "text": "Hello there"
        }}}),
    ] {
        handle_app_server_message(&mut harness.protocol, message, tx).unwrap();
    }
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::AssistantDelta(delta) if delta == "Hello"
    ));
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::ActivityDelta(delta) if delta == "Working"
    ));
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::WorkStatus(CodexWorkStatus::Editing)
    ));
    assert!(matches!(
        harness.events.try_recv().unwrap(),
        CodexEvent::AssistantMessage(text) if text == "Hello there"
    ));
}

#[cfg(unix)]
#[test]
fn worker_starts_fake_app_server_and_reaps_it_on_shutdown() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake-codex");
    fs::write(
        &executable,
        "#!/bin/sh\necho $$ > worker.pid\nwhile IFS= read -r request; do\n  case \"$request\" in\n    *'\"method\":\"initialize\"'*) printf '{\"id\":1,\"result\":{}}\\n' ;;\n    *'\"method\":\"account/read\"'*) printf '{\"id\":2,\"result\":{\"account\":null}}\\n' ;;\n  esac\ndone\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let (command_tx, command_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    let project_root = temp.path().to_owned();
    let worker = thread::spawn(move || {
        run_worker_with_executable(&project_root, command_rx, events_tx, executable, false)
    });

    assert!(matches!(
        events_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        CodexEvent::AccountStatus(None)
    ));
    let pid: u32 = fs::read_to_string(temp.path().join("worker.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    command_tx.send(CodexCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let still_running = ProcessCommand::new("sh")
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .unwrap()
        .success();
    assert!(!still_running, "fake Codex child was left running");
}

#[cfg(unix)]
#[test]
fn default_turn_uses_configured_model_and_rejects_unsupported_server_requests() {
    let mut harness = ProtocolHarness::new();
    harness.protocol.thread_id = Some("thread-1".into());
    harness
        .protocol
        .queue_chat_message("Hello".into(), String::new(), String::new())
        .unwrap();
    let turn = harness.read_message();
    assert!(turn["params"]["model"].is_null());
    assert!(turn["params"]["effort"].is_null());
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": "server-request-1", "method": "item/tool/requestUserInput", "params": {}}),
        &harness.events_tx,
    )
    .unwrap();
    let response = harness.read_message();
    assert_eq!(response["id"], "server-request-1");
    assert_eq!(response["error"]["code"], -32601);
    assert!(harness.protocol.turn_active);
}

#[cfg(unix)]
#[test]
fn successful_login_refreshes_account_and_reports_completion() {
    let mut harness = ProtocolHarness::new();
    harness.protocol.login_active = true;
    handle_app_server_message(
        &mut harness.protocol,
        json!({"method": "account/login/completed", "params": {"success": true}}),
        &harness.events_tx,
    )
    .unwrap();
    let read = harness.read_message();
    assert_eq!(read["method"], "account/read");
    handle_app_server_message(
        &mut harness.protocol,
        json!({"id": read["id"], "result": {"account": {
            "type": "chatgpt", "email": "person@example.com", "planType": "plus"
        }}}),
        &harness.events_tx,
    )
    .unwrap();
    assert!(
        matches!(harness.events.try_recv().unwrap(), CodexEvent::LoginCompleted(account)
        if account.email.as_deref() == Some("person@example.com"))
    );
    assert!(!harness.protocol.login_active);
}

#[test]
#[ignore = "requires an installed Codex CLI, ChatGPT login, and a small inference request"]
fn live_app_server_streams_a_complete_reply() {
    let workspace = tempfile::tempdir().unwrap();
    let client = CodexClient::new(workspace.path(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut response = String::new();
    let mut saw_delta = false;
    loop {
        let event = client
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("live Codex request timed out");
        match event {
            CodexEvent::AccountStatus(Some(_)) => client.open_chat().unwrap(),
            CodexEvent::AccountStatus(None) => panic!("Sign in with ChatGPT in Codex first"),
            CodexEvent::ChatReady => client
                .send_chat_message(
                    "Reply with exactly AIVS_OK. Do not use tools or change any files.".into(),
                    String::new(),
                    String::new(),
                )
                .unwrap(),
            CodexEvent::AssistantDelta(delta) => {
                saw_delta = true;
                response.push_str(&delta);
            }
            CodexEvent::AssistantMessage(text) => response = text,
            CodexEvent::ChatTurnCompleted => break,
            CodexEvent::Error(error)
            | CodexEvent::ChatError(error)
            | CodexEvent::Unavailable(error) => panic!("Live Codex failed: {error}"),
            _ => {}
        }
    }
    assert!(saw_delta, "did not receive a streamed assistant delta");
    assert_eq!(response.trim(), "AIVS_OK");
}
