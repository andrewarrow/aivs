use super::*;
use crate::codex::{self, ProductionRequest};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::time::Instant;

fn image_bytes(item: &Value) -> Result<Vec<u8>> {
    if item["failure"]["type"] == "usageLimitExceeded" {
        return Err(
            "Codex image-generation usage limit reached. Resume when your image allowance resets."
                .into(),
        );
    }
    if item["status"] != "completed" {
        return Err(
            "Codex image generation did not complete. See the saved image-generation log.".into(),
        );
    }
    if let Some(path) = item["savedPath"]
        .as_str()
        .map(Path::new)
        .filter(|path| path.is_file())
    {
        return fs::read(path).map_err(|e| e.to_string());
    }
    if let Some(encoded) = item["result"].as_str().filter(|s| !s.is_empty()) {
        let encoded = if encoded.starts_with("data:") {
            encoded.split_once(',').map_or(encoded, |(_, bytes)| bytes)
        } else {
            encoded
        };
        return STANDARD
            .decode(encoded)
            .map_err(|_| "Codex returned invalid image bytes.".into());
    }
    Err(format!(
        "Codex returned no saved image or image bytes. {SETUP}"
    ))
}

fn observed_inputs(item: &Value) -> Result<Value> {
    let mut inputs = serde_json::Map::new();
    if let Some(rows) = item["inputImages"].as_array() {
        for row in rows {
            let path = Path::new(text(row, "path")?)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            inputs.insert(path.to_string_lossy().into_owned(), row["sha256"].clone());
        }
    }
    Ok(Value::Object(inputs))
}

fn dimension(args: &Value, name: &str) -> Result<u32> {
    args[name]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| "Storyboard dimensions must be positive integers.".into())
}

struct Checkpoint {
    binding: Value,
    thread: Value,
    images: Vec<Value>,
    workspace: PathBuf,
    path: PathBuf,
}

impl Checkpoint {
    fn save_image(&mut self, item: &Value) -> Result<()> {
        let id = text(item, "id")?;
        let position = self.images.iter().position(|previous| previous["id"] == id);
        let mut merged = position
            .map(|i| self.images[i].clone())
            .unwrap_or_else(|| json!({}));
        for (key, value) in item.as_object().ok_or("Invalid image generation event.")? {
            if !value.is_null() {
                merged[key] = value.clone();
            }
        }
        if merged["status"] == "completed" {
            let bytes = image_bytes(&merged)?;
            let artifact = self.workspace.join(format!("{}.image", &hash(id)[..16]));
            if artifact.exists() {
                if fs::read(&artifact).map_err(|e| e.to_string())? != bytes {
                    return Err("A completed Codex image changed during generation.".into());
                }
            } else {
                exclusive_bytes(&artifact, &bytes)?;
            }
            merged
                .as_object_mut()
                .ok_or("Invalid image item.")?
                .remove("result");
            merged["savedPath"] = json!(artifact);
            merged["artifact_sha256"] = json!(hash(&bytes));
        }
        match position {
            Some(i) => self.images[i] = merged,
            None => self.images.push(merged),
        }
        if self
            .images
            .iter()
            .any(|row| row["id"] == id && row["status"] == "completed")
        {
            atomic_json(
                &self.path,
                &json!({"binding": self.binding, "thread": self.thread,
                "images": self.images.iter().filter(|row| row["status"] == "completed").collect::<Vec<_>>()}),
            )?;
        }
        Ok(())
    }

    fn load(&mut self, expected: &Value) -> Result<()> {
        if !self.path.is_file() {
            return Ok(());
        }
        let saved = read_json(&self.path)?;
        if saved["binding"] != self.binding {
            return Err("Saved Codex image belongs to different storyboard inputs.".into());
        }
        for item in saved["images"]
            .as_array()
            .ok_or("Invalid saved Codex images.")?
        {
            if hash(image_bytes(item)?) != text(item, "artifact_sha256")? {
                return Err("Saved Codex image bytes changed.".into());
            }
            self.images.push(item.clone());
        }
        self.thread = saved.get("thread").cloned().unwrap_or_else(|| json!({}));
        let mut rejected = false;
        for item in &self.images {
            if observed_inputs(item)? != *expected {
                rejected = true;
                break;
            }
        }
        if rejected {
            archive(&self.path, "images.rejected", "json")?;
            self.images.clear();
            self.thread = json!({});
        }
        Ok(())
    }
}

pub(super) fn generate(
    args: &Value,
    root: &Path,
    identity: &Value,
    cancelled: Arc<AtomicBool>,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let output = PathBuf::from(text(args, "output")?);
    if !output.is_absolute() {
        return Err("Storyboard output must be absolute.".into());
    }
    if output.exists() {
        return Err(format!(
            "Refusing to overwrite storyboard image: {}",
            output.display()
        ));
    }
    let (width, height) = (dimension(args, "width")?, dimension(args, "height")?);
    let prompt = text(args, "prompt")?;
    let seed = args["seed"].as_i64().ok_or("Invalid storyboard seed.")?;
    let mut references = reference_paths(args)?;
    let edit_target = args["edit_target"]
        .as_str()
        .map(|path| Path::new(path).canonicalize().map_err(|e| e.to_string()))
        .transpose()?;
    if let Some(target) = &edit_target {
        references.retain(|path| path != target);
        references.insert(0, target.clone());
    }
    let inputs = storyboard_images::prepare(&references, &output, edit_target.is_some())?;
    let mut expected = serde_json::Map::new();
    for path in &inputs.paths {
        expected.insert(path.to_string_lossy().into_owned(), json!(file_hash(path)?));
    }
    let expected = Value::Object(expected);
    let instruction = instruction(
        args,
        &(prompt.to_owned() + &inputs.mapping),
        &inputs.paths,
        width,
        height,
    )?;
    let binding = json!({"backend": configuration(), "instruction": instruction, "inputs": expected,
        "source_inputs": inputs.sources, "identity": identity, "width": width, "height": height, "requested_seed": seed});
    let parent = output
        .parent()
        .ok_or("Missing storyboard output directory.")?;
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid image output filename.")?;
    let workspace = parent.join(format!("{stem}.codex-{}", &fingerprint(&binding)?[..16]));
    fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    fs::write(workspace.join("request.txt"), &instruction).map_err(|e| e.to_string())?;
    let mut checkpoint = Checkpoint {
        binding,
        thread: json!({}),
        images: Vec::new(),
        path: workspace.join("images.json"),
        workspace: workspace.clone(),
    };
    let started = Instant::now();
    checkpoint.load(&expected)?;
    if checkpoint.images.is_empty() {
        let attempt = workspace.join(format!("attempt-{}", timestamp()));
        fs::create_dir(&attempt).map_err(|e| e.to_string())?;
        let executable = text(identity, "executable")?;
        let request = ProductionRequest {
            command: vec![
                executable.into(),
                "app-server".into(),
                "--enable".into(),
                "image_generation".into(),
            ],
            instruction,
            cwd: workspace.clone(),
            bootstrap_cwd: bootstrap(root),
            timeout_seconds: (1200.0 - started.elapsed().as_secs_f64()).max(0.0),
            events_path: attempt.join("events.jsonl"),
            errors_path: attempt.join("errors.txt"),
            response_path: attempt.join("response.txt"),
            output_schema: None,
            image_paths: inputs.paths.clone(),
            image_generation: true,
        };
        codex::run_production(request, cancelled, |event| {
            match event["type"].as_str() {
                Some("image") => checkpoint.save_image(&event["item"]),
                Some("thread") => { checkpoint.thread = event["result"].clone(); Ok(()) },
                Some("activity_delta") => activity(emit, format!("Codex: {}", event["delta"].as_str().unwrap_or(""))),
                _ => emit(event),
            }
        }).map_err(|failure| if failure.kind == "timeout" {
            "Codex storyboard image generation timed out. Saved work is preserved; Resume to retry.".to_owned()
        } else { failure.message })?;
    } else {
        activity(
            emit,
            "Reusing the completed Codex image-generation checkpoint",
        )?;
    }
    let completed = checkpoint
        .images
        .iter()
        .filter(|item| item["status"] == "completed")
        .collect::<Vec<_>>();
    if completed.is_empty() {
        if let Some(item) = checkpoint.images.last() {
            image_bytes(item)?;
        }
    }
    if completed.len() != 1 {
        return Err(format!(
            "Codex must return exactly one generated image; received {}. See the logs in {}. {SETUP}",
            completed.len(),
            workspace.display()
        ));
    }
    let item = completed[0];
    let bytes = image_bytes(item)?;
    if observed_inputs(item)? != expected {
        return Err(format!(
            "Codex {} did not confirm use of exactly the supplied visual references through imageGeneration.inputImages path/SHA-256 metadata. This app-server protocol is unsupported for reference generation; update Codex and run storyboard_backend.py check --refresh. The generated image was not accepted; see {}. {SETUP}",
            identity["codex_version"]
                .as_str()
                .unwrap_or("unknown version"),
            checkpoint.path.display()
        ));
    }
    validate_inputs(&inputs.sources)?;
    let tool_sources = inputs
        .paths
        .iter()
        .map(|path| json!({"path": path, "sha256": expected[path.to_string_lossy().as_ref()]}))
        .collect::<Vec<_>>();
    validate_inputs(&tool_sources)?;
    let normalized = storyboard_images::normalize(&bytes, &output, width, height)?;
    let original_path = Path::new(text(item, "savedPath")?)
        .strip_prefix(parent)
        .map_err(|_| "Generated image must remain in its output directory.")?;
    let mut result = identity.clone();
    let mut image_prompt = prompt.to_owned();
    if let Some(composition) = args["composition"].as_str().filter(|s| !s.is_empty()) {
        image_prompt.push_str(&format!("\nComposition: {composition}"));
    }
    let runtime = json!({"pipeline": if edit_target.is_some() { "targeted-edit" } else if !inputs.paths.is_empty() { "reference-image-generation" } else { "text-to-image" },
        "edit_target": edit_target, "correction": args["correction"], "preserve": args.get("preserve").unwrap_or(&json!([])),
        "source_inputs": inputs.sources, "reference_mapping": inputs.mapping, "normalized_content_box": normalized["normalized_content_box"],
        "prompt": image_prompt, "generation_directory": workspace, "requested_seed": seed,
        "thread_id": checkpoint.thread["thread"]["id"], "model": checkpoint.thread.get("model").unwrap_or(&json!("codex-configured")),
        "model_provider": checkpoint.thread["modelProvider"], "image_generation_id": item["id"], "revised_prompt": item["revisedPrompt"],
        "input_images": item.get("inputImages").unwrap_or(&json!([])),
        "original_image": {"file": original_path, "width": normalized["source_width"], "height": normalized["source_height"], "sha256": hash(&bytes)},
        "elapsed_seconds": (started.elapsed().as_secs_f64()*1000.0).round()/1000.0, "sha256": normalized["sha256"]});
    result
        .as_object_mut()
        .ok_or("Invalid image generation identity.")?
        .extend(runtime.as_object().ok_or("Invalid image runtime.")?.clone());
    Ok(result)
}
