use super::*;
use crate::{media, motion, storyboard};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
pub fn identity(ctx: &mut Context<'_>) -> Result<String> {
    let implementation =
        fs::read_to_string(ctx.root.join("python/motion_previs.py")).map_err(|e| e.to_string())?;
    let rust = ctx.implementation("motion_previs.py")?;
    let continuity = boards::review_policy(ctx)?;
    ctx.digest(&json!({"policy":motion::POLICY,"implementation":implementation,"rust":rust,"continuity":continuity,"image_backend":boards::backend(ctx)?}))
}
fn timing(ctx: &mut Context<'_>, plan: &Value, manifest: &Value) -> Result<()> {
    let hash = ctx.digest(plan)?;
    motion::validate_timing(plan, manifest, &json!(hash), 24)
}
fn base(ctx: &mut Context<'_>, run: &Path) -> Result<(Value, Value)> {
    let plan = read(&crate::project::creative_file(run, "shooting-script.json")?)?;
    let boards = read(&crate::project::creative_file(run, "storyboards.json")?)?;
    if plan["planning_stage"] != "final" {
        return Err("Finish the existing plan before preparing motion keyframes.".into());
    }
    boards::accepted(ctx, &plan, &boards, run)?;
    Ok((plan, boards))
}
pub fn anchors(row: &Value, run: &Path) -> Result<Value> {
    if rows(&row["keyframes"])?.len() == 1 {
        return Ok(Value::Null);
    }
    let frames=rows(&row["keyframes"] )?.iter().map(|f|Ok(json!({"file":storyboard::resolve_image(f,run)?,"sha256":f["image"]["sha256"],"frame":f["frame"],"strength":f["strength"]}))).collect::<Result<Vec<_>>>()?;
    Ok(json!(frames))
}
fn evidence(run: &Path, record: &Value) -> Result<PathBuf> {
    let root = run.canonicalize().map_err(|e| e.to_string())?;
    let path = root
        .join(text(record, "file")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("Keyframe review escapes run directory.".into());
    }
    Ok(path)
}
pub fn accepted(
    ctx: &mut Context<'_>,
    plan: &Value,
    boards: &Value,
    manifest: &Value,
    run: &Path,
) -> Result<Value> {
    boards::accepted(ctx, plan, boards, run)?;
    timing(ctx, plan, manifest)?;
    if manifest["status"] != "accepted" || manifest["implementation"] != identity(ctx)? {
        return Err("Motion keyframes have no current acceptance.".into());
    }
    let schema = rule("schema", json!({"name":"motion.frame"}))?;
    let hash = ctx.digest(plan)?;
    for row in rows(&manifest["shots"])? {
        let number = row["shot"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("Invalid motion shot.")? as usize;
        let original = rows(&boards["shots"])?
            .get(number - 1)
            .ok_or("Invalid motion shot.")?;
        let frames = rows(&row["keyframes"])?;
        if frames
            .first()
            .is_none_or(|f| f["image"] != original["image"])
        {
            return Err(
                "Motion opening pixels must retain the accepted original storyboard.".into(),
            );
        }
        if frames.len() > 1 {
            media::dispatch(
                "prepare_image_anchors",
                &json!({"anchors":anchors(row,run)?,"frames":plan["shots"][number-1]["seconds"].as_u64().ok_or("Invalid shot duration.")?*24,"width":original["image"]["width"],"height":original["image"]["height"]}),
                &ctx.cancelled,
            )?;
        }
        for frame in frames {
            let image = storyboard::resolve_image(frame, run)?;
            for reference in rows(&frame["request"]["references"])? {
                storyboard::resolve_image(&json!({"image":reference}), run)?;
            }
            if let Some(repair) = frame["request"].get("repair") {
                storyboard::resolve_image(&json!({"image":repair["rejected_image"]}), run)?;
                let file = evidence(run, &repair["review"])?;
                if ctx.digest(&read(&file)?)? != repair["review"]["sha256"] {
                    return Err("Keyframe repair evidence changed.".into());
                }
            }
            let file = evidence(run, &frame["review"])?;
            let review = read(&file)?;
            let mut state = json!({});
            for key in schema["properties"]
                .as_object()
                .ok_or("Invalid motion frame schema.")?
                .keys()
            {
                state[key] = frame[key].clone();
            }
            if ctx.digest(&review)? != frame["review"]["sha256"]
                || review["binding"]
                    != json!({"request":ctx.digest(&frame["request"])?,"image":frame["image"]})
                || frame["request"]["frame"] != state
                || frame["request"]["plan_sha256"] != hash
                || motion::validate_review(plan, number, frame, &review["result"])?["accepted"]
                    != true
            {
                return Err(
                    "Keyframe acceptance is stale, rejected or contradicts its physical contract."
                        .into(),
                );
            }
            let decoded = media::dispatch(
                "prepare_source_image",
                &json!({"source":image}),
                &ctx.cancelled,
            )?;
            if decoded["width"] != frame["image"]["width"]
                || decoded["height"] != frame["image"]["height"]
            {
                return Err("Keyframe dimensions changed.".into());
            }
        }
    }
    Ok(manifest.clone())
}
fn plan_motion(ctx: &mut Context<'_>, run: &Path) -> Result<Value> {
    let (plan, boards) = base(ctx, run)?;
    let folder = run.join("motion");
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let hash = ctx.digest(&plan)?;
    let binding = json!({"plan_sha256":hash,"boards":boards::review_binding(ctx,&plan,&boards)?,"implementation":identity(ctx)?});
    let mut directed = Vec::new();
    for (index, shot) in rows(&plan["shots"])?.iter().enumerate() {
        let number = index + 1;
        let file = folder.join(format!("{number:03}.plan.json"));
        let row = if file.is_file() {
            let saved = read(&file)?;
            if saved["binding"] != binding {
                return Err(
                    "Saved motion direction is stale; preserve it and use a new run directory."
                        .into(),
                );
            }
            saved["direction"].clone()
        } else {
            let opening = storyboard::resolve_image(&boards["shots"][index], run)?;
            let prompt = format!(
                "{}{}",
                include_str!("motion-plan.txt"),
                json!({"shot_number":number,"shot":shot,"world":plan["world"],"source":plan["source"]})
            );
            let row = review::structured(
                ctx,
                &format!("plan-{number:03}"),
                prompt,
                rule("schema", json!({"name":"motion.shot"}))?,
                &folder,
                json!([opening]),
                Instant::now(),
                |ctx, value| {
                    if value["shot"] != number {
                        return Err("Wrong motion shot number.".into());
                    }
                    let mut provisional = directed.clone();
                    provisional.push(value.clone());
                    for (offset, shot) in rows(&plan["shots"])?.iter().enumerate().skip(number) {
                        provisional
                            .push(json!({"shot":offset+1,"keyframes":motion::fallback(shot,24)?}));
                    }
                    timing(
                        ctx,
                        &plan,
                        &json!({"version":1,"policy":motion::POLICY,"plan_sha256":hash,"shots":provisional}),
                    )?;
                    Ok(value)
                },
            )?;
            save(&file, &json!({"binding":binding,"direction":row}))?;
            row
        };
        directed.push(row);
    }
    let manifest = json!({"version":1,"policy":motion::POLICY,"plan_sha256":hash,"shots":directed});
    timing(ctx, &plan, &manifest)?;
    save(&folder.join("plan.json"), &manifest)?;
    Ok(manifest)
}
fn inspect(
    ctx: &mut Context<'_>,
    plan: &Value,
    number: usize,
    frame: &Value,
    image: &Path,
    inputs: &[PathBuf],
    folder: &Path,
    binding: &Value,
) -> Result<Value> {
    let checkpoint = folder.join("review.json");
    if checkpoint.is_file() {
        let saved = read(&checkpoint)?;
        if saved["binding"] != *binding {
            return Err("Saved keyframe review belongs to different pixels or contracts.".into());
        }
        return motion::validate_review(plan, number, frame, &saved["result"]);
    }
    let prompt = format!(
        "{}{}",
        include_str!("motion-review.txt"),
        json!({"shot":number,"frame":frame,"world":plan["world"],"source":plan["source"],"reference_files_in_attachment_order":inputs})
    );
    let mut images = vec![image.to_path_buf()];
    images.extend_from_slice(inputs);
    let result = review::structured(
        ctx,
        "inspect",
        prompt,
        rule("schema", json!({"name":"motion.review"}))?,
        folder,
        json!(images),
        Instant::now(),
        |_, value| motion::validate_review(plan, number, frame, &value),
    )?;
    save(&checkpoint, &json!({"binding":binding,"result":result}))?;
    Ok(result)
}
fn orphan(file: &Path) -> Result<()> {
    if file.exists() {
        let name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Invalid keyframe filename.")?;
        fs::rename(
            file,
            file.with_file_name(format!("{name}.interrupted-{}.png", artifacts::timestamp())),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn attempt(
    ctx: &mut Context<'_>,
    plan: &Value,
    number: usize,
    frame: &Value,
    original: &Value,
    request: &Value,
    inputs: &[PathBuf],
    folder: &Path,
    run: &Path,
    seed: i64,
    rejected: Option<&Value>,
    previous_review: Option<&Value>,
    fresh: bool,
) -> Result<(Value, Value)> {
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let checkpoint = folder.join("frame.json");
    let width = &original["image"]["width"];
    let height = &original["image"]["height"];
    let mut entry;
    let image = if checkpoint.is_file() {
        entry = read(&checkpoint)?;
        if entry["request"] != *request {
            return Err(
                "Saved keyframe inputs changed; preserve artifacts and use a new run.".into(),
            );
        }
        storyboard::resolve_image(&entry, run)?
    } else if frame["frame"] == 0 {
        let image = storyboard::resolve_image(original, run)?;
        entry = frame.clone();
        entry["image"] = original["image"].clone();
        entry["request"] = request.clone();
        save(&checkpoint, &entry)?;
        image
    } else {
        let image = folder.join("frame.png");
        let partial = folder.join("frame.partial.png");
        orphan(&partial)?;
        orphan(&image)?;
        let mut refs = rows(&request["references"])?.clone();
        if let Some(rejected) = rejected {
            if !fresh {
                refs.insert(0, rejected["image"].clone());
            }
        }
        let numbered = refs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut r = r.clone();
                r["image_number"] = json!(i + 1);
                r
            })
            .collect::<Vec<_>>();
        let mut prompt = format!(
            "{}{}",
            include_str!("motion-image.txt"),
            json!({"state":frame["state"],"world":plan["world"],"time_seconds":frame["frame"].as_f64().ok_or("Invalid keyframe time.")?/24.0,"reference_images":numbered})
        );
        let mut args = json!({"prompt":prompt,"references":inputs,"output":partial,"seed":seed,"width":width,"height":height,"composition":frame["composition"]});
        if let (Some(rejected), Some(review)) = (rejected, previous_review) {
            if !fresh {
                args["edit_target"] = json!(storyboard::resolve_image(rejected, run)?);
                args["correction"] = json!(format!(
                    "{}\nFindings: {}\n{prompt}",
                    text(review, "correction")?,
                    review["findings"]
                ));
                args["preserve"] = json!([
                    "correct face and anatomy",
                    "camera and framing",
                    "costume, environment and lighting",
                    "all geometry and contacts already consistent with the timed physical state"
                ]);
            } else {
                prompt.push_str(&format!(
                    "\nPrevious defects to avoid: {} {}",
                    text(review, "correction")?,
                    review["findings"]
                ));
                args["prompt"] = json!(prompt);
            }
        }
        ctx.activity(format!(
            "Preparing shot {number} keyframe at {:.3}s",
            frame["frame"].as_f64().unwrap_or_default() / 24.0
        ))?;
        let runtime = boards::image_call(ctx, "imagegen_generate", args)?;
        let info = media::dispatch(
            "prepare_source_image",
            &json!({"source":partial}),
            &ctx.cancelled,
        )?;
        if info["source_sha256"] != runtime["sha256"]
            || info["width"] != *width
            || info["height"] != *height
        {
            return Err("Generated keyframe has unexpected pixels or dimensions.".into());
        }
        fs::rename(&partial, &image).map_err(|e| e.to_string())?;
        entry = frame.clone();
        entry["request"] = request.clone();
        entry["runtime"] = runtime;
        entry["image"] = json!({"file":image.strip_prefix(run).map_err(|e|e.to_string())?,"sha256":info["source_sha256"],"width":width,"height":height});
        save(&checkpoint, &entry)?;
        image
    };
    for reference in rows(&request["references"])? {
        storyboard::resolve_image(&json!({"image":reference}), run)?;
    }
    if let Some(rejected) = rejected {
        storyboard::resolve_image(rejected, run)?;
    }
    let binding = json!({"request":ctx.digest(request)?,"image":entry["image"]});
    let result = inspect(ctx, plan, number, frame, &image, inputs, folder, &binding)?;
    let file = folder.join("review.json");
    entry["review"] = json!({"file":file.strip_prefix(run).map_err(|e|e.to_string())?,"sha256":ctx.digest(&read(&file)?)?});
    Ok((entry, result))
}
fn generate(ctx: &mut Context<'_>, run: &Path) -> Result<Value> {
    let (plan, boards) = base(ctx, run)?;
    boards::image_call(ctx, "imagegen_check", json!({}))?;
    let motion = plan_motion(ctx, run)?;
    let render = read(&run.join("render.json"))?;
    let profiles = &render["characters"];
    let seed = render["seed"].as_i64().ok_or("Invalid render seed.")?;
    let mut generated = Vec::new();
    for (index, (shot, direction)) in rows(&plan["shots"])?
        .iter()
        .zip(rows(&motion["shots"])?)
        .enumerate()
    {
        let number = index + 1;
        let original = &boards["shots"][index];
        let opening = storyboard::resolve_image(original, run)?;
        let mut frames: Vec<Value> = Vec::new();
        for frame in rows(&direction["keyframes"])? {
            let mut timed = shot.clone();
            timed["continuity"]["opening"] = frame["state"].clone();
            timed["opening_visible_roles"] = json!(
                rows(&frame["state"]["characters"])?
                    .iter()
                    .filter(|c| c["presence"] == "visible")
                    .map(|c| c["role"].clone())
                    .collect::<Vec<_>>()
            );
            let selected = boards::frame_request(
                ctx,
                &plan,
                &timed,
                profiles,
                run,
                &Value::Null,
                number,
                seed,
            )?["references"]
                .clone();
            let mut descriptions = BTreeMap::new();
            let mut canonical = Vec::new();
            for r in rows(&selected)? {
                let image = storyboard::resolve_image(&json!({"image":r}), run)?;
                descriptions.insert(image.clone(), r.clone());
                canonical.push(image);
            }
            let prior = if let Some(last) = frames.last() {
                storyboard::resolve_image(last, run)?
            } else {
                opening.clone()
            };
            let mut opening_desc = original["image"].clone();
            opening_desc["purpose"] = json!("approved opening frame");
            descriptions.insert(opening.clone(), opening_desc);
            let mut prior_desc = frames
                .last()
                .map(|f| f["image"].clone())
                .unwrap_or(original["image"].clone());
            prior_desc["purpose"] = json!("preceding accepted pose");
            descriptions.insert(prior.clone(), prior_desc);
            let mut seen = BTreeSet::new();
            let inputs = std::iter::once(prior)
                .chain(std::iter::once(opening.clone()))
                .chain(canonical)
                .filter(|p| seen.insert(p.clone()))
                .collect::<Vec<_>>();
            let mut reference_records = Vec::new();
            for p in &inputs {
                let mut record = descriptions
                    .get(p)
                    .cloned()
                    .ok_or("Missing keyframe reference description.")?;
                record["file"] = json!(p.strip_prefix(run).map_err(|e| e.to_string())?);
                record["sha256"] = json!(artifacts::file_hash(p)?);
                reference_records.push(record);
            }
            let request = json!({"implementation":identity(ctx)?,"plan_sha256":motion["plan_sha256"],"shot":number,"frame":frame,"references":reference_records});
            let frame_number = frame["frame"].as_u64().ok_or("Invalid keyframe number.")?;
            let folder = run
                .join("motion")
                .join(format!("{number:03}"))
                .join(format!("{frame_number:05}"));
            let frame_seed = seed
                .checked_add(number as i64)
                .and_then(|n| n.checked_add(frame_number as i64))
                .ok_or("Keyframe seed overflow.")?;
            let (mut entry, mut report) = attempt(
                ctx, &plan, number, frame, original, &request, &inputs, &folder, run, frame_seed,
                None, None, false,
            )?;
            for retry in 1..=3 {
                if report["accepted"] == true || frame_number == 0 {
                    break;
                }
                let fresh = report["repair_strategy"] == "regenerate" || retry == 3;
                let mut repaired = request.clone();
                repaired["repair"] = json!({"attempt":retry,"strategy":if fresh{"regenerate"}else{"targeted-edit"},"rejected_image":entry["image"],"review":entry["review"],"correction":report["correction"]});
                let result = attempt(
                    ctx,
                    &plan,
                    number,
                    frame,
                    original,
                    &repaired,
                    &inputs,
                    &folder.join(format!("repair-{retry}")),
                    run,
                    frame_seed + retry * 7919,
                    Some(&entry),
                    Some(&report),
                    fresh,
                )?;
                entry = result.0;
                report = result.1;
            }
            if report["accepted"] != true {
                return Err(format!(
                    "Keyframe {number}/{frame_number} rejected after bounded repair; all attempts preserved under {}",
                    folder.display()
                ));
            }
            frames.push(entry);
        }
        generated.push(json!({"shot":number,"keyframes":frames}));
    }
    let mut manifest = motion;
    manifest["shots"] = json!(generated);
    manifest["implementation"] = json!(identity(ctx)?);
    manifest["status"] = json!("accepted");
    accepted(ctx, &plan, &boards, &manifest, run)?;
    save(&run.join("motion/keyframes.json"), &manifest)?;
    Ok(manifest)
}
fn animatic(ctx: &mut Context<'_>, run: &Path) -> Result<Value> {
    let (plan, boards) = base(ctx, run)?;
    let manifest = read(&run.join("motion/keyframes.json"))?;
    accepted(ctx, &plan, &boards, &manifest, run)?;
    ctx.activity("Assembling timed keyframe animatic without LTX weights")?;
    let hash = ctx.digest(&manifest)?;
    media::dispatch(
        "assemble_animatic",
        &json!({"plan":plan,"manifest":manifest,"run_dir":run,"manifest_sha256":hash}),
        &ctx.cancelled,
    )
}
pub fn dispatch(ctx: &mut Context<'_>, args: &Value) -> Result<Value> {
    let run = path(args, "run_dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    match args["command"].as_str().unwrap_or("prepare") {
        "plan" => plan_motion(ctx, &run),
        "generate" => generate(ctx, &run),
        "accepted" => accepted(ctx, &args["plan"], &args["boards"], &args["manifest"], &run),
        "animatic" => animatic(ctx, &run),
        "compare" => comparison(ctx, &run, args),
        "prepare" => {
            generate(ctx, &run)?;
            let output = animatic(ctx, &run)?;
            ctx.event(json!({"type":"complete","output":output,"status":"previsualized"}))?;
            Ok(output)
        }
        _ => Err("Unknown motion command.".into()),
    }
}
fn comparison(ctx: &mut Context<'_>, run: &Path, args: &Value) -> Result<Value> {
    let (mut plan, boards) = base(ctx, run)?;
    let manifest = read(&run.join("motion/keyframes.json"))?;
    accepted(ctx, &plan, &boards, &manifest, run)?;
    let number = args["shot_number"].as_u64().unwrap_or(1) as usize;
    let shot = rows(&plan["shots"])?
        .get(number.checked_sub(1).ok_or("Invalid comparison shot.")?)
        .ok_or("Comparison shot number is outside the plan.")?
        .clone();
    let row = &manifest["shots"][number - 1];
    if shot["seconds"] != 5 || !matches!(rows(&row["keyframes"])?.len(), 3 | 4) {
        return Err(
            "The first comparison requires a five-second shot with three or four keyframes.".into(),
        );
    }
    let profile = args["profile"].as_str().unwrap_or("distilled");
    let directory = run
        .join("motion/comparison")
        .join(format!("{number:03}-{profile}"));
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let seed = (read(&run.join("render.json"))?["seed"]
        .as_i64()
        .ok_or("Invalid seed.")?
        + number as i64
        - 1)
    .rem_euclid(1 << 31);
    let binding = json!({"motion":ctx.digest(&manifest)?,"shot":number,"seed":seed,"config":model_config(profile)?,"prompt":shot["prompt"]});
    let opening = &row["keyframes"][0];
    for mode in ["single", "multiple"] {
        let folder = directory.join(mode);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let output = folder.join("video.mp4");
        let file = folder.join("render.json");
        if output.is_file() && file.is_file() {
            let saved = read(&file)?;
            if saved["binding"] != binding || saved["sha256"] != artifacts::file_hash(&output)? {
                return Err(
                    "Saved comparison output differs; preserve it and use another run.".into(),
                );
            }
            continue;
        }
        orphan(&output)?;
        accepted(ctx, &plan, &boards, &manifest, run)?;
        let runtime=ctx.worker("ltx_render",json!({"prompt":shot["prompt"],"seed":seed,"frames":120,"output":output,"image_file":storyboard::resolve_image(opening,run)?,"width":opening["image"]["width"],"height":opening["image"]["height"],"profile":profile,"image_anchors":if mode=="multiple"{anchors(row,run)?}else{Value::Null}}))?;
        save(
            &file,
            &json!({"binding":binding,"sha256":artifacts::file_hash(&output)?,"runtime":runtime}),
        )?;
    }
    remove(&mut plan, &["render_format"]);
    let mut outputs = json!({});
    let mut images = Vec::new();
    for mode in ["single", "multiple"] {
        let clip = directory.join(mode).join("video.mp4");
        review::shot(
            ctx,
            &plan,
            number - 1,
            &clip,
            &directory.join(mode).join("review"),
        )?;
        outputs[mode] = json!(artifacts::file_hash(&clip)?);
        let evidence = media::dispatch(
            "visual_review_sheets",
            &json!({"clip":clip,"directory":directory.join(mode).join("samples"),"seconds":5,"shot_number":number}),
            &ctx.cancelled,
        )?;
        images.extend(rows(&evidence["contact_sheets"])?.clone());
    }
    let mut review_binding = binding;
    review_binding["outputs"] = outputs;
    let report = directory.join("comparison.json");
    if report.is_file() {
        if read(&report)?["binding"] != review_binding {
            return Err("Comparison observations belong to different outputs.".into());
        }
    } else {
        let properties = json!({"visual_differences":{"type":"string"},"geometry":{"type":"string"},"appearance":{"type":"string"},"motion":{"type":"string"},"limitations":{"type":"string"}});
        let schema = json!({"type":"object","additionalProperties":false,"required":properties.as_object().ok_or("Invalid comparison schema.")?.keys().collect::<Vec<_>>(),"properties":properties});
        let observations=review::structured(ctx,"comparison","Compare actual video contact sheets in order: single-anchor then multiple-anchor. Describe observed telephone geometry, hands, cord, character identity and motion. Distinguish visible evidence from unsampled motion. Guides do not guarantee exact motion or continuity. These are experimental matched-seed outputs, not a general benchmark.".into(),schema,&directory,json!(images),Instant::now(),|_,v|Ok(v))?;
        save(
            &report,
            &json!({"binding":review_binding,"observations":observations}),
        )?;
    }
    Ok(json!(report))
}
