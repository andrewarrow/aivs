use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::{
    ChatGptAccount, CodexEvent, CodexWorkStatus, PendingRequest, ProtocolState, write_message,
};

pub(super) fn handle_app_server_message(
    protocol: &mut ProtocolState,
    message: Value,
    events: &Sender<CodexEvent>,
) -> Result<(), String> {
    // Requests from the server have both a method and an id. They must receive
    // a response even when this minimal client does not implement the method.
    if message.get("method").is_some()
        && let Some(id) = message.get("id")
    {
        return write_message(
            &mut protocol.stdin,
            &json!({"id": id, "error": {
                "code": -32601, "message": "This client does not support this request."
            }}),
        );
    }
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let Some(pending) = protocol.pending.remove(&id) else {
            return Ok(());
        };
        if let Some(error) = message.get("error") {
            let detail = format_pending_error(pending, error);
            match pending {
                PendingRequest::Login => protocol.login_active = false,
                PendingRequest::ThreadStart => {
                    protocol.thread_start_pending = false;
                    protocol.queued_messages.clear();
                    let _ = events.send(CodexEvent::ChatError(detail));
                    return Ok(());
                }
                PendingRequest::TurnStart => {
                    protocol.turn_active = false;
                    protocol.turn_id = None;
                    protocol.cancel_requested = false;
                    let _ = events.send(CodexEvent::ChatError(detail));
                    return Ok(());
                }
                PendingRequest::TurnInterrupt => {
                    protocol.turn_interrupt_pending = false;
                    let _ = events.send(CodexEvent::CancelError(detail));
                    return Ok(());
                }
                PendingRequest::Initialize | PendingRequest::Account { .. } => {}
            }
            return Err(detail);
        }
        let result = message
            .get("result")
            .ok_or_else(|| "Codex App Server returned an incomplete response.".to_owned())?;
        return match pending {
            PendingRequest::Initialize => {
                protocol.notify("initialized", json!({}))?;
                protocol.initialized = true;
                protocol.read_account(false)?;
                if protocol.queued_login {
                    protocol.queued_login = false;
                    protocol.begin_login()?;
                }
                if protocol.chat_requested {
                    protocol.ensure_thread()?;
                }
                Ok(())
            }
            PendingRequest::Account { completes_login } => {
                let account = parse_account_result(result)?;
                if completes_login && protocol.login_active {
                    let account = account.ok_or_else(|| {
                        "ChatGPT sign-in finished without a connected account.".to_owned()
                    })?;
                    protocol.login_active = false;
                    let _ = events.send(CodexEvent::LoginCompleted(account));
                } else {
                    let _ = events.send(CodexEvent::AccountStatus(account));
                }
                Ok(())
            }
            PendingRequest::Login => {
                let auth_url = result
                    .get("authUrl")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        protocol.login_active = false;
                        "Codex App Server did not return a ChatGPT sign-in URL.".to_owned()
                    })?;
                if !is_allowed_auth_url(auth_url) {
                    protocol.login_active = false;
                    return Err(
                        "Codex App Server returned an invalid ChatGPT sign-in URL.".to_owned()
                    );
                }
                if !open_browser(auth_url) {
                    protocol.login_active = false;
                    return Err(
                        "Could not open the browser for ChatGPT sign-in. Check your browser association and try again."
                            .to_owned(),
                    );
                }
                let _ = events.send(CodexEvent::BrowserOpened);
                Ok(())
            }
            PendingRequest::ThreadStart => {
                let thread_id = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        "Codex App Server returned an incomplete chat thread.".to_owned()
                    })?;
                protocol.thread_start_pending = false;
                protocol.thread_id = Some(thread_id.to_owned());
                let _ = events.send(CodexEvent::ChatReady);
                if let Err(message) = protocol.start_queued_turn() {
                    let _ = events.send(CodexEvent::ChatError(message));
                }
                Ok(())
            }
            PendingRequest::TurnStart => {
                protocol.turn_id = Some(
                    result
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            protocol.turn_active = false;
                            "Codex App Server returned an incomplete turn.".to_owned()
                        })?,
                );
                if protocol.cancel_requested {
                    protocol.interrupt_turn()?;
                }
                Ok(())
            }
            PendingRequest::TurnInterrupt => {
                protocol.turn_interrupt_pending = false;
                Ok(())
            }
        };
    }

    match message.get("method").and_then(Value::as_str) {
        Some("account/login/completed") => {
            let params = message.get("params").unwrap_or(&Value::Null);
            if params.get("success").and_then(Value::as_bool) == Some(true) {
                protocol.read_account(true)?;
            } else {
                protocol.login_active = false;
                let detail = params
                    .get("error")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("The ChatGPT sign-in was cancelled or could not be completed.");
                let _ = events.send(CodexEvent::Error(detail.to_owned()));
            }
        }
        Some("account/updated") => {
            let auth_mode = message.pointer("/params/authMode").and_then(Value::as_str);
            if matches!(auth_mode, Some("chatgpt" | "chatgptAuthTokens")) {
                protocol.read_account(protocol.login_active)?;
            } else if !protocol.login_active {
                let _ = events.send(CodexEvent::AccountStatus(None));
            }
        }
        Some("item/agentMessage/delta") => {
            if let Some(delta) = message
                .pointer("/params/delta")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                let _ = events.send(CodexEvent::AssistantDelta(delta.to_owned()));
            }
        }
        Some("item/reasoning/summaryTextDelta") => {
            if let Some(delta) = reasoning_summary_delta(&message) {
                let _ = events.send(CodexEvent::ActivityDelta(delta.to_owned()));
            }
        }
        Some("item/started") => {
            if let Some(status) = work_status_for_started_item(&message) {
                let _ = events.send(CodexEvent::WorkStatus(status));
            }
        }
        Some("item/completed") => {
            if message.pointer("/params/item/type").and_then(Value::as_str) == Some("agentMessage")
                && let Some(text) = message.pointer("/params/item/text").and_then(Value::as_str)
            {
                let _ = events.send(CodexEvent::AssistantMessage(text.to_owned()));
            }
        }
        Some("turn/completed") => {
            protocol.turn_active = false;
            protocol.turn_id = None;
            protocol.turn_interrupt_pending = false;
            protocol.cancel_requested = false;
            let status = message
                .pointer("/params/turn/status")
                .and_then(Value::as_str);
            if status == Some("interrupted") {
                let _ = events.send(CodexEvent::ChatTurnCancelled);
            } else if status == Some("failed") {
                let detail = message
                    .pointer("/params/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex could not complete that request.");
                let _ = events.send(CodexEvent::ChatError(detail.to_owned()));
            } else if status == Some("completed") {
                let _ = events.send(CodexEvent::ChatTurnCompleted);
            } else {
                let _ = events.send(CodexEvent::ChatError(format!(
                    "Codex returned an unknown turn status: {}",
                    status.unwrap_or("missing")
                )));
            }
            protocol.start_queued_turn()?;
        }
        Some("error") => {
            let detail = message
                .pointer("/params/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex returned an error.");
            if message
                .pointer("/params/willRetry")
                .and_then(Value::as_bool)
                == Some(true)
            {
                log::warn!("Codex App Server reported a retryable error: {detail}");
            } else {
                let _ = events.send(CodexEvent::ChatError(detail.to_owned()));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn work_status_for_started_item(message: &Value) -> Option<CodexWorkStatus> {
    match message.pointer("/params/item/type").and_then(Value::as_str) {
        Some("reasoning") => Some(CodexWorkStatus::Thinking),
        Some("fileChange") => Some(CodexWorkStatus::Editing),
        Some("commandExecution") => Some(CodexWorkStatus::Checking),
        Some("agentMessage") => Some(CodexWorkStatus::Working),
        Some("mcpToolCall" | "dynamicToolCall" | "webSearch") => Some(CodexWorkStatus::Working),
        _ => None,
    }
}

pub(super) fn reasoning_summary_delta(message: &Value) -> Option<&str> {
    message
        .pointer("/params/delta")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

pub(super) fn parse_account_result(result: &Value) -> Result<Option<ChatGptAccount>, String> {
    let Some(account) = result.get("account") else {
        return Err("Codex App Server returned an incomplete account status.".to_owned());
    };
    if account.is_null() {
        return Ok(None);
    }
    let account_type = account.get("type").and_then(Value::as_str);
    if !matches!(account_type, Some("chatgpt" | "chatgptAuthTokens")) {
        return Ok(None);
    }
    Ok(Some(ChatGptAccount {
        email: optional_nonempty_string(account.get("email")),
        plan_type: optional_nonempty_string(account.get("planType")),
    }))
}

fn optional_nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn format_pending_error(pending: PendingRequest, error: &Value) -> String {
    let detail = error
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown error");
    let action = match pending {
        PendingRequest::Initialize => "initialize Codex App Server",
        PendingRequest::Account { .. } => "read the ChatGPT account",
        PendingRequest::Login => "start ChatGPT sign-in",
        PendingRequest::ThreadStart => "start the Codex chat",
        PendingRequest::TurnStart => "send the Codex message",
        PendingRequest::TurnInterrupt => "cancel the Codex message",
    };
    format!("Could not {action}: {detail}")
}

pub(super) fn is_allowed_auth_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host == "chatgpt.com"
            || host.ends_with(".chatgpt.com")
            || host == "openai.com"
            || host.ends_with(".openai.com")
    })
}

fn open_browser(url: &str) -> bool {
    webbrowser::open(url).is_ok()
}
