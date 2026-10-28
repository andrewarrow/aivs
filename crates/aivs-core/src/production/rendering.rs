use super::*;
use crate::{character_conditioning, inference, media, render_formats, shot_renderer, storyboard};
use std::time::Instant;

#[derive(Default)]
struct RenderProgress {
    fraction: f64,
    estimate: Option<(Instant, f64)>,
}

impl RenderProgress {
    fn event(&mut self, fraction: f64, label: String, remaining: Option<f64>) -> Value {
        self.fraction = fraction.min(0.99).max(self.fraction);
        if let Some(seconds) = remaining {
            self.estimate = Some((Instant::now(), seconds));
        }
        let remaining = self
            .estimate
            .map(|(at, seconds)| seconds - at.elapsed().as_secs_f64())
            .filter(|seconds| *seconds > 0.0);
        json!({"type":"render_progress","data":{"label":label,"fraction":self.fraction,"remaining_seconds":remaining}})
    }
}

fn progress(
    ctx: &mut Context<'_>,
    last: &mut RenderProgress,
    fraction: f64,
    label: String,
    remaining: Option<f64>,
) -> Result<()> {
    ctx.event(last.event(fraction, label, remaining))
}
fn shot_update(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    report: &Value,
    run: &Path,
) -> Result<()> {
    shot_renderer::storage(
        "take_save",
        &json!({"shot_dir":run.join("shots").join(format!("{:03}",index+1)),"report":report}),
        &ctx.cancelled,
    )?;
    if ctx.options["update_run_report"] == true {
        let mut saved = read(&run.join("render.json"))?;
        let shared = crate::project::creative_root(run)? != run;
        let mut row = if shared {
            json!({"shot":index+1,"stable_id":plan["shots"][index]["stable_id"],
                "report":format!("shots/{:03}/render.json",index+1),
                "video":format!("shots/{:03}/video.mp4",index+1)})
        } else {
            plan["shots"][index].clone()
        };
        if shared {
            for key in ["status", "seed", "take", "seconds", "sha256"] {
                if let Some(value) = report.get(key) {
                    row[key] = value.clone();
                }
            }
            if let Some(review) = report.get("sync_review") {
                row["sync_review"] = review.clone();
            }
        } else {
            merge(&mut row, report)?;
        }
        saved["shots"][index] = row;
        save(&run.join("render.json"), &saved)?;
    }
    ctx.event(json!({"type":"shot","index":index,"report":report}))
}
fn conditioning(ctx: &mut Context<'_>, operation: &str, mut args: Value) -> Result<Value> {
    args["root"] = json!(ctx.root);
    character_conditioning::dispatch(operation, &args, &ctx.cancelled, |mut e| {
        if let Some(activity) = e.get("activity").cloned() {
            e = json!({"type":"activity","message":activity});
        }
        (ctx.emit)(e)
    })
}
pub fn render(ctx: &mut Context<'_>, args: &Value) -> Result<Value> {
    if args["render_config"]
        .get("backend")
        .is_some_and(|backend| backend != "mlx")
    {
        return Err(
            "The cloud comparison renderer has been retired; production renders use local MLX."
                .into(),
        );
    }
    let plan = &args["plan"];
    let validated = shot_renderer::validate_plan(plan)?;
    let shots = rows(&plan["shots"])?;
    let seconds = validated["seconds"]
        .as_u64()
        .ok_or("Invalid production duration.")?;
    let seed = args["seed"].as_i64().ok_or("seed must be an integer.")?;
    let review_enabled = args["review_enabled"].as_bool().unwrap_or(true);
    let automatic = review_enabled && plan["planning_policy"] == "single-render-v1";
    let requires = plan["storyboard_policy"] == STORYBOARD_POLICY
        || plan["source"]["storyboard_policy"] == STORYBOARD_POLICY;
    let boards = &args["storyboard"];
    if requires && boards.is_null() {
        return Err(
            "This plan requires its validated first-frame storyboard for every shot.".into(),
        );
    }
    let output = path(args, "output")?;
    let run = output.parent().ok_or("Missing output directory.")?;
    fs::create_dir_all(run).map_err(|e| e.to_string())?;
    let profile = args["profile"]
        .as_str()
        .or(args["render_config"]["profile"].as_str())
        .unwrap_or("distilled");
    let spec = if plan.get("render_format").is_some() {
        render_formats::for_profile(&plan["render_format"], profile)?
    } else {
        Value::Null
    };
    let (width, height) = if spec.is_null() {
        boards::dimensions(plan)?
    } else {
        (
            spec["model_width"].as_u64().ok_or("Invalid model width.")?,
            spec["model_height"]
                .as_u64()
                .ok_or("Invalid model height.")?,
        )
    };
    let bindings = if boards.is_null() {
        Value::Null
    } else {
        boards::prepare(ctx, plan, boards, run)?
    };
    if requires {
        boards::accepted(ctx, plan, boards, run)?;
    }
    let motion = &args["motion_manifest"];
    if !motion.is_null() {
        motion::accepted(ctx, plan, boards, motion, run)?;
    }
    let profiles = &args["character_profiles"];
    let assets = if profiles.is_null() {
        Value::Null
    } else {
        conditioning(
            ctx,
            "conditioning_prepare",
            json!({"plan":plan,"profiles":profiles,"run_dir":run,"storyboard_conditioned":!bindings.is_null()}),
        )?
    };
    let mut execution = plan.clone();
    let mut summaries = Vec::new();
    let mut reviews = Vec::new();
    let mut last = RenderProgress::default();
    let mut completed = 0_u64;
    let started = Instant::now();
    let config = &args["render_config"];
    for (index, planned) in shots.iter().enumerate() {
        ctx.check()?;
        let base = ((i128::from(seed) + index as i128).rem_euclid(1 << 31)) as i64;
        let shot = execution["shots"][index].clone();
        let binding = bindings.get(index).cloned().unwrap_or(Value::Null);
        let motion_row = motion["shots"].get(index).cloned().unwrap_or(Value::Null);
        let planned_hash = if automatic {
            Some(ctx.shot_hash(plan, planned)?)
        } else {
            None
        };
        let fixed_audio = dialogue_audio::select(
            ctx,
            plan,
            index,
            &assets[index]["voice"],
            run,
            args["resume"] == true,
        )?;
        if !fixed_audio.is_null() {
            ctx.activity(format!(
                "Shot {}: Reusing the reviewed dialogue performance",
                index + 1
            ))?;
        }
        let prepared = shot_renderer::storage(
            "take_prepare",
            &json!({"plan":plan,"shot":shot,"index":index,"base_seed":base,"run_dir":run,"width":width,"height":height,"frame_binding":binding,"motion_row":motion_row,"assets":assets.get(index),"previous_summary":summaries.last(),"render_config":config,"automatic_review":automatic,"planned_shot_sha256":planned_hash,"storyboard_acceptance":if requires{&boards["continuity_acceptance"]}else{&Value::Null},"dialogue_audio":fixed_audio}),
            &ctx.cancelled,
        )?;
        let shot_dir = path(&prepared, "shot_dir")?;
        let archive = path(&prepared, "archive_dir")?;
        let clip = path(&prepared, "clip")?;
        let native = path(&prepared, "native_clip")?;
        let previous = if prepared["use_previous_frame"] == true {
            Some(
                run.join("shots")
                    .join(format!("{index:03}"))
                    .join("video.mp4"),
            )
        } else {
            None
        };
        let mut report = prepared["report"].clone();
        let mut committed = false;
        let outcome = (|| -> Result<()> {
            let mut fingerprints = Vec::new();
            if args["resume"] == true {
                progress(
                    ctx,
                    &mut last,
                    last_value(&report, completed, seconds),
                    format!("Shot {}/{}: Checking saved takes", index + 1, shots.len()),
                    None,
                )?;
                let candidates = shot_renderer::storage(
                    "take_candidates",
                    &json!({"shot_dir":shot_dir,"archive_dir":archive}),
                    &ctx.cancelled,
                )?;
                for entry in rows(&candidates)? {
                    let candidate = &entry["report"];
                    if !candidate.is_null()
                        && !candidate["storyboard"].is_null()
                        && candidate["storyboard"] != binding
                    {
                        let mut adapted = planned.clone();
                        for key in ["prompt", "prompt_template", "storyboard_prompt"] {
                            adapted[key] = candidate[key].clone();
                        }
                        fingerprints.push(
                            json!({"report":candidate,"fingerprint":ctx.shot_hash(plan,&adapted)?}),
                        );
                    }
                }
            }
            let recovered = shot_renderer::storage(
                "take_recover",
                &json!({"run_dir":run,"index":index,"shot_dir":shot_dir,"archive_dir":archive,"native_clip":native,"report":report,"shot":shot,"resume":args["resume"],"convert_voice":prepared["convert_voice"],"automatic_review":automatic,"adapted_fingerprints":fingerprints,"previous_clip":previous}),
                &ctx.cancelled,
            )?;
            report = recovered["report"].clone();
            execution["shots"][index] = recovered["shot"].clone();
            committed = true;
            shot_update(ctx, plan, index, &report, run)?;
            let reused = recovered["reused"] == true;
            let reuse_native = recovered["reuse_native"] == true;
            let duration = report["seconds"].as_u64().ok_or("Invalid shot duration.")?;
            if !reused {
                if prepared["use_previous_frame"] == true && !reuse_native {
                    media::dispatch(
                        "extract_last_frame",
                        &json!({"clip":previous,"frame_index":prepared["source_frame"],"image":report["continuity"]["source_image"]}),
                        &ctx.cancelled,
                    )?;
                }
                if automatic
                    && bindings.is_null()
                    && shot_renderer::uses_previous_frame(&plan["shots"], &json!(index))?
                    && !reviews.is_empty()
                    && !reuse_native
                    && !(recovered["pending"] == true
                        && matches!(
                            report["adaptation"]["status"].as_str(),
                            Some("applied" | "unchanged")
                        ))
                {
                    let adapted = review::adapt(
                        ctx,
                        &execution,
                        index,
                        reviews.last().ok_or("Missing preceding review.")?,
                        &shot_dir.join("adaptation"),
                        false,
                    )?;
                    execution["shots"][index] = adapted.clone();
                    for key in ["prompt", "prompt_template", "adaptation"] {
                        if let Some(value) = adapted.get(key) {
                            report[key] = value.clone();
                        }
                    }
                    shot_update(ctx, plan, index, &report, run)?;
                }
                if !reuse_native {
                    let binding = &report["storyboard"];
                    let image = if !binding.is_null() {
                        Some(storyboard::resolve_image(binding, run)?)
                    } else {
                        report["continuity"]["source_image"]
                            .as_str()
                            .map(PathBuf::from)
                    };
                    let portrait = &assets[index]["image"];
                    if binding.is_null() && prepared["use_previous_frame"] != true {
                        if let Some(expected) = portrait["sha256"].as_str() {
                            if artifacts::file_hash(
                                image.as_ref().ok_or("Missing saved portrait.")?,
                            )? != expected
                            {
                                return Err(
                                    "Saved character portrait changed before rendering.".into()
                                );
                            }
                        }
                    }
                    if requires {
                        boards::accepted(ctx, plan, boards, run)?;
                    }
                    let anchors = if !motion.is_null() {
                        motion::accepted(ctx, plan, boards, motion, run)?;
                        motion::anchors(&motion_row, run)?
                    } else {
                        Value::Null
                    };
                    let audio_file = dialogue_audio::file(ctx, &fixed_audio)?;
                    let payload = json!({"prompt":report["prompt"],"seed":report["seed"],"frames":duration*24,"output":native,"image_file":image,"hold_image_framing":false,"width":width,"height":height,"profile":profile,"image_anchors":anchors,"audio_file":audio_file});
                    let shot_started = Instant::now();
                    let take = report["take"].clone();
                    let emitter = &mut ctx.emit;
                    let render_progress = &mut last;
                    let runtime = inference::run(
                        &json!({"root":ctx.root,"python":ctx.python,"worker_operation":"ltx_render","payload":payload}),
                        ctx.cancelled.clone(),
                        |event| {
                            if event["type"] != "inference_progress" {
                                return emitter(event);
                            }
                            let data = &event["data"];
                            let Some(fraction) = data["fraction"].as_f64() else {
                                return Ok(());
                            };
                            let fraction = fraction.clamp(0.0, 1.0);
                            let by_duration =
                                (completed as f64 + duration as f64 * fraction) / seconds as f64;
                            let remaining = data["remaining_seconds"].as_f64().map(|remaining| {
                                // Use the shot's predicted total cost, rather
                                // than dividing elapsed time by a smoothed bar.
                                let shot_total =
                                    shot_started.elapsed().as_secs_f64() + remaining.max(0.0);
                                let rendering = remaining.max(0.0)
                                    + (seconds - completed - duration) as f64 * shot_total
                                        / duration as f64;
                                // The bar reserves its last 3% for production
                                // finishing; reserve the same share of time.
                                rendering
                                    + (started.elapsed().as_secs_f64() + rendering) * 0.03 / 0.97
                            });
                            emitter(render_progress.event(
                                (0.97 * by_duration).min(0.97),
                                format!(
                                    "Shot {}/{}, take {take}: {}",
                                    index + 1,
                                    shots.len(),
                                    data["label"].as_str().unwrap_or("Rendering")
                                ),
                                remaining,
                            ))
                        },
                    )?;
                    report["runtime"] = runtime;
                    if prepared["convert_voice"] == true {
                        report["native_sha256"] = json!(artifacts::file_hash(&native)?);
                        report["status"] = json!("converting-voice");
                        shot_update(ctx, plan, index, &report, run)?;
                    }
                }
                if prepared["convert_voice"] == true {
                    ctx.activity(format!(
                        "Shot {}/{}: Applying saved character voice",
                        index + 1,
                        shots.len()
                    ))?;
                    report["voice_conversion"] = conditioning(
                        ctx,
                        "conditioning_apply_voice",
                        json!({"source":native,"clip":clip,"voice":assets[index]["voice"],"run_dir":run,"seconds":duration,"width":width,"height":height}),
                    )?;
                }
            }
            committed = false;
            report = shot_renderer::storage(
                "take_complete",
                &json!({"shot_dir":shot_dir,"report":report,"reused":reused}),
                &ctx.cancelled,
            )?;
            committed = true;
            shot_update(ctx, plan, index, &report, run)?;
            if automatic {
                progress(
                    ctx,
                    &mut last,
                    0.97 * completed as f64 / seconds as f64,
                    format!(
                        "Shot {}/{}: Inspecting rendered action",
                        index + 1,
                        shots.len()
                    ),
                    None,
                )?;
                let observed = match review::shot(
                    ctx,
                    &execution,
                    index,
                    &clip,
                    &shot_dir.join("visual-review"),
                ) {
                    Ok(v) => v,
                    Err(error) => {
                        ctx.check()?;
                        json!({"status":"unavailable","summary":format!("Visual assessment unavailable: {error}"),"clip_sha256":report["sha256"],"observed_end_state":"","continuity_risk":false,"issues":[],"contract_evidence":[]})
                    }
                };
                report["observed_end_state"] = observed["observed_end_state"].clone();
                report["visual_review"] = observed.clone();
                if observed["status"] != "passed" {
                    ctx.activity(format!(
                        "Shot {}: {} Keeping this take under the single-render policy.",
                        index + 1,
                        observed["summary"]
                            .as_str()
                            .unwrap_or("Visual assessment unavailable.")
                    ))?;
                }
                reviews.push(observed);
            }
            remove(&mut report, &["error"]);
            report["sync_review"] = sync::pending(plan, index)?;
            let mut assessments = vec![report["sync_review"].clone()];
            if automatic {
                assessments.push(report["visual_review"].clone());
            }
            report["status"] = shot_renderer::render_status(&json!(assessments))?;
            let mut summary = report.clone();
            summary["index"] = json!(index);
            remove(
                &mut summary,
                &[
                    "start_state",
                    "end_state",
                    "beat_ids",
                    "scene_id",
                    "storyboard_acceptance",
                    "observed_source_clip_sha256",
                ],
            );
            summaries.push(summary);
            completed += duration;
            shot_update(ctx, plan, index, &report, run)?;
            progress(
                ctx,
                &mut last,
                0.97 * completed as f64 / seconds as f64,
                format!(
                    "Shot {}/{} {}",
                    index + 1,
                    shots.len(),
                    if reused { "reused" } else { "complete" }
                ),
                None,
            )?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let failed = shot_renderer::storage(
                "take_failure",
                &json!({"shot_dir":shot_dir,"report":report,"committed":committed,"status":if ctx.check().is_err(){"interrupted"}else{"failed"},"error":error}),
                &ctx.cancelled,
            )?;
            let _ = (ctx.emit)(json!({"type":"shot","index":index,"report":failed}));
            return Err(error);
        }
    }
    progress(ctx, &mut last, 0.97, "Assembling shots".into(), None)?;
    let (delivery_width, delivery_height) = if spec.is_null() {
        (json!(width), json!(height))
    } else {
        (spec["width"].clone(), spec["height"].clone())
    };
    let video = shot_renderer::storage(
        "take_assemble",
        &json!({"clip_count":shots.len(),"output":output,"seconds":seconds,"fade_out":validated["fade_out"],"width":delivery_width,"height":delivery_height,"crop":!spec.is_null(),"profile":profile}),
        &ctx.cancelled,
    )?;
    // Review the delivered soundtrack after conversion, assembly and fades.
    // A preview pass never certifies newly generated HQ footage.
    let delivery_hash = artifacts::file_hash(&output)?;
    let mut sync_reviews = Vec::new();
    let mut offset = 0_u64;
    for (index, shot) in shots.iter().enumerate() {
        ctx.check()?;
        progress(
            ctx,
            &mut last,
            0.98,
            format!(
                "Shot {}/{}: Checking speech and lip sync",
                index + 1,
                shots.len()
            ),
            None,
        )?;
        let observed = sync::shot(ctx, plan, index, &output, &delivery_hash, offset, run)?;
        ctx.activity(format!(
            "Shot {}: {}",
            index + 1,
            observed["summary"]
                .as_str()
                .unwrap_or("Lip sync needs review.")
        ))?;
        let receipt = json!({"policy":sync::POLICY,"status":observed["status"],
            "summary":observed["summary"],"delivery_sha256":delivery_hash,
            "file":format!("shots/{:03}/sync-review/review.json", index+1)});
        let mut report = read(
            &run.join("shots")
                .join(format!("{:03}/render.json", index + 1)),
        )?;
        report["sync_review"] = receipt.clone();
        match dialogue_audio::publish(
            ctx,
            plan,
            index,
            &assets[index]["voice"],
            run,
            &report,
            &observed,
        ) {
            Ok(value) if !value.is_null() => report["reusable_dialogue"] = value,
            Ok(_) => {}
            Err(error) => {
                ctx.check()?;
                ctx.activity(format!(
                    "Shot {}: Could not save reusable dialogue: {error}",
                    index + 1
                ))?;
            }
        }
        let mut assessments = vec![observed.clone()];
        if automatic {
            assessments.push(report["visual_review"].clone());
        }
        report["status"] = shot_renderer::render_status(&json!(assessments))?;
        shot_update(ctx, plan, index, &report, run)?;
        summaries[index]["sync_review"] = receipt;
        summaries[index]["status"] = report["status"].clone();
        sync_reviews.push(observed);
        offset += shot["seconds"].as_u64().ok_or("Invalid shot duration.")?;
    }
    let scene = if automatic {
        progress(
            ctx,
            &mut last,
            0.98,
            "Assessing scene fidelity".into(),
            None,
        )?;
        let observed = match review::scene(
            ctx,
            &execution,
            &json!(reviews),
            &run.join("scene-review"),
        ) {
            Ok(value) => value,
            Err(error) => {
                ctx.check()?;
                json!({"status":"unavailable","summary":format!("Scene assessment unavailable: {error}")})
            }
        };
        ctx.activity(format!(
            "Scene visual assessment: {}. {}",
            observed["status"].as_str().unwrap_or("unavailable"),
            observed["summary"].as_str().unwrap_or_default()
        ))?;
        Some(observed)
    } else {
        None
    };
    progress(
        ctx,
        &mut last,
        0.99,
        "Validating complete video".into(),
        None,
    )?;
    let mut all = reviews;
    all.extend(sync_reviews.iter().cloned());
    if let Some(scene) = &scene {
        all.push(scene.clone());
    }
    let status = shot_renderer::render_status(&json!(all))?;
    let mut result = json!({"status":status,"backend":"mlx","pipeline":"directed-shots","output_frames":seconds*24,"elapsed_seconds":(started.elapsed().as_secs_f64()*100.0).round()/100.0,"shots":summaries,"video":video});
    result["lip_sync_policy"] = json!(sync::POLICY);
    result["reused_dialogue_shots"] = json!(
        summaries
            .iter()
            .filter(|shot| shot["dialogue_audio"].is_object())
            .count()
    );
    result["sync_review"] = json!({"policy":sync::POLICY,
        "passed_shots":sync_reviews.iter().filter(|r| r["status"]=="passed").count(),
        "issue_shots":sync_reviews.iter().filter(|r| r["status"]=="issues_detected").count(),
        "unassessed_shots":sync_reviews.iter().filter(|r| r["status"]=="unassessed").count()});
    if !config.is_null() {
        result["render_config"] = config.clone();
    }
    if !bindings.is_null() {
        result["image_conditioning"] = json!("storyboard-first-frame");
    }
    if !assets.is_null() {
        result["character_conditioning"] = json!("saved-character-assets-v1");
        result["identity_review"] = json!(
            "pending: compare portraits and voice identity; see separate speech sync assessment"
        );
    }
    if let Some(scene) = scene {
        result["scene_review"] = scene;
        result["render_policy"] = json!("single-render-v1");
    }
    Ok(result)
}
fn last_value(_report: &Value, completed: u64, seconds: u64) -> f64 {
    0.97 * completed as f64 / seconds as f64
}
