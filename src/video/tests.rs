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
                    egui::CentralPanel::default()
                        .show(ui, |ui| workspace.ui(ui, &Ok("test prompt".into())));
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
                            egui::CentralPanel::default()
                                .show(ui, |ui| workspace.ui(ui, &Ok("test prompt".into())));
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
def create(request, profiles, run_dir, **options):
    assert request == expected_request
    assert profiles == expected_profiles
    pathlib.Path(run_dir, 'shooting-script.json').write_text('{}')
    return {'version': 1, 'source_sha256': 'test', 'seconds': 8, 'fade_out': True, 'shots': [
        {'seconds': 8, 'beat_ids': [1], 'prompt_template': 'A traveler turns toward the road.',
         'prompt': 'Eli as JOE turns toward the road.', 'start_state': 'Beside the car.',
         'end_state': 'Facing the road.', 'continue_previous': False}
    ]}
video.create_shooting_script = create
def render(plan, seed, output, **options):
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
    let request = screenplay.render_request().unwrap();
    let request_value: serde_json::Value = serde_json::from_str(&request).unwrap();
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
    let path = events
        .try_iter()
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
