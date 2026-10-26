use super::*;

fn character(name: &str) -> CastCharacter {
    CastCharacter {
        id: name.to_lowercase(),
        name: name.into(),
    }
}

#[test]
fn casting_is_explicit_and_recasting_preserves_script_names_and_dialogue() {
    let mut screenplay = Screenplay::default();
    assert_eq!(screenplay.roles(), ["JOE", "SUE"]);
    assert_eq!(
        screenplay.render_request().unwrap_err(),
        "Assign characters to JOE and SUE before generating."
    );
    screenplay.assign("SUE", character("Ava"));
    assert_eq!(
        screenplay.render_request().unwrap_err(),
        "Assign characters to JOE before generating."
    );
    screenplay.assign("JOE", character("Eli"));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request().unwrap()).unwrap();
    assert_eq!(request["version"], 1);
    assert_eq!(
        request["cast"],
        serde_json::json!([
            {"role": "JOE", "character": "eli"},
            {"role": "SUE", "character": "ava"}
        ])
    );
    let script = request["script"].as_str().unwrap();
    assert_eq!(script.matches("SUE: \"").count(), 2);
    assert!(script.contains("JOE (looking around): \"He was an optimistic man.\""));
    for &(element, text) in SCENE {
        if matches!(element, Element::Dialogue | Element::Action) {
            assert!(script.contains(text));
        }
    }
    screenplay.assign("SUE", character("Nora"));
    screenplay.assign("UNKNOWN", character("Maya"));
    let recast: serde_json::Value =
        serde_json::from_str(&screenplay.render_request().unwrap()).unwrap();
    assert_eq!(recast["script"], request["script"]);
    assert_eq!(recast["cast"][0], request["cast"][0]);
    assert_eq!(recast["cast"][1]["character"], "nora");
    assert_eq!(screenplay.document.cast.len(), 2);
}

#[test]
fn every_cue_parenthetical_and_dialogue_accepts_a_drop_for_the_whole_role() {
    let mut screenplay = Screenplay::default();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |screenplay: &mut Screenplay, events| {
        time += 0.05;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 1000.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| screenplay.ui(ui, &[character("Ava"), character("Eli")]),
        )
    };
    let pointer = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut screenplay, vec![]);
    let output = frame(&mut screenplay, vec![]);
    // Include every dialogue location, and the parenthetical between cue and speech.
    let mut role = "";
    let targets = output
        .shapes
        .iter()
        .filter_map(|shape| {
            let egui::epaint::Shape::Text(shape) = &shape.shape else {
                return None;
            };
            let text = shape.galley.text();
            if matches!(text, "SUE" | "JOE") {
                role = if text == "SUE" { "SUE" } else { "JOE" };
            }
            SCENE
                .iter()
                .any(|&(element, content)| {
                    content == text
                        && matches!(
                            element,
                            Element::Character | Element::Parenthetical | Element::Dialogue
                        )
                })
                .then_some((role, shape.pos + egui::vec2(4.0, 4.0)))
        })
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 11);
    for (index, (role, pos)) in targets.into_iter().enumerate() {
        let actor = character(if index % 2 == 0 { "Ava" } else { "Eli" });
        frame(&mut screenplay, vec![egui::Event::PointerMoved(pos)]);
        frame(&mut screenplay, vec![pointer(egui::pos2(20.0, 20.0), true)]);
        egui::DragAndDrop::set_payload(&ctx, actor.clone());
        frame(&mut screenplay, vec![egui::Event::PointerMoved(pos)]);
        frame(&mut screenplay, vec![pointer(pos, false)]);
        assert_eq!(screenplay.document.cast.get(role), Some(&actor));
        let output = frame(&mut screenplay, vec![]);
        let expected = format!("played by {}", actor.name);
        let annotation_count = output.shapes.iter().filter(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::Text(shape) if shape.galley.text() == expected)
        }).count();
        let occurrences = SCENE
            .iter()
            .filter(|&&(element, name)| {
                element == Element::Character && screenplay.document.cast.get(name) == Some(&actor)
            })
            .count();
        assert_eq!(
            annotation_count, occurrences,
            "every occurrence must reflect the new cast"
        );
    }
}

#[test]
fn keyboard_opens_the_cast_picker_and_choices_can_assign_and_clear_a_role() {
    let mut screenplay = Screenplay::default();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut frame = |screenplay: &mut Screenplay, events, focus| {
        time += 0.05;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(390.0, 844.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                if focus {
                    ui.memory_mut(|memory| memory.request_focus(ui.id().with(("role", 0_usize))));
                }
                screenplay.role_ui(
                    ui,
                    "SUE",
                    "He made it sound closer.",
                    0,
                    egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(300.0, 44.0)),
                    &[character("Ava"), character("Eli")],
                );
            },
        )
    };
    for choice in ["Ava", "Uncast"] {
        frame(&mut screenplay, vec![], true);
        frame(
            &mut screenplay,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        let output = frame(
            &mut screenplay,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(shape) if shape.galley.text() == choice => {
                    Some(shape.pos + egui::vec2(4.0, 4.0))
                }
                _ => None,
            })
            .expect("keyboard activation should show the cast picker");
        frame(&mut screenplay, vec![egui::Event::PointerMoved(pos)], false);
        for pressed in [true, false] {
            frame(
                &mut screenplay,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            );
        }
        assert_eq!(
            screenplay.document.cast.get("SUE"),
            (choice == "Ava").then(|| character("Ava")).as_ref()
        );
    }
}

struct Editor {
    ctx: egui::Context,
    time: f64,
}

impl Editor {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
        }
    }

    fn frame(&mut self, screenplay: &mut Screenplay, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 0.05;
        self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 1000.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| screenplay.ui(ui, &[character("Ava"), character("Eli")]),
        )
    }

    fn key(&mut self, screenplay: &mut Screenplay, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(
            screenplay,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        );
        self.frame(
            screenplay,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            }],
        );
    }

    fn focus(&mut self, screenplay: &mut Screenplay, index: usize, cursor: usize) {
        screenplay.focus = Some((screenplay.document.blocks[index].id, cursor));
        self.frame(screenplay, vec![]);
        self.frame(screenplay, vec![]);
    }

    fn type_text(&mut self, screenplay: &mut Screenplay, text: &str) {
        self.frame(screenplay, vec![egui::Event::Text(text.into())]);
    }

    fn click(&mut self, screenplay: &mut Screenplay, pos: egui::Pos2) {
        self.frame(screenplay, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(
                screenplay,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }
}

#[test]
fn clicking_dialogue_edits_text_instead_of_opening_casting() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    editor.frame(&mut screenplay, vec![]);
    let output = editor.frame(&mut screenplay, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text)
                if text.galley.text() == "Good news. It's definitely the car." =>
            {
                Some(text.pos + egui::vec2(2.0, 4.0))
            }
            _ => None,
        })
        .unwrap();
    editor.click(&mut screenplay, pos);
    editor.type_text(&mut screenplay, "Well, ");
    assert!(screenplay.document.blocks[4].text.contains("Well, "));
    assert!(screenplay.document.cast.is_empty());
    assert_eq!(screenplay.active, screenplay.document.blocks[4].id);
}

#[test]
fn keyboard_writing_creates_elements_changes_format_and_undoes_structure() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    let index = 3;
    editor.focus(&mut screenplay, index, 3);
    let count = screenplay.document.blocks.len();
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    assert_eq!(
        screenplay.document.blocks[index + 1].kind,
        Element::Dialogue
    );
    editor.type_text(&mut screenplay, "A new line.");
    assert_eq!(screenplay.document.blocks[index + 1].text, "A new line.");
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::SHIFT);
    editor.type_text(&mut screenplay, "Still speaking.");
    assert_eq!(
        screenplay.document.blocks[index + 1].text,
        "A new line.\nStill speaking."
    );
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(
        screenplay.document.blocks[index + 2].kind,
        Element::Character
    );
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    editor.key(
        &mut screenplay,
        egui::Key::Z,
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    );
    assert_eq!(screenplay.document.blocks.len(), count + 2);
    assert_eq!(
        screenplay.document.blocks[index + 2].kind,
        Element::Character
    );
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(
        screenplay.document.blocks[index + 2].kind,
        Element::Parenthetical
    );
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::SHIFT);
    assert_eq!(
        screenplay.document.blocks[index + 2].kind,
        Element::Character
    );
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document.blocks.len(), count + 2);
}

#[test]
fn navigation_moves_between_elements_without_deleting_a_just_emptied_element() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    screenplay.document.blocks[4].text = "x".into();
    let count = screenplay.document.blocks.len();
    editor.focus(&mut screenplay, 4, 1);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert!(screenplay.document.blocks[4].text.is_empty());
    assert_eq!(screenplay.document.blocks.len(), count);
    editor.key(&mut screenplay, egui::Key::ArrowUp, egui::Modifiers::NONE);
    assert_eq!(screenplay.active, screenplay.document.blocks[3].id);
    editor.key(&mut screenplay, egui::Key::ArrowDown, egui::Modifiers::NONE);
    assert_eq!(screenplay.active, screenplay.document.blocks[4].id);
}

#[test]
fn splitting_replaces_a_unicode_selection_and_keeps_other_blocks() {
    let mut screenplay = Screenplay::default();
    screenplay.active = 1;
    screenplay.document.blocks[1].text = "Café — déjà vu".into();
    let following = screenplay.document.blocks[2].clone();
    screenplay.split(4..7);
    assert_eq!(screenplay.document.blocks[1].text, "Café");
    assert_eq!(screenplay.document.blocks[2].text, "déjà vu");
    assert_eq!(screenplay.document.blocks[2].kind, Element::Action);
    assert_eq!(screenplay.document.blocks[3], following);
}

#[test]
fn renamed_roles_and_new_scenes_reach_the_generation_request() {
    let mut screenplay = Screenplay::default();
    screenplay.assign("JOE", character("Eli"));
    screenplay.assign("SUE", character("Ava"));
    let before = screenplay.snapshot();
    assert!(screenplay.rename_role("JOE", "sue").is_err());
    screenplay.rename_role("JOE", "sam").unwrap();
    screenplay.record(
        Snapshot {
            document: before.document.clone(),
            active: before.active,
        },
        None,
    );
    assert_eq!(screenplay.roles(), ["SAM", "SUE"]);
    assert_eq!(
        screenplay
            .document
            .blocks
            .iter()
            .filter(|b| b.kind == Element::Character && b.text == "SAM")
            .count(),
        3
    );
    assert_eq!(screenplay.document.cast.get("SAM"), Some(&character("Eli")));
    screenplay.undo();
    assert_eq!(screenplay.document, before.document);
    screenplay.redo();
    screenplay.active = screenplay.document.blocks.last().unwrap().id;
    screenplay.insert(Element::Heading);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "INT. DINER - NIGHT".into();
    screenplay.insert(Element::Action);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "Rain taps the glass.".into();
    screenplay.insert(Element::Character);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "Sam".into();
    screenplay.insert(Element::Parenthetical);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "quietly".into();
    screenplay.insert(Element::Dialogue);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "We made it.\nAt last.".into();
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request().unwrap()).unwrap();
    assert_eq!(
        request["cast"][0],
        serde_json::json!({"role":"SAM", "character":"eli"})
    );
    assert!(request["script"].as_str().unwrap().contains(
        "INT. DINER - NIGHT\n\nRain taps the glass.\n\nSAM (quietly): \"We made it. At last.\""
    ));
    // Verify the edited request against the actual Python screenplay parser.
    let mut child = std::process::Command::new("python3").args(["-c", "import json, sys; sys.path.insert(0, 'python'); from shooting_script import parse_request; request=json.load(sys.stdin); profiles=[{'id': c['character'], 'description': 'A performer.'} for c in request['cast']]; parsed=parse_request(request, profiles); assert parsed['beats'][-1]['text'] == 'We made it. At last.'; assert parsed['beats'][-1]['scene'] == 'INT. DINER - NIGHT'"])
        .current_dir(env!("CARGO_MANIFEST_DIR")).stdin(std::process::Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn invalid_dialogue_is_blocked_and_removed_roles_do_not_require_casting() {
    let mut screenplay = Screenplay::default();
    screenplay
        .document
        .blocks
        .retain(|b| b.kind != Element::Character);
    assert!(
        screenplay
            .render_request()
            .unwrap_err()
            .starts_with("Add a character before dialogue")
    );
    screenplay
        .document
        .blocks
        .retain(|b| !matches!(b.kind, Element::Dialogue | Element::Parenthetical));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request().unwrap()).unwrap();
    assert_eq!(request["cast"], serde_json::json!([]));
    screenplay.document.blocks.truncate(1);
    assert!(screenplay.render_request().is_err());
    screenplay.delete();
    assert_eq!(screenplay.document.blocks.len(), 1);
    assert_eq!(screenplay.document.blocks[0].kind, Element::Action);
}

#[test]
fn workspace_save_round_trips_and_a_corrupt_file_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let mut screenplay = Screenplay::load(root.path());
    screenplay.document.blocks[1].text = "New action with café, rain, and a long road.".into();
    screenplay.assign("JOE", character("Eli"));
    screenplay.assign("SUE", character("Ava"));
    screenplay.save();
    assert!(screenplay.save_error.is_none());
    let restored = Screenplay::load(root.path());
    assert_eq!(screenplay.document, restored.document);
    assert_eq!(screenplay.render_request(), restored.render_request());
    let path = root.path().join("screenplay.json");
    fs::write(&path, b"{broken").unwrap();
    let mut broken = Screenplay::load(root.path());
    assert!(broken.load_failed);
    assert!(
        broken
            .save_error
            .as_deref()
            .unwrap()
            .contains("Original file preserved")
    );
    broken.save();
    assert!(broken.render_request().is_err());
    drop(broken);
    assert_eq!(fs::read(&path).unwrap(), b"{broken");
}

#[test]
fn save_failures_are_visible_and_pending_edits_are_saved_on_close() {
    let root = tempfile::tempdir().unwrap();
    let mut screenplay = Screenplay::load(root.path());
    let before = screenplay.snapshot();
    screenplay.document.blocks[0].text = "INT. ROOM - DAY".into();
    screenplay.record(before, None);
    drop(screenplay);
    let restored = Screenplay::load(root.path());
    assert_eq!(restored.document.blocks[0].text, "INT. ROOM - DAY");
    let mut screenplay = Screenplay::load(&root.path().join("missing-directory"));
    screenplay.save();
    assert!(
        screenplay
            .save_error
            .as_deref()
            .unwrap()
            .contains("Could not save")
    );
}

#[test]
fn long_scripts_grow_the_paper_and_keep_the_last_element_inside() {
    let mut screenplay = Screenplay::default();
    for _ in 0..50 {
        screenplay.insert(Element::Action);
        let index = screenplay.active_index();
        screenplay.document.blocks[index].text =
            "A long action paragraph with enough words to wrap across multiple lines. ".repeat(5);
    }
    screenplay.focus = None;
    let mut editor = Editor::new();
    let output = editor.frame(&mut screenplay, vec![]);
    let page = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) if rect.fill == egui::Color32::WHITE => Some(rect.rect),
            _ => None,
        })
        .unwrap();
    assert!(page.height() > 792.0 * 3.0);
    for shape in output.shapes {
        if let egui::epaint::Shape::Text(text) = shape.shape
            && text.galley.text().starts_with("A long action paragraph")
        {
            assert!(page.contains_rect(egui::Rect::from_min_size(text.pos, text.galley.size())));
        }
    }
}
