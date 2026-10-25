use super::*;
use crate::codex::CodexCommand;
use std::path::Path;

fn app() -> (App, std::sync::mpsc::Receiver<CodexCommand>) {
    let (client, commands, _events) = CodexClient::test_channels();
    (
        App {
            video: crate::video::VideoWorkspace::ready_for_test(PathBuf::from("/workspace")),
            characters: crate::characters::CharacterGallery::load(Path::new("/workspace")),
            codex_started: true,
            root: PathBuf::from("/workspace"),
            client: Some(client),
            account: None,
            checking: false,
            login_pending: false,
            chat_open: false,
            opening: false,
            ready: false,
            busy: false,
            cancelling: false,
            streaming: false,
            messages: Vec::new(),
            draft: String::new(),
            status: String::new(),
            error: None,
            quit: false,
        },
        commands,
    )
}

fn account() -> ChatGptAccount {
    ChatGptAccount {
        email: Some("person@example.com".into()),
        plan_type: Some("plus".into()),
    }
}

#[test]
fn opening_chat_before_account_check_starts_it_once_account_is_known() {
    let (mut app, commands) = app();
    app.open_chat();
    assert!(commands.try_recv().is_err());
    app.handle_event(CodexEvent::AccountStatus(Some(account())));
    assert!(matches!(
        commands.try_recv().unwrap(),
        CodexCommand::OpenChat
    ));
    app.open_chat();
    assert!(
        commands.try_recv().is_err(),
        "must not create a second thread"
    );
}

#[test]
fn chat_sends_draft_and_reconciles_streamed_messages_without_duplicates() {
    let (mut app, commands) = app();
    app.ready = true;
    app.account = Some(account());
    app.draft = "Hello".into();
    app.send();
    assert!(
        matches!(commands.try_recv().unwrap(), CodexCommand::SendChatMessage {
        message, model, reasoning_effort
    } if message == "Hello" && model.is_empty() && reasoning_effort.is_empty())
    );
    assert!(app.busy);
    assert!(app.draft.is_empty());
    app.handle_event(CodexEvent::AssistantDelta("Hi".into()));
    app.handle_event(CodexEvent::AssistantDelta(" there".into()));
    app.handle_event(CodexEvent::AssistantMessage("Hi there!".into()));
    app.handle_event(CodexEvent::ChatTurnCompleted);
    assert!(!app.busy);
    assert_eq!(app.messages.len(), 2);
    assert_eq!(app.messages[1].text, "Hi there!");
    // A second assistant item must remain a distinct message.
    app.handle_event(CodexEvent::AssistantMessage("Another item".into()));
    assert_eq!(app.messages.len(), 3);
}

#[test]
fn stop_failure_keeps_turn_busy_and_disconnect_keeps_unsent_draft() {
    let (mut app, _commands) = app();
    app.busy = true;
    app.cancelling = true;
    app.handle_event(CodexEvent::CancelError("Could not stop yet".into()));
    assert!(app.busy);
    assert!(!app.cancelling);
    app.draft = "Keep this draft".into();
    app.handle_event(CodexEvent::Unavailable("Server stopped".into()));
    assert!(!app.busy);
    assert!(app.client.is_none());
    assert_eq!(app.draft, "Keep this draft");
}

#[test]
fn video_workspace_and_optional_chat_fit_supported_sizes() {
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        for chat_open in [false, true] {
            let (mut app, _commands) = app();
            app.account = Some(account());
            app.ready = true;
            app.chat_open = chat_open;
            app.draft = "A long draft that wraps naturally. ".repeat(40);
            app.messages.push(Message {
                user: false,
                text: "Long response ".repeat(100),
            });
            let ctx = egui::Context::default();
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            // Run two frames to let panel sizing settle.
            for _ in 0..2 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(bounds),
                        ..Default::default()
                    },
                    |ui| {
                        app.ui(ui);
                        assert!(ui.min_rect().right() <= bounds.right() + 1.0);
                        assert!(ui.min_rect().bottom() <= bounds.bottom() + 1.0);
                    },
                );
            }
            let gallery =
                egui::containers::panel::PanelState::load(&ctx, egui::Id::new("characters"))
                    .expect("characters panel should be present after setup");
            assert!(
                (gallery.size().x - width * 0.30).abs() <= 1.0,
                "gallery width at {width}x{height} should be 30%, got {}",
                gallery.size().x
            );
        }
    }
}
