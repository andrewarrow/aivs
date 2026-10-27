//! Shot execution policy, saved-take provenance, and run-local recovery.
//! Legacy artifact JSON fingerprints remain adapter-owned.
mod storage;
use crate::continuity::Result;
use serde_json::{Value, json};

const SEED_MODULUS: i128 = 1 << 31;
const RETAKE_SEED_STRIDE: i128 = 1_000_003;

pub fn storage(
    operation: &str,
    args: &Value,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    storage::dispatch(operation, args, cancelled)
}

fn take_number(saved: &Value) -> Option<u64> {
    match saved.get("take") {
        None => Some(1),
        Some(value) => value.as_u64().filter(|n| *n > 0),
    }
}

fn take_seed(base_seed: i64, take: u64) -> i64 {
    (i128::from(base_seed) + (i128::from(take) - 1) * RETAKE_SEED_STRIDE).rem_euclid(SEED_MODULUS)
        as i64
}

pub fn validate_plan(plan: &Value) -> Result<Value> {
    if !plan.is_object() || plan["version"].as_u64() != Some(1) {
        return Err("Shooting plan must be an object with version 1.".into());
    }
    let shots = plan["shots"]
        .as_array()
        .filter(|shots| !shots.is_empty())
        .ok_or("Shooting plan must contain at least one shot.")?;
    let has_scene_ids = shots.iter().any(|shot| shot.get("scene_id").is_some());
    let mut seconds = 0_u64;
    for (index, shot) in shots.iter().enumerate() {
        let number = index + 1;
        if !shot.is_object() {
            return Err(format!("Shot {number} must be an object."));
        }
        let duration = shot["seconds"]
            .as_u64()
            .filter(|seconds| (4..=12).contains(seconds))
            .ok_or_else(|| {
                format!("Shot {number} duration must be an integer from 4 to 12 seconds.")
            })?;
        if !shot["prompt"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(format!("Shot {number} needs a non-empty prompt."));
        }
        let continuation = shot["continue_previous"]
            .as_bool()
            .ok_or_else(|| format!("Shot {number} needs a boolean continue_previous value."))?;
        if has_scene_ids && shot["scene_id"].as_u64().is_none() {
            return Err(format!(
                "Shot {number} needs a nonnegative integer scene_id."
            ));
        }
        if continuation {
            if index == 0 {
                return Err("The first shot cannot continue a previous shot.".into());
            }
            if has_scene_ids && shot["scene_id"] != shots[index - 1]["scene_id"] {
                return Err(format!("Shot {number} cannot continue across scenes."));
            }
        }
        for field in ["start_state", "end_state"] {
            if !shot[field].is_string() {
                return Err(format!("Shot {number} needs a string {field} value."));
            }
        }
        if !shot["beat_ids"].is_array() {
            return Err(format!("Shot {number} needs a beat_ids list."));
        }
        seconds = seconds
            .checked_add(duration)
            .ok_or("Shooting plan duration overflow.")?;
    }
    if plan["seconds"].as_f64() != Some(seconds as f64) {
        return Err("Shooting plan seconds must equal the sum of its shot durations.".into());
    }
    let fade_out = plan["fade_out"]
        .as_bool()
        .ok_or("Shooting plan needs a boolean fade_out value.")?;
    Ok(json!({"seconds": seconds, "fade_out": fade_out}))
}

pub fn uses_previous_frame(shots: &Value, index: &Value) -> Result<bool> {
    let shots = shots.as_array().ok_or("Missing planned shots.")?;
    let index = index
        .as_u64()
        .and_then(|i| usize::try_from(i).ok())
        .filter(|i| *i < shots.len())
        .ok_or("Invalid shot index.")?;
    if index == 0 {
        return Ok(false);
    }
    // Scene identity preserves wardrobe/identity across camera cuts. Repeated
    // headings in distinct scenes must not inherit each other's final frame.
    if shots[index].get("scene_id").is_some() {
        Ok(shots[index]["scene_id"] == shots[index - 1]["scene_id"])
    } else {
        shots[index]["continue_previous"]
            .as_bool()
            .ok_or_else(|| "Shot needs a boolean continue_previous value.".into())
    }
}

fn valid_saved_report(saved: &Value) -> bool {
    saved.is_object()
        && saved["prompt"].is_string()
        && (saved["seed"].is_i64() || saved["seed"].is_u64())
        && (saved["seconds"].is_i64() || saved["seconds"].is_u64())
        && saved["beat_ids"].is_array()
        && saved["continuity"].is_object()
        && take_number(saved).is_some()
}

fn planned_storyboard(report: &Value) -> &Value {
    report
        .get("planned_storyboard")
        .unwrap_or(&report["storyboard"])
}

/// A matching report still needs independent byte evidence before reuse.
/// Legacy takes without a predecessor hash request a decoded final-frame hash.
/// Hash mismatches on saved clips are errors, never permission for a new take.
pub fn match_saved_take(args: &Value) -> Result<Value> {
    let saved = &args["saved"];
    let report = &args["report"];
    let mismatch = || Ok(json!({"status": "mismatch"}));
    if !valid_saved_report(saved) {
        return mismatch();
    }
    let take = take_number(saved).ok_or("Invalid saved take number.")?;
    if saved["character_assets"] != report["character_assets"]
        || (!report["storyboard_acceptance"].is_null()
            && (saved["storyboard_acceptance"] != report["storyboard_acceptance"]
                || saved["prompt"] != report["prompt"]))
        || planned_storyboard(saved) != planned_storyboard(report)
    {
        return mismatch();
    }
    let adapted_storyboard = saved["storyboard"] != report["storyboard"];
    if adapted_storyboard
        && (!saved["storyboard"].is_object()
            || saved["adaptation"]["status"] != "applied"
            || saved["storyboard"]["image"]["sha256"] != saved["continuity"]["source_image_sha256"])
    {
        return mismatch();
    }
    if (saved["adaptation"]["status"] == "applied"
        && saved["adaptation"]["source_clip_sha256"] != report["observed_source_clip_sha256"])
        || saved["render_config"] != report["render_config"]
        || saved["motion_keyframes"] != report["motion_keyframes"]
    {
        return mismatch();
    }
    let voice = &report["character_assets"]["voice"];
    if voice.get("file").is_some() && args["is_final_clip"] == true {
        let converted = &saved["voice_conversion"];
        if !converted.is_object()
            || !converted["inputs"].is_object()
            || converted["character"] != voice["character"]
            || converted["inputs"]["reference"] != voice["sha256"]
            || converted["identity"] != voice["converter"]
        {
            return mismatch();
        }
    }
    let base_seed = report["base_seed"]
        .as_i64()
        .ok_or("Invalid take base seed.")?;
    if saved.get("base_seed").unwrap_or(&report["base_seed"]) != &report["base_seed"]
        || saved["seed"].as_i64() != Some(take_seed(base_seed, take))
        || ["seconds", "beat_ids", "scene_id"]
            .iter()
            .any(|key| saved[*key] != report[*key])
    {
        return mismatch();
    }
    // An already rendered adaptation remains authoritative on Resume. New
    // advisory visual opinions must not invalidate expensive matching footage.
    if !saved["planned_shot_sha256"].is_null() {
        if saved["planned_shot_sha256"] != report["planned_shot_sha256"]
            || saved["planned_prompt"] != report["planned_prompt"]
        {
            return mismatch();
        }
    } else if saved["prompt"] != report["prompt"] {
        return mismatch();
    }
    let continuity = &saved["continuity"];
    for key in [
        "conditioning",
        "source_shot",
        "source_frame",
        "source_image_sha256",
    ] {
        if (key != "source_image_sha256" || !adapted_storyboard)
            && continuity[key] != report["continuity"][key]
        {
            return mismatch();
        }
    }
    if args["has_previous_clip"] == true {
        if !continuity["source_clip_sha256"].is_null() {
            if continuity["source_clip_sha256"] != report["continuity"]["source_clip_sha256"] {
                return mismatch();
            }
        } else {
            let reference = &saved["runtime"]["source_image"];
            if !reference.is_object()
                || !reference["source_sha256"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            {
                return mismatch();
            }
            if args["legacy_frame_sha256"].is_null() {
                let frame = report["continuity"]["source_frame"]
                    .as_u64()
                    .ok_or("Saved take needs a valid predecessor frame index.")?;
                return Ok(json!({"status": "needs_previous_frame", "source_frame": frame}));
            }
            if args["legacy_frame_sha256"] != reference["source_sha256"] {
                return mismatch();
            }
        }
    }
    if !saved["sha256"].is_null() {
        if args["clip_sha256"].is_null() {
            return Ok(json!({"status": "needs_clip_hash"}));
        }
        if saved["sha256"] != args["clip_sha256"] {
            return Err(format!(
                "Saved video changed: {}. Review it before replacing the take.",
                args["clip_path"].as_str().unwrap_or("saved clip")
            ));
        }
    }
    Ok(json!({"status": "matched"}))
}

/// Advisory review can change the displayed status, never request a retake.
pub fn render_status(reviews: &Value) -> Result<Value> {
    let reviews = reviews
        .as_array()
        .ok_or("Render status needs visual reviews.")?;
    let mut unavailable = false;
    let mut issues = false;
    for review in reviews {
        match review["status"].as_str() {
            Some("passed") => {}
            Some("issues_detected") => issues = true,
            Some("unavailable") => unavailable = true,
            _ => return Err("Visual assessment returned no valid report.".into()),
        }
    }
    Ok(json!(if issues {
        "rendered-with-issues"
    } else if unavailable {
        "rendered-needs-review"
    } else {
        "complete"
    }))
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "validate_render_plan" => validate_plan(&args["plan"]),
        "uses_previous_frame" => {
            uses_previous_frame(&args["shots"], &args["index"]).map(Value::Bool)
        }
        "take_seed" => {
            let base_seed = args["base_seed"]
                .as_i64()
                .ok_or("Invalid take base seed.")?;
            let take = args["take"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("Invalid take number.")?;
            Ok(json!(take_seed(base_seed, take)))
        }
        "validate_saved_take_report" => Ok(if valid_saved_report(&args["report"]) {
            args["report"].clone()
        } else {
            Value::Null
        }),
        "match_saved_take" => match_saved_take(args),
        "render_review_status" => render_status(&args["reviews"]),
        _ => Err(format!("Unknown shot renderer operation: {operation}")),
    }
}
