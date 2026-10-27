use super::*;

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
