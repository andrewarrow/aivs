//! Rules for advisory frame observations and preservation of unrendered shots.
//! These reports describe evidence; they never authorize another video render.
use crate::continuity::Result;
use serde::Deserialize;
use serde_json::{Value, json};

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|error| format!("Invalid visual review: {error}"))
}

fn text(value: &str, label: &str, limit: usize) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > limit {
        return Err(format!("Invalid {label} in visual review."));
    }
    Ok(())
}

fn timestamp(value: f64, seconds: f64) -> bool {
    value.is_finite() && (0.0..=seconds).contains(&value)
}

/// Keep requirement strings and order identical to existing Python artifacts.
pub fn contract(shot: &Value) -> Result<Vec<String>> {
    if !shot.is_object() {
        return Err("Visual requirements need a shot object.".into());
    }
    let mut requirements = match shot.get("visual_contract") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => decode::<Vec<String>>(value)?,
    };
    if requirements.is_empty() {
        requirements = [
            "The assigned source action and dramatic purpose are visibly conveyed.",
            "People, garments and handled objects retain plausible identity and motion throughout.",
            "The final visible state agrees with the planned end state.",
        ]
        .map(str::to_owned)
        .to_vec();
    }
    #[derive(Deserialize)]
    struct Action {
        beat_id: i64,
        part: i64,
        parts: i64,
        action: String,
    }
    if let Some(coverage) = shot.get("action_coverage") {
        for action in decode::<Vec<Action>>(coverage)? {
            requirements.push(format!(
                "Source action {}, part {}/{}: {}",
                action.beat_id, action.part, action.parts, action.action
            ));
        }
    }
    Ok(requirements)
}

#[derive(Deserialize)]
struct Issue {
    severity: String,
    category: String,
    start_seconds: f64,
    end_seconds: f64,
    evidence: String,
    affects_continuation: bool,
}

#[derive(Deserialize)]
struct ContractEvidence {
    requirement: String,
    status: String,
    evidence: String,
    timestamps: Vec<f64>,
}

#[derive(Deserialize)]
struct ShotReview {
    status: String,
    summary: String,
    observed_end_state: String,
    continuity_risk: bool,
    issues: Vec<Issue>,
    contract_evidence: Vec<ContractEvidence>,
}

/// Validate fresh observations and saved reports with the same rules.
/// Preserve cache metadata and untouched observation fields in the result.
pub fn validate_shot(value: &Value, shot: &Value) -> Result<Value> {
    let review: ShotReview = decode(value)?;
    if !matches!(review.status.as_str(), "passed" | "issues_detected") {
        return Err("Visual shot review needs a status.".into());
    }
    text(&review.summary, "summary", 4000)?;
    text(&review.observed_end_state, "observed end state", 2400)?;
    let seconds = shot["seconds"]
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .ok_or("Visual review needs a valid shot duration.")?;
    if review.issues.len() > 24 {
        return Err("Visual issues must be a bounded list.".into());
    }
    for issue in &review.issues {
        if !matches!(issue.severity.as_str(), "minor" | "major" | "uncertain")
            || !matches!(
                issue.category.as_str(),
                "physics" | "continuity" | "action" | "tone" | "framing" | "text"
            )
            || !timestamp(issue.start_seconds, seconds)
            || !timestamp(issue.end_seconds, seconds)
            || issue.end_seconds < issue.start_seconds
        {
            return Err("Visual issues require valid evidence intervals and severity.".into());
        }
        text(&issue.evidence, "issue evidence", 4000)?;
    }
    let requirements = contract(shot)?;
    if review.contract_evidence.len() != requirements.len() {
        return Err("Review every visual requirement exactly once.".into());
    }
    for (requirement, item) in requirements.iter().zip(&review.contract_evidence) {
        if &item.requirement != requirement
            || !matches!(item.status.as_str(), "satisfied" | "missing" | "uncertain")
            || item.timestamps.is_empty()
            || item.timestamps.len() > 16
            || item.timestamps.iter().any(|t| !timestamp(*t, seconds))
        {
            return Err("Visual contract evidence needs exact requirements and timestamps.".into());
        }
        text(&item.evidence, "contract evidence", 4000)?;
    }
    let detected = !review.issues.is_empty()
        || review.continuity_risk
        || review
            .contract_evidence
            .iter()
            .any(|item| item.status != "satisfied");
    if (review.status == "issues_detected") != detected {
        return Err("Review status contradicts its evidence.".into());
    }
    if review.continuity_risk != review.issues.iter().any(|item| item.affects_continuation) {
        return Err("Continuation risk must agree with its timestamped issues.".into());
    }
    Ok(value.clone())
}

#[derive(Deserialize)]
struct PreservationEvidence {
    requirement: String,
    status: String,
    evidence: String,
}

#[derive(Deserialize)]
struct PreservationReview {
    approved: bool,
    source_preservation: String,
    continuity: String,
    requirements: Vec<PreservationEvidence>,
    issues: Vec<String>,
}

pub fn validate_preservation(value: &Value, requirements: &Value) -> Result<Value> {
    let review: PreservationReview = decode(value)?;
    let requirements: Vec<String> = decode(requirements)?;
    text(&review.source_preservation, "source preservation", 4000)?;
    text(&review.continuity, "adaptation continuity", 4000)?;
    if review.requirements.len() != requirements.len() {
        return Err(
            "Independently account for every unchanged visual and action requirement.".into(),
        );
    }
    for (requirement, item) in requirements.iter().zip(&review.requirements) {
        if &item.requirement != requirement
            || !matches!(item.status.as_str(), "preserved" | "missing" | "uncertain")
        {
            return Err("Preservation evidence must match the original requirements.".into());
        }
        text(&item.evidence, "preservation evidence", 4000)?;
    }
    if review.issues.len() > 24 {
        return Err("Preservation review needs a bounded issues list.".into());
    }
    for issue in &review.issues {
        text(issue, "preservation issue", 4000)?;
    }
    if review.approved
        && (!review.issues.is_empty()
            || review
                .requirements
                .iter()
                .any(|item| item.status != "preserved"))
    {
        return Err("Preservation approval contradicts missing or uncertain evidence.".into());
    }
    Ok(value.clone())
}

#[derive(Deserialize)]
struct Adaptation {
    safe_to_apply: bool,
    preserves_source_contract: bool,
    reason: String,
}

pub fn validate_adaptation(value: &Value, storyboard_mode: bool) -> Result<Value> {
    let adaptation: Adaptation = decode(value)?;
    text(&adaptation.reason, "adaptation reason", 4000)?;
    if adaptation.safe_to_apply && adaptation.preserves_source_contract {
        text(
            value["prompt_template"]
                .as_str()
                .ok_or("Invalid adapted template in visual review.")?,
            "adapted template",
            4000,
        )?;
        if storyboard_mode {
            text(
                value["storyboard_prompt"]
                    .as_str()
                    .ok_or("Invalid adapted storyboard composition in visual review.")?,
                "adapted storyboard composition",
                1200,
            )?;
        }
    }
    Ok(value.clone())
}

#[derive(Deserialize)]
struct Finding {
    shot_numbers: Vec<usize>,
    evidence: String,
}

#[derive(Deserialize)]
struct SceneReview {
    status: String,
    summary: String,
    story_findings: Vec<Finding>,
}

/// Recompute scene status from recorded evidence, including on Resume.
pub fn validate_scene(value: &Value, plan: &Value, shot_reviews: &Value) -> Result<Value> {
    let shots = plan["shots"]
        .as_array()
        .ok_or("Scene review needs planned shots.")?;
    let observations = shot_reviews
        .as_array()
        .ok_or("Scene review needs shot reports.")?;
    if observations.len() != shots.len() {
        return Err("Scene observation requires a report for every rendered shot.".into());
    }
    let review: SceneReview = decode(value)?;
    if !matches!(
        review.status.as_str(),
        "passed" | "issues_detected" | "unavailable"
    ) {
        return Err("Scene review needs a valid status.".into());
    }
    text(&review.summary, "scene summary", 4000)?;
    if review.story_findings.len() > 64 {
        return Err("Scene findings must be a bounded list.".into());
    }
    for item in &review.story_findings {
        if item.shot_numbers.is_empty()
            || item
                .shot_numbers
                .iter()
                .any(|n| *n == 0 || *n > shots.len())
        {
            return Err("Scene findings need valid shot references.".into());
        }
        text(&item.evidence, "scene evidence", 4000)?;
    }
    let mut unobserved = Vec::new();
    let mut detected = !review.story_findings.is_empty();
    for (index, observation) in observations.iter().enumerate() {
        match observation["status"].as_str() {
            Some("issues_detected") => detected = true,
            Some("passed") => {}
            _ => unobserved.push(index + 1),
        }
    }
    let status = if detected {
        "issues_detected"
    } else if review.status == "passed" && !unobserved.is_empty() {
        "unavailable"
    } else {
        &review.status
    };
    let mut result = value.clone();
    result["status"] = json!(status);
    result["unobserved_shots"] = json!(unobserved);
    Ok(result)
}

pub fn dispatch(operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "visual_contract" => contract(&args["shot"]).map(|items| json!(items)),
        "validate_visual_shot" => validate_shot(&args["value"], &args["shot"]),
        "validate_visual_preservation" => {
            validate_preservation(&args["value"], &args["requirements"])
        }
        "validate_visual_adaptation" => validate_adaptation(
            &args["value"],
            args["storyboard_mode"]
                .as_bool()
                .ok_or("Adaptation needs a storyboard mode.")?,
        ),
        "validate_visual_scene" => {
            validate_scene(&args["value"], &args["plan"], &args["shot_reviews"])
        }
        _ => Err(format!("Unknown visual review operation: {operation}")),
    }
}
