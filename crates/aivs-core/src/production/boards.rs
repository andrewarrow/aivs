use super::*;
use crate::{characters, media, prop_reference, render_formats, storyboard, storyboard_backend};
pub const POLICY: &str = "per-shot-first-frame-v2";

pub fn backend(ctx: &Context<'_>) -> Result<Value> {
    match std::env::var("AIVS_STORYBOARD_BACKEND")
        .as_deref()
        .unwrap_or("codex-imagegen")
    {
        "codex-imagegen" => Ok(storyboard_backend::configuration()),
        "mflux" => Ok(
            json!({"backend":"mflux","runtime_version":"0.24.0","model":"black-forest-labs/FLUX.2-klein-4B",
            "model_revision":"e7b7dc27f91deacad38e78976d1f2b499d76a294","quantization":8,"steps":4,"guidance":1.0,"scheduler":"flow_match_euler_discrete"}),
        ),
        _ => {
            let _ = ctx;
            Err("AIVS_STORYBOARD_BACKEND must be codex-imagegen or mflux.".into())
        }
    }
}
pub fn image_call(ctx: &mut Context<'_>, operation: &str, mut args: Value) -> Result<Value> {
    args["root"] = json!(ctx.root);
    if backend(ctx)?["backend"] != "mflux" {
        return storyboard_backend::dispatch(
            operation,
            &args,
            ctx.cancelled.clone(),
            &mut ctx.emit,
        );
    }
    if operation != "imagegen_generate" {
        return ctx.worker("mflux_check", json!({}));
    }
    let output = path(&args, "output")?;
    if output.exists() {
        return Err(format!(
            "Refusing to overwrite storyboard image: {}",
            output.display()
        ));
    }
    let mut references = rows(&args["references"])?.clone();
    let mut prompt = text(&args, "prompt")?.to_owned();
    if let Some(target) = args["edit_target"].as_str() {
        references.retain(|v| v.as_str() != Some(target));
        references.insert(0, json!(target));
        let preserve = rows(&args["preserve"])?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        prompt = format!(
            "Edit the first image. {} Preserve unchanged: {preserve}",
            text(&args, "correction")?
        );
    }
    let prepared = storyboard_backend::dispatch(
        "prepare_image_references",
        &json!({"root":ctx.root,"references":references,"output":output,"keep_first":!args["edit_target"].is_null()}),
        ctx.cancelled.clone(),
        &mut ctx.emit,
    )?;
    let mut runtime=ctx.worker("mflux_generate",json!({"prompt":format!("{prompt}{}",prepared["mapping"].as_str().unwrap_or_default()),
        "references":prepared["paths"],"composition":args["composition"],"output":output,"seed":args["seed"],"width":args["width"],"height":args["height"]}))?;
    for source in rows(&prepared["sources"])? {
        if artifacts::file_hash(&path(source, "path")?)? != text(source, "sha256")? {
            return Err("A storyboard reference changed during local generation.".into());
        }
    }
    runtime["source_inputs"] = prepared["sources"].clone();
    runtime["reference_mapping"] = prepared["mapping"].clone();
    Ok(runtime)
}
pub fn dimensions(plan: &Value) -> Result<(u64, u64)> {
    if plan.get("render_format").is_some() {
        let spec = render_formats::resolve(&plan["render_format"])?;
        Ok((
            spec["model_width"].as_u64().ok_or("Invalid model width.")?,
            spec["model_height"]
                .as_u64()
                .ok_or("Invalid model height.")?,
        ))
    } else {
        Ok((1280, 704))
    }
}
pub fn prop_binding(ctx: &mut Context<'_>, spec: &Value, image: &Value) -> Result<Value> {
    Ok(
        json!({"policy":"prop-structure-v2","reviewer_sha256":ctx.implementation("prop_reference.py")?,"definition_sha256":ctx.digest(spec)?,"image_sha256":image["sha256"]}),
    )
}
pub fn contracts(ctx: &mut Context<'_>, plan: &Value, boards: &Value) -> Result<Value> {
    let specs = if plan.get("world").is_some() {
        storyboard::reference_specs(plan)?
    } else {
        json!([])
    };
    let mut hashes = json!({});
    let mut bindings = json!({});
    let mut shared = json!({});
    for spec in rows(&specs)? {
        let id = text(spec, "id")?;
        hashes[id] = json!(ctx.digest(spec)?);
        if spec["kind"] != "prop" {
            continue;
        }
        if let Some(entry) = boards["references"]
            .as_array()
            .and_then(|a| a.iter().find(|r| r["id"] == id))
        {
            bindings[id] = prop_binding(ctx, spec, &entry["image"])?;
        }
        let definition = &spec["definition"];
        if rows(&definition["components"])?.is_empty()
            && rows(&definition["relationships"])?.is_empty()
        {
            let mut design = json!({});
            for key in ["type", "shape", "size", "color", "features"] {
                design[key] = definition[key].clone();
            }
            shared[id] = json!(ctx.digest(&design)?);
        }
    }
    Ok(json!({"specs":specs,"spec_hashes":hashes,"prop_bindings":bindings,"shared_designs":shared}))
}
pub fn prepare(ctx: &mut Context<'_>, plan: &Value, boards: &Value, run: &Path) -> Result<Value> {
    let mut fingerprints = Vec::new();
    for shot in rows(&plan["shots"])? {
        fingerprints.push(ctx.shot_hash(plan, shot)?);
    }
    storyboard::dispatch(
        "storyboard_prepare",
        &json!({"plan":plan,"manifest":boards,"run_dir":run,"fingerprints":fingerprints}),
    )
}
pub fn references(
    ctx: &mut Context<'_>,
    plan: &Value,
    boards: &Value,
    run: &Path,
) -> Result<Value> {
    let mut args = contracts(ctx, plan, boards)?;
    merge(
        &mut args,
        &json!({"plan":plan,"boards":boards,"run_dir":run}),
    )?;
    storyboard::dispatch("storyboard_prepare_references", &args)
}
pub fn review_policy(ctx: &mut Context<'_>) -> Result<String> {
    let mut identity = json!({});
    for name in [
        "storyboard.py",
        "shooting_script.py",
        "shooting-script-review-instructions.txt",
    ] {
        identity[name] = json!(ctx.implementation(name)?);
    }
    ctx.digest(&identity)
}
pub fn review_binding(ctx: &mut Context<'_>, plan: &Value, boards: &Value) -> Result<Value> {
    let policy = review_policy(ctx)?;
    let source = ctx.digest(&plan["source"])?;
    let hash = ctx.digest(plan)?;
    rule(
        "storyboard_review_binding",
        json!({"plan":plan,"boards":boards,"review_policy_sha256":policy,"source_snapshot_sha256":source,"plan_sha256":hash}),
    )
}
pub fn accepted(ctx: &mut Context<'_>, plan: &Value, boards: &Value, run: &Path) -> Result<Value> {
    rule(
        "validate_storyboard_manifest",
        json!({"plan":plan,"boards":boards}),
    )?;
    prepare(ctx, plan, boards, run)?;
    references(ctx, plan, boards, run)?;
    let mut profiles = Vec::new();
    for supplied in rows(&plan["source"]["characters"])? {
        profiles.push(characters::read_profile(
            &run.join("characters")
                .join(text(supplied, "id")?)
                .join("character.json"),
        )?);
    }
    let screenplay = run.join("screenplay.txt");
    let script = if screenplay.is_file() {
        Some(fs::read_to_string(screenplay).map_err(|e| e.to_string())?)
    } else {
        None
    };
    rule(
        "validate_storyboard_snapshots",
        json!({"plan":plan,"profiles":profiles,"screenplay":script}),
    )?;
    let file = run.join("storyboard-review.json");
    if !file.is_file() {
        return Err(
            "Storyboard sequence has no accepted continuity review; stop before video inference."
                .into(),
        );
    }
    let report = read(&file)?;
    let binding = review_binding(ctx, plan, boards)?;
    let digest = ctx.digest(&report)?;
    rule(
        "validate_acceptance",
        json!({"plan":plan,"boards":boards,"report":report,"binding":binding,"report_sha256":digest}),
    )?;
    Ok(report)
}
pub(super) fn frame_request(
    ctx: &mut Context<'_>,
    plan: &Value,
    shot: &Value,
    profiles: &Value,
    run: &Path,
    prior: &Value,
    number: usize,
    seed: i64,
) -> Result<Value> {
    let library = if run.join("storyboard-references.json").is_file() {
        read(&run.join("storyboard-references.json"))?
    } else {
        Value::Null
    };
    let mut args = contracts(ctx, plan, &library)?;
    let (width, height) = dimensions(plan)?;
    let world = ctx.digest(plan.get("world").unwrap_or(&json!({})))?;
    merge(
        &mut args,
        &json!({"plan":plan,"shot":shot,"profiles":profiles,"run_dir":run,"prior":prior,"number":number,"seed":seed,"width":width,"height":height,"library":library,"world_sha256":world,"backend":backend(ctx)?}),
    )?;
    storyboard::dispatch("storyboard_frame_request", &args)
}
fn orphan(file: &Path) -> Result<()> {
    if file.exists() {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Invalid image filename.")?;
        let ext = file.extension().and_then(|s| s.to_str()).unwrap_or("png");
        fs::rename(
            file,
            file.with_file_name(format!(
                "{stem}.interrupted-{}.{ext}",
                artifacts::timestamp()
            )),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn image_info(file: &Path, flag: &AtomicBool) -> Result<Value> {
    media::dispatch("prepare_source_image", &json!({"source":file}), flag)
}
fn image_asset(file: &Path, run: &Path, info: &Value) -> Result<Value> {
    Ok(
        json!({"file":file.strip_prefix(run).map_err(|e|e.to_string())?,"sha256":info["source_sha256"],"width":info["width"],"height":info["height"]}),
    )
}
fn reference_attempt(
    ctx: &mut Context<'_>,
    spec: &Value,
    request: &Value,
    run: &Path,
) -> Result<(Value, PathBuf)> {
    let hash = ctx.digest(request)?;
    let folder = run.join("storyboards/references").join(&hash);
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let checkpoint = folder.join("reference.json");
    if checkpoint.is_file() {
        let entry = read(&checkpoint)?;
        if entry["generation_sha256"] != hash || entry["generation"] != *request {
            return Err("Reference checkpoint inputs changed.".into());
        }
        storyboard::resolve_image(&entry, run)?;
        return Ok((entry, folder));
    }
    ctx.activity(format!(
        "Generating reusable {} reference",
        text(spec, "id")?
    ))?;
    image_call(ctx, "imagegen_check", json!({}))?;
    save(&folder.join("request.json"), request)?;
    let target = folder.join("reference.png");
    orphan(&target)?;
    let inputs = rows(&request["inputs"])?
        .iter()
        .map(|r| storyboard::resolve_image(&json!({"image":r}), run))
        .collect::<Result<Vec<_>>>()?;
    let runtime = image_call(
        ctx,
        "imagegen_generate",
        json!({"prompt":request["prompt"],"references":inputs,"output":target,"seed":request["seed"],"width":1280,"height":704}),
    )?;
    let info = image_info(&target, &ctx.cancelled)?;
    if info["source_sha256"] != runtime["sha256"] {
        return Err("Physical reference changed during generation.".into());
    }
    let entry = json!({"id":spec["id"],"kind":spec["kind"],"definition_sha256":ctx.digest(spec)?,"generation_sha256":hash,"generation":request,"image":image_asset(&target,run,&info)?,"runtime":runtime});
    save(&checkpoint, &entry)?;
    Ok((entry, folder))
}
pub fn generate_references(
    ctx: &mut Context<'_>,
    plan: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
    repair_ids: &Value,
    round: u64,
) -> Result<Value> {
    let file = run.join("storyboard-references.json");
    let saved = if file.is_file() {
        read(&file)?
    } else {
        json!({"version":1,"references":[]})
    };
    if saved
        .get("source_sha256")
        .is_some_and(|v| *v != plan["source_sha256"])
    {
        return Err("Saved physical references belong to another source.".into());
    }
    let mut entries = rows(&saved["references"])?.clone();
    let specs = storyboard::reference_specs(plan)?;
    for (i, spec) in rows(&specs)?.iter().enumerate() {
        let mut inputs = json!([]);
        if spec["kind"] == "body" {
            let cast = rows(&plan["source"]["cast"])?
                .iter()
                .find(|c| c["role"] == spec["definition"]["role"])
                .ok_or("Unknown reference role.")?;
            let p = rows(profiles)?
                .iter()
                .find(|p| p["id"] == cast["character"])
                .ok_or("Unknown reference performer.")?;
            inputs = json!([{"file":format!("characters/{}/{}",text(p,"id")?,text(&p["image"],"file")?),"sha256":p["image"]["sha256"]}]);
        }
        let attempt = if rows(repair_ids)?.contains(&spec["id"]) {
            round
        } else {
            0
        };
        if let Some(old) = entries.iter().find(|r| r["id"] == spec["id"]) {
            if attempt == 0
                && old["definition_sha256"] == ctx.digest(spec)?
                && old["generation"]["backend"] == backend(ctx)?
            {
                storyboard::resolve_image(old, run)?;
                if spec["kind"] != "prop"
                    || prop_reference::require_accepted(
                        spec,
                        old,
                        &prop_binding(ctx, spec, &old["image"])?,
                    )
                    .is_ok()
                {
                    continue;
                }
            }
        }
        let mut prompt = spec["prompt"].clone();
        let mut last = Value::Null;
        let mut approved = false;
        let count = if spec["kind"] == "prop" { 3 } else { 1 };
        let mut folder = PathBuf::new();
        for retry in 0..count {
            let request_seed =
                (i128::from(seed) + 100003 + i as i128 + i128::from(attempt) * 1009 + retry * 7919)
                    .rem_euclid(1 << 31) as i64;
            let mut request = json!({"policy":POLICY,"spec":spec,"inputs":inputs,"prompt":prompt,"seed":request_seed,"backend":backend(ctx)?});
            if retry > 0 {
                request["structure_retry"] = json!(retry);
                request["repair_of"] = last["image"]["sha256"].clone();
            }
            (last, folder) = reference_attempt(ctx, spec, &request, run)?;
            if spec["kind"] != "prop" {
                approved = true;
                break;
            }
            let binding = prop_binding(ctx, spec, &last["image"])?;
            let name = format!("prop-reference-{}", &ctx.digest(&binding)?[..16]);
            let image = storyboard::resolve_image(&last, run)?;
            last["prop_validation"] = prop_reference::review(
                &json!({"spec":spec,"binding":binding,"name":name,"source_sha256":plan["source_sha256"],"image_path":image,"run_dir":run,"checkpoint":folder.join("structure-review.json"),"executable":ctx.options["executable"],"limit":ctx.options["planning_budget_seconds"].as_f64().unwrap_or(600.0)}),
                ctx.cancelled.clone(),
                &mut ctx.emit,
            )?;
            save(&folder.join("reference.json"), &last)?;
            if last["prop_validation"]["approved"] == true {
                approved = true;
                break;
            }
            prompt = json!(prop_reference::generation_prompt(
                &spec["definition"],
                Some(&last["prop_validation"]["observation"]["regeneration_prompt"])
            )?);
            ctx.activity(format!(
                "Rejected {} reference ({}/3): {}",
                text(spec, "id")?,
                retry + 1,
                prop_reference::failure_summary(&last["prop_validation"])?
            ))?;
        }
        if !approved {
            return Err(format!(
                "Canonical {} failed structural inspection after 3 images. {} Full inspection: {}",
                text(spec, "id")?,
                prop_reference::failure_summary(&last["prop_validation"])?,
                folder.join("structure-review.json").display()
            ));
        }
        if let Some(old) = entries.iter_mut().find(|r| r["id"] == spec["id"]) {
            *old = last;
        } else {
            entries.push(last);
        }
        save(
            &file,
            &json!({"version":1,"source_sha256":plan["source_sha256"],"references":entries}),
        )?;
    }
    let ordered = rows(&specs)?
        .iter()
        .map(|s| {
            entries
                .iter()
                .find(|r| r["id"] == s["id"])
                .cloned()
                .ok_or("Missing generated reference.".into())
        })
        .collect::<Result<Vec<_>>>()?;
    let result = json!({"version":1,"source_sha256":plan["source_sha256"],"references":ordered});
    save(&file, &result)?;
    Ok(result)
}
fn render_frame(
    ctx: &mut Context<'_>,
    request: &Value,
    partial: &Path,
    target: &Path,
    run: &Path,
) -> Result<Value> {
    orphan(partial)?;
    orphan(target)?;
    let refs = rows(&request["references"])?
        .iter()
        .map(|r| storyboard::resolve_image(&json!({"image":r}), run))
        .collect::<Result<Vec<_>>>()?;
    let mut args = json!({"prompt":request["prompt"],"references":refs,"output":partial,"seed":request["seed"],"width":request["width"],"height":request["height"],"composition":request["composition"]});
    if let Some(reason) = request["repair_reason"].as_str() {
        if !request["edit_target"].is_null() {
            args["edit_target"] = json!(storyboard::resolve_image(
                &json!({"image":request["edit_target"]}),
                run
            )?);
            args["correction"] = json!(format!("{reason}\n{}", text(request, "prompt")?));
            args["preserve"] = request["preserve"].clone();
            args["prompt"] = json!("Apply only the requested correction.");
        } else {
            args["prompt"] = json!(format!(
                "{}\nCorrection: {reason}",
                text(request, "prompt")?
            ));
        }
    }
    let runtime = image_call(ctx, "imagegen_generate", args)?;
    let info = image_info(partial, &ctx.cancelled)?;
    if info["source_sha256"] != runtime["sha256"]
        || info["width"] != request["width"]
        || info["height"] != request["height"]
    {
        return Err("Storyboard did not produce the requested image dimensions or bytes.".into());
    }
    for r in rows(&request["references"])? {
        storyboard::resolve_image(&json!({"image":r}), run)?;
    }
    fs::rename(partial, target).map_err(|e| e.to_string())?;
    Ok(json!({"runtime":runtime,"image":image_asset(target,run,&info)?}))
}
pub fn regenerate(
    ctx: &mut Context<'_>,
    plan: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
    index: usize,
    reason: &str,
    directory: &Path,
    prior: &Value,
) -> Result<Value> {
    if !directory.starts_with(run) || directory == run || reason.trim().is_empty() {
        return Err(
            "Repaired storyboard images need a concrete reason and a run-local directory.".into(),
        );
    }
    characters::verify(rows(profiles)?, run)?;
    let shot = rows(&plan["shots"])?
        .get(index)
        .ok_or("Invalid storyboard repair shot index.")?;
    let mut request = frame_request(ctx, plan, shot, profiles, run, prior, index + 1, seed)?;
    request["repair_reason"] = json!(reason);
    request["edit_target"] = prior.clone();
    request["preserve"] = json!([
        "correct facial identity and anatomy",
        "camera framing and lighting",
        "correct costume and environment",
        "all correct prop geometry and placement"
    ]);
    let hash = ctx.digest(&request)?;
    let fingerprint = ctx.shot_hash(plan, shot)?;
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let checkpoint = directory.join("storyboard.json");
    if checkpoint.is_file() {
        let entry = read(&checkpoint)?;
        if entry["generation_sha256"] != hash
            || entry["generation"] != request
            || entry["planned_shot_sha256"] != fingerprint
            || entry["shot"] != index + 1
        {
            return Err("Saved storyboard repair belongs to different inputs.".into());
        }
        storyboard::resolve_image(&entry, run)?;
        return Ok(entry);
    }
    let backend = image_call(ctx, "imagegen_check", json!({}))?;
    save(&directory.join("request.json"), &request)?;
    ctx.activity(format!("Regenerating storyboard {}: {reason}", index + 1))?;
    let generated = render_frame(
        ctx,
        &request,
        &directory.join("frame.partial.png"),
        &directory.join("frame.png"),
        run,
    )?;
    let mut entry = json!({"shot":index+1,"planned_shot_sha256":fingerprint,"generation_sha256":hash,"generation":request,"backend":backend});
    merge(&mut entry, &generated)?;
    save(&checkpoint, &entry)?;
    Ok(entry)
}
pub fn generate(
    ctx: &mut Context<'_>,
    plan: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
) -> Result<Value> {
    fs::create_dir_all(run).map_err(|e| e.to_string())?;
    characters::verify(rows(profiles)?, run)?;
    for supplied in rows(&plan["source"]["characters"])? {
        let saved = rows(profiles)?
            .iter()
            .find(|p| p["id"] == supplied["id"])
            .ok_or("Storyboard plan and saved character snapshots do not match.")?;
        if !supplied
            .as_object()
            .is_some_and(|s| s.iter().all(|(k, v)| saved.get(k) == Some(v)))
        {
            return Err("Storyboard plan and saved character snapshots do not match.".into());
        }
    }
    let library = generate_references(ctx, plan, profiles, run, seed, &json!([]), 0)?;
    let directory = run.join("storyboards");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let file = run.join("storyboards.json");
    let mut manifest = if file.is_file() {
        read(&file)?
    } else {
        json!({"version":1,"policy":POLICY,"source_sha256":plan["source_sha256"],"seed":seed,"status":"generating","shots":[]})
    };
    if manifest["version"] != 1
        || manifest["policy"] != POLICY
        || manifest["seed"] != seed
        || manifest["source_sha256"] != plan["source_sha256"]
        || rows(&manifest["shots"])?.len() > rows(&plan["shots"])?.len()
    {
        return Err("Saved storyboards belong to different generation inputs.".into());
    }
    manifest["references"] = library["references"].clone();
    save(&file, &manifest)?;
    let mut checked = false;
    for (index, shot) in rows(&plan["shots"])?.iter().enumerate() {
        ctx.check()?;
        let number = index + 1;
        let request = frame_request(ctx, plan, shot, profiles, run, &Value::Null, number, seed)?;
        let hash = ctx.digest(&request)?;
        let fingerprint = ctx.shot_hash(plan, shot)?;
        if let Some(entry) = manifest["shots"].get(index).cloned() {
            if entry["shot"] != number
                || entry["planned_shot_sha256"] != fingerprint
                || entry["generation_sha256"]
                    != ctx.digest(entry.get("generation").unwrap_or(&json!({})))?
            {
                return Err(format!(
                    "Saved storyboard {number} has different inputs; start a new run."
                ));
            }
            let old = storyboard::resolve_image(&entry, run)?;
            if entry["generation_sha256"] == hash && entry["generation"] == request {
                ctx.activity(format!("Reusing saved storyboard {number}"))?;
                continue;
            }
            if old == directory.join(format!("{number:03}.png")) {
                let archived = directory.join(format!(
                    "{number:03}.superseded-{}.png",
                    artifacts::timestamp()
                ));
                fs::rename(old, &archived).map_err(|e| e.to_string())?;
                manifest["shots"][index]["image"]["file"] =
                    json!(archived.strip_prefix(run).map_err(|e| e.to_string())?);
                save(&file, &manifest)?;
                save(&archived.with_extension("json"), &manifest["shots"][index])?;
            }
            remove(&mut manifest, &["continuity_acceptance"]);
        }
        if !checked {
            manifest["backend"] = image_call(ctx, "imagegen_check", json!({}))?;
            checked = true;
        }
        ctx.activity(format!(
            "Generating storyboard {number}/{}",
            rows(&plan["shots"])?.len()
        ))?;
        manifest["status"] = json!("generating");
        manifest["current_shot"] = json!(number);
        remove(&mut manifest, &["error"]);
        save(&file, &manifest)?;
        save(
            &directory.join(format!("{number:03}.request.json")),
            &request,
        )?;
        let result = render_frame(
            ctx,
            &request,
            &directory.join(format!("{number:03}.partial.png")),
            &directory.join(format!("{number:03}.png")),
            run,
        );
        match result {
            Ok(generated) => {
                let mut entry = json!({"shot":number,"planned_shot_sha256":fingerprint,"generation_sha256":hash,"generation":request});
                merge(&mut entry, &generated)?;
                let entries = manifest["shots"]
                    .as_array_mut()
                    .ok_or("Invalid storyboard entries.")?;
                if index < entries.len() {
                    entries[index] = entry;
                } else {
                    entries.push(entry);
                }
                save(&file, &manifest)?;
            }
            Err(error) => {
                manifest["status"] = json!(if ctx.check().is_err() {
                    "interrupted"
                } else {
                    "failed"
                });
                manifest["error"] = json!(error);
                save(&file, &manifest)?;
                return Err(error);
            }
        }
    }
    prepare(ctx, plan, &manifest, run)?;
    manifest["status"] = json!("complete");
    remove(&mut manifest, &["current_shot", "error"]);
    save(&file, &manifest)?;
    Ok(manifest)
}
