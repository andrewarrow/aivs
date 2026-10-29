use super::*;
use std::collections::BTreeSet;
fn requirements(plan: &Value) -> Result<Value> {
    shooting("shooting_fidelity_requirements", json!({"plan":plan}))
}
fn validate(report: Value, plan: &Value, require_pass: bool) -> Result<Value> {
    shooting(
        "validate_shooting_fidelity",
        json!({"report":report,"plan":plan,"require_pass":require_pass}),
    )
}
fn replace(
    ctx: &mut Context<'_>,
    plan: &Value,
    response: &Value,
    numbers: &[usize],
) -> Result<Value> {
    if rows(&response["shots"])?.len() != numbers.len() {
        return Err("Fidelity repair must return exactly the named shots.".into());
    }
    let mut candidate = plan.clone();
    for (n, row) in numbers.iter().zip(rows(&response["shots"])?) {
        if row["shot"].as_u64() != Some(*n as u64) {
            return Err("Fidelity repair shot numbers must match the requested order.".into());
        }
        let template = text(row, "prompt_template")?.trim();
        if template.is_empty() || template.chars().count() > 1600 || template.contains('\0') {
            return Err(
                "Repaired motion prompt must be nonempty text of at most 1600 characters.".into(),
            );
        }
        candidate["shots"][n - 1]["prompt_template"] = json!(template);
        remove(&mut candidate["shots"][n - 1], &["prompt"]);
    }
    let source = &plan["source"];
    let mut revised = ctx.compile(&candidate, &source_request(source), &source["characters"])?;
    let hash = ctx.plan_hash(&revised)?;
    revised["motion_plan"] = plan["motion_plan"].clone();
    revised["motion_plan"]["plan_sha256"] = json!(hash);
    Ok(revised)
}
fn save_repair(
    ctx: &mut Context<'_>,
    file: &Path,
    context: &str,
    plan: &Value,
    numbers: &[usize],
    attempts: u64,
) -> Result<()> {
    let response = numbers
        .iter()
        .map(|n| json!({"shot":n,"prompt_template":plan["shots"][n-1]["prompt_template"]}))
        .collect::<Vec<_>>();
    let hash = ctx.plan_hash(plan)?;
    save(
        file,
        &json!({"context_sha256":context,"status":"complete","attempts":attempts,"shot_numbers":numbers,"plan_sha256":hash,"response":{"shots":response}}),
    )
}
fn calls(run: &Path, kind: &str) -> Result<Vec<(u64, PathBuf)>> {
    let prefix = format!("shooting-script-{kind}-source-fidelity-repair-");
    let extension = if kind == "request" { ".txt" } else { ".json" };
    let mut found = Vec::new();
    for entry in fs::read_dir(run).map_err(|e| e.to_string())? {
        let e = entry.map_err(|e| e.to_string())?;
        if let Some(n) = e
            .file_name()
            .to_str()
            .and_then(|s| s.strip_prefix(&prefix))
            .and_then(|s| s.strip_suffix(extension))
            .and_then(|s| s.parse::<u64>().ok())
        {
            found.push((n, e.path()));
        }
    }
    found.sort_by_key(|(n, _)| *n);
    Ok(found)
}

/// Keep useful prompt repairs even when the old attempt never reached final
/// publication. The checkpoint must belong to these exact director/image inputs.
pub(super) fn saved_template(
    ctx: &mut Context<'_>,
    run: &Path,
    director: &Value,
    boards: &Value,
    number: usize,
) -> Result<Option<Value>> {
    let file = run.join("source-fidelity-repair.json");
    let Ok(repair) = read(&file) else {
        return Ok(None);
    };
    if repair["status"] != "complete" {
        return Ok(None);
    }
    let images = rows(&boards["shots"])?
        .iter()
        .map(|s| json!({"shot":s["shot"],"image":s["image"]}))
        .collect::<Vec<_>>();
    let director_hash = ctx.plan_hash(director)?;
    let image_hash = ctx.digest(&json!(images))?;
    let context = ctx.digest(&json!({
        "director_plan_sha256":director_hash,"storyboard_sha256":image_hash,
    }))?;
    if repair["context_sha256"] != context {
        return Ok(None);
    }
    Ok(repair["response"]["shots"]
        .as_array()
        .and_then(|shots| shots.iter().find(|s| s["shot"] == number))
        .and_then(|s| s["prompt_template"].as_str())
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 1600 && !s.contains('\0'))
        .map(|s| json!(s)))
}

fn advisory(ctx: &mut Context<'_>, mut plan: Value, run: &Path) -> Result<Value> {
    let hash = ctx.plan_hash(&plan)?;
    let mut receipt = json!({"version":1,"plan_sha256":hash,
        "policy":"motion-review-v1","status":"not-run"});
    // Preserve an existing audit honestly, including negative findings. Missing
    // wording in a second textual audit must not veto the passed image/motion
    // review or start another Codex review/repair cycle.
    if let Ok(saved) = read(&run.join("source-fidelity-review.json"))
        && saved["plan_sha256"] == hash
        && let Ok(report) = validate(saved["report"].clone(), &plan, false)
    {
        let warnings = rows(&report["checks"])?
            .iter()
            .filter(|c| c["preserved"] == false)
            .count();
        receipt["status"] = json!("advisory");
        receipt["report"] = report;
        if warnings > 0 {
            ctx.activity(format!(
                "Continuing with {warnings} saved source-fidelity note(s); image and motion review passed"
            ))?;
        }
    }
    plan["source_fidelity"] = receipt;
    Ok(plan)
}

pub fn ensure(ctx: &mut Context<'_>, mut plan: Value, run: &Path) -> Result<Value> {
    let source = plan["source"].clone();
    let context=ctx.digest(&json!({"director_plan_sha256":plan["motion_plan"]["director_plan_sha256"],"storyboard_sha256":plan["motion_plan"]["storyboard_sha256"]}))?;
    let repair_path = run.join("source-fidelity-repair.json");
    let mut repaired = false;
    let mut attempts = 0;
    if repair_path.is_file() {
        let repair = read(&repair_path)?;
        if repair["context_sha256"] == context {
            attempts = repair["attempts"]
                .as_u64()
                .filter(|n| (1..=2).contains(n))
                .ok_or("Saved fidelity repair has an invalid attempt count.")?;
            let numbers = rows(&repair["shot_numbers"])?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n > 0 && *n <= rows(&plan["shots"]).map_or(0, Vec::len))
                        .ok_or("Saved fidelity repair has invalid shot numbers.".into())
                })
                .collect::<Result<Vec<_>>>()?;
            if numbers
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                != numbers
            {
                return Err("Saved fidelity repair has invalid shot numbers.".into());
            }
            match repair["status"].as_str() {
                Some("complete") => {
                    plan = replace(ctx, &plan, &repair["response"], &numbers)?;
                    if repair["plan_sha256"] != ctx.plan_hash(&plan)? {
                        return Err(
                            "Saved fidelity repair no longer matches its motion directions.".into(),
                        );
                    }
                    repaired = true;
                }
                Some("started") => {
                    let first = repair["first_response"]
                        .as_u64()
                        .filter(|n| *n > 0)
                        .ok_or("Saved fidelity repair has an invalid response checkpoint.")?;
                    for (n, file) in calls(run, "response")?.into_iter().rev() {
                        if n < first {
                            continue;
                        }
                        if let Ok(revised) = read(&file)
                            .and_then(|response| replace(ctx, &plan, &response, &numbers))
                        {
                            save_repair(ctx, &repair_path, &context, &revised, &numbers, attempts)?;
                            plan = revised;
                            repaired = true;
                            break;
                        }
                    }
                }
                _ => return Err("Saved fidelity repair has an invalid status.".into()),
            }
        }
    }
    if std::env::var("AIVS_STRICT_SOURCE_FIDELITY").as_deref() != Ok("1") {
        return advisory(ctx, plan, run);
    }
    ctx.activity("Checking final motion prompts with the optional strict source-fidelity audit")?;
    loop {
        ctx.check()?;
        let hash = ctx.plan_hash(&plan)?;
        let review = run.join("source-fidelity-review.json");
        let saved = if review.is_file() {
            read(&review)?
        } else {
            json!({})
        };
        let report = if saved["plan_sha256"] == hash {
            validate(saved["report"].clone(), &plan, false)?
        } else {
            let shots = rows(&plan["shots"])?
                .iter()
                .enumerate()
                .map(|(n, s)| json!({"shot":n+1,"prompt":s["prompt"]}))
                .collect::<Vec<_>>();
            let instruction = format!(
                "{}\nDATA\n{}",
                include_str!("fidelity-review.txt").trim_end(),
                json!({"script":source["script"],"requirements":requirements(&plan)?,"shots":shots})
            );
            let report = ctx.plan_call(
                "source-fidelity",
                instruction,
                schema("fidelity")?,
                &source,
                run,
                json!([]),
                |_, v| validate(v, &plan, false),
            )?;
            save(
                &review,
                &json!({"version":1,"plan_sha256":hash,"report":report}),
            )?;
            report
        };
        let failed = rows(&report["checks"])?
            .iter()
            .filter(|c| c["preserved"] != true)
            .cloned()
            .collect::<Vec<_>>();
        if failed.is_empty() {
            plan["source_fidelity"] = json!({"version":1,"plan_sha256":hash,"report":report});
            return Ok(plan);
        }
        if repaired {
            validate(report.clone(), &plan, true)?;
        }
        let requirements = requirements(&plan)?;
        let mut numbers = BTreeSet::new();
        for check in &failed {
            let requirement = rows(&requirements)?
                .iter()
                .find(|r| r["id"] == check["id"])
                .ok_or("Unknown fidelity requirement.")?;
            for n in rows(&requirement["shot_numbers"])? {
                numbers.insert(n.as_u64().ok_or("Invalid fidelity shot number.")? as usize);
            }
        }
        let numbers = numbers.into_iter().collect::<Vec<_>>();
        if attempts >= 2 {
            return Err("The source-fidelity repair retry limit is exhausted. Saved prompts and findings are preserved.".into());
        }
        let budget = ctx.budget(&source, run)?;
        if budget["used_seconds"].as_f64().unwrap_or(0.0)
            >= budget["limit_seconds"].as_f64().unwrap_or(600.0)
        {
            return Err("The total planning budget is exhausted. Increase --planning-budget-seconds to resume the source-fidelity repair.".into());
        }
        let instruction = format!(
            "{}\nDATA\n{}",
            include_str!("fidelity-repair.txt").trim_end(),
            json!({"plan":plan,"failed_checks":failed,"shot_numbers":numbers})
        );
        attempts += 1;
        let first = calls(run, "request")?.last().map_or(1, |(n, _)| n + 1);
        save(
            &repair_path,
            &json!({"context_sha256":context,"status":"started","attempts":attempts,"shot_numbers":numbers,"first_response":first}),
        )?;
        let revised = ctx.plan_call(
            "source-fidelity-repair",
            instruction,
            schema("fidelity_repair")?,
            &source,
            run,
            json!([]),
            |ctx, v| replace(ctx, &plan, &v, &numbers),
        )?;
        save_repair(ctx, &repair_path, &context, &revised, &numbers, attempts)?;
        plan = revised;
        repaired = true;
    }
}
