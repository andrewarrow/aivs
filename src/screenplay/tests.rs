use super::*;

#[test]
fn fountain_import_converts_elements_saves_and_undoes_the_screenplay_and_cast() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Café.FOUNTAIN");
    let source = "\u{feff}Title: Test screenplay\r\nAuthor: A Writer\r\nNotes:\r\n    A draft\r\n\r\n\
        # Act one\r\n= Outline only\r\n\r\n/* Omitted scene. */\r\n\r\n\
        **FADE IN:**\r\n\r\nINT. CAFÉ - DAY #12#\r\n\r\n\
        A **bright** room. [[An editorial note.]]\r\nAnother line of action.\r\n\r\n\
        JOSÉ (V.O.)(CONT’D)\r\n(quietly)\r\nHello, _world_.\r\nStill here.\r\n\r\n\
        @McDUCK ^\r\n~A song.\r\n\r\n> CUT TO:\r\n\r\n\
        .THE HALLWAY\r\n\r\n!BANG\r\nBANG\r\n\r\n> _**THE END**_ <\r\n\r\n====\r\n";
    fs::write(&path, source).unwrap();
    let mut screenplay = Screenplay::load(root.path());
    screenplay.assign("SUE", character("Ava"));
    let original = screenplay.document.clone();
    let old_next_id = screenplay.next_id;
    screenplay.import_fountain(&path).unwrap();
    assert_eq!(
        screenplay
            .document
            .blocks
            .iter()
            .map(|b| (b.kind, b.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Element::Transition, "FADE IN:"),
            (Element::Heading, "INT. CAFÉ - DAY"),
            (Element::Action, "A bright room.\nAnother line of action."),
            (Element::Character, "JOSÉ"),
            (Element::Parenthetical, "(V.O.)"),
            (Element::Parenthetical, "(quietly)"),
            (Element::Dialogue, "Hello, world.\nStill here."),
            (Element::Character, "McDUCK"),
            (Element::Dialogue, "A song."),
            (Element::Transition, "CUT TO:"),
            (Element::Heading, "THE HALLWAY"),
            (Element::Action, "BANG\nBANG"),
            (Element::Action, "THE END"),
        ]
    );
    assert!(screenplay.document.cast.is_empty());
    assert_eq!(screenplay.active, old_next_id);
    assert!(
        screenplay
            .document
            .blocks
            .iter()
            .all(|block| block.id >= old_next_id)
    );
    assert_eq!(screenplay.focus, Some((old_next_id, 0)));
    assert!(screenplay.save_error.is_none());
    let imported = screenplay.document.clone();
    let restored = Screenplay::load(root.path());
    // IDs are local to an editing session and are reassigned when loading.
    assert_eq!(
        restored
            .document
            .blocks
            .iter()
            .map(|block| (block.kind, block.text.as_str()))
            .collect::<Vec<_>>(),
        imported
            .blocks
            .iter()
            .map(|block| (block.kind, block.text.as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(restored.document.cast, imported.cast);
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
    screenplay.undo();
    assert_eq!(screenplay.document, original);
    screenplay.redo();
    assert_eq!(screenplay.document, imported);
    screenplay.assign("JOSÉ", character("Eli"));
    screenplay.assign("McDUCK", character("Ava"));
    let scene_id = screenplay.scenes()[0].id;
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(scene_id).unwrap()).unwrap();
    assert!(
        request["script"]
            .as_str()
            .unwrap()
            .contains("JOSÉ (V.O.; quietly): \"Hello, world. Still here.\"")
    );
}

#[test]
fn fountain_import_failures_preserve_the_current_document_and_saved_file() {
    let root = tempfile::tempdir().unwrap();
    let mut screenplay = Screenplay::load(root.path());
    screenplay.assign("JOE", character("Eli"));
    screenplay.save();
    let original = screenplay.document.clone();
    let saved = fs::read(root.path().join("screenplay.json")).unwrap();
    let path = root.path().join("script.fountain");
    for source in [
        "",
        " \n\t",
        "Title: No body\nAuthor: Writer\n",
        "/* omitted */\n[[note]]\n# Outline\n= Synopsis\n===",
        "bad\0text",
    ] {
        fs::write(&path, source).unwrap();
        assert!(screenplay.import_fountain(&path).is_err());
        assert_eq!(screenplay.document, original);
        assert!(screenplay.undo.is_empty());
        assert_eq!(
            fs::read(root.path().join("screenplay.json")).unwrap(),
            saved
        );
    }
    fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert!(
        screenplay
            .import_fountain(&path)
            .unwrap_err()
            .contains("Could not read")
    );
    assert!(
        screenplay
            .import_fountain(&root.path().join("missing.fountain"))
            .is_err()
    );
    assert!(
        screenplay
            .import_fountain(&root.path().join("script.txt"))
            .unwrap_err()
            .contains(".fountain")
    );
    assert_eq!(screenplay.document, original);
    fs::write(root.path().join("screenplay.json"), "{broken").unwrap();
    let mut broken = Screenplay::load(root.path());
    fs::write(&path, "A new scene.\n").unwrap();
    assert!(!broken.can_import());
    assert!(broken.import_fountain(&path).is_err());
    assert_eq!(
        fs::read(root.path().join("screenplay.json")).unwrap(),
        b"{broken"
    );
}

#[test]
fn fountain_import_preserves_action_boundaries_dialogue_lines_and_literal_punctuation() {
    let elements = fountain::parse(
        "INT. ROOM - DAY\nEXT. ROOM - DAY\n\n\
        ESTABLISHING\n\n...an action, not a heading.\n\n\
        !UPPERCASE ACTION\nSTAYS ACTION\n\n\
        \tEVE (cont'd)\n\t(a long\n\tparenthetical)\n\tME TOO!\n  \n\tStill talking.\n\n\
        23 (O.S.)\nA number alone is not a character.\n\n\
        \\!BANG\n\nA literal \\*star\\*, file_name, and **bold** text.\n",
    )
    .unwrap();
    assert_eq!(
        elements,
        vec![
            (Element::Action, "INT. ROOM - DAY\nEXT. ROOM - DAY".into()),
            (Element::Action, "ESTABLISHING".into()),
            (Element::Action, "...an action, not a heading.".into()),
            (Element::Action, "UPPERCASE ACTION\nSTAYS ACTION".into()),
            (Element::Character, "EVE".into()),
            (Element::Parenthetical, "(a long\nparenthetical)".into()),
            (Element::Dialogue, "ME TOO!\n\nStill talking.".into()),
            (
                Element::Action,
                "23 (O.S.)\nA number alone is not a character.".into()
            ),
            (Element::Action, "!BANG".into()),
            (
                Element::Action,
                "A literal *star*, file_name, and bold text.".into()
            ),
        ]
    );
    for prefix in ["INT", "EXT", "EST", "I/E", "I./E", "INT/EXT", "INT./EXT"] {
        for separator in [". ", " ", " - "] {
            let heading = format!("{prefix}{separator}ROOM - DAY");
            assert_eq!(
                fountain::parse(&heading).unwrap(),
                vec![(Element::Heading, heading.clone())]
            );
            assert_eq!(
                fountain::parse(&format!("{heading}\nA line of action.")).unwrap(),
                vec![(Element::Action, format!("{heading}\nA line of action."))]
            );
        }
    }
    for (cue, name) in [
        ("EXTENSION", "EXTENSION"),
        ("INT", "INT"),
        ("@INT. ROOM - DAY", "INT. ROOM - DAY"),
    ] {
        assert_eq!(
            fountain::parse(&format!("{cue}\nStill here.\n")).unwrap(),
            vec![
                (Element::Character, name.into()),
                (Element::Dialogue, "Still here.".into())
            ]
        );
    }
}

#[test]
#[ignore = "requires the optional big.fountain sample in the repository root"]
fn imports_the_big_fountain_sample_into_editable_elements() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("big.fountain");
    let root = tempfile::tempdir().unwrap();
    let mut screenplay = Screenplay::load(root.path());
    screenplay.import_fountain(&path).unwrap();
    assert!(screenplay.document.blocks.len() > 2000);
    assert!(
        screenplay
            .document
            .blocks
            .iter()
            .filter(|b| b.kind == Element::Heading)
            .count()
            > 100
    );
    assert!(screenplay.roles().contains(&"EDWARD".into()));
    assert!(screenplay.roles().contains(&"WILL".into()));
    assert!(
        screenplay
            .roles()
            .iter()
            .all(|role| !role.contains(['(', ')', '^']))
    );
    assert_eq!(screenplay.document.blocks.last().unwrap().text, "THE END");
    assert_eq!(Screenplay::load(root.path()).document, screenplay.document);
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                ..Default::default()
            },
            |ui| screenplay.ui(ui, &[]),
        );
        assert!(!output.shapes.is_empty());
    }
}

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
        screenplay.render_request(0).unwrap_err(),
        "Assign characters to JOE and SUE before generating."
    );
    screenplay.assign("SUE", character("Ava"));
    assert_eq!(
        screenplay.render_request(0).unwrap_err(),
        "Assign characters to JOE before generating."
    );
    screenplay.assign("JOE", character("Eli"));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
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
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
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
    assert_eq!(screenplay.document.blocks.len(), count + 2);
    assert_eq!(screenplay.document.blocks[index + 2].kind, Element::Action);
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document.blocks.len(), count + 2);
    assert_eq!(
        screenplay.document.blocks[index + 2].kind,
        Element::Character
    );
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
    let scene_id = screenplay.active;
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
        serde_json::from_str(&screenplay.render_request(scene_id).unwrap()).unwrap();
    assert_eq!(
        request["cast"][0],
        serde_json::json!({"role":"SAM", "character":"eli"})
    );
    assert_eq!(request["cast"].as_array().unwrap().len(), 1);
    assert_eq!(
        request["script"].as_str().unwrap(),
        "INT. DINER - NIGHT\n\nRain taps the glass.\n\nSAM (quietly): \"We made it. At last.\""
    );
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
fn scene_requests_include_only_the_selected_scene_and_its_cast() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("scenes.fountain");
    fs::write(
        &path,
        format!(
            "FADE IN:\n\nEXT. ROAD - DAY\n\n{}\n\nJOE\nStill waiting.\n\n\
             INT. DINER - NIGHT\n\nRain taps the glass.\n\nSAM\n(quietly)\nWe made it.\n\n\
             INT. DINER - NIGHT\n\nThe diner is empty.\n\nFADE OUT.\n",
            "Dust blows across the road. ".repeat(4000)
        ),
    )
    .unwrap();
    let mut screenplay = Screenplay::load(root.path());
    screenplay.import_fountain(&path).unwrap();
    screenplay.assign("SAM", character("Eli"));
    let scenes = screenplay.scenes();
    assert_eq!(
        scenes.iter().map(|scene| scene.heading).collect::<Vec<_>>(),
        [
            "EXT. ROAD - DAY",
            "INT. DINER - NIGHT",
            "INT. DINER - NIGHT"
        ]
    );
    assert_ne!(scenes[1].id, scenes[2].id);
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(scenes[1].id).unwrap()).unwrap();
    assert_eq!(
        request["script"],
        "INT. DINER - NIGHT\n\nRain taps the glass.\n\nSAM (quietly): \"We made it.\""
    );
    assert_eq!(
        request["cast"],
        serde_json::json!([{"role": "SAM", "character": "eli"}])
    );
    let silent: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(scenes[2].id).unwrap()).unwrap();
    assert_eq!(
        silent["script"],
        "INT. DINER - NIGHT\n\nThe diner is empty.\n\nFADE OUT."
    );
    assert_eq!(silent["cast"], serde_json::json!([]));
    assert!(
        screenplay
            .render_request(scenes[0].id)
            .unwrap_err()
            .contains("100,000")
    );
}

#[test]
fn invalid_and_empty_scene_selections_never_include_adjacent_scenes() {
    let mut screenplay = Screenplay::default();
    screenplay.document.blocks = [
        (Element::Heading, "EXT. ROAD - DAY"),
        (Element::Action, "Dust blows across the road."),
        (Element::Heading, "   "),
        (Element::Action, "This unfinished scene is excluded."),
        (Element::Heading, "INT. DINER - NIGHT"),
        (Element::Heading, "EXT. ROAD - NIGHT"),
        (Element::Dialogue, "Missing a speaker."),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (kind, text))| Block {
        id: id as u64,
        kind,
        text: text.into(),
    })
    .collect();
    assert_eq!(screenplay.scenes().len(), 3);
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
    assert_eq!(
        request["script"],
        "EXT. ROAD - DAY\n\nDust blows across the road."
    );
    for id in [1, 2, u64::MAX] {
        assert_eq!(
            screenplay.render_request(id).unwrap_err(),
            "Select a scene before generating."
        );
    }
    assert_eq!(
        screenplay.render_request(4).unwrap_err(),
        "Add action or dialogue before generating."
    );
    assert_eq!(
        screenplay.render_request(5).unwrap_err(),
        "Add a character before dialogue at element 7."
    );
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
            .render_request(0)
            .unwrap_err()
            .starts_with("Add a character before dialogue")
    );
    screenplay
        .document
        .blocks
        .retain(|b| !matches!(b.kind, Element::Dialogue | Element::Parenthetical));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
    assert_eq!(request["cast"], serde_json::json!([]));
    screenplay.document.blocks.truncate(1);
    assert!(screenplay.render_request(0).is_err());
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
    assert_eq!(screenplay.render_request(0), restored.render_request(0));
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
    assert!(broken.render_request(0).is_err());
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

#[test]
fn tab_after_a_character_adds_instructions_and_enter_resumes_existing_dialogue() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    let count = screenplay.document.blocks.len();
    let dialogue = screenplay.document.blocks[4].clone();
    editor.focus(&mut screenplay, 3, 3);
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[3].text, "JOE");
    assert_eq!(screenplay.document.blocks[4].kind, Element::Parenthetical);
    assert_eq!(screenplay.document.blocks[4].text, "()");
    editor.type_text(&mut screenplay, "softly");
    assert_eq!(screenplay.document.blocks[4].text, "(softly)");
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    assert_eq!(screenplay.active, dialogue.id);
    assert_eq!(screenplay.document.blocks[5], dialogue);
    screenplay.assign("JOE", character("Eli"));
    screenplay.assign("SUE", character("Ava"));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
    assert!(
        request["script"]
            .as_str()
            .unwrap()
            .contains("JOE (softly): \"Good news. It's definitely the car.\"")
    );
}

#[test]
fn tab_within_dialogue_preserves_speech_and_enter_continues_after_instructions() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    let count = screenplay.document.blocks.len();
    editor.focus(&mut screenplay, 4, 10);
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[4].text, "Good news.");
    assert_eq!(screenplay.document.blocks[5].kind, Element::Parenthetical);
    assert_eq!(screenplay.document.blocks[5].text, "()");
    assert_eq!(screenplay.document.blocks[6].kind, Element::Dialogue);
    assert_eq!(
        screenplay.document.blocks[6].text,
        " It's definitely the car."
    );
    editor.type_text(&mut screenplay, "a beat");
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(screenplay.active, screenplay.document.blocks[6].id);
    assert_eq!(screenplay.document.blocks.len(), count + 2);
    editor.type_text(&mut screenplay, "Actually,");
    assert_eq!(
        screenplay.document.blocks[6].text,
        "Actually, It's definitely the car."
    );
    screenplay.assign("JOE", character("Eli"));
    screenplay.assign("SUE", character("Ava"));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
    assert!(
        request["script"].as_str().unwrap().contains(
            "JOE: \"Good news.\"\n\nJOE (a beat): \"Actually, It's definitely the car.\""
        )
    );
}

#[test]
fn tab_at_the_end_of_dialogue_adds_an_instruction_then_a_new_spoken_line() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    let count = screenplay.document.blocks.len();
    let end = screenplay.document.blocks[4].text.chars().count();
    editor.focus(&mut screenplay, 4, end);
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    editor.type_text(&mut screenplay, "whispering");
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    editor.type_text(&mut screenplay, "We should go.");
    assert_eq!(screenplay.document.blocks.len(), count + 2);
    assert_eq!(screenplay.document.blocks[5].text, "(whispering)");
    assert_eq!(screenplay.document.blocks[6].kind, Element::Dialogue);
    assert_eq!(screenplay.document.blocks[6].text, "We should go.");
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document.blocks.len(), count + 1);
    assert_eq!(screenplay.document.blocks[5].kind, Element::Parenthetical);
}

#[test]
fn tab_opens_existing_instructions_and_consecutive_instructions_are_preserved() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    let count = screenplay.document.blocks.len();
    editor.focus(&mut screenplay, 8, 3);
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), count);
    assert_eq!(screenplay.active, screenplay.document.blocks[9].id);
    editor.type_text(&mut screenplay, "smiling, ");
    editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(screenplay.active, screenplay.document.blocks[10].id);
    screenplay.active = screenplay.document.blocks[9].id;
    screenplay.insert(Element::Parenthetical);
    let index = screenplay.active_index();
    screenplay.document.blocks[index].text = "(softly)".into();
    screenplay.assign("JOE", character("Eli"));
    screenplay.assign("SUE", character("Ava"));
    let request: serde_json::Value =
        serde_json::from_str(&screenplay.render_request(0).unwrap()).unwrap();
    assert!(
        request["script"]
            .as_str()
            .unwrap()
            .contains("JOE (smiling, looking around; softly): \"He was an optimistic man.\"")
    );
}

#[test]
fn backspace_only_changes_format_after_all_text_is_gone() {
    for (kind, previous) in [
        (Element::Parenthetical, Element::Dialogue),
        (Element::Dialogue, Element::Character),
        (Element::Character, Element::Action),
        (Element::Action, Element::Heading),
        (Element::Transition, Element::Heading),
    ] {
        let mut screenplay = Screenplay::default();
        let mut editor = Editor::new();
        screenplay.document.blocks[4].kind = kind;
        screenplay.document.blocks[4].text = "x".into();
        let original = screenplay.document.clone();
        editor.focus(&mut screenplay, 4, 0);
        editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
        assert_eq!(
            screenplay.document, original,
            "text at the start of {kind:?} must be preserved"
        );
        editor.focus(&mut screenplay, 4, 1);
        editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
        assert!(screenplay.document.blocks[4].text.is_empty());
        assert_eq!(
            screenplay.document.blocks[4].kind, kind,
            "deleting the last character must not change format"
        );
        editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
        assert_eq!(screenplay.document.blocks[4].kind, previous);
        assert_eq!(screenplay.document.blocks.len(), original.blocks.len());
        assert_eq!(screenplay.document.blocks[4].id, original.blocks[4].id);
        assert_eq!(screenplay.document.blocks[..4], original.blocks[..4]);
        assert_eq!(screenplay.document.blocks[5..], original.blocks[5..]);
    }
}

#[test]
fn repeated_empty_backspaces_reach_a_scene_heading_and_support_undo_and_new_scenes() {
    for heading in ["EXT. BEACH - DAY", "INT. KITCHEN - NIGHT"] {
        let mut screenplay = Screenplay::default();
        let mut editor = Editor::new();
        screenplay.document.blocks[4].kind = Element::Parenthetical;
        screenplay.document.blocks[4].text.clear();
        let count = screenplay.document.blocks.len();
        let id = screenplay.document.blocks[4].id;
        editor.focus(&mut screenplay, 4, 0);
        for kind in [
            Element::Dialogue,
            Element::Character,
            Element::Action,
            Element::Heading,
        ] {
            editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
            assert_eq!(screenplay.document.blocks[4].kind, kind);
            assert!(screenplay.document.blocks[4].text.is_empty());
            assert_eq!(screenplay.document.blocks.len(), count);
            assert_eq!(screenplay.active, id);
        }
        editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
        assert_eq!(screenplay.document.blocks[4].kind, Element::Action);
        editor.key(
            &mut screenplay,
            egui::Key::Z,
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        );
        assert_eq!(screenplay.document.blocks[4].kind, Element::Heading);
        editor.type_text(&mut screenplay, heading);
        assert_eq!(screenplay.document.blocks[4].text, heading);
        editor.key(&mut screenplay, egui::Key::Enter, egui::Modifiers::NONE);
        assert_eq!(screenplay.document.blocks[5].kind, Element::Action);
        editor.type_text(&mut screenplay, "The light changes.");
        assert_eq!(screenplay.document.blocks[5].text, "The light changes.");
        assert_eq!(screenplay.document.blocks.len(), count + 1);
    }
}

#[test]
fn empty_parentheses_are_deleted_before_backspace_changes_the_element_type() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    editor.focus(&mut screenplay, 3, 3);
    editor.key(&mut screenplay, egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[4].text, "()");
    editor.type_text(&mut screenplay, "x");
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[4].text, "()");
    assert_eq!(screenplay.document.blocks[4].kind, Element::Parenthetical);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert!(screenplay.document.blocks[4].text.is_empty());
    assert_eq!(screenplay.document.blocks[4].kind, Element::Parenthetical);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[4].kind, Element::Dialogue);
}

#[test]
fn backspace_removes_an_empty_heading_and_continues_at_the_previous_element_end() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    screenplay.document.blocks[4].kind = Element::Heading;
    screenplay.document.blocks[4].text.clear();
    screenplay.document.blocks[3].text = "JOSÉ".into();
    let original = screenplay.document.clone();
    editor.focus(&mut screenplay, 4, 0);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), original.blocks.len() - 1);
    assert_eq!(screenplay.active, original.blocks[3].id);
    assert_eq!(screenplay.document.blocks[3].text, "JOSÉ");
    assert_eq!(screenplay.document.blocks[..4], original.blocks[..4]);
    assert_eq!(screenplay.document.blocks[4..], original.blocks[5..]);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks[3].text, "JOS");
    assert_eq!(screenplay.document.blocks[3].kind, Element::Character);
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document.blocks[3].text, "JOSÉ");
    editor.key(&mut screenplay, egui::Key::Z, egui::Modifiers::COMMAND);
    assert_eq!(screenplay.document, original);
    editor.key(
        &mut screenplay,
        egui::Key::Z,
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
    );
    assert_eq!(screenplay.document.blocks.len(), original.blocks.len() - 1);
}

#[test]
fn repeated_backspaces_can_clear_the_whole_script_and_leave_a_writable_heading() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    screenplay.document.blocks = [
        (Element::Heading, "INT. ROOM - DAY"),
        (Element::Action, "Rain."),
        (Element::Character, "JOE"),
        (Element::Parenthetical, "(quietly)"),
        (Element::Dialogue, "Café."),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (kind, text))| Block {
        id: id as u64,
        kind,
        text: text.into(),
    })
    .collect();
    editor.focus(&mut screenplay, 4, 5);
    let mut reached_blank = false;
    for _ in 0..100 {
        editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
        if screenplay.document.blocks.len() == 1 && screenplay.document.blocks[0].text.is_empty() {
            reached_blank = true;
            break;
        }
    }
    assert!(
        reached_blank,
        "Backspace must not get stuck at an element boundary"
    );
    assert_eq!(screenplay.document.blocks[0].kind, Element::Heading);
    for _ in 0..3 {
        editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    }
    assert_eq!(screenplay.document.blocks.len(), 1);
    editor.type_text(&mut screenplay, "EXT. ROAD - NIGHT");
    assert_eq!(screenplay.document.blocks[0].text, "EXT. ROAD - NIGHT");
}

#[test]
fn backspace_can_remove_the_first_empty_heading_without_losing_following_text() {
    let mut screenplay = Screenplay::default();
    let mut editor = Editor::new();
    screenplay.document.blocks[0].text.clear();
    let following = screenplay.document.blocks[1].clone();
    let count = screenplay.document.blocks.len();
    editor.focus(&mut screenplay, 0, 0);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(screenplay.document.blocks.len(), count - 1);
    assert_eq!(screenplay.document.blocks[0], following);
    assert_eq!(screenplay.active, following.id);
    editor.key(&mut screenplay, egui::Key::Backspace, egui::Modifiers::NONE);
    assert_eq!(
        screenplay.document.blocks[0].text,
        following
            .text
            .chars()
            .take(following.text.chars().count() - 1)
            .collect::<String>()
    );
}
