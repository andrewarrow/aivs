//! Production rules and Codex sessions without a desktop, GPU, or Python runtime.
pub mod artifacts;
pub mod character_conditioning;
pub mod characters;
pub mod codex;
pub mod continuity;
pub mod dialogue;
pub mod downloads;
pub mod inference;
pub mod media;
pub mod motion;
pub mod planning;
pub mod preproduction;
pub mod prop_reference;
pub mod render_estimate;
pub mod render_formats;
pub mod shooting_script;
pub mod shot_renderer;
pub mod storyboard;
pub mod storyboard_backend;
pub mod storyboard_images;
pub mod visual_review;

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub const PROTOCOL_VERSION: u64 = 1;

/// One request and response per line. Domain failures never terminate the worker.
/// Legacy screenplay fingerprints remain Python-owned; image artifacts are Rust-owned.
pub fn serve(reader: impl BufRead + Send + 'static, mut writer: impl Write) -> std::io::Result<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let parent_closed = cancelled.clone();
    let (input, requests) = mpsc::channel();
    std::thread::spawn(move || {
        for line in reader.lines() {
            if input.send(line).is_err() {
                break;
            }
        }
        parent_closed.store(true, Ordering::Relaxed);
    });
    for line in requests {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let mut error_kind = "validation";
                let result = if request["version"].as_u64() == Some(PROTOCOL_VERSION) {
                    if request["operation"] == "implementation" {
                        Ok(json!({
                            "crates/aivs-core/src/lib.rs": include_str!("lib.rs"),
                            "crates/aivs-core/src/planning.rs": include_str!("planning.rs"),
                            "crates/aivs-core/src/inference.rs": include_str!("inference.rs"),
                            "crates/aivs-core/src/downloads.rs": include_str!("downloads.rs"),
                            "crates/aivs-core/src/preproduction/storage.rs": include_str!("preproduction/storage.rs"),
                            "crates/aivs-core/src/storyboard.rs": include_str!("storyboard.rs"),
                            "crates/aivs-core/src/shot_renderer/storage.rs": include_str!("shot_renderer/storage.rs"),
                            "crates/aivs-core/src/prop_reference/production.rs": include_str!("prop_reference/production.rs"),
                            "crates/aivs-core/src/prop_reference/schema.json": include_str!("prop_reference/schema.json"),
                            "crates/aivs-core/src/prop_reference/instructions.txt": include_str!("prop_reference/instructions.txt"),
                            "crates/aivs-core/src/shooting_script/batch.rs": include_str!("shooting_script/batch.rs"),
                            "crates/aivs-core/src/shooting_script/storage.rs": include_str!("shooting_script/storage.rs"),
                            "crates/aivs-core/src/visual_review/production.rs": include_str!("visual_review/production.rs"),
                            "crates/aivs-core/src/visual_review/schemas.json": include_str!("visual_review/schemas.json"),
                            "crates/aivs-core/src/visual_review/prompts.json": include_str!("visual_review/prompts.json"),
                            "crates/aivs-core/src/artifacts.rs": include_str!("artifacts.rs"),
                            "crates/aivs-core/src/characters.rs": include_str!("characters.rs"),
                            "crates/aivs-core/src/character_conditioning.rs": include_str!("character_conditioning.rs"),
                            "crates/aivs-core/src/characters/prompt.rs": include_str!("characters/prompt.rs"),
                            "crates/aivs-core/src/dialogue.rs": include_str!("dialogue.rs"),
                            "crates/aivs-core/src/media/audio.rs": include_str!("media/audio.rs"),
                            "crates/aivs-core/src/media/animatic.rs": include_str!("media/animatic.rs"),
                            "crates/aivs-core/src/media/inference.rs": include_str!("media/inference.rs"),
                            "crates/aivs-core/src/media/source.rs": include_str!("media/source.rs"),
                            "crates/aivs-core/src/media/review.rs": include_str!("media/review.rs"),
                            "crates/aivs-core/src/storyboard_backend.rs": include_str!("storyboard_backend.rs"),
                            "crates/aivs-core/src/storyboard_backend/preflight.rs": include_str!("storyboard_backend/preflight.rs"),
                            "crates/aivs-core/src/storyboard_backend/session.rs": include_str!("storyboard_backend/session.rs"),
                            "crates/aivs-core/src/storyboard_images.rs": include_str!("storyboard_images.rs"),
                            "crates/aivs-core/src/continuity.rs": include_str!("continuity.rs"),
                            "crates/aivs-core/src/continuity/acceptance.rs": include_str!("continuity/acceptance.rs"),
                            "crates/aivs-core/src/continuity/artifacts.rs": include_str!("continuity/artifacts.rs"),
                            "crates/aivs-core/src/continuity/dispatch.rs": include_str!("continuity/dispatch.rs"),
                            "crates/aivs-core/src/continuity/facts.rs": include_str!("continuity/facts.rs"),
                            "crates/aivs-core/src/continuity/plan.rs": include_str!("continuity/plan.rs"),
                            "crates/aivs-core/src/continuity/review.rs": include_str!("continuity/review.rs"),
                            "crates/aivs-core/src/continuity/schemas.rs": include_str!("continuity/schemas.rs"),
                            "crates/aivs-core/src/continuity/types.rs": include_str!("continuity/types.rs"),
                            "crates/aivs-core/src/continuity/world.rs": include_str!("continuity/world.rs"),
                            "crates/aivs-core/src/codex.rs": include_str!("codex.rs"),
                            "crates/aivs-core/src/motion.rs": include_str!("motion.rs"),
                            "crates/aivs-core/src/media.rs": include_str!("media.rs"),
                            "crates/aivs-core/src/motion/planning.rs": include_str!("motion/planning.rs"),
                            "crates/aivs-core/src/motion/review.rs": include_str!("motion/review.rs"),
                            "crates/aivs-core/src/prop_reference.rs": include_str!("prop_reference.rs"),
                            "crates/aivs-core/src/render_formats.rs": include_str!("render_formats.rs"),
                            "crates/aivs-core/src/render_estimate.rs": include_str!("render_estimate.rs"),
                            "crates/aivs-core/src/preproduction.rs": include_str!("preproduction.rs"),
                            "crates/aivs-core/src/shot_renderer.rs": include_str!("shot_renderer.rs"),
                            "crates/aivs-core/src/shooting_script.rs": include_str!("shooting_script.rs"),
                            "crates/aivs-core/src/shooting_script/compile.rs": include_str!("shooting_script/compile.rs"),
                            "crates/aivs-core/src/shooting_script/prompt.rs": include_str!("shooting_script/prompt.rs"),
                            "crates/aivs-core/src/shooting_script/approval.rs": include_str!("shooting_script/approval.rs"),
                            "crates/aivs-core/src/shooting_script/schemas.rs": include_str!("shooting_script/schemas.rs"),
                            "crates/aivs-core/src/shooting_script/source.rs": include_str!("shooting_script/source.rs"),
                            "crates/aivs-core/src/shooting_script/unicode.rs": include_str!("shooting_script/unicode.rs"),
                            "crates/aivs-core/src/visual_review.rs": include_str!("visual_review.rs"),
                        }))
                    } else if request["operation"] == "planning_budget" {
                        planning::budget(&request["args"])
                    } else if request["operation"] == "valid_cached_model_file" {
                        Ok(downloads::cached_file(&request["args"]))
                    } else if request["operation"] == "preflight_downloads" {
                        error_kind = "inference";
                        downloads::preflight(&request["args"])
                    } else if request["operation"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("take_") && op != "take_seed")
                    {
                        shot_renderer::storage(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                            &cancelled,
                        )
                    } else if request["operation"] == "inference" {
                        error_kind = "inference";
                        inference::run(&request["args"], cancelled.clone(), |event| {
                            let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                            serde_json::to_writer(&mut writer, &envelope)
                                .map_err(|e| e.to_string())?;
                            writer
                                .write_all(b"\n")
                                .and_then(|_| writer.flush())
                                .map_err(|e| e.to_string())
                        })
                    } else if request["operation"] == "prop_failure_summary" {
                        prop_reference::failure_summary(&request["args"]["report"])
                            .map(Value::String)
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "storyboard_reference_specs"
                                | "storyboard_visible_state"
                                | "storyboard_resolve_image"
                                | "storyboard_prepare"
                                | "storyboard_prepare_references"
                                | "storyboard_frame_request"
                        )
                    ) {
                        storyboard::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "planning_attempt"
                                | "visual_review_attempt"
                                | "visual_review_shot"
                                | "visual_review_scene"
                                | "visual_review_storyboard"
                                | "prop_review"
                        )
                    ) {
                        let operation = request["operation"].as_str().unwrap_or_default();
                        let mut emit = |event| {
                            let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                            serde_json::to_writer(&mut writer, &envelope)
                                .map_err(|e| e.to_string())?;
                            writer
                                .write_all(b"\n")
                                .and_then(|_| writer.flush())
                                .map_err(|e| e.to_string())
                        };
                        match operation {
                            "prop_review" => {
                                error_kind = "codex";
                                prop_reference::review(
                                    &request["args"],
                                    cancelled.clone(),
                                    &mut emit,
                                )
                            }
                            "planning_attempt" | "visual_review_attempt" => {
                                let result = if operation == "planning_attempt" {
                                    planning::attempt(
                                        &request["args"],
                                        cancelled.clone(),
                                        &mut emit,
                                    )
                                } else {
                                    planning::review_attempt(
                                        &request["args"],
                                        cancelled.clone(),
                                        &mut emit,
                                    )
                                };
                                result.map_err(|failure| {
                                    error_kind = failure.kind;
                                    failure.message
                                })
                            }
                            _ => {
                                error_kind = "codex";
                                visual_review::run(
                                    operation,
                                    &request["args"],
                                    cancelled.clone(),
                                    &mut emit,
                                )
                            }
                        }
                    } else if request["operation"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("character_"))
                    {
                        characters::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if request["operation"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("conditioning_"))
                    {
                        character_conditioning::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"], &cancelled, |event| {
                                let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                                serde_json::to_writer(&mut writer, &envelope).map_err(|e| e.to_string())?;
                                writer.write_all(b"\n").and_then(|_| writer.flush()).map_err(|e| e.to_string())
                            },
                        ).map_err(|error| {
                            if error.starts_with("Saved-character voices require") || error.starts_with("Saved voice conversion failed")
                                || error.starts_with("Saved voice converter returned") {
                                error_kind = "inference";
                            }
                            error
                        })
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "prepare_source_image"
                                | "prepare_ltx_render"
                                | "prepare_source_audio"
                                | "prepare_image_anchors"
                        )
                    ) {
                        media::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                            &cancelled,
                        )
                    } else if request["operation"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("render_estimate_"))
                    {
                        render_estimate::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if request["operation"] == "duration_estimate" {
                        request["args"]["prompt"]
                            .as_str()
                            .ok_or_else(|| "Duration estimate needs prompt text.".into())
                            .and_then(dialogue::estimate)
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "imagegen_configuration"
                                | "imagegen_check_local"
                                | "imagegen_check"
                                | "imagegen_generate"
                                | "prepare_image_references"
                        )
                    ) {
                        error_kind = "imagegen";
                        storyboard_backend::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                            cancelled.clone(),
                            |event| {
                                let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                                serde_json::to_writer(&mut writer, &envelope)
                                    .map_err(|e| e.to_string())?;
                                writer
                                    .write_all(b"\n")
                                    .and_then(|_| writer.flush())
                                    .map_err(|e| e.to_string())
                            },
                        )
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "parse_shooting_request"
                                | "shooting_validate_saved"
                                | "shooting_publish"
                                | "validate_motion_batch"
                                | "shooting_schema"
                                | "compile_shooting_script"
                                | "shooting_fingerprint_content"
                                | "shooting_fidelity_requirements"
                                | "validate_shooting_fidelity"
                                | "validate_shooting_review"
                                | "shooting_script_display"
                                | "shooting_instruction_context"
                        )
                    ) {
                        shooting_script::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "extract_last_frame"
                                | "finish_ltx_render"
                                | "assemble_animatic"
                                | "assemble_shots"
                                | "validate_video"
                                | "validate_supplied_audio"
                                | "visual_review_sheets"
                        )
                    ) {
                        error_kind = "media";
                        media::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                            &cancelled,
                        )
                    } else if request["operation"] == "resolve_format" {
                        render_formats::resolve(&request["args"]["name"])
                    } else if request["operation"] == "framing_prompt" {
                        Ok(render_formats::framing(&request["args"]["name"]))
                    } else if request["operation"] == "codex" {
                        error_kind = "codex";
                        match serde_json::from_value(request["args"].clone()) {
                            Ok(args) => codex::run_production(args, cancelled.clone(), |event| {
                                let envelope = json!({"version": PROTOCOL_VERSION, "id": request["id"], "event": event});
                                serde_json::to_writer(&mut writer, &envelope).map_err(|e| e.to_string())?;
                                writer.write_all(b"\n").and_then(|_| writer.flush()).map_err(|e| e.to_string())
                            }).map_err(|failure| { error_kind = failure.kind; failure.message }),
                            Err(error) => Err(format!("Invalid Codex request: {error}")),
                        }
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "validate_render_plan"
                                | "uses_previous_frame"
                                | "take_seed"
                                | "validate_saved_take_report"
                                | "match_saved_take"
                                | "render_review_status"
                        )
                    ) {
                        shot_renderer::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "validate_storyboard_repairs"
                                | "validate_storyboard_direction"
                                | "copy_final_preproduction"
                                | "storyboard_repair_candidate"
                                | "reserve_storyboard_repair"
                                | "resume_storyboard_repair"
                                | "complete_storyboard_repair"
                        )
                    ) {
                        preproduction::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if matches!(
                        request["operation"].as_str(),
                        Some(
                            "visual_contract"
                                | "visual_schema"
                                | "visual_instruction"
                                | "validate_visual_shot"
                                | "validate_visual_preservation"
                                | "validate_visual_adaptation"
                                | "validate_visual_scene"
                        )
                    ) {
                        visual_review::dispatch(
                            request["operation"].as_str().unwrap_or_default(),
                            &request["args"],
                        )
                    } else if request["operation"] == "prop_generation_prompt" {
                        let args = &request["args"];
                        prop_reference::generation_prompt(
                            &args["definition"],
                            args.get("replacement"),
                        )
                        .map(Value::String)
                    } else if request["operation"] == "assess_prop" {
                        let args = &request["args"];
                        prop_reference::assess(&args["definition"], &args["observation"])
                            .map(|failures| json!(failures))
                    } else if request["operation"] == "validate_prop_acceptance" {
                        let args = &request["args"];
                        prop_reference::require_accepted(
                            &args["spec"],
                            &args["entry"],
                            &args["binding"],
                        )
                    } else if request["operation"] == "validate_motion_review" {
                        let args = &request["args"];
                        args["number"]
                            .as_u64()
                            .and_then(|number| usize::try_from(number).ok())
                            .ok_or_else(|| "Invalid motion shot number.".to_owned())
                            .and_then(|number| {
                                motion::validate_review(
                                    &args["plan"],
                                    number,
                                    &args["frame"],
                                    &args["value"],
                                )
                            })
                    } else if request["operation"] == "motion_fallback" {
                        let args = &request["args"];
                        args["fps"]
                            .as_u64()
                            .ok_or_else(|| "Invalid motion frame rate.".to_owned())
                            .and_then(|fps| motion::fallback(&args["shot"], fps))
                    } else if request["operation"] == "validate_motion_timing" {
                        let args = &request["args"];
                        args["fps"]
                            .as_u64()
                            .ok_or_else(|| "Invalid motion frame rate.".to_owned())
                            .and_then(|fps| {
                                motion::validate_timing(
                                    &args["plan"],
                                    &args["motion"],
                                    &args["plan_sha256"],
                                    fps,
                                )
                            })
                            .map(|()| Value::Null)
                    } else {
                        continuity::dispatch(&request)
                    }
                } else {
                    Err("Unsupported AIVS engine protocol version.".into())
                };
                match result {
                    Ok(result) => {
                        json!({"version": PROTOCOL_VERSION, "id": request["id"], "result": result})
                    }
                    Err(error) => {
                        json!({"version": PROTOCOL_VERSION, "id": request["id"], "error": error, "error_kind": error_kind})
                    }
                }
            }
            Err(error) => {
                json!({"version": PROTOCOL_VERSION, "id": null, "error": error.to_string()})
            }
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}
