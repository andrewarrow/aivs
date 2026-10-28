use super::*;
use crate::{characters, shooting_script::PLANNER_VERSION};
use std::collections::{BTreeMap, BTreeSet};

pub fn save_director(plan: &Value, run: &Path) -> Result<()> {
    save(&run.join("storyboard-plan.json"), plan)?;
    let display = rows(&plan["shots"])?
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "SHOT {} | {}s\n{}\n",
                i + 1,
                s["seconds"],
                s["storyboard_prompt"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(run.join("storyboard-plan.txt"), display).map_err(|e| e.to_string())
}
pub fn director(
    ctx: &mut Context<'_>,
    request: &Value,
    profiles: &Value,
    run: &Path,
) -> Result<Value> {
    let source = ctx.source(request, profiles)?;
    if source["storyboard_policy"] != STORYBOARD_POLICY {
        return Err("New planning requires per-shot storyboard first frames.".into());
    }
    fs::create_dir_all(run).map_err(|e| e.to_string())?;
    let draft = run.join("storyboard-plan.json");
    let validate = |ctx: &mut Context<'_>, candidate: Value| -> Result<Value> {
        if candidate["planning_policy"] != "single-render-v1" {
            return Err("The director plan needs the source-bound production contract.".into());
        }
        let mut plan = ctx.compile(&candidate, request, profiles)?;
        plan["planner_version"] = json!(PLANNER_VERSION);
        plan["planning_stage"] = json!("storyboard");
        Ok(plan)
    };
    if draft.is_file() {
        let saved = read(&draft)?;
        if saved["planner_version"] != PLANNER_VERSION || saved["planning_stage"] != "storyboard" {
            return Err("Saved storyboard direction belongs to a different planner.".into());
        }
        let plan = validate(ctx, saved)?;
        ctx.activity("Reusing saved storyboard direction.")?;
        return Ok(plan);
    }
    ctx.budget(&source, run)?;
    characters::verify(rows(profiles)?, run)?;
    let mut images = rows(profiles)?
        .iter()
        .filter(|p| p["image"].is_object())
        .map(|p| Ok(characters::directory(run, text(p, "id")?)?.join(text(&p["image"], "file")?)))
        .collect::<Result<Vec<_>>>()?;
    if let Some(set) = source.get("set") {
        crate::sets::verify(set, run)?;
        images.push(crate::storyboard::resolve_image(
            &json!({"image":set["image"]}),
            run,
        )?);
    }
    let direction = fs::read_to_string(ctx.root.join("python/shooting-script-instructions.txt"))
        .map_err(|e| e.to_string())?;
    let body = shooting("shooting_instruction_context", json!({"source":source}))?;
    ctx.activity("Planning 1/2: directing storyboard compositions.")?;
    let plan = ctx.plan_call(
        "director",
        format!("{}\n\n{}", direction.trim_end(), body),
        schema("director")?,
        &source,
        run,
        json!(images),
        |ctx, mut candidate| {
            incremental::assign_ids(&mut candidate, &source)?;
            validate(ctx, candidate)
        },
    )?;
    save_director(&plan, run)?;
    Ok(plan)
}
fn sequence(plan: &Value, rows: &Value) -> Result<Vec<Value>> {
    let result = rule("review_sequence", json!({"plan":plan,"rows":rows}))?;
    // Preserve shot order even when JSON object keys reach double digits.
    let mut numbers = result
        .as_object()
        .ok_or("Invalid sequence reviews.")?
        .keys()
        .map(|s| s.parse::<usize>().map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>>>()?;
    numbers.sort();
    Ok(numbers
        .iter()
        .map(|n| result[n.to_string()].clone())
        .collect())
}
fn checkpoint(
    ctx: &mut Context<'_>,
    plan: &Value,
    boards: &Value,
    run: &Path,
    settings: &Value,
    hashes: &BTreeMap<usize, String>,
    rows: &Value,
) -> Result<()> {
    let mut saved = json!({});
    for (number, row) in rows.as_object().ok_or("Invalid motion rows.")? {
        let n = number.parse::<usize>().map_err(|e| e.to_string())?;
        saved[number] =
            json!({"input_sha256":hashes.get(&n).ok_or("Invalid cached shot number.")?,"row":row});
    }
    save(
        &run.join("motion-directions.json"),
        &json!({"version":3,"policy":settings["policy"],"review_policy_sha256":boards::review_policy(ctx)?,"shots":saved}),
    )?;
    let binding = boards::review_binding(ctx, plan, boards)?;
    save(
        &run.join("storyboard-review.json"),
        &json!({"version":1,"policy":settings["policy"],"binding":binding,"status":"reviewing","shots":sequence(plan,rows)?}),
    )
}
pub fn finalize(
    ctx: &mut Context<'_>,
    draft: &Value,
    boards: &mut Value,
    run: &Path,
) -> Result<Value> {
    let settings = rule("continuity_settings", json!({}))?;
    let source = &draft["source"];
    let request = source_request(source);
    let profiles = &source["characters"];
    let plan = ctx.compile(draft, &request, profiles)?;
    if plan["planner_version"] != PLANNER_VERSION || plan["planning_stage"] != "storyboard" {
        return Err("The motion pass requires a saved storyboard director plan.".into());
    }
    let bindings = boards::prepare(ctx, &plan, boards, run)?;
    boards::references(ctx, &plan, boards, run)?;
    let images = rows(&bindings)?
        .iter()
        .map(|b| json!({"shot":b["shot"],"image":b["image"]}))
        .collect::<Vec<_>>();
    let board_hash = ctx.digest(&json!(images))?;
    let director_hash = ctx.plan_hash(&plan)?;
    let file = run.join("shooting-script.json");
    if file.is_file() {
        let final_plan = ctx.load_plan(&file, &request, profiles)?;
        boards::accepted(ctx, &final_plan, boards, run)?;
        return Ok(final_plan);
    }
    ctx.budget(source, run)?;
    let cache = run.join("motion-directions.json");
    let saved = if cache.is_file() {
        read(&cache)?
    } else {
        json!({})
    };
    let cached = if saved["policy"] == settings["policy"] {
        saved.get("shots").cloned().unwrap_or(json!({}))
    } else {
        json!({})
    };
    if !cached.is_object() {
        return Err("Saved sequence directions must contain shot records.".into());
    }
    let review_policy = boards::review_policy(ctx)?;
    let mut hashes = BTreeMap::new();
    let mut directed = json!({});
    let mut pending = Vec::new();
    for n in 1..=rows(&plan["shots"])?.len() {
        let references = boards
            .get("references")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let hash = ctx.digest(&incremental::motion_input(
            &plan,
            &images,
            n,
            &references,
            &review_policy,
        )?)?;
        if cached[n.to_string()].is_object()
            && cached[n.to_string()]["input_sha256"] == hash
            && incremental::valid_motion_row(&plan, n, &cached[n.to_string()]["row"])
        {
            directed[n.to_string()] = cached[n.to_string()]["row"].clone();
        } else if let Some(row) =
            incremental::parent_motion(ctx, &plan, &images, n, &references, &review_policy, run)?
        {
            directed[n.to_string()] = row;
        } else {
            pending.push(n);
        }
        hashes.insert(n, hash);
    }
    if directed.as_object().is_some_and(|rows| !rows.is_empty()) {
        checkpoint(ctx, &plan, boards, run, &settings, &hashes, &directed)?;
    }
    let direction = fs::read_to_string(
        ctx.root
            .join("python/shooting-script-review-instructions.txt"),
    )
    .map_err(|e| e.to_string())?;
    let mut references = Vec::new();
    for cast in rows(&source["cast"])? {
        let p = rows(profiles)?
            .iter()
            .find(|p| p["id"] == cast["character"])
            .ok_or("Unknown cast portrait.")?;
        references.push(json!({"id":format!("portrait:{}",text(cast,"role")?),"file":format!("characters/{}/{}",text(p,"id")?,text(&p["image"],"file")?),"sha256":p["image"]["sha256"]}));
    }
    for r in boards["references"].as_array().into_iter().flatten() {
        let mut asset = r["image"].clone();
        asset["id"] = r["id"].clone();
        references.push(asset);
    }
    let batch = settings["batch_size"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("Invalid continuity batch size.")? as usize;
    for numbers in pending.chunks(batch) {
        let mut ids = BTreeSet::new();
        if boards["reference_scope"] == "shot-dependencies-v1" {
            for n in numbers {
                ids.extend(crate::storyboard::review_reference_ids(
                    &plan,
                    &plan["shots"][n - 1],
                )?);
            }
        } else {
            ids.extend(
                boards["references"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["id"].as_str().map(str::to_owned)),
            );
        }
        let references = references
            .iter()
            .filter(|r| {
                r["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("portrait:") || ids.contains(id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let context = numbers
            .iter()
            .flat_map(|n| [n.saturating_sub(1), *n, n + 1])
            .filter(|n| hashes.contains_key(n))
            .collect::<BTreeSet<_>>();
        let batch_images = context
            .iter()
            .map(|n| images[n - 1].clone())
            .collect::<Vec<_>>();
        let mut attachments = references
            .iter()
            .map(|r| crate::storyboard::resolve_image(&json!({"image":r}), run))
            .collect::<Result<Vec<_>>>()?;
        for image in &batch_images {
            attachments.push(crate::storyboard::resolve_image(image, run)?);
        }
        let prior: serde_json::Map<String, Value> = directed
            .as_object()
            .ok_or("Invalid cached directions.")?
            .iter()
            .filter(|(n, _)| n.parse::<usize>().is_ok_and(|n| n < numbers[0]))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let instruction = format!(
            "{}\n\n{}",
            direction.trim_end(),
            json!({"plan":plan,"shots_to_direct":numbers,"images_in_shot_order":batch_images,"reference_images_first":references,"persistent_review_state":sequence(&plan,&json!(prior))?,"reference_ids_to_review":ids})
        );
        ctx.activity(format!(
            "Planning 2/2: reviewing sequence and directing shots {}–{}.",
            numbers[0],
            numbers[numbers.len() - 1]
        ))?;
        let result=ctx.plan_call("motion",instruction,schema("motion")?,source,run,json!(attachments),|ctx,candidate|{
            let checked=shooting("validate_motion_batch",json!({"plan":plan,"numbers":numbers,"candidate":candidate,"reference_ids":ids,"rows":directed}))?;
            for trial in rows(&checked["trial_plans"] )?{ctx.compile(trial,&request,profiles)?;}Ok(checked["rows"].clone())
        })?;
        for (n, row) in numbers.iter().zip(rows(&result)?) {
            directed[n.to_string()] = row.clone();
        }
        checkpoint(ctx, &plan, boards, run, &settings, &hashes, &directed)?;
    }
    let reviews = sequence(&plan, &directed)?;
    let mut issues = Vec::new();
    let mut shots = Vec::new();
    for (i, shot) in rows(&plan["shots"])?.iter().enumerate() {
        let number = i + 1;
        let row = &directed[number.to_string()];
        let review = reviews.get(i).ok_or("Missing sequence review.")?;
        let bad = rows(&review["reference_findings"])?
            .iter()
            .filter(|r| r["decision"] != "pass" || r["confidence"].as_f64().unwrap_or(0.0) < 0.8)
            .map(|r| r["id"].clone())
            .collect::<Vec<_>>();
        if review["decision"] != "pass" || !bad.is_empty() {
            let violations = rows(&review["violations"])?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ");
            let detail = if !violations.is_empty() {
                violations
            } else {
                row["blocking_issue"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(review["evidence"].as_str())
                    .unwrap_or("Unknown storyboard defect.")
                    .to_owned()
            };
            issues.push(json!({"shot":number,"issue":detail,"storyboard_prompt":row["storyboard_prompt"],"image_observations":row["image_observations"],"failure_kind":review["failure_kind"],"plan_repair":row["plan_repair"],"world_repair":row["world_repair"],"reference_ids":bad}));
        }
        let mut revised = shot.clone();
        revised["prompt_template"] = row["prompt_template"].clone();
        remove(&mut revised, &["prompt"]);
        shots.push(revised);
    }
    let mut report = json!({"version":1,"policy":settings["policy"],"binding":boards::review_binding(ctx,&plan,boards)?,"status":if issues.is_empty(){"reviewed"}else{"repair-required"},"shots":reviews});
    save(&run.join("storyboard-review.json"), &report)?;
    if !issues.is_empty() {
        return Ok(json!({"repair_issues":issues}));
    }
    let mut candidate = plan.clone();
    candidate["shots"] = json!(shots);
    candidate["planning_stage"] = json!("final");
    let mut final_plan = ctx.compile(&candidate, &request, profiles)?;
    let fingerprint = ctx.plan_hash(&final_plan)?;
    let observations = hashes
        .keys()
        .map(|n| directed[n.to_string()]["image_observations"].clone())
        .collect::<Vec<_>>();
    final_plan["motion_plan"] = json!({"version":1,"director_plan_sha256":director_hash,"storyboard_sha256":board_hash,"images":images,"image_observations":observations,"plan_sha256":fingerprint});
    ctx.activity("Checking final motion prompts against essential source actions and reveals.")?;
    final_plan = fidelity::ensure(ctx, final_plan, run)?;
    report["status"] = json!("accepted");
    report["binding"] = boards::review_binding(ctx, &final_plan, boards)?;
    save(&run.join("storyboard-review.json"), &report)?;
    boards["continuity_acceptance"] =
        json!({"policy":settings["policy"],"review_sha256":ctx.digest(&report)?});
    save(&run.join("storyboards.json"), boards)?;
    ctx.publish(&final_plan, run)?;
    Ok(final_plan)
}
