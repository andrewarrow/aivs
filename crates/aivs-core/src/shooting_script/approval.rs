//! Evidence checks for source fidelity and saved preproduction approvals.
use super::{PLANNING_POLICY, array, bounded};
use crate::continuity::Result;
use serde_json::{Value, json};

pub(super) fn requirements(plan: &Value) -> Result<Value> {
    let shots = array(&plan["shots"], "Source fidelity needs planned shots.")?;
    let mut requirements = Vec::new();
    for beat in array(
        &plan["source"]["beats"],
        "Source fidelity needs source beats.",
    )? {
        let numbers = shots
            .iter()
            .enumerate()
            .filter(|(_, shot)| {
                shot["beat_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&beat["id"]))
            })
            .map(|(index, _)| index + 1)
            .collect::<Vec<_>>();
        requirements.push(json!({"id": format!("source:{}", beat["id"]), "source_beat": beat, "shot_numbers": numbers}));
    }
    for (offset, shot) in shots.iter().enumerate() {
        let number = offset + 1;
        for (index, part) in array(
            &shot["action_coverage"],
            "Source fidelity needs action coverage.",
        )?
        .iter()
        .enumerate()
        {
            requirements.push(json!({"id": format!("action:{number}:{}", index + 1), "action": part, "shot_numbers": [number]}));
        }
        for (index, requirement) in array(
            &shot["visual_contract"],
            "Source fidelity needs a visual contract.",
        )?
        .iter()
        .enumerate()
        {
            requirements.push(json!({"id": format!("contract:{number}:{}", index + 1), "requirement": requirement, "shot_numbers": [number]}));
        }
    }
    Ok(json!(requirements))
}

pub(super) fn fidelity(report: &Value, plan: &Value, require_pass: bool) -> Result<Value> {
    let checks = array(
        &report["checks"],
        "Source fidelity must check every source beat, action part and visual requirement.",
    )?;
    let requirements = requirements(plan)?;
    let requirements = requirements.as_array().unwrap();
    if checks.len() != requirements.len() {
        return Err(
            "Source fidelity must check every source beat, action part and visual requirement."
                .into(),
        );
    }
    let shots = array(&plan["shots"], "Missing planned shots.")?;
    for (requirement, check) in requirements.iter().zip(checks) {
        if !check.is_object()
            || check["id"] != requirement["id"]
            || !check["preserved"].is_boolean()
        {
            return Err(
                "Source fidelity checks must identify each requirement exactly once, in order."
                    .into(),
            );
        }
        let id = requirement["id"].as_str().unwrap_or_default();
        bounded(&check["reason"], &format!("Fidelity reason for {id}"), 1200)?;
        let error = format!("Fidelity check {id} needs concrete final-prompt evidence.");
        let evidence = array(&check["evidence"], &error)?;
        if evidence.len() > 64 || (check["preserved"] == true && evidence.is_empty()) {
            return Err(error);
        }
        for item in evidence {
            let index = item["shot"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| {
                    *n >= 1
                        && *n <= shots.len()
                        && requirement["shot_numbers"]
                            .as_array()
                            .is_some_and(|numbers| numbers.contains(&item["shot"]))
                })
                .ok_or(
                    "Fidelity evidence must belong to a shot assigned to that source requirement.",
                )?;
            let quote = bounded(&item["quote"], "Fidelity prompt quote", 4000)?;
            if !shots[index - 1]["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains(quote))
            {
                return Err(
                    "Fidelity evidence must quote the actual final prompt verbatim.".into(),
                );
            }
        }
    }
    if require_pass {
        let failures = checks
            .iter()
            .filter(|check| check["preserved"] == false)
            .map(|check| {
                format!(
                    "{}: {}",
                    check["id"].as_str().unwrap_or_default(),
                    check["reason"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            return Err(
                "Final motion prompts omit or contradict essential source material: ".to_owned()
                    + &failures.join("; "),
            );
        }
    }
    Ok(report.clone())
}

pub(super) fn review(report: &Value, plan: &Value, require_approval: bool) -> Result<()> {
    let approved = report["approved"]
        .as_bool()
        .ok_or("Preproduction review needs an explicit approval decision.")?;
    bounded(&report["summary"], "Review summary", 4000)?;
    let beats = array(&plan["source"]["beats"], "Missing source beats.")?;
    let shots = array(&plan["shots"], "Missing planned shots.")?;
    let beat_reviews = array(
        &report["beat_reviews"],
        "Preproduction review must account for every source beat.",
    )?;
    if beat_reviews.len() != beats.len() {
        return Err("Preproduction review must account for every source beat.".into());
    }
    let modern = plan["planning_policy"] == PLANNING_POLICY;
    for (beat, review) in beats.iter().zip(beat_reviews) {
        if review["beat_id"].as_u64().is_none() || review["beat_id"] != beat["id"] {
            return Err("Beat reviews must map every source beat in order.".into());
        }
        let numbers = shots
            .iter()
            .enumerate()
            .filter(|(_, shot)| {
                shot["beat_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&beat["id"]))
            })
            .map(|(index, _)| json!(index + 1))
            .collect::<Vec<_>>();
        if modern {
            if review["shot_numbers"].as_array() != Some(&numbers) {
                return Err(
                    "Beat reviews must identify every segment covering the source beat.".into(),
                );
            }
        } else if review["shot_number"].as_u64().is_none()
            || numbers.first() != Some(&review["shot_number"])
        {
            return Err(
                "Beat reviews must map every source beat to its actual shot, in order.".into(),
            );
        }
        bounded(
            &review["intent"],
            &format!("Intent of beat {}", beat["id"]),
            4000,
        )?;
        bounded(
            &review["evidence"],
            &format!("Prompt evidence for beat {}", beat["id"]),
            4000,
        )?;
    }
    let shot_reviews = array(
        &report["shot_reviews"],
        "Preproduction review must rehearse every shot.",
    )?;
    if shot_reviews.len() != shots.len() {
        return Err("Preproduction review must rehearse every shot.".into());
    }
    for (offset, (shot, review)) in shots.iter().zip(shot_reviews).enumerate() {
        let number = offset + 1;
        if review["shot_number"].as_u64() != Some(number as u64) {
            return Err("Shot reviews must be in shot order.".into());
        }
        bounded(
            &review["staging"],
            &format!("Shot {number} staging review"),
            4000,
        )?;
        bounded(
            &review["identity_and_state"],
            &format!("Shot {number} identity/state review"),
            4000,
        )?;
        if modern {
            if !matches!(
                review["render_risk"].as_str(),
                Some("low" | "medium" | "high")
            ) {
                return Err(format!(
                    "Shot {number} needs an explicit residual render-risk decision."
                ));
            }
            for factor in array(
                &review["risk_factors"],
                &format!("Shot {number} needs a render-risk factor list."),
            )? {
                bounded(factor, &format!("Shot {number} render-risk factor"), 1200)?;
            }
            bounded(
                &review["risk_mitigation"],
                &format!("Shot {number} concrete risk mitigation"),
                4000,
            )?;
            let evidence = array(
                &review["contract_evidence"],
                &format!(
                    "Shot {number} review must cite prompt evidence for every visual requirement."
                ),
            )?;
            if evidence.len() != array(&shot["visual_contract"], "Missing visual contract.")?.len()
            {
                return Err(format!(
                    "Shot {number} review must cite prompt evidence for every visual requirement."
                ));
            }
            for item in evidence {
                bounded(
                    item,
                    &format!("Shot {number} visual contract evidence"),
                    4000,
                )?;
            }
            if require_approval && approved && review["render_risk"] == "high" {
                return Err(format!(
                    "Shot {number} still has high render risk; reject it for automatic planning repair."
                ));
            }
        }
        let timeline = array(
            &review["timeline"],
            &format!("Shot {number} needs a timed physical rehearsal."),
        )?;
        if timeline.is_empty() {
            return Err(format!("Shot {number} needs a timed physical rehearsal."));
        }
        let duration = shot["seconds"].as_f64().ok_or("Missing shot duration.")?;
        let mut cursor: f64 = 0.0;
        for interval in timeline {
            if !interval.is_object() {
                return Err(format!("Shot {number} has an invalid rehearsal interval."));
            }
            let times = interval["start_seconds"]
                .as_f64()
                .zip(interval["end_seconds"].as_f64());
            let (start, end) = times
                .filter(|(start, end)| {
                    start.is_finite()
                        && end.is_finite()
                        && (*start - cursor).abs() <= 0.01
                        && *start >= 0.0
                        && start < end
                        && *end <= duration
                })
                .ok_or_else(|| {
                    format!("Shot {number} rehearsal must continuously cover its actual duration.")
                })?;
            let _ = start;
            bounded(
                &interval["action"],
                &format!("Shot {number} rehearsal action"),
                4000,
            )?;
            cursor = end;
        }
        if (cursor - duration).abs() > 0.01 {
            return Err(format!(
                "Shot {number} rehearsal does not cover its full duration."
            ));
        }
    }
    let issues = array(
        &report["issues"],
        "Preproduction review needs an issues list.",
    )?;
    for issue in issues {
        bounded(issue, "Unresolved review issue", 4000)?;
    }
    if require_approval && (!approved || !issues.is_empty()) {
        let detail = if issues.is_empty() {
            report["summary"].as_str().unwrap_or_default().to_owned()
        } else {
            issues
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        };
        return Err(format!("Preproduction review blocked rendering: {detail}"));
    }
    Ok(())
}
