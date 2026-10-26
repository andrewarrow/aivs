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
    assert_eq!(roles(), ["JOE", "SUE"]);
    assert_eq!(
        screenplay.render_prompt().unwrap_err(),
        "Assign characters to JOE and SUE before generating."
    );
    screenplay.assign("SUE", character("Ava"));
    assert_eq!(
        screenplay.render_prompt().unwrap_err(),
        "Assign characters to JOE before generating."
    );
    screenplay.assign("JOE", character("Eli"));
    let prompt = screenplay.render_prompt().unwrap();
    assert!(prompt.contains("Cast: @eli plays JOE; @ava plays SUE."));
    assert!(!prompt.contains("@sue"));
    assert!(!prompt.contains("@joe"));
    assert_eq!(prompt.matches("SUE: \"").count(), 2);
    assert!(prompt.contains("JOE (looking around): \"He was an optimistic man.\""));
    for &(element, text) in SCENE {
        if matches!(element, Element::Dialogue | Element::Action) {
            assert!(prompt.contains(text));
        }
    }
    screenplay.assign("SUE", character("Nora"));
    screenplay.assign("UNKNOWN", character("Maya"));
    let recast = screenplay.render_prompt().unwrap();
    assert_eq!(recast, prompt.replace("@ava plays SUE", "@nora plays SUE"));
    assert_eq!(screenplay.cast.len(), 2);
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
        assert_eq!(screenplay.cast.get(role), Some(&actor));
        let output = frame(&mut screenplay, vec![]);
        let expected = format!("played by {}", actor.name);
        let annotation_count = output.shapes.iter().filter(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::Text(shape) if shape.galley.text() == expected)
        }).count();
        let occurrences = SCENE
            .iter()
            .filter(|&&(element, name)| {
                element == Element::Character && screenplay.cast.get(name) == Some(&actor)
            })
            .count();
        assert_eq!(
            annotation_count, occurrences,
            "every occurrence must reflect the new cast"
        );
    }
}
