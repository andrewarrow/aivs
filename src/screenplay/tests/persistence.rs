use super::*;

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
