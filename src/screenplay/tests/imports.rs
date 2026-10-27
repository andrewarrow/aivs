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
