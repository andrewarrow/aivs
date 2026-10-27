//! Storyboard repair policy, journal transitions, and reuse of approved assets.
//! Python supplies existing fingerprints; Rust never reserializes them for hashing.
mod storage;
use crate::continuity::{self, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_STORYBOARD_REPAIR_ROUNDS: usize = 2;

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Complete,
}

#[derive(Deserialize)]
struct Round {
    status: Status,
}

#[derive(Deserialize)]
struct Journal {
    version: u64,
    source_sha256: String,
    rounds: Vec<Round>,
}

fn journal(state: &Value, source: &Value) -> Result<Journal> {
    let fail = "Saved storyboard repairs do not match this run.";
    let parsed: Journal = serde_json::from_value(state.clone()).map_err(|_| fail)?;
    if parsed.version != 1
        || source.as_str() != Some(parsed.source_sha256.as_str())
        || parsed.rounds.len() > MAX_STORYBOARD_REPAIR_ROUNDS
        || parsed
            .rounds
            .iter()
            .rev()
            .skip(1)
            .any(|r| r.status != Status::Complete)
    {
        return Err(fail.into());
    }
    Ok(parsed)
}

fn shots(plan: &Value) -> Result<&Vec<Value>> {
    plan["shots"]
        .as_array()
        .ok_or_else(|| "Storyboard repair needs planned shots.".into())
}

fn shot_number(value: &Value, count: usize) -> Result<usize> {
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=count).contains(n))
        .ok_or_else(|| "Storyboard repair needs valid shot numbers.".into())
}

fn issues_for<'a>(issues: &'a Value, plan: &Value) -> Result<&'a Vec<Value>> {
    let fail = "Storyboard repair needs unique shots and valid replacement compositions.";
    let rows = issues
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or("Storyboard repair needs specific shot findings.")?;
    let mut seen = BTreeSet::new();
    for issue in rows {
        let number = shot_number(&issue["shot"], shots(plan)?.len())?;
        let prompt = issue["storyboard_prompt"].as_str().ok_or(fail)?;
        if !seen.insert(number)
            || prompt.trim().is_empty()
            || prompt.chars().count() > 1200
            || prompt.contains('\0')
            || prompt.contains("{{")
            || prompt.contains("}}")
            || !issue["issue"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(fail.into());
        }
    }
    Ok(rows)
}

fn reference_ids(issues: &[Value]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for issue in issues {
        if let Some(value) = issue.get("reference_ids") {
            let references: Vec<String> = serde_json::from_value(value.clone())
                .map_err(|_| "Storyboard repair needs valid reference IDs.")?;
            ids.extend(references);
        }
    }
    Ok(ids)
}

fn replacement<'a>(issue: &'a Value, field: &str) -> Result<Option<&'a Value>> {
    match issue.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) if values.is_empty() => Ok(None),
        Some(Value::Array(values)) if values.len() == 1 && values[0].is_object() => {
            Ok(values.first())
        }
        _ => Err(format!(
            "Storyboard {field} must contain at most one replacement object."
        )),
    }
}

/// Propose changes before the Python prompt compiler validates token and source contracts.
pub fn candidate(director: &Value, issues: &Value, state: &Value) -> Result<Value> {
    let parsed = journal(state, &director["source_sha256"])?;
    if parsed
        .rounds
        .last()
        .is_some_and(|r| r.status == Status::Pending)
    {
        return Err("Finish the pending storyboard repair before reserving another round.".into());
    }
    if parsed.rounds.len() >= MAX_STORYBOARD_REPAIR_ROUNDS {
        let findings = issues
            .as_array()
            .ok_or("Storyboard repair needs shot findings.")?;
        let detail = findings
            .iter()
            .map(|issue| {
                format!(
                    "shot {}: {}",
                    issue["shot"],
                    issue["issue"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(json!({
            "status": "exhausted",
            "limit": MAX_STORYBOARD_REPAIR_ROUNDS,
            "message": format!("Storyboards still cannot support the source after {MAX_STORYBOARD_REPAIR_ROUNDS} automatic repair rounds: {detail}. Saved images and findings are preserved."),
            "diagnostic": {
                "policy": continuity::POLICY, "status": "rejected", "reason": "repair-allowance-exhausted",
                "repair_rounds": parsed.rounds.len(), "source_sha256": director["source_sha256"],
                "issues": issues, "video_inference_allowed": false,
            },
        }));
    }
    let issues = issues_for(issues, director)?;
    let mut revised = director.clone();
    let mut world = director["world"].clone();
    for issue in issues {
        let number = shot_number(&issue["shot"], shots(director)?.len())?;
        if let Some(value) = replacement(issue, "plan_repair")? {
            revised["shots"][number - 1] = value.clone();
        }
        if let Some(value) = replacement(issue, "world_repair")? {
            if world != director["world"] && &world != value {
                return Err("Sequence review proposed conflicting canonical world repairs.".into());
            }
            world = value.clone();
        }
        let shot = revised["shots"][number - 1]
            .as_object_mut()
            .ok_or("Storyboard repair needs a replacement shot object.")?;
        shot.insert(
            "storyboard_prompt".into(),
            json!(
                issue["storyboard_prompt"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
            ),
        );
        shot.remove("prompt");
    }
    revised["world"] = world;
    Ok(json!({"status": "ready", "limit": MAX_STORYBOARD_REPAIR_ROUNDS, "plan": revised}))
}

/// Reserve one complete repair attempt, including every invalidated image.
/// `changed_shots` compares the legacy Python fingerprints before and after compilation.
pub fn reserve(args: &Value) -> Result<Value> {
    let state = &args["state"];
    let director = &args["director"];
    let revised = &args["revised"];
    let parsed = journal(state, &director["source_sha256"])?;
    if parsed.rounds.len() >= MAX_STORYBOARD_REPAIR_ROUNDS
        || parsed
            .rounds
            .last()
            .is_some_and(|r| r.status == Status::Pending)
    {
        return Err("A storyboard repair cannot be reserved in the current journal state.".into());
    }
    let count = shots(director)?.len();
    if shots(revised)?.len() != count || revised["source_sha256"] != director["source_sha256"] {
        return Err("Storyboard repair changed the source or shot coverage.".into());
    }
    let mut issues = issues_for(&args["issues"], revised)?.clone();
    let seen: BTreeSet<_> = issues
        .iter()
        .map(|i| shot_number(&i["shot"], count))
        .collect::<Result<_>>()?;
    let mut changed = seen.clone();
    for value in args["changed_shots"]
        .as_array()
        .ok_or("Missing changed storyboard shots.")?
    {
        changed.insert(shot_number(value, count)?);
    }
    let mut prior_images = BTreeMap::new();
    if !args["boards"].is_null() {
        let boards = &args["boards"];
        let entries = boards["shots"]
            .as_array()
            .ok_or("Missing storyboard shots.")?;
        let mut images = BTreeMap::new();
        let bad_refs = reference_ids(&issues)?;
        for entry in entries {
            let number = shot_number(&entry["shot"], count)?;
            if images.insert(number, &entry["image"]).is_some() {
                return Err("Storyboard repair needs one image per planned shot.".into());
            }
            if let Some(references) = entry["generation"]["references"].as_array() {
                for reference in references {
                    let depends = if let Some(ids) = reference.get("reference_ids") {
                        ids.as_array()
                            .ok_or("Invalid storyboard reference IDs.")?
                            .iter()
                            .any(|id| id.as_str().is_some_and(|id| bad_refs.contains(id)))
                    } else {
                        reference["reference_id"]
                            .as_str()
                            .is_some_and(|id| bad_refs.contains(id))
                    };
                    if depends {
                        changed.insert(number);
                    }
                }
            }
        }
        if images.len() != count {
            return Err("Storyboard repair needs one image per planned shot.".into());
        }
        for number in continuity::dependent_shots(boards, changed)?.difference(&seen) {
            let index = number
                .checked_sub(1)
                .filter(|n| *n < count)
                .ok_or("Invalid dependent storyboard shot.")?;
            issues.push(json!({"shot": number,
                "issue": "Regenerate dependent image after upstream physical/composition repair.",
                "storyboard_prompt": revised["shots"][index]["storyboard_prompt"]}));
        }
        issues.sort_by_key(|issue| issue["shot"].as_u64().unwrap_or_default());
        for issue in &issues {
            let number = shot_number(&issue["shot"], count)?;
            prior_images.insert(number.to_string(), images[&number]);
        }
    }
    let seed = args["seed"]
        .as_i64()
        .ok_or("Invalid storyboard repair seed.")?;
    let fingerprint = args["before_plan_sha256"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Missing storyboard repair plan fingerprint.")?;
    let mut result = state.clone();
    result["rounds"]
        .as_array_mut()
        .ok_or("Missing storyboard repair rounds.")?
        .push(json!({
            "status": "pending", "before_plan_sha256": fingerprint, "plan": revised,
            "issues": issues, "prior_images": prior_images, "reference_seed": seed,
            "seed": (i128::from(seed) + parsed.rounds.len() as i128 + 1).rem_euclid(1 << 31),
        }));
    Ok(result)
}

/// Check either side of an interrupted plan/manifest publication before generating images.
pub fn resume(args: &Value) -> Result<Value> {
    let state = &args["state"];
    let parsed = journal(state, &args["source_sha256"])?;
    if !parsed
        .rounds
        .last()
        .is_some_and(|r| r.status == Status::Pending)
    {
        return Err("There is no pending storyboard repair to finish.".into());
    }
    let repair = &state["rounds"][parsed.rounds.len() - 1];
    if !args["director_sha256"].is_string()
        || !args["revised_sha256"].is_string()
        || (args["director_sha256"] != repair["before_plan_sha256"]
            && args["director_sha256"] != args["revised_sha256"])
    {
        return Err("Storyboard direction changed during its saved repair.".into());
    }
    if repair["plan"]["source_sha256"] != args["source_sha256"] {
        return Err("Saved storyboard repair changed its source.".into());
    }
    if repair["seed"].as_i64().is_none()
        || repair["reference_seed"].as_i64().is_none()
        || !repair["before_plan_sha256"].is_string()
    {
        return Err("Saved storyboard repair has invalid seeds or plan identity.".into());
    }
    let issues = issues_for(&repair["issues"], &repair["plan"])?;
    Ok(
        json!({"repair": repair, "round_number": parsed.rounds.len(),
        "reference_ids": reference_ids(issues)?}),
    )
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "validate_storyboard_direction" => storage::direction(args),
        "copy_final_preproduction" => storage::copy_final(args),
        "validate_storyboard_repairs" => {
            journal(&args["state"], &args["source_sha256"])?;
            Ok(args["state"].clone())
        }
        "storyboard_repair_candidate" => {
            candidate(&args["director"], &args["issues"], &args["state"])
        }
        "reserve_storyboard_repair" => reserve(args),
        "resume_storyboard_repair" => resume(args),
        "complete_storyboard_repair" => {
            let context = resume(args)?;
            let number = context["round_number"]
                .as_u64()
                .ok_or("Missing storyboard repair round.")? as usize;
            let mut state = args["state"].clone();
            state["rounds"][number - 1]["status"] = json!("complete");
            Ok(state)
        }
        _ => Err(format!("Unknown preproduction operation: {operation}")),
    }
}
