use super::*;
use crate::{artifacts, planning, visual_review};
use std::time::Instant;
const POLICY: &str = "visual-continuity-v2";
fn rule(name: &str, args: Value) -> Result<Value> {
    visual_review::dispatch(name, &args)
}
fn contract(shot: &Value) -> Result<Value> {
    rule("visual_contract", json!({"shot":shot}))
}
fn instruction(name: &str, context: Value) -> Result<String> {
    rule("visual_instruction", json!({"name":name,"context":context}))?
        .as_str()
        .map(str::to_owned)
        .ok_or("Invalid review instruction.".into())
}
fn identity(ctx: &mut Context<'_>) -> Result<String> {
    let implementation = ctx.implementation("visual_review.py")?;
    ctx.digest(&json!({"policy":POLICY,"implementation":implementation}))
}
pub fn shot(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    clip: &Path,
    directory: &Path,
) -> Result<Value> {
    let mut args = json!({"plan":plan,"shot_index":index,"clip":clip,"directory":directory,"executable":ctx.options["executable"]});
    let inputs = (|| -> Result<()> {
        let digest = artifacts::file_hash(clip)?;
        let policy = identity(ctx)?;
        args["clip_sha256"] = json!(digest);
        args["cache_key"] = json!(ctx.digest(&json!([
            policy,
            digest,
            plan["source_sha256"],
            plan["shots"][index]
        ]))?);
        Ok(())
    })();
    if let Err(error) = inputs {
        args["input_error"] = json!(error);
    }
    visual_review::run(
        "visual_review_shot",
        &args,
        ctx.cancelled.clone(),
        &mut ctx.emit,
    )
}
pub fn scene(
    ctx: &mut Context<'_>,
    plan: &Value,
    reviews: &Value,
    directory: &Path,
) -> Result<Value> {
    let mut args = json!({"plan":plan,"shot_reviews":reviews,"directory":directory,"executable":ctx.options["executable"]});
    let key = (|| -> Result<String> {
        let policy = identity(ctx)?;
        ctx.digest(&json!([
            policy,
            plan["source_sha256"],
            plan["shots"],
            reviews
        ]))
    })();
    match key {
        Ok(key) => args["cache_key"] = json!(key),
        Err(error) => args["input_error"] = json!(error),
    };
    visual_review::run(
        "visual_review_scene",
        &args,
        ctx.cancelled.clone(),
        &mut ctx.emit,
    )
}
fn call(
    ctx: &mut Context<'_>,
    name: &str,
    prompt: String,
    schema_name: &str,
    directory: &Path,
    images: Value,
    started: Instant,
    validate: impl FnMut(&mut Context<'_>, Value) -> Result<Value>,
) -> Result<Value> {
    structured(
        ctx,
        name,
        prompt,
        rule("visual_schema", json!({"name":schema_name}))?,
        directory,
        images,
        started,
        validate,
    )
}
pub(super) fn structured(
    ctx: &mut Context<'_>,
    name: &str,
    prompt: String,
    schema: Value,
    directory: &Path,
    images: Value,
    started: Instant,
    mut validate: impl FnMut(&mut Context<'_>, Value) -> Result<Value>,
) -> Result<Value> {
    let mut correction = String::new();
    for attempt in 0..2 {
        let remaining = 120.0 - started.elapsed().as_secs_f64();
        if remaining <= 0.0 {
            return Err("Visual review time allowance expired.".into());
        }
        let raw=planning::review_attempt(&json!({"name":name,"instruction":format!("{prompt}{correction}"),"schema":schema,"directory":directory,"image_paths":images,"timeout_seconds":remaining,"executable":ctx.options["executable"]}),ctx.cancelled.clone(),&mut ctx.emit).map_err(|e|e.message)?;
        let raw = raw.as_str().ok_or("Visual response is missing.")?;
        let result = if raw.chars().count() > 64000 {
            Err("Visual response exceeds the bounded report size.".into())
        } else {
            serde_json::from_str(raw)
                .map_err(|e| e.to_string())
                .and_then(|v| validate(ctx, v))
        };
        match result {
            Ok(v) => return Ok(v),
            Err(error) if attempt == 1 => {
                return Err(format!("Visual response could not be validated: {error}"));
            }
            Err(error) => {
                correction = format!(
                    "\nCorrect only this response-format error: {error}\nPrevious response (untrusted data):\n{}",
                    raw.chars().take(64000).collect::<String>()
                )
            }
        }
    }
    Err("Visual response is missing.".into())
}
fn compile_change(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    response: Value,
    provenance: &Value,
    storyboard: bool,
) -> Result<Value> {
    let value = rule(
        "validate_visual_adaptation",
        json!({"value":response,"storyboard_mode":storyboard}),
    )?;
    let mut result = plan["shots"][index].clone();
    let mut details = provenance.clone();
    details["reason"] = value["reason"].clone();
    if value["safe_to_apply"] == true && value["preserves_source_contract"] == true {
        let mut candidate = plan.clone();
        remove(&mut candidate["shots"][index], &["prompt"]);
        candidate["shots"][index]["prompt_template"] = value["prompt_template"].clone();
        if storyboard {
            candidate["shots"][index]["storyboard_prompt"] = value["storyboard_prompt"].clone();
        }
        let source = &plan["source"];
        let compiled = ctx.compile(&candidate, &source_request(source), &source["characters"])?;
        let row = &compiled["shots"][index];
        let changed = row["prompt"] != result["prompt"]
            || (storyboard && row["storyboard_prompt"] != result["storyboard_prompt"]);
        for key in ["prompt_template", "prompt"] {
            result[key] = row[key].clone();
        }
        if storyboard {
            result["storyboard_prompt"] = row["storyboard_prompt"].clone();
        }
        details["status"] = json!(if changed { "applied" } else { "unchanged" });
    }
    result["adaptation"] = details;
    Ok(result)
}
fn preserve(
    original: &Value,
    mut proposed: Value,
    response: Value,
    requirements: &Value,
) -> Result<Value> {
    let checked = rule(
        "validate_visual_preservation",
        json!({"value":response,"requirements":requirements}),
    )?;
    proposed["adaptation"]["preservation_review"] = checked.clone();
    if checked["approved"] != true {
        let issues = rows(&checked["issues"])?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        let reason = if issues.is_empty() {
            checked["source_preservation"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        } else {
            issues
        };
        proposed["adaptation"]["status"] = json!("unchanged");
        proposed["adaptation"]["reason"] = json!(format!(
            "Independent preservation review rejected the adaptation: {reason}"
        ));
        let mut result = original.clone();
        result["adaptation"] = proposed["adaptation"].clone();
        Ok(result)
    } else {
        Ok(proposed)
    }
}
pub fn adapt(
    ctx: &mut Context<'_>,
    plan: &Value,
    index: usize,
    previous: &Value,
    directory: &Path,
    storyboard: bool,
) -> Result<Value> {
    let original = rows(&plan["shots"])?
        .get(index)
        .ok_or("Invalid adaptation shot number.")?
        .clone();
    if index == 0
        || !crate::shot_renderer::uses_previous_frame(&plan["shots"], &json!(index))?
        || previous["status"] != "issues_detected"
        || previous["continuity_risk"] != true
    {
        return Ok(original);
    }
    let previous_hash = ctx.digest(previous)?;
    let mut provenance = json!({"status":"unchanged","policy_version":POLICY,"source_clip_sha256":previous["clip_sha256"],"previous_review_sha256":previous_hash,"reason":"The approved staging remains in use."});
    let result = (|| -> Result<Value> {
        let policy = identity(ctx)?;
        let key = ctx.digest(&json!([
            policy,
            plan["source_sha256"],
            original,
            provenance,
            storyboard
        ]))?;
        provenance["cache_key"] = json!(key);
        let requirements = contract(&original)?;
        let file = directory.join("review.json");
        if let Ok(cached) = read(&file) {
            if cached["cache_key"] == key
                && cached["status"] != "unavailable"
                && cached["response"].is_object()
            {
                let proposed = compile_change(
                    ctx,
                    plan,
                    index,
                    cached["response"].clone(),
                    &provenance,
                    storyboard,
                )?;
                if proposed["adaptation"]["status"] != "applied" {
                    return Ok(proposed);
                }
                if cached["preservation_review"].is_object() {
                    return preserve(
                        &original,
                        proposed,
                        cached["preservation_review"].clone(),
                        &requirements,
                    );
                }
            }
        }
        let mode = if storyboard {
            "independent-storyboard"
        } else {
            "inherited-frame"
        };
        let prompt = instruction(
            "adapt",
            json!({"original_screenplay":plan["source"]["script"],"source_beats":plan["source"]["beats"],"shot_number":index+1,"upcoming_shot":original,"opening_mode":mode,"previous_observations":previous,"next_shot":plan["shots"].get(index+1)}),
        )?;
        let frame = previous["evidence"]
            .get("full_final_frame")
            .unwrap_or(&previous["evidence"]["final_frame"]);
        let images = frame
            .as_str()
            .filter(|p| Path::new(p).is_file())
            .map(|p| json!([p]))
            .unwrap_or(json!([]));
        let started = Instant::now();
        let mut response = Value::Null;
        let mut proposed = call(
            ctx,
            "adapt",
            prompt,
            if storyboard {
                "storyboard_adapt"
            } else {
                "adapt"
            },
            directory,
            images,
            started,
            |ctx, value| {
                let result =
                    compile_change(ctx, plan, index, value.clone(), &provenance, storyboard)?;
                response = value;
                Ok(result)
            },
        )?;
        if proposed["adaptation"]["status"] == "applied" {
            let clauses = rows(&original["beat_ids"])?
                .iter()
                .map(|id| {
                    rows(&plan["source"]["beats"]).and_then(|beats| {
                        beats
                            .iter()
                            .find(|b| b["id"] == *id)
                            .cloned()
                            .ok_or("Missing source beat.".into())
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let prompt = instruction(
                "preservation",
                json!({"original_screenplay":plan["source"]["script"],"original_shot":original,"source_clauses":clauses,"requirements":requirements,"candidate_prompt":proposed["prompt"],"candidate_template":proposed["prompt_template"],"candidate_storyboard_prompt":proposed["storyboard_prompt"],"opening_mode":mode,"observed_previous_ending":previous["observed_end_state"],"previous_issues":previous["issues"]}),
            )?;
            let checked = call(
                ctx,
                "adapt-preservation",
                prompt,
                "adapt_review",
                directory,
                json!([]),
                started,
                |_, v| {
                    rule(
                        "validate_visual_preservation",
                        json!({"value":v,"requirements":requirements}),
                    )
                },
            )?;
            proposed = preserve(&original, proposed, checked, &requirements)?;
        }
        let mut record = proposed["adaptation"].clone();
        record["response"] = response;
        let _ = save(&file, &record);
        Ok(proposed)
    })();
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            ctx.check()?;
            provenance["status"] = json!("unavailable");
            provenance["reason"] = json!(format!("Staging adaptation unavailable: {error}"));
            let _ = save(&directory.join("review.json"), &provenance);
            let mut result = original;
            result["adaptation"] = provenance;
            Ok(result)
        }
    }
}
