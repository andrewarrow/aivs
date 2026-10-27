use super::*;
use crate::codex::CodexCommand;
use std::path::Path;

fn app() -> (App, std::sync::mpsc::Receiver<CodexCommand>) {
    let (client, commands, _events) = CodexClient::test_channels();
    (
        App {
            video: crate::video::VideoWorkspace::ready_for_test(PathBuf::from("/workspace")),
            project: crate::project::Project::for_test(Path::new("/workspace")),
            pending_project_action: None,
            screenplay_fraction: 0.5,
            characters: crate::characters::CharacterGallery::load(Path::new("/workspace")),
            sets: crate::sets::SetGallery::load(Path::new("/workspace")),
            characters_open: true,
            sets_open: true,
            screenplay: crate::screenplay::Screenplay::default(),
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
            import_error: None,
            settings_open: false,
            settings_error: None,
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
fn file_settings_modal_selects_and_persists_format_at_supported_sizes() {
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, _commands) = app();
        app.root = root.path().to_owned();
        app.project = crate::project::Project::for_test(root.path());
        app.video = crate::video::VideoWorkspace::ready_for_test(app.root.clone());
        app.video.output_for_test(app.root.join("video.mp4"));
        let ctx = egui::Context::default();
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        let frame = |app: &mut App, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    events,
                    ..Default::default()
                },
                |ui| app.ui(ui),
            )
        };
        let text_rect = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                        Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label} should be visible at {width}×{height}"))
        };
        let click = |pos| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let _ = frame(&mut app, vec![]);
        #[cfg(not(target_os = "macos"))]
        {
            let output = frame(&mut app, vec![]);
            let _ = frame(&mut app, click(text_rect(&output, "File").center()));
            let output = frame(&mut app, vec![]);
            let _ = frame(&mut app, click(text_rect(&output, "Settings…").center()));
        }
        #[cfg(target_os = "macos")]
        app.execute_command(AppCommand::Settings);
        assert!(app.settings_open);
        let output = frame(&mut app, vec![]);
        for label in [
            "Settings",
            "Render format",
            "1280 × 720 · 16:9",
            "1080 × 1920 · 9:16",
            "Done",
        ] {
            assert!(bounds.contains_rect(text_rect(&output, label)));
        }
        let output = frame(
            &mut app,
            click(text_rect(&output, "1080 × 1920 · 9:16").center()),
        );
        assert_eq!(
            app.video.render_format,
            crate::settings::RenderFormat::Portrait
        );
        assert_eq!(
            crate::settings::RenderFormat::load(root.path()),
            crate::settings::RenderFormat::Portrait
        );
        let _ = frame(&mut app, click(text_rect(&output, "Done").center()));
        assert!(!app.settings_open);
        app.settings_open = true;
        let _ = frame(&mut app, vec![]);
        let _ = frame(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!app.settings_open, "Escape dismisses settings");
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
        (2560.0, 1440.0),
    ] {
        for chat_open in [false, true] {
            let (mut app, _commands) = app();
            app.account = Some(account());
            app.ready = true;
            app.video
                .output_for_test(PathBuf::from("/workspace/video.mp4"));
            app.chat_open = chat_open;
            app.draft = "A long draft that wraps naturally. ".repeat(40);
            app.messages.push(Message {
                user: false,
                text: "Long response ".repeat(100),
            });
            let ctx = egui::Context::default();
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            // Exercise the split extremes as well as the default, letting sizing settle.
            for fraction in [0.3, 0.5, 0.7, 0.7] {
                app.screenplay_fraction = fraction;
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
            let expected_gallery_width = (width * 0.30).min(160.0);
            assert!(
                (gallery.size().x - expected_gallery_width).abs() <= 1.0,
                "gallery width at {width}x{height} should be {expected_gallery_width}, got {}",
                gallery.size().x
            );
        }
    }
}

#[test]
fn sample_screenplay_fits_one_letter_page_at_supported_scales() {
    // Narrow panes intentionally scroll horizontally; test paper bounds with
    // enough viewport width to paint the entire page at each supported scale.
    for width in [560.0, 612.0, 900.0] {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 1000.0),
                )),
                ..Default::default()
            },
            |ui| crate::screenplay::Screenplay::default().ui(ui, &[], &[]),
        );
        let page = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect) if rect.fill == egui::Color32::WHITE => {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("screenplay needs a white paper surface");
        assert!((page.width() / page.height() - 612.0 / 792.0).abs() < 0.001);
        for text in ["EXT. DESERT ROAD - DUSK", "SUE", "JOE", "FADE OUT."] {
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(shape) if shape.galley.text() == text => Some(shape),
                    _ => None,
                })
                .expect("scene heading, characters, and ending must be present");
            let text_rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            assert!(page.contains_rect(text_rect), "script must fit on one page");
        }
    }
}

#[test]
fn screenplay_divider_drags_to_limits_and_resets_with_mouse_and_keyboard() {
    let (mut app, _commands) = app();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |app: &mut App, events| {
        time += 0.05;
        let mut bounds = egui::Rect::NOTHING;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                bounds = ui.available_rect_before_wrap();
                app.workspace_ui(ui);
            },
        );
        bounds
    };
    let pointer = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let bounds = frame(&mut app, vec![]);
    frame(&mut app, vec![]);
    assert_eq!(app.screenplay_fraction, 0.5);
    for (target_y, expected) in [(bounds.bottom() + 100.0, 0.7), (-100.0, 0.3)] {
        let start = egui::pos2(
            bounds.center().x,
            bounds.top() + (bounds.height() - 12.0) * app.screenplay_fraction + 6.0,
        );
        frame(&mut app, vec![egui::Event::PointerMoved(start)]);
        frame(&mut app, vec![pointer(start, true)]);
        let end = egui::pos2(start.x, target_y);
        frame(&mut app, vec![egui::Event::PointerMoved(end)]);
        frame(&mut app, vec![pointer(end, false)]);
        assert_eq!(app.screenplay_fraction, expected);
    }
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![key(egui::Key::ArrowDown)]);
    assert!((app.screenplay_fraction - 0.35).abs() < 0.001);
    frame(&mut app, vec![key(egui::Key::Home)]);
    assert_eq!(app.screenplay_fraction, 0.5);

    app.screenplay_fraction = 0.7;
    let pos = egui::pos2(
        bounds.center().x,
        bounds.top() + (bounds.height() - 12.0) * 0.7 + 6.0,
    );
    frame(&mut app, vec![egui::Event::PointerMoved(pos)]);
    for _ in 0..2 {
        frame(&mut app, vec![pointer(pos, true)]);
        frame(&mut app, vec![pointer(pos, false)]);
    }
    assert_eq!(app.screenplay_fraction, 0.5);
}

#[test]
fn gallery_selection_keeps_the_screenplay_and_hides_the_old_prompt() {
    let (mut app, _commands) = app();
    let ctx = egui::Context::default();
    let frame = |app: &mut App, events| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.ui(ui),
        )
    };
    let _ = frame(&mut app, vec![]);
    let mut output = frame(&mut app, vec![]);
    for (name, portrait) in [("Ava", false), ("Eli", true), ("Ava", false)] {
        let label = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == name => Some(text.pos),
                _ => None,
            })
            .expect("character name must be visible");
        let pos = label + egui::vec2(8.0, if portrait { 60.0 } else { 6.0 });
        output = frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(labels.contains(&"EXT. DESERT ROAD - DUSK"));
        assert!(labels.contains(&"Generate video"));
        assert!(!labels.contains(&"Prompt"));
        assert!(
            app.screenplay.render_request(0).is_err(),
            "browsing must not cast a role"
        );
    }
}

#[test]
fn gallery_names_and_portraits_drag_onto_roles_and_recast_all_occurrences() {
    let (mut app, _commands) = app();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |app: &mut App, events| {
        time += 0.05;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 1800.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| app.ui(ui),
        )
    };
    let pointer = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![]);
    let mut output = frame(&mut app, vec![]);
    for (name, portrait, target, occurrence) in [
        ("Ava", false, "SUE", 0),
        ("Eli", true, "We've still got some light.", 0),
        ("Eli", false, "SUE", 1),
        ("Ava", true, "EXT. DESERT ROAD - DUSK", 0),
    ] {
        let positions = |text| {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(shape) if shape.galley.text() == text => {
                        Some(shape.pos)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let source = positions(name)[0] + egui::vec2(8.0, if portrait { 60.0 } else { 6.0 });
        let target_pos = positions(target)[occurrence] + egui::vec2(8.0, 6.0);
        let previous = app.screenplay.render_request(0);
        frame(&mut app, vec![egui::Event::PointerMoved(source)]);
        frame(&mut app, vec![pointer(source, true)]);
        frame(
            &mut app,
            vec![egui::Event::PointerMoved(source - egui::vec2(24.0, 0.0))],
        );
        let payload = egui::DragAndDrop::payload::<crate::characters::CastCharacter>(&ctx)
            .expect("both the name and portrait should start a character drag");
        assert_eq!(payload.name, name);
        frame(&mut app, vec![egui::Event::PointerMoved(target_pos)]);
        frame(&mut app, vec![pointer(target_pos, false)]);
        output = frame(&mut app, vec![]);
        if target == "SUE" && occurrence == 0 {
            assert_eq!(
                app.screenplay.render_request(0).unwrap_err(),
                "Assign characters to JOE before generating."
            );
        } else if target == "EXT. DESERT ROAD - DUSK" {
            assert_eq!(
                app.screenplay.render_request(0),
                previous,
                "dropping on action must not change the cast"
            );
        } else {
            let request: serde_json::Value =
                serde_json::from_str(&app.screenplay.render_request(0).unwrap()).unwrap();
            assert_eq!(request["cast"][0]["character"], "eli");
            if occurrence == 1 {
                assert_eq!(request["cast"][1]["character"], "eli");
                let annotations = output.shapes.iter().filter(|shape| {
                    matches!(&shape.shape, egui::epaint::Shape::Text(shape) if shape.galley.text() == "played by Eli")
                }).count();
                assert_eq!(
                    annotations, 5,
                    "all five cues should show the shared casting"
                );
            } else {
                assert_eq!(request["cast"][1]["character"], "ava");
            }
        }
    }
}
