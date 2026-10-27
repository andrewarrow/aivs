use super::*;

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
