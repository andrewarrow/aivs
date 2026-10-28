use super::*;
use crate::{preproduction, shooting_script::PLANNER_VERSION};
use std::time::Instant;
fn policy(operation: &str, args: Value) -> Result<Value> {
    preproduction::dispatch(operation, &args)
}
fn rounded(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}
fn director_key(ctx: &mut Context<'_>, request: &Value, profiles: &Value) -> Result<String> {
    let source = ctx.source(request, profiles)?;
    let mut implementation = json!({});
    for name in ["shooting_script.py", "shooting-script-instructions.txt"] {
        implementation[name] = json!(ctx.implementation(name)?);
    }
    ctx.codec(&json!({"source":source["source_sha256"],"implementation":implementation,"tokenizer_revision":MODEL_REVISION}),true,false)
}
pub fn cache_key(ctx: &mut Context<'_>, request: &Value, profiles: &Value) -> Result<String> {
    let director = director_key(ctx, request, profiles)?;
    let mut implementation = json!({});
    for name in [
        "shooting-script-review-instructions.txt",
        "storyboard.py",
        "storyboard_backend.py",
        "storyboard_mflux.py",
        "prop_reference.py",
        "preproduction.py",
    ] {
        implementation[name] = json!(ctx.implementation(name)?);
    }
    ctx.digest(&json!({"director":director,"backend":boards::backend(ctx)?,"implementation":implementation}))
}
fn prepare_director(
    ctx: &mut Context<'_>,
    request: &Value,
    profiles: &Value,
    run: &Path,
) -> Result<Value> {
    if !run.join("storyboard-plan.json").exists() {
        if let Some(plan) = incremental::director(ctx, request, profiles, run)? {
            return Ok(plan);
        }
    }
    let key = director_key(ctx, request, profiles)?;
    let index = ctx
        .preproduction_root()
        .join("directors")
        .join(format!("{key}.json"));
    let draft = run.join("storyboard-plan.json");
    if ctx.options["fresh"] != true && !draft.exists() && index.is_file() {
        let result = (|| -> Result<Value> {
            let saved = read(&index)?;
            let candidate = &saved["plan"];
            if saved["key"] != key
                || saved["plan_sha256"] != ctx.digest(candidate)?
                || candidate["planner_version"] != PLANNER_VERSION
                || candidate["planning_stage"] != "storyboard"
            {
                return Err("Director cache identity changed.".into());
            }
            ctx.compile(candidate, request, profiles)?;
            Ok(candidate.clone())
        })();
        match result {
            Ok(candidate) => {
                direction::save_director(&candidate, run)?;
                ctx.activity("Reusing director coverage; image rendering and inspection have separate caches")?;
            }
            Err(error) => ctx.activity(format!(
                "Cached director plan is unavailable ({error}); preparing coverage"
            ))?,
        }
    }
    let director = direction::director(ctx, request, profiles, run)?;
    if !run.join("storyboard-repairs.json").exists() {
        let hash = ctx.digest(&director)?;
        let stored = if let Some(project) = ctx.project_root() {
            artifacts::store_object(&project, &director)?
        } else {
            director.clone()
        };
        if let Err(error) = save(&index, &json!({"key":key,"plan_sha256":hash,"plan":stored})) {
            ctx.activity(format!(
                "Director plan is saved locally; shared cache could not be updated: {error}"
            ))?;
        }
    }
    Ok(director)
}
pub fn rebind(
    ctx: &mut Context<'_>,
    director: &Value,
    final_plan: &Value,
    boards: &Value,
    run: &Path,
) -> Result<Value> {
    policy(
        "validate_storyboard_direction",
        json!({"director":director,"final":final_plan}),
    )?;
    if boards::prepare(ctx, final_plan, boards, run).is_ok() {
        return Ok(boards.clone());
    }
    boards::prepare(ctx, director, boards, run)?;
    let mut rebound = boards.clone();
    for entry in rebound["shots"]
        .as_array_mut()
        .ok_or("Missing storyboard shots.")?
    {
        let n = entry["shot"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("Invalid storyboard number.")? as usize;
        let shot = rows(&final_plan["shots"])?
            .get(n - 1)
            .ok_or("Invalid storyboard number.")?;
        entry["planned_shot_sha256"] = json!(ctx.shot_hash(final_plan, shot)?);
    }
    boards::prepare(ctx, final_plan, &rebound, run)?;
    Ok(rebound)
}
pub(super) fn load_final(
    ctx: &mut Context<'_>,
    directory: &Path,
    request: &Value,
    profiles: &Value,
) -> Result<(Value, Value, Value)> {
    let plan = ctx.load_plan(&directory.join("shooting-script.json"), request, profiles)?;
    if plan["planning_stage"] != "final" {
        return Err("Reuse requires a completed two-pass storyboard plan.".into());
    }
    let director = ctx.compile(
        &read(&directory.join("storyboard-plan.json"))?,
        request,
        profiles,
    )?;
    if director["planning_stage"] != "storyboard"
        || plan["motion_plan"]["director_plan_sha256"] != ctx.plan_hash(&director)?
    {
        return Err("The motion plan does not match its saved storyboard direction.".into());
    }
    let boards = rebind(
        ctx,
        &director,
        &plan,
        &read(&directory.join("storyboards.json"))?,
        directory,
    )?;
    let bindings = boards::prepare(ctx, &plan, &boards, directory)?;
    let images = rows(&bindings)?
        .iter()
        .map(|r| json!({"shot":r["shot"],"image":r["image"]}))
        .collect::<Vec<_>>();
    if plan["motion_plan"]["images"] != json!(images) {
        return Err(
            "The storyboard images differ from those inspected during shot direction.".into(),
        );
    }
    if plan["motion_plan"]["storyboard_sha256"] != ctx.digest(&json!(images))? {
        return Err("The motion plan has an invalid storyboard fingerprint.".into());
    }
    boards::accepted(ctx, &plan, &boards, directory)?;
    Ok((plan, director, boards))
}
fn finish_repair(
    ctx: &mut Context<'_>,
    director: &Value,
    boards: &Value,
    state: &mut Value,
    request: &Value,
    profiles: &Value,
    run: &Path,
) -> Result<(Value, Value)> {
    let repair = rows(&state["rounds"])?
        .last()
        .ok_or("Missing reserved repair.")?
        .clone();
    let revised = ctx.compile(&repair["plan"], request, profiles)?;
    let context_args = json!({"state":state,"source_sha256":director["source_sha256"],"director_sha256":ctx.plan_hash(director)?,"revised_sha256":ctx.plan_hash(&revised)?});
    let context = policy("resume_storyboard_repair", context_args.clone())?;
    let round = context["round_number"]
        .as_u64()
        .ok_or("Invalid repair round.")?;
    let library = boards::generate_references(
        ctx,
        &revised,
        profiles,
        run,
        repair["reference_seed"]
            .as_i64()
            .ok_or("Missing reference seed.")?,
        &context["reference_ids"],
        round,
    )?;
    let mut changed = boards.clone();
    for issue in rows(&repair["issues"])? {
        let number = issue["shot"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("Invalid repair shot.")? as usize;
        let directory = run
            .join("storyboards/repairs")
            .join(format!("{round:02}"))
            .join(format!("{number:03}"));
        let entry = boards::regenerate(
            ctx,
            &revised,
            profiles,
            run,
            repair["seed"].as_i64().ok_or("Missing repair seed.")?,
            number - 1,
            text(issue, "issue")?,
            &directory,
            &repair["prior_images"][number.to_string()],
        )?;
        let target = changed["shots"]
            .as_array_mut()
            .and_then(|a| a.get_mut(number - 1))
            .ok_or("Invalid repair shot.")?;
        *target = entry;
    }
    changed["references"] = library["references"].clone();
    changed["status"] = json!("complete");
    remove(&mut changed, &["continuity_acceptance"]);
    boards::prepare(ctx, &revised, &changed, run)?;
    save(&run.join("storyboards.json"), &changed)?;
    direction::save_director(&revised, run)?;
    *state = policy("complete_storyboard_repair", context_args)?;
    save(&run.join("storyboard-repairs.json"), state)?;
    Ok((revised, changed))
}
fn reserve(
    ctx: &mut Context<'_>,
    director: &Value,
    issues: &Value,
    state: &mut Value,
    seed: i64,
    boards: &Value,
    request: &Value,
    profiles: &Value,
    run: &Path,
) -> Result<Value> {
    let decision = policy(
        "storyboard_repair_candidate",
        json!({"director":director,"issues":issues,"state":state}),
    )?;
    if decision["status"] == "exhausted" {
        let diagnostic = &decision["diagnostic"];
        save(&run.join("storyboard-diagnostic.json"), diagnostic)?;
        let review_file = run.join("storyboard-review.json");
        if review_file.is_file() {
            let mut review = read(&review_file)?;
            review["status"] = diagnostic["status"].clone();
            review["reason"] = diagnostic["reason"].clone();
            save(&review_file, &review)?;
        }
        return Err(text(&decision, "message")?.into());
    }
    let revised = ctx.compile(&decision["plan"], request, profiles)?;
    direction::validate_cast_anchors(&revised)?;
    let mut changed = Vec::new();
    for (i, (before, after)) in rows(&director["shots"])?
        .iter()
        .zip(rows(&revised["shots"])?)
        .enumerate()
    {
        if ctx.shot_hash(director, before)? != ctx.shot_hash(&revised, after)? {
            changed.push(i + 1);
        }
    }
    let hash = ctx.plan_hash(director)?;
    let reserved = policy(
        "reserve_storyboard_repair",
        json!({"state":state,"director":director,"revised":revised,"issues":issues,"boards":boards,"changed_shots":changed,"before_plan_sha256":hash,"seed":seed}),
    )?;
    let review = run.join("storyboard-review.json");
    if review.is_file() {
        save(
            &run.join(format!(
                "storyboard-review-before-repair-{}.json",
                rows(&state["rounds"])?.len() + 1
            )),
            &read(&review)?,
        )?;
    }
    save(&run.join("storyboard-repairs.json"), &reserved)?;
    *state = reserved;
    Ok(decision["limit"].clone())
}
pub fn prepare(
    ctx: &mut Context<'_>,
    request: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
) -> Result<Value> {
    if ctx.project_root().is_some() && !run.join("shooting-script.json").is_file() {
        return revisions::prepare(ctx, request, profiles, run, seed);
    }
    prepare_local(ctx, request, profiles, run, seed)
}

pub(super) fn prepare_local(
    ctx: &mut Context<'_>,
    request: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
) -> Result<Value> {
    fs::create_dir_all(run).map_err(|e| e.to_string())?;
    let key = cache_key(ctx, request, profiles)?;
    let index = ctx.preproduction_root().join(format!("{key}.json"));
    if run.join("shooting-script.json").is_file() {
        let (plan, _, boards) = load_final(ctx, run, request, profiles)?;
        save(&run.join("storyboards.json"), &boards)?;
        ctx.activity("Reusing this run's completed shooting plan and storyboard images")?;
        return Ok(json!({"plan":plan,"boards":boards}));
    }
    let supplied = ctx.options["shooting_script_path"]
        .as_str()
        .map(PathBuf::from);
    let mut source = None;
    if let Some(file) = &supplied {
        let file = file.canonicalize().map_err(|e| e.to_string())?;
        if file.file_name().is_none_or(|n| n != "shooting-script.json") {
            return Err(
                "Select the finalized shooting-script.json beside its storyboard assets.".into(),
            );
        }
        source = file.parent().map(Path::to_path_buf);
    } else if ctx.options["fresh"] != true
        && !run.join("input.json").is_file()
        && !run.join("storyboard-plan.json").exists()
        && index.is_file()
    {
        if let Ok(record) = read(&index) {
            if record["key"] == key {
                source = record["directory"].as_str().and_then(|p| {
                    let path = Path::new(p);
                    let path = if path.is_absolute() {
                        path.to_owned()
                    } else {
                        ctx.project_root()?.join(path)
                    };
                    path.canonicalize().ok()
                });
            }
        }
    }
    if let Some(source) = source {
        match load_final(ctx, &source, request, profiles) {
            Ok((plan, director, boards)) => {
                let hash = ctx.plan_hash(&plan)?;
                policy(
                    "copy_final_preproduction",
                    json!({"source":source,"destination":run,"plan":plan,"director":director,"boards":boards,"fingerprint":hash}),
                )?;
                boards::accepted(ctx, &plan, &boards, run)?;
                save(
                    &run.join("preproduction.json"),
                    &json!({"cache_key":key,"reused":true,"source_directory":source,"planning_seconds":0,"storyboard_seconds":0}),
                )?;
                ctx.activity("Reusing the validated shooting plan and storyboards; video seed and profile may differ")?;
                return Ok(json!({"plan":plan,"boards":boards}));
            }
            Err(error) => {
                if supplied.is_some() {
                    return Err(error);
                }
                ctx.activity(format!(
                    "Cached preproduction is unavailable ({error}); preparing a new plan"
                ))?;
            }
        }
    }
    let plan_only = ctx.options["plan_only"] == true;
    if !plan_only {
        boards::image_call(ctx, "imagegen_check", json!({}))?;
    }
    let mut director = prepare_director(ctx, request, profiles, run)?;
    if plan_only {
        return Ok(json!({"plan":director,"boards":null}));
    }
    let budget = ctx.budget(&director["source"], run)?;
    if budget["used_seconds"].as_f64().unwrap_or(0.0)
        >= budget["limit_seconds"].as_f64().unwrap_or(600.0)
    {
        return Err("The total planning budget is exhausted. Increase --planning-budget-seconds before generating storyboards.".into());
    }
    let started = Instant::now();
    let repair_path = run.join("storyboard-repairs.json");
    let saved = if repair_path.is_file() {
        read(&repair_path)?
    } else {
        json!({"version":1,"source_sha256":director["source_sha256"],"rounds":[]})
    };
    let mut repairs = policy(
        "validate_storyboard_repairs",
        json!({"state":saved,"source_sha256":director["source_sha256"]}),
    )?;
    let mut boards = if !rows(&repairs["rounds"])?.is_empty() {
        let boards = read(&run.join("storyboards.json"))?;
        if rows(&repairs["rounds"])?
            .last()
            .is_some_and(|r| r["status"] == "pending")
        {
            let (revised, boards) = finish_repair(
                ctx,
                &director,
                &boards,
                &mut repairs,
                request,
                profiles,
                run,
            )?;
            director = revised;
            boards
        } else {
            boards
        }
    } else {
        boards::generate(ctx, &director, profiles, run, seed)?
    };
    boards::prepare(ctx, &director, &boards, run)?;
    let mut storyboard_seconds = rounded(started.elapsed().as_secs_f64());
    let plan = loop {
        let result = direction::finalize(ctx, &director, &mut boards, run)?;
        if let Some(issues) = result.get("repair_issues") {
            let budget = ctx.budget(&director["source"], run)?;
            if budget["used_seconds"].as_f64().unwrap_or(0.0)
                >= budget["limit_seconds"].as_f64().unwrap_or(600.0)
            {
                return Err("The planning budget is exhausted before storyboard repair; increase --planning-budget-seconds to continue.".into());
            }
            let limit = reserve(
                ctx,
                &director,
                issues,
                &mut repairs,
                seed,
                &boards,
                request,
                profiles,
                run,
            )?;
            ctx.activity(format!(
                "Repairing {} storyboard(s), round {}/{limit}",
                rows(issues)?.len(),
                rows(&repairs["rounds"])?.len()
            ))?;
            let started = Instant::now();
            let (revised, new_boards) = finish_repair(
                ctx,
                &director,
                &boards,
                &mut repairs,
                request,
                profiles,
                run,
            )?;
            director = revised;
            boards = new_boards;
            storyboard_seconds += rounded(started.elapsed().as_secs_f64());
        } else {
            break result;
        }
    };
    boards = rebind(ctx, &director, &plan, &boards, run)?;
    save(&run.join("storyboards.json"), &boards)?;
    boards::accepted(ctx, &plan, &boards, run)?;
    let budget = read(&run.join("planning-budget.json"))?;
    save(
        &run.join("preproduction.json"),
        &json!({"cache_key":key,"reused":false,"storyboard_seconds":storyboard_seconds,"planning_budget":budget}),
    )?;
    let directory = ctx
        .project_root()
        .and_then(|root| run.strip_prefix(root).ok().map(Path::to_owned))
        .unwrap_or_else(|| run.to_owned());
    if let Err(error) = save(&index, &json!({"key":key,"directory":directory})) {
        ctx.activity(format!("Preproduction is saved in this run; the shared cache index could not be updated: {error}"))?;
    }
    Ok(json!({"plan":plan,"boards":boards}))
}
