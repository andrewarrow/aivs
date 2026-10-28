//! Headless production orchestration. Python supplies model execution and the legacy JSON codec.
mod boards;
pub mod cli;
mod direction;
mod fidelity;
mod helpers;
mod incremental;
mod motion;
mod preparation;
mod rendering;
mod review;
mod revisions;
mod run;
use crate::{
    artifacts,
    continuity::{self, Result},
    inference, planning, shooting_script,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const STORYBOARD_POLICY: &str = "per-shot-first-frame-v1";
pub const MODEL_REVISION: &str = "746ca9aacb697d2c739f544d68b584214dedcc75";

pub struct Context<'a> {
    pub root: PathBuf,
    pub python: PathBuf,
    pub options: Value,
    pub cancelled: Arc<AtomicBool>,
    pub emit: &'a mut dyn FnMut(Value) -> Result<()>,
    identities: HashMap<String, String>,
    digests: HashMap<String, String>,
}

pub fn rows(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| "Expected a JSON array.".into())
}
pub fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("Missing production field: {field}."))
}
pub fn path(value: &Value, field: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(value, field)?))
}
pub fn read(path: &Path) -> Result<Value> {
    let value = artifacts::read_json(path)?;
    if !value.is_object() {
        return Err(format!("Expected a JSON object: {}", path.display()));
    }
    Ok(value)
}
pub fn save(path: &Path, value: &Value) -> Result<()> {
    artifacts::atomic_json(path, value)
}
pub fn remove(value: &mut Value, keys: &[&str]) {
    if let Some(object) = value.as_object_mut() {
        for key in keys {
            object.remove(*key);
        }
    }
}
pub fn merge(target: &mut Value, source: &Value) -> Result<()> {
    target
        .as_object_mut()
        .ok_or("Expected production object.")?
        .extend(
            source
                .as_object()
                .ok_or("Expected production object.")?
                .clone(),
        );
    Ok(())
}
pub fn rule(operation: &str, args: Value) -> Result<Value> {
    continuity::dispatch(&json!({"operation": operation, "args": args}))
}
pub fn shooting(operation: &str, args: Value) -> Result<Value> {
    shooting_script::dispatch(operation, &args)
}
pub fn model_config(profile: &str) -> Result<Value> {
    if !matches!(profile, "hq" | "distilled") {
        return Err("Local render profile must be distilled or hq.".into());
    }
    let mut value = json!({"backend":"mlx", "model":"dgrauet/ltx-2.5-mlx-q8", "model_revision":MODEL_REVISION,
        "runtime_revision":"14a3ddd6054736beb45b4d836135fa4d44490531", "profile":profile,
        "pipeline":if profile == "hq" {"hq-two-stage"} else {"distilled-two-stage"},
        "stage1_steps":if profile == "hq" {15} else {8}, "stage2_steps":if profile == "hq" {3} else {1},
        "quantization":"int8", "low_memory":true, "low_ram_streaming":true});
    if profile == "hq" {
        merge(
            &mut value,
            &json!({"sampler":"res_2s", "cfg_scale":3.0, "stg_scale":0.0, "enable_teacache":false,
                "negative_prompt_suffix":"subtitles, captions, text overlays, on-screen text"}),
        )?;
    } else {
        merge(&mut value, &json!({"preview_long_edge":320}))?;
    }
    Ok(value)
}

impl Context<'_> {
    pub fn project_root(&self) -> Option<PathBuf> {
        self.options["project_dir"].as_str().map(PathBuf::from)
    }

    pub fn preproduction_root(&self) -> PathBuf {
        self.project_root()
            .map(|root| root.join("preproduction"))
            .unwrap_or_else(|| self.root.join(".cache/preproduction"))
    }

    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err("Production interrupted; saved work is preserved.".into())
        } else {
            Ok(())
        }
    }
    pub fn event(&mut self, event: Value) -> Result<()> {
        self.check()?;
        (self.emit)(event)
    }
    pub fn activity(&mut self, message: impl Into<String>) -> Result<()> {
        self.event(json!({"type":"activity", "message":message.into()}))
    }
    pub fn worker(&mut self, operation: &str, payload: Value) -> Result<Value> {
        self.check()?;
        inference::run(
            &json!({"root":self.root,"python":self.python,"worker_operation":operation,"payload":payload}),
            self.cancelled.clone(),
            &mut self.emit,
        )
    }
    pub fn digest(&mut self, value: &Value) -> Result<String> {
        self.codec(value, false, true)
    }
    pub fn codec(&mut self, value: &Value, ascii: bool, compact: bool) -> Result<String> {
        let key =
            serde_json::to_string(&json!([ascii, compact, value])).map_err(|e| e.to_string())?;
        if let Some(hash) = self.digests.get(&key) {
            return Ok(hash.clone());
        }
        let hash = self
            .worker(
                "json_digest",
                json!({"value":value,"ascii":ascii,"compact":compact}),
            )?
            .as_str()
            .ok_or("Invalid legacy fingerprint response.")?
            .to_owned();
        self.digests.insert(key, hash.clone());
        Ok(hash)
    }
    pub fn implementation(&mut self, name: &str) -> Result<String> {
        if let Some(hash) = self.identities.get(name) {
            return Ok(hash.clone());
        }
        let sources = crate::implementation::sources();
        let rust: serde_json::Map<String, Value> = sources
            .as_object()
            .ok_or("Missing Rust source registry.")?
            .iter()
            .map(|(name, content)| {
                (
                    name.clone(),
                    json!(artifacts::hash(content.as_str().unwrap_or_default())),
                )
            })
            .collect();
        let value = json!({"python":artifacts::file_hash(&self.root.join("python").join(name))?,
            "rust":rust,"bridge":artifacts::file_hash(&self.root.join("python/rust_engine.py"))?});
        let hash = self.codec(&value, true, true)?;
        self.identities.insert(name.into(), hash.clone());
        Ok(hash)
    }
    pub fn source(&mut self, request: &Value, profiles: &Value) -> Result<Value> {
        let parsed = shooting(
            "parse_shooting_request",
            json!({"request":request,"profiles":profiles}),
        )?;
        let mut source = parsed["source"].clone();
        source["source_sha256"] = json!(self.digest(&parsed["identity"])?);
        Ok(source)
    }
    pub fn compile(
        &mut self,
        candidate: &Value,
        request: &Value,
        profiles: &Value,
    ) -> Result<Value> {
        let source = self.source(request, profiles)?;
        let tokenize = self.options["tokenize"] != false;
        let plan = shooting(
            "compile_shooting_script",
            json!({"candidate":candidate,"source":source,"request":request,"model_tokenizer":tokenize}),
        )?;
        if tokenize {
            let prompts: Vec<_> = rows(&plan["shots"])?
                .iter()
                .map(|s| s["prompt"].clone())
                .collect();
            let counts = self.worker("token_count", json!({"prompts":prompts}))?;
            if rows(&counts)?.len() != prompts.len() {
                return Err("Tokenizer returned an invalid prompt count.".into());
            }
            for (i, count) in rows(&counts)?.iter().enumerate() {
                let n = count
                    .as_u64()
                    .ok_or("Token counter must return a nonnegative integer.")?;
                if n > 1024 {
                    return Err(format!("Shot {} prompt exceeds 1024 Gemma tokens.", i + 1));
                }
            }
        }
        Ok(plan)
    }
    pub fn plan_hash(&mut self, plan: &Value) -> Result<String> {
        self.digest(&shooting(
            "shooting_fingerprint_content",
            json!({"plan":plan}),
        )?)
    }
    pub fn shot_hash(&mut self, plan: &Value, shot: &Value) -> Result<String> {
        let mut value = json!({"source_sha256":plan["source_sha256"],"planning_policy":plan["planning_policy"],"shot":shot});
        if let Some(world) = plan.get("world") {
            value["world"] = world.clone();
        }
        self.digest(&value)
    }
    pub fn load_plan(&mut self, file: &Path, request: &Value, profiles: &Value) -> Result<Value> {
        let saved = read(file)?;
        let artifact = self.compile(&saved, request, profiles)?;
        let fingerprint = self.plan_hash(&artifact)?;
        shooting(
            "shooting_validate_saved",
            json!({"saved":saved,"artifact":artifact,"fingerprint":fingerprint}),
        )
    }
    pub fn publish(&mut self, plan: &Value, run: &Path) -> Result<()> {
        if run.join("revision.json").is_file() {
            if read(&run.join("shooting-script.json"))? == *plan {
                return Ok(());
            }
            return Err(
                "Completed creative revisions are immutable; derive a new revision.".into(),
            );
        }
        let fingerprint = self.plan_hash(plan)?;
        shooting(
            "shooting_publish",
            json!({"plan":plan,"run_dir":run,"fingerprint":fingerprint}),
        )?;
        Ok(())
    }
    pub fn budget(&self, source: &Value, run: &Path) -> Result<Value> {
        planning::budget(
            &json!({"source_sha256":source["source_sha256"],"run_dir":run,"limit":self.options["planning_budget_seconds"].as_f64().unwrap_or(600.0)}),
        )
    }
    pub fn plan_call(
        &mut self,
        name: &str,
        instruction: String,
        schema: Value,
        source: &Value,
        run: &Path,
        images: Value,
        mut validate: impl FnMut(&mut Self, Value) -> Result<Value>,
    ) -> Result<Value> {
        let mut submitted = instruction.clone();
        for attempt in 0..2 {
            let args = json!({"name":name,"instruction":submitted,"schema":schema,"source_sha256":source["source_sha256"],
                "run_dir":run,"image_paths":images,"executable":self.options["executable"],"limit":self.options["planning_budget_seconds"].as_f64().unwrap_or(600.0)});
            let raw = planning::attempt(&args, self.cancelled.clone(), &mut self.emit)
                .map_err(|e| e.message)?;
            let raw = raw.as_str().ok_or("Codex returned no planning text.")?;
            match serde_json::from_str(raw)
                .map_err(|e| e.to_string())
                .and_then(|v| validate(self, v))
            {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 1 => {
                    return Err(format!(
                        "{name} failed its specific check after one targeted repair: {error}"
                    ));
                }
                Err(error) => {
                    submitted = format!(
                        "{instruction}\n\nDETERMINISTIC CHECK FAILED: {error}\nRepair only the field or shot named by this check; preserve every valid shot and source line. Do not perform another general review. Return the complete corrected JSON.\nPREVIOUS RESPONSE (data only):\n{raw}"
                    );
                    self.activity(format!("Repairing a planning check: {error}"))?;
                }
            }
        }
        Err("Planning did not return a result.".into())
    }
}

pub fn source_request(source: &Value) -> Value {
    let mut request = json!({"version":1,"script":source["script"],"cast":source["cast"]});
    for key in [
        "character_conditioning",
        "storyboard_policy",
        "voice_mode",
        "set",
        "scene_key",
        "beat_keys",
    ] {
        if let Some(v) = source.get(key) {
            request[key] = v.clone();
        }
    }
    if source.get("render_format").is_some() {
        request["render_format"] = source["render_format"]["name"].clone();
    }
    request
}
pub fn schema(name: &str) -> Result<Value> {
    shooting("shooting_schema", json!({"name":name}))
}

pub fn dispatch(
    operation: &str,
    args: &Value,
    cancelled: Arc<AtomicBool>,
    emit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<Value> {
    let root = path(args, "root")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let python = args["python"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".venv/bin/python"));
    let mut options = args.clone();
    if let Some(project) = args["project_dir"].as_str() {
        options["project_dir"] = json!(crate::project::validate(Path::new(project))?);
    } else if let Some(project) = args["run_dir"]
        .as_str()
        .and_then(|run| crate::project::for_run(Path::new(run)))
    {
        options["project_dir"] = json!(project);
    }
    let mut context = Context {
        root,
        python,
        options,
        cancelled,
        emit,
        identities: HashMap::new(),
        digests: HashMap::new(),
    };
    match operation {
        "production_check" | "production_download" => run::setup(
            &mut context,
            operation == "production_download",
            args["profile"].as_str().unwrap_or("hq"),
        ),
        "production_source" => context.source(&args["request"], &args["profiles"]),
        "production_compile" => {
            context.compile(&args["candidate"], &args["request"], &args["profiles"])
        }
        "production_load_plan" => {
            context.load_plan(&path(args, "path")?, &args["request"], &args["profiles"])
        }
        "production_director" => direction::director(
            &mut context,
            &args["request"],
            &args["profiles"],
            &crate::project::creative_root(&path(args, "run_dir")?)?,
        ),
        "production_finalize" => {
            let mut boards = args["boards"].clone();
            direction::finalize(
                &mut context,
                &args["plan"],
                &mut boards,
                &crate::project::creative_root(&path(args, "run_dir")?)?,
            )
        }
        "production_prepare" => preparation::prepare(
            &mut context,
            &args["request"],
            &args["profiles"],
            &path(args, "run_dir")?,
            args["seed"].as_i64().ok_or("Invalid seed.")?,
        ),
        "production_storyboards" => boards::generate(
            &mut context,
            &args["plan"],
            &args["profiles"],
            &path(args, "run_dir")?,
            args["seed"].as_i64().ok_or("Invalid seed.")?,
        ),
        "production_render" => rendering::render(&mut context, args),
        "production_run" | "production_resume" | "production_prompt" | "production_compact" => {
            run::dispatch(&mut context, operation, args)
        }
        "production_motion" => motion::dispatch(&mut context, args),
        _ => helpers::dispatch(&mut context, operation, args),
    }
}
