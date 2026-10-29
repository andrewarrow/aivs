//! Project-owned creative revisions. Runs bind to these rather than copying them.
use super::*;

fn revision_directory(project: &Path, id: &str) -> Result<PathBuf> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid creative revision ID.".into());
    }
    Ok(project.join("preproduction/revisions").join(id))
}

pub(super) fn bind(run: &Path, project: &Path, directory: &Path, reused: bool) -> Result<()> {
    save(
        &run.join("preproduction.json"),
        &json!({
            "kind":"aivs-revision-binding", "version":1,
            "revision":directory.file_name().and_then(|s| s.to_str()),
            "directory":directory.strip_prefix(project).map_err(|e| e.to_string())?,
            "reused":reused,
        }),
    )
}

fn save_request(project: &Path, run: &Path, request: &Value) -> Result<()> {
    let mut source = request.clone();
    remove(&mut source, &["render_profile", "planning_budget_seconds"]);
    let mut settings = json!({});
    for key in ["render_profile", "planning_budget_seconds"] {
        if let Some(value) = request.get(key) {
            settings[key] = value.clone();
        }
    }
    save(
        &run.join("screenplay-request.json"),
        &json!({"$aivs_base":artifacts::store_object(project,&source)?,"$aivs_fields":settings}),
    )
}

fn scene(source: &Value) -> Value {
    source
        .get("scene_key")
        .cloned()
        .unwrap_or_else(|| json!({"heading":source["beats"].get(0).map(|b| &b["scene"])}))
}

fn compatible_scene(before: &Value, after: &Value) -> bool {
    if before.get("scene_key").is_some() && after.get("scene_key").is_some() {
        before["scene_key"] == after["scene_key"]
    } else {
        before["beats"].get(0).map(|b| &b["scene"]) == after["beats"].get(0).map(|b| &b["scene"])
    }
}

fn creative_name(name: &str) -> bool {
    name.starts_with("shooting-script")
        || name.starts_with("storyboard")
        || name.starts_with("source-fidelity")
        || matches!(
            name,
            "motion-directions.json" | "planning-budget.json" | "screenplay.txt"
        )
}

fn artifact_json(name: &str) -> bool {
    // Raw planner responses may be empty or incomplete after a failed attempt.
    // Preserve their original bytes, even when a later attempt succeeds.
    name.ends_with(".json") && !name.starts_with("shooting-script-response-")
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("Creative artifacts cannot contain symlinks.".into());
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        } else if kind.is_file() && entry.file_name() != ".DS_Store" {
            fs::copy(entry.path(), target.join(entry.file_name())).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Import the old layout once without removing files still referenced by old footage.
/// Only project-owned image descriptors are rewritten; raw task traces stay intact.
fn import(ctx: &mut Context<'_>, legacy: &Path, project: &Path) -> Result<PathBuf> {
    if legacy.starts_with(project.join("preproduction/revisions"))
        && legacy.join("revision.json").is_file()
    {
        return Ok(legacy.to_owned());
    }
    let plan = read(&legacy.join("shooting-script.json"))?;
    let receipt = read(&legacy.join("preproduction.json")).unwrap_or(Value::Null);
    if let Some(original) = receipt["source_directory"].as_str().map(PathBuf::from) {
        if let Ok(original) = original.canonicalize() {
            if original != legacy
                && original.starts_with(project.join("runs"))
                && read(&original.join("shooting-script.json")).is_ok_and(|p| p == plan)
                && original.join("motion-directions.json").is_file()
            {
                return import(ctx, &original, project);
            }
        }
    }
    let boards = read(&legacy.join("storyboards.json"))?;
    let id = artifacts::hash(
        serde_json::to_vec(&json!({"legacy_plan":plan,"boards":boards}))
            .map_err(|e| e.to_string())?,
    );
    let directory = revision_directory(project, &id)?;
    if directory.join("revision.json").is_file() {
        return Ok(directory);
    }
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(legacy).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !creative_name(&name) {
            continue;
        }
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&entry.path(), &directory.join(&name))?;
        } else {
            if artifact_json(&name) {
                save(
                    &directory.join(&name),
                    &artifacts::read_json(&entry.path())?,
                )?;
            } else {
                fs::copy(entry.path(), directory.join(&name)).map_err(|e| e.to_string())?;
            }
        }
    }
    let report = if legacy.join("render.json").is_file() {
        read(&legacy.join("render.json"))?
    } else {
        read(&legacy.join("input.json"))?
    };
    for library in ["characters", "sets"] {
        if report["libraries"].get(library).is_none() && legacy.join(library).is_dir() {
            copy_tree(&legacy.join(library), &directory.join(library))?;
        }
    }
    let request = if legacy.join("screenplay-request.json").is_file() {
        read(&legacy.join("screenplay-request.json"))?
    } else {
        source_request(&plan["source"])
    };
    save(
        &directory.join("input.json"),
        &json!({"request":request,
        "source":plan["source"],"libraries":report["libraries"],"seed":boards["seed"]}),
    )?;
    fn relocate(value: &mut Value, legacy: &Path, project: &Path, directory: &Path) -> Result<()> {
        if let Some(object) = value.as_object_mut() {
            if let Some(file) = object
                .get("file")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                if Path::new(&file).starts_with("storyboards")
                    && object
                        .get("scope")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == "run")
                {
                    let original = legacy
                        .join(&file)
                        .canonicalize()
                        .map_err(|e| e.to_string())?;
                    if !original.starts_with(legacy) {
                        return Err("Legacy image escapes its run.".into());
                    }
                    object.insert(
                        "file".into(),
                        json!(
                            directory
                                .join(file)
                                .strip_prefix(project)
                                .map_err(|e| e.to_string())?
                        ),
                    );
                    object.insert("scope".into(), json!("project"));
                } else if Path::new(&file).starts_with("references")
                    && object.get("scope").and_then(Value::as_str) == Some("project")
                {
                    let asset = Value::Object(object.clone());
                    let original =
                        crate::storyboard::resolve_image(&json!({"image":asset}), legacy)?;
                    let target = project.join(&file);
                    if target.exists() {
                        if artifacts::file_hash(&target)? != artifacts::file_hash(&original)? {
                            return Err(
                                "A supplied reference conflicts with a project asset.".into()
                            );
                        }
                    } else {
                        artifacts::exclusive_bytes(
                            &target,
                            &fs::read(original).map_err(|e| e.to_string())?,
                        )?;
                    }
                }
            }
            for (key, child) in object.iter_mut() {
                // These are original receipts, whose hashes must continue to
                // describe the exact request and response that produced them.
                if key != "generation" && key != "runtime" {
                    relocate(child, legacy, project, directory)?;
                }
            }
        } else if let Some(array) = value.as_array_mut() {
            for child in array {
                relocate(child, legacy, project, directory)?;
            }
        }
        Ok(())
    }
    let mut plan = plan;
    let mut boards = boards;
    let mut review = read(&legacy.join("storyboard-review.json"))?;
    relocate(&mut plan, legacy, project, &directory)?;
    relocate(&mut boards, legacy, project, &directory)?;
    relocate(&mut review, legacy, project, &directory)?;
    let images = rows(&boards["shots"])?
        .iter()
        .map(|s| json!({"shot":s["shot"],"image":s["image"]}))
        .collect::<Vec<_>>();
    plan["motion_plan"]["images"] = json!(images);
    plan["motion_plan"]["storyboard_sha256"] = json!(ctx.digest(&json!(images))?);
    let old_policy = review["binding"]["review_policy_sha256"].clone();
    review["binding"] = boards::review_binding(ctx, &plan, &boards)?;
    review["binding"]["review_policy_sha256"] = old_policy;
    boards["continuity_acceptance"]["review_sha256"] = json!(ctx.digest(&review)?);
    save(&directory.join("storyboards.json"), &boards)?;
    save(&directory.join("storyboard-review.json"), &review)?;
    save(
        &directory.join("storyboard-references.json"),
        &json!({"version":1,"source_sha256":plan["source_sha256"],"references":boards["references"]}),
    )?;
    ctx.publish(&plan, &directory)?;
    seal(project, &directory, &id, &plan, Value::Null)?;
    Ok(directory)
}

fn seal(project: &Path, directory: &Path, id: &str, plan: &Value, parent: Value) -> Result<()> {
    // Resolve structured artifacts before replacing any manifest; raw planner
    // responses remain untouched and publishing is restartable.
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file()
            && artifact_json(&entry.file_name().to_string_lossy())
            && entry.file_name() != "revision.json"
        {
            files.push((entry.path(), artifacts::read_json(&entry.path())?));
        }
    }
    for (file, value) in files {
        save(&file, &artifacts::store_object(project, &value)?)?;
    }
    save(
        &directory.join("revision.json"),
        &json!({"kind":"aivs-creative-revision","version":1,
        "id":id,"parent":parent,"source_sha256":plan["source_sha256"],"scene":scene(&plan["source"]),
        "shooting_script":"shooting-script.json","storyboards":"storyboards.json","status":"complete"}),
    )
}

fn parent(
    ctx: &mut Context<'_>,
    project: &Path,
    source: &Value,
    current: &Path,
) -> Result<Option<PathBuf>> {
    if let Some(id) = ctx.options["parent_revision"].as_str() {
        let directory = revision_directory(project, id)?;
        let manifest = read(&directory.join("revision.json"))?;
        if manifest["status"] != "complete" {
            return Err("Parent revision is not complete.".into());
        }
        return Ok(Some(directory));
    }
    let head = project
        .join("preproduction/heads")
        .join(format!("{}.json", ctx.digest(&scene(source))?));
    // A failed motion/review pass still leaves useful direction and images.
    // Prefer the latest compatible attempt over the last completed head so a
    // new run does not discard work simply because final acceptance is absent.
    let runs = project.join("runs");
    let mut candidates = if runs.is_dir() {
        fs::read_dir(runs)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    candidates.sort();
    for run in candidates.into_iter().rev() {
        let Ok(creative) = crate::project::creative_root(&run) else {
            continue;
        };
        if creative == current {
            continue;
        }
        let Ok(plan) = read(&creative.join("storyboard-plan.json")) else {
            continue;
        };
        if plan["planning_stage"] != "storyboard" || !compatible_scene(&plan["source"], source) {
            continue;
        }
        if creative != run {
            return Ok(Some(creative));
        }
        // Legacy run-local pixels must be imported before another revision can
        // reference them. Import requires the old final acceptance records.
        if run.join("shooting-script.json").is_file()
            && run.join("storyboard-review.json").is_file()
        {
            return import(ctx, &run, project).map(Some);
        }
    }
    if head.is_file() {
        let record = read(&head)?;
        let directory = revision_directory(project, text(&record, "revision")?)?;
        if directory != current && directory.join("revision.json").is_file() {
            return Ok(Some(directory));
        }
    }
    Ok(None)
}

pub(super) fn prepare(
    ctx: &mut Context<'_>,
    request: &Value,
    profiles: &Value,
    run: &Path,
    seed: i64,
) -> Result<Value> {
    let project = ctx
        .project_root()
        .ok_or("Missing project for revision preparation.")?;
    let source = ctx.source(request, profiles)?;
    let mut id = preparation::cache_key(ctx, request, profiles)?;
    if let Some(file) = ctx.options["shooting_script_path"]
        .as_str()
        .map(PathBuf::from)
    {
        let file = file.canonicalize().map_err(|e| e.to_string())?;
        if file.file_name().is_none_or(|s| s != "shooting-script.json") {
            return Err(
                "Select the finalized shooting-script.json beside its storyboard assets.".into(),
            );
        }
        let supplied = read(&file)?;
        id = ctx.digest(&json!({"key":id,"supplied_plan":supplied}))?;
        let imported = import(
            ctx,
            file.parent().ok_or("Missing supplied plan directory.")?,
            &project,
        )?;
        ctx.options["shooting_script_path"] = json!(imported.join("shooting-script.json"));
    }
    if let Some(parent) = ctx.options.get("parent_revision") {
        id = ctx.digest(&json!({"key":id,"parent":parent}))?;
    }
    if ctx.options["fresh"] == true {
        id = artifacts::hash(format!("{id}:{}", artifacts::timestamp()));
    }
    let bound = crate::project::creative_root(run)?;
    let directory = if bound != run {
        bound
    } else {
        revision_directory(&project, &id)?
    };
    let id = directory
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid revision directory.")?
        .to_owned();
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let manifest = directory.join("revision.json");
    bind(run, &project, &directory, manifest.is_file())?;
    save_request(&project, run, request)?;
    if manifest.is_file() {
        let record = read(&manifest)?;
        if record["source_sha256"] != source["source_sha256"] {
            return Err("This run is pinned to a different screenplay revision.".into());
        }
        let (plan, _, boards) = preparation::load_final(ctx, &directory, request, profiles)?;
        let head = project
            .join("preproduction/heads")
            .join(format!("{}.json", ctx.digest(&scene(&source))?));
        save(&head, &json!({"revision":id}))?;
        ctx.activity("Reusing the pinned creative revision; rendering settings are independent")?;
        return Ok(json!({"plan":plan,"boards":boards}));
    }
    let input = directory.join("input.json");
    let saved = if input.is_file() {
        read(&input)?
    } else {
        let parent = if let Some(file) = ctx.options["shooting_script_path"].as_str() {
            Path::new(file).parent().map(Path::to_owned)
        } else if ctx.options["fresh"] == true {
            None
        } else {
            parent(ctx, &project, &source, &directory)?
        };
        let report = read(&run.join("render.json"))?;
        let saved = json!({"request":source_request(&source),"source":source,"libraries":report["libraries"],
            "seed":seed,"parent":parent.as_ref().map(|p| p.strip_prefix(&project).map(Path::to_owned)).transpose().map_err(|e| e.to_string())?});
        save(&input, &saved)?;
        fs::write(directory.join("screenplay.txt"), text(request, "script")?)
            .map_err(|e| e.to_string())?;
        saved
    };
    if saved["source"]["source_sha256"] != source["source_sha256"] {
        return Err("Creative revision inputs changed.".into());
    }
    if let Some(relative) = saved["parent"].as_str() {
        let parent = project
            .join(relative)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !parent.starts_with(project.join("preproduction/revisions")) {
            return Err("Parent revision escapes the project.".into());
        }
        ctx.options["parent_preproduction"] = json!(parent);
    }
    let seed = saved["seed"]
        .as_i64()
        .ok_or("Missing revision generation seed.")?;
    let result = preparation::prepare_local(ctx, request, profiles, &directory, seed)?;
    if result["plan"]["planning_stage"] == "final" {
        seal(
            &project,
            &directory,
            &id,
            &result["plan"],
            saved["parent"].clone(),
        )?;
        let head = project
            .join("preproduction/heads")
            .join(format!("{}.json", ctx.digest(&scene(&source))?));
        save(&head, &json!({"revision":id}))?;
    }
    Ok(result)
}
