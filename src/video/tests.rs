use super::*;
use std::time::Instant;

#[test]
fn token_gate_requires_a_local_nonempty_hf_api_without_exposing_it() {
    let root = tempfile::tempdir().unwrap();
    assert!(!has_token(root.path()));
    for contents in [
        "HF_TOKEN=hf_other",
        "HF_API=",
        "HF_API=''",
        "HF_API=\"\"",
        "# HF_API=hf_other",
        "HF_API= # missing",
        "HF_API=\"\" # missing",
        "HF_API='   ' # missing",
        "HF_API='unterminated",
        "HF_API=hf_old\nHF_API=\n",
    ] {
        std::fs::write(root.path().join(".env"), contents).unwrap();
        assert!(!has_token(root.path()), "{contents}");
    }
    std::fs::write(root.path().join(".env"), "export HF_API='hf_test_secret'\n").unwrap();
    assert!(has_token(root.path()));
    let (sender, events) = mpsc::channel();
    let worker = Worker {
        root: root.path().to_owned(),
        sender,
        cancel: Arc::default(),
    };
    std::fs::remove_file(root.path().join(".env")).unwrap();
    worker.setup().unwrap();
    assert!(matches!(events.recv().unwrap(), Event::NeedsToken));
    assert!(events.try_recv().is_err());
    assert!(!root.path().join(".venv").exists());
}

#[test]
fn protocol_tracks_real_progress_and_requires_explicit_readiness() {
    let (sender, events) = mpsc::channel();
    let stream = br#"library stdout
AIVS_EVENT {"event":"progress","label":"Downloading","fraction":0.5}
AIVS_EVENT {"event":"needs_token"}
AIVS_EVENT {"event":"error","message":"Missing token"}
"#;
    let output = read_output(std::io::Cursor::new(stream.to_vec()), sender, true)
        .join()
        .unwrap();
    assert!(!output.ready);
    assert!(output.needs_token);
    assert_eq!(output.error.as_deref(), Some("Missing token"));
    assert!(matches!(events.recv().unwrap(), Event::Log(line) if line == "library stdout"));
    assert!(
        matches!(events.recv().unwrap(), Event::Progress(label, Some(0.5)) if label == "Downloading")
    );
}

#[test]
fn setup_errors_and_missing_token_stay_locked_until_a_ready_event() {
    let mut workspace = VideoWorkspace::idle(PathBuf::from("/workspace"));
    assert!(!workspace.is_ready());
    workspace.handle_event(Event::Installing);
    assert!(!workspace.is_ready());
    workspace.handle_event(Event::Failed("network unavailable".into()));
    assert!(!workspace.is_ready());
    workspace.handle_event(Event::NeedsToken);
    assert!(!workspace.is_ready());
    workspace.handle_event(Event::Ready);
    assert!(workspace.is_ready());
    assert!(workspace.error.is_none());
    workspace.rendering = true;
    workspace.handle_event(Event::Failed("GPU error".into()));
    assert!(workspace.is_ready());
    assert!(!workspace.rendering);
}

#[test]
fn fatal_generation_errors_alert_once_without_a_ui_frame() {
    let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
    let (sender, events) = mpsc::channel();
    workspace.job = Some(Job {
        events,
        cancel: Arc::default(),
        thread: None,
    });
    workspace.rendering = true;
    workspace.remaining_seconds = Some(200.0);
    sender.send(Event::Log("Retrying a take".into())).unwrap();
    workspace.poll(&egui::Context::default());
    assert!(workspace.take_failure_alert().is_none());
    assert!(workspace.has_job());
    sender.send(Event::Failed("GPU error".into())).unwrap();
    workspace.poll(&egui::Context::default());
    assert_eq!(workspace.take_failure_alert().as_deref(), Some("GPU error"));
    assert_eq!(workspace.status, "Video generation failed.");
    assert!(workspace.remaining_seconds.is_none());
    assert_eq!(workspace.error.as_deref(), Some("GPU error"));
    assert!(!workspace.has_job());
    assert!(!workspace.is_rendering());
    workspace.poll(&egui::Context::default());
    assert!(workspace.take_failure_alert().is_none());
    // A duplicate terminal event cannot alert again after the job has ended.
    workspace.handle_event(Event::Failed("GPU error".into()));
    assert!(workspace.take_failure_alert().is_none());
}

#[test]
fn setup_stop_success_and_cancel_races_do_not_alert() {
    let mut workspace = VideoWorkspace::idle(PathBuf::from("/workspace"));
    workspace.handle_event(Event::Failed("Setup failed".into()));
    assert!(workspace.take_failure_alert().is_none());
    workspace.handle_event(Event::Ready);
    workspace.rendering = true;
    workspace.handle_event(Event::Stopped);
    assert!(workspace.take_failure_alert().is_none());
    workspace.rendering = true;
    workspace.handle_event(Event::Complete(PathBuf::from("/workspace/video.mp4")));
    assert!(workspace.take_failure_alert().is_none());
    workspace.rendering = true;
    workspace.stopping = true;
    workspace.handle_event(Event::Failed("Renderer exited while stopping".into()));
    assert!(workspace.take_failure_alert().is_none());
}

#[test]
fn unexpectedly_disconnected_workers_fail_and_alert_unless_cancelled() {
    for cancelled in [false, true] {
        let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
        let (sender, events) = mpsc::channel();
        drop(sender);
        workspace.rendering = true;
        workspace.job = Some(Job {
            events,
            cancel: Arc::new(AtomicBool::new(cancelled)),
            thread: None,
        });
        workspace.poll(&egui::Context::default());
        assert!(!workspace.has_job());
        assert!(!workspace.is_rendering());
        assert_eq!(workspace.take_failure_alert().is_some(), !cancelled);
        assert_eq!(workspace.error.is_some(), !cancelled);
    }
}

#[test]
fn render_progress_keeps_estimates_separate_from_logs_and_completes_only_on_success() {
    let (sender, events) = mpsc::channel();
    let stream = br#"AIVS_EVENT {"event":"render_progress","label":"Stage 1/2 denoising","fraction":0.35,"remaining_seconds":240}
AIVS_EVENT {"event":"render_progress","label":"Stage 2/2 denoising","fraction":0.3,"remaining_seconds":300}
AIVS_EVENT {"event":"render_progress","label":"Validating","fraction":1,"remaining_seconds":null}
"#;
    read_output(std::io::Cursor::new(stream.to_vec()), sender, true)
        .join()
        .unwrap();
    let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
    workspace.rendering = true;
    workspace.handle_event(events.recv().unwrap());
    workspace.handle_event(Event::Log("a transformer diagnostic".into()));
    assert_eq!(workspace.fraction, Some(0.35));
    assert_eq!(workspace.status, "Stage 1/2 denoising");
    assert_eq!(workspace.remaining_seconds, Some(240.0));
    workspace.handle_event(events.recv().unwrap());
    assert_eq!(workspace.fraction, Some(0.35));
    workspace.handle_event(events.recv().unwrap());
    assert_eq!(workspace.fraction, Some(0.99));
    assert!(workspace.rendering);
    assert!(workspace.output.is_none());
    workspace.handle_event(Event::Complete(PathBuf::from("/workspace/video.mp4")));
    assert_eq!(workspace.fraction, Some(1.0));
    assert!(!workspace.rendering);
    assert!(workspace.reveal_output);
    assert!(workspace.remaining_seconds.is_none());
}

#[test]
fn planning_activity_is_visible_without_replacing_the_phase_or_inventing_progress() {
    let (sender, events) = mpsc::channel();
    let stream = br#"AIVS_EVENT {"event":"progress","label":"Preparing shooting script with Codex"}
AIVS_EVENT {"event":"activity_delta","id":"plan-1","delta":"Mapping dialogue "}
AIVS_EVENT {"event":"activity_delta","id":"plan-1","delta":"to shots. \ud83e\udded"}
"#;
    read_output(std::io::Cursor::new(stream.to_vec()), sender, true)
        .join()
        .unwrap();
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
        workspace.rendering = true;
        workspace.started_at = Instant::now() - Duration::from_secs(65);
        workspace.output = Some(PathBuf::from("/workspace/previous-video.mp4"));
        workspace.handle_event(Event::Progress(
            "Preparing shooting script with Codex".into(),
            None,
        ));
        for event in [
            Event::LogDelta {
                id: "plan-1".into(),
                delta: "Mapping dialogue ".into(),
            },
            Event::LogDelta {
                id: "plan-1".into(),
                delta: "to shots. 🧭".into(),
            },
        ] {
            workspace.handle_event(event);
        }
        assert_eq!(workspace.status, "Preparing shooting script with Codex");
        assert!(workspace.fraction.is_none());
        let ctx = egui::Context::default();
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        let mut output = None;
        for _ in 0..3 {
            output = Some(ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        workspace.ui(ui, &crate::screenplay::Screenplay::default())
                    });
                    assert!(ui.min_rect().right() <= bounds.right() + 1.0);
                },
            ));
        }
        let output = output.unwrap();
        for expected in [
            "Preparing shooting script with Codex · 1m 5s elapsed",
            "Codex: Mapping dialogue to shots. 🧭",
        ] {
            let visible = output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text)
                    if text.galley.text() == expected && text.visual_bounding_rect().intersects(shape.clip_rect))
            });
            assert!(visible, "{expected} should be visible at {width}×{height}");
        }
    }
    let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
    for event in events.try_iter() {
        workspace.handle_event(event);
    }
    assert_eq!(
        workspace.logs.back().unwrap(),
        "Codex: Mapping dialogue to shots. 🧭"
    );
    assert_eq!(workspace.status, "Preparing shooting script with Codex");
}

#[test]
fn activity_delta_transport_coalesces_only_consecutive_fragments_from_each_stream() {
    let (sender, events) = mpsc::channel();
    let stream = br#"AIVS_EVENT {"event":"activity_delta","id":"one","delta":"A "}
AIVS_EVENT {"event":"activity_delta","id":"one","delta":"caf\u00e9\ncontinues"}
AIVS_EVENT {"event":"activity","message":"renderer diagnostic"}
AIVS_EVENT {"event":"activity_delta","id":"one","delta":" after diagnostic"}
AIVS_EVENT {"event":"activity_delta","id":"two","delta":"Second"}
AIVS_EVENT {"event":"activity_delta","id":"two","delta":" stream"}
"#;
    read_output(std::io::Cursor::new(stream.to_vec()), sender, true)
        .join()
        .unwrap();

    let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
    for event in events.try_iter() {
        workspace.handle_event(event);
    }
    assert_eq!(
        workspace.logs,
        VecDeque::from([
            "Codex: A café\ncontinues".into(),
            "renderer diagnostic".into(),
            "Codex:  after diagnostic".into(),
            "Codex: Second stream".into(),
        ])
    );
}

#[test]
fn completed_video_has_a_visible_player_at_supported_sizes() {
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        let mut workspace = VideoWorkspace::ready_for_test(PathBuf::from("/workspace"));
        let path = PathBuf::from("/workspace/folder with spaces/視訊 #1.mp4");
        workspace.handle_event(Event::Complete(path.clone()));
        let ctx = egui::Context::default();
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        for _ in 0..4 {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        workspace.ui(ui, &crate::screenplay::Screenplay::default())
                    });
                    assert!(ui.min_rect().right() <= bounds.right() + 1.0);
                },
            );
        }
        let target = workspace.playback_target.as_ref().unwrap();
        assert_eq!(target.path, path);
        assert!(target.rect.intersects(target.clip_rect));
        assert!(target.rect.right() <= bounds.right());
        assert!(target.rect.height() >= 44.0);
    }
}

#[test]
fn scene_selector_lists_headings_selects_one_scene_and_fits_supported_sizes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("scenes.fountain");
    let long_heading = format!("INT. {} - NIGHT", "A VERY LONG LOCATION NAME ".repeat(15));
    std::fs::write(
        &path,
        format!(
            "EXT. ROAD - DAY\n\nDust blows.\n\nINT. DINER - NIGHT\n\nRain falls.\n\n\
             EXT. ROAD - DAY\n\nThe sun sets.\n\n{long_heading}\n\nA door opens.\n"
        ),
    )
    .unwrap();
    let replacement = root.path().join("replacement.fountain");
    std::fs::write(&replacement, "INT. STATION - DAY\n\nA train arrives.\n").unwrap();
    let headingless = root.path().join("headingless.fountain");
    std::fs::write(&headingless, "A scene needs a heading.\n").unwrap();
    for (width, height) in [
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        let mut screenplay = crate::screenplay::Screenplay::load(root.path());
        screenplay.import_fountain(&path).unwrap();
        let scenes = screenplay.scenes();
        let first_id = scenes[0].id;
        let second_id = scenes[1].id;
        let mut workspace = VideoWorkspace::ready_for_test(root.path().to_owned());
        let ctx = egui::Context::default();
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        let mut time = 0.0;
        let mut frame =
            |workspace: &mut VideoWorkspace, screenplay: &crate::screenplay::Screenplay, events| {
                time += 0.05;
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(bounds),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| workspace.ui(ui, screenplay));
                        assert!(ui.min_rect().right() <= bounds.right() + 1.0);
                        assert!(ui.min_rect().bottom() <= bounds.bottom() + 1.0);
                    },
                )
            };
        let text_rect = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.text() == label
                            && text.visual_bounding_rect().intersects(shape.clip_rect) =>
                    {
                        Some(text.visual_bounding_rect())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label} should be visible at {width}×{height}"))
        };
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut workspace, &screenplay, vec![]);
        let output = frame(&mut workspace, &screenplay, vec![]);
        assert_eq!(workspace.selected_scene, Some(first_id));
        let selected = text_rect(&output, "1. EXT. ROAD - DAY");
        assert!(selected.bottom() < text_rect(&output, "Generate video").top());
        let pos = selected.center();
        frame(
            &mut workspace,
            &screenplay,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        frame(&mut workspace, &screenplay, vec![pointer(pos, false)]);
        let output = frame(&mut workspace, &screenplay, vec![]);
        for label in ["2. INT. DINER - NIGHT", "3. EXT. ROAD - DAY"] {
            assert!(bounds.contains_rect(text_rect(&output, label)));
        }
        let pos = text_rect(&output, "2. INT. DINER - NIGHT").center();
        frame(
            &mut workspace,
            &screenplay,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        frame(&mut workspace, &screenplay, vec![pointer(pos, false)]);
        let output = frame(&mut workspace, &screenplay, vec![]);
        assert_eq!(workspace.selected_scene, Some(second_id));
        text_rect(&output, "2. INT. DINER - NIGHT");
        let request: serde_json::Value =
            serde_json::from_str(&screenplay.render_request(second_id).unwrap()).unwrap();
        assert_eq!(request["script"], "INT. DINER - NIGHT\n\nRain falls.");

        screenplay.import_fountain(&replacement).unwrap();
        frame(&mut workspace, &screenplay, vec![]);
        assert_eq!(workspace.selected_scene, Some(screenplay.scenes()[0].id));
        screenplay.import_fountain(&headingless).unwrap();
        let output = frame(&mut workspace, &screenplay, vec![]);
        assert!(workspace.selected_scene.is_none());
        text_rect(&output, "No scenes");
        text_rect(&output, "Add a scene heading before generating.");
    }
}

#[test]
fn startup_modal_and_render_states_fit_supported_sizes() {
    for (width, height) in [
        (390.0, 360.0),
        (390.0, 844.0),
        (768.0, 1024.0),
        (1280.0, 800.0),
        (1440.0, 900.0),
    ] {
        for setup in [
            Setup::Checking,
            Setup::NeedsToken,
            Setup::Installing,
            Setup::Failed,
            Setup::Ready,
        ] {
            let mut workspace = VideoWorkspace::idle(PathBuf::from(format!(
                "/workspace/{}",
                "long folder name/".repeat(12)
            )));
            workspace.setup = setup;
            workspace.status = "Downloading large model files. ".repeat(12);
            workspace.error =
                Some("Connection unavailable. Retry setup when you are online. ".repeat(8));
            workspace
                .logs
                .push_back("a long renderer message ".repeat(25));
            let ctx = egui::Context::default();
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            for _ in 0..3 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(bounds),
                        ..Default::default()
                    },
                    |ui| {
                        if workspace.is_ready() {
                            egui::CentralPanel::default().show(ui, |ui| {
                                workspace.ui(ui, &crate::screenplay::Screenplay::default())
                            });
                        } else {
                            workspace.setup_ui(ui);
                        }
                        assert!(
                            ui.min_rect().right() <= bounds.right() + 1.0,
                            "setup={:?}, min={:?}, bounds={bounds:?}",
                            workspace.setup,
                            ui.min_rect()
                        );
                        assert!(ui.min_rect().bottom() <= bounds.bottom() + 1.0);
                    },
                );
                if let Some(layer) = ctx.memory(|memory| memory.top_modal_layer()) {
                    let rect = ctx.memory(|memory| memory.area_rect(layer.id)).unwrap();
                    assert!(rect.right() <= bounds.right() + 1.0, "{rect:?} {bounds:?}");
                    assert!(
                        rect.bottom() <= bounds.bottom() + 1.0,
                        "{rect:?} {bounds:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn child_failure_cannot_be_mistaken_for_a_ready_environment() {
    let root = tempfile::tempdir().unwrap();
    let (sender, _events) = mpsc::channel();
    let worker = Worker {
        root: root.path().to_owned(),
        sender,
        cancel: Arc::default(),
    };
    let mut command = Command::new("python3");
    command.args([
        "-c",
        "print('AIVS_EVENT {\"event\":\"ready\"}'); raise SystemExit(1)",
    ]);
    assert!(worker.run(command).is_err());
}

#[cfg(unix)]
#[test]
fn screenplay_request_reaches_python_and_its_translated_shot_reaches_the_renderer() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let python = root.path().join(".venv/bin/python");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(
        &python,
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path('args.json').write_text(json.dumps(sys.argv[1:]))
sys.path.insert(0, str(pathlib.Path(sys.argv[2]).parent))
import video
video.check_environment = lambda: None
assert os.environ.get('AIVS_CODEX_PATH'), 'the configured Codex executable must reach Python'
expected_request = json.loads(pathlib.Path('expected-request.json').read_text())
expected_profiles = [
    {'id': 'eli', 'name': 'Eli', 'description': 'A dark-haired traveler.', 'voice_description': 'Warm tenor.', 'voice_id': 'eli-voice'},
    {'id': 'ava', 'name': 'Ava', 'description': 'A red-haired traveler.', 'voice_description': 'Clear alto.', 'voice_id': 'ava-voice'},
]
video.characters.resolve_characters = lambda *args, **kwargs: expected_profiles
video.characters.snapshot_characters = lambda profiles, *args, **kwargs: profiles
video.prompt_token_counter = lambda: (lambda text: len(text.split()))
def create(request, profiles, run_dir, **options):
    assert request == expected_request
    assert profiles == expected_profiles
    options['on_activity']('Codex: Mapping dialogue to shots.')
    options['on_activity_delta']('codex-stream', 'Drafting shot ✨')
    pathlib.Path(run_dir, 'shooting-script.json').write_text('{}')
    return {'version': 1, 'render_format': request['render_format'], 'source_sha256': 'test', 'seconds': 8, 'fade_out': True, 'shots': [
        {'seconds': 8, 'beat_ids': [1], 'prompt_template': 'A traveler turns toward the road.',
         'prompt': 'Eli as JOE turns toward the road.', 'start_state': 'Beside the car.',
         'end_state': 'Facing the road.', 'continue_previous': False}
    ]}
video.create_shooting_script = create
def render(plan, seed, output, **options):
    assert plan['render_format'] == 'portrait'
    assert plan['shots'][0]['prompt'] == 'Eli as JOE turns toward the road.'
    output.write_bytes(b'test video')
    return {'output_frames': plan['seconds'] * 24}
video.render_shooting_script = render
sys.argv = sys.argv[2:]
raise SystemExit(video.main())
"#,
    )
    .unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (sender, events) = mpsc::channel();
    let worker = Worker {
        root: root.path().to_owned(),
        sender,
        cancel: Arc::default(),
    };
    let mut screenplay = crate::screenplay::Screenplay::default();
    let path = root.path().join("scenes.fountain");
    std::fs::write(
        &path,
        "EXT. GARDEN - DAY\n\nUNCAST\nWait here.\n\n\
         EXT. ROAD - DUSK\n\nJoe turns toward the road.\n\nJOE\nReady?\n\nSUE\nReady.\n\n\
         INT. DINER - NIGHT\n\nThe door closes.\n",
    )
    .unwrap();
    screenplay.import_fountain(&path).unwrap();
    screenplay.assign(
        "SUE",
        crate::characters::CastCharacter {
            id: "ava".into(),
            name: "Ava".into(),
        },
    );
    screenplay.assign(
        "JOE",
        crate::characters::CastCharacter {
            id: "eli".into(),
            name: "Eli".into(),
        },
    );
    let mut workspace = VideoWorkspace::ready_for_test(root.path().to_owned());
    workspace.render_format = crate::settings::RenderFormat::Portrait;
    let request = workspace
        .render_request(&screenplay, screenplay.scenes()[1].id)
        .unwrap();
    let request_value: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(
        request_value["script"],
        "EXT. ROAD - DUSK\n\nJoe turns toward the road.\n\nJOE: \"Ready?\"\n\nSUE: \"Ready.\""
    );
    assert_eq!(request_value["cast"].as_array().unwrap().len(), 2);
    assert_eq!(request_value["render_format"], "portrait");
    std::fs::write(
        root.path().join("expected-request.json"),
        serde_json::to_vec(&request_value).unwrap(),
    )
    .unwrap();
    worker.render(&request).unwrap();
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(root.path().join("args.json")).unwrap()).unwrap();
    assert_eq!(
        args,
        ["-u", SCRIPT, "render", "--screenplay-json", &request]
    );
    let events: Vec<_> = events.try_iter().collect();
    assert!(events.iter().any(
        |event| matches!(event, Event::Log(line) if line == "Codex: Mapping dialogue to shots.")
    ));
    assert!(events.iter().any(|event| matches!(event,
        Event::LogDelta { id, delta } if id == "codex-stream" && delta == "Drafting shot ✨"
    )));
    let path = events
        .into_iter()
        .find_map(|event| match event {
            Event::Complete(path) => Some(path),
            _ => None,
        })
        .expect("Python should complete the mocked inference");
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.parent().unwrap().join("render.json")).unwrap())
            .unwrap();
    assert_eq!(report["original_script"], request_value["script"]);
    assert_eq!(report["cast"], request_value["cast"]);
    assert_eq!(report["characters"][0]["id"], "eli");
    assert_eq!(report["characters"][1]["id"], "ava");
    assert_eq!(report["shooting_script"], "shooting-script.json");
    assert_eq!(report["seconds"], 8);
    assert_eq!(report["render_format"], "portrait");
    assert_eq!(report["width"], 1080);
    assert_eq!(report["height"], 1920);
    assert_eq!(
        report["shots"][0]["prompt"],
        "Eli as JOE turns toward the road."
    );
    assert_eq!(report["runtime"]["output_frames"].as_u64().unwrap(), 8 * 24);
}

#[cfg(unix)]
#[test]
fn stopping_a_job_terminates_its_process_group_and_unblocks_output_readers() {
    let root = tempfile::tempdir().unwrap();
    let (sender, events) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        root: root.path().to_owned(),
        sender,
        cancel: cancel.clone(),
    };
    let thread = thread::spawn(move || {
        let mut command = Command::new("python3");
        command.args(["-u", "-c", "import subprocess,time; subprocess.Popen(['sleep','30']); print('started',flush=True); time.sleep(30)"]);
        worker.run(command)
    });
    assert!(
        matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Log(line) if line == "started")
    );
    let started = Instant::now();
    cancel.store(true, Ordering::Relaxed);
    assert!(thread.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "installs the pinned Python environment; set AIVS_TEST_CACHE to an existing Hugging Face cache"]
fn live_setup_installs_python_then_skips_installation_on_the_next_launch() {
    let cache = PathBuf::from(std::env::var_os("AIVS_TEST_CACHE").expect("set AIVS_TEST_CACHE"));
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "HF_API=hf_offline_test_only\n").unwrap();
    std::fs::create_dir(root.path().join(".cache")).unwrap();
    std::os::unix::fs::symlink(cache, root.path().join(".cache/huggingface")).unwrap();
    for install in [true, false] {
        let (sender, events) = mpsc::channel();
        let worker = Worker {
            root: root.path().to_owned(),
            sender,
            cancel: Arc::default(),
        };
        worker.setup().unwrap();
        drop(worker);
        let events: Vec<_> = events.into_iter().collect();
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, Event::Installing)),
            install
        );
        assert!(events.iter().any(|event| matches!(event, Event::Ready)));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::NeedsToken | Event::Failed(_)))
        );
    }
}
