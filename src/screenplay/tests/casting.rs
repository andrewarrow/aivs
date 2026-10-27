use super::*;

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
