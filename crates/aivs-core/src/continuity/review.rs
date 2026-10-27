//! Assess pixel observations and carry accepted evidence through the sequence.
use super::facts::{facts, get, key_label, persistent, physical_key, text, unknown};
use super::plan::source_evidence;
use super::types::{Plan, SceneState};
use super::{FactKey, Result, decode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    observed_opening_state: SceneState,
    source_transition_evidence: Vec<Value>,
    violations: Vec<String>,
    evidence: String,
    neighbor_comparison: String,
    confidence: f64,
    decision: String,
    failure_kind: String,
    #[serde(rename = "reference_findings")]
    _reference_findings: Vec<Value>,
}

pub(super) fn review_shot_data(
    plan: &Plan,
    plan_json: &Value,
    number: usize,
    row: &Value,
) -> Result<Value> {
    let shot = number
        .checked_sub(1)
        .and_then(|i| plan.shots.get(i))
        .ok_or("Invalid continuity shot number.")?;
    let review: Review = decode(&row["continuity_review"])?;
    let observed = facts(&review.observed_opening_state)?;
    let expected = facts(&shot.continuity.opening)?;
    if !review.confidence.is_finite() || !(0.0..=1.0).contains(&review.confidence) {
        return Err("Visual evidence confidence must be between zero and one.".into());
    }
    text(&review.evidence)?;
    text(&review.neighbor_comparison)?;
    if !matches!(
        review.decision.as_str(),
        "pass" | "repair-required" | "uncertain"
    ) {
        return Err("Invalid storyboard review decision.".into());
    }
    if !matches!(
        review.failure_kind.as_str(),
        "none" | "image" | "plan" | "uncertain"
    ) {
        return Err("Invalid storyboard failure kind.".into());
    }
    let mut violations = review.violations;
    for violation in &violations {
        text(violation)?;
    }
    let allowed: BTreeSet<_> = plan.shots[..number]
        .iter()
        .flat_map(|s| s.beat_ids.iter().copied())
        .collect();
    let mut evidence = review.source_transition_evidence;
    for item in &evidence {
        let ids: Vec<i64> = decode(&item["beat_ids"])?;
        source_evidence(
            &ids,
            item["quote"].as_str().ok_or("Invalid source quote.")?,
            &plan.source.beats,
            &allowed,
        )?;
        text(
            item["explanation"]
                .as_str()
                .ok_or("Missing source explanation.")?,
        )?;
    }
    let events = shot
        .continuity
        .transitions
        .iter()
        .filter(|t| t.timing == "across-cut")
        .chain(
            plan.shots
                .get(number.wrapping_sub(2))
                .into_iter()
                .flat_map(|s| &s.continuity.transitions)
                .filter(|t| t.timing == "during-shot"),
        );
    for event in events {
        let item = json!({"beat_ids": event.beat_ids, "quote": event.quote,
            "explanation": format!("Authorized {} {}: {} -> {}. {}", event.entity, event.field, event.before, event.after, event.motivation)});
        if !evidence.contains(&item) {
            evidence.push(item);
        }
    }
    let visible_props: BTreeSet<_> = shot
        .continuity
        .opening
        .props
        .iter()
        .filter(|p| p.visibility == "visible")
        .map(|p| p.id.as_str())
        .collect();
    let mut uncertainties = Vec::new();
    for (key, wanted) in &expected {
        let critical = matches!(
            key.1.as_str(),
            "presence" | "owner" | "support" | "value" | "costume" | "state" | "visibility"
        ) || key.1.starts_with("visibility.")
            || physical_key(key);
        if !critical || unknown(wanted) {
            continue;
        }
        let actual = get(&observed, key).unwrap_or("unknown");
        if unknown(actual) {
            let prop_visible = key
                .0
                .strip_prefix("prop:")
                .is_some_and(|id| visible_props.contains(id));
            let connection_visible = key
                .0
                .strip_prefix("relationship:")
                .is_some_and(|id| visible_props.contains(id))
                && visible_props.contains(
                    key.1
                        .split_once(':')
                        .map_or(key.1.as_str(), |(_, object)| object),
                );
            if matches!(wanted.as_str(), "visible" | "revealed")
                || prop_visible
                || connection_visible
            {
                uncertainties.push(format!(
                    "Cannot verify {}: expected {wanted}, observed {actual}",
                    key_label(key)
                ));
            }
        } else if actual != wanted {
            violations.push(format!(
                "{}: expected {wanted}, observed {actual}",
                key_label(key)
            ));
        }
    }
    for character in &review.observed_opening_state.characters {
        if character.presence == "visible" && !shot.opening_visible_roles.contains(&character.role)
        {
            violations.push(format!("Unscripted opening presence: {}", character.role));
        }
    }
    for relation in &review.observed_opening_state.relationships {
        if relation.relation != "docked-in" || relation.value != "yes" {
            continue;
        }
        let subject = review
            .observed_opening_state
            .props
            .iter()
            .find(|p| p.id == relation.subject);
        let support = review
            .observed_opening_state
            .props
            .iter()
            .find(|p| p.id == relation.object);
        if let Some(subject) = subject {
            let support_owner = support.map_or("unknown", |p| p.owner.as_str());
            if !unknown(&subject.owner)
                && subject.owner != "none"
                && !unknown(&subject.support)
                && subject.support != relation.object
                && !unknown(support_owner)
                && support_owner != subject.owner
            {
                violations.push(format!(
                    "Component {} is both docked and held by {}",
                    relation.subject, subject.owner
                ));
            }
        }
    }
    let mut decision = review.decision;
    if !violations.is_empty()
        || row
            .get("blocking_issue")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty())
    {
        decision = "repair-required".into();
    } else if !uncertainties.is_empty()
        || review.confidence < 0.8
        || review.failure_kind == "uncertain"
    {
        decision = "uncertain".into();
    }
    if review.failure_kind == "plan" && decision == "pass" {
        decision = "repair-required".into();
    }
    violations.extend(uncertainties);
    let mut seen = BTreeSet::new();
    violations.retain(|v| seen.insert(v.clone()));
    let mut result = row["continuity_review"].clone();
    result["shot"] = json!(number);
    result["expected_opening_state"] =
        plan_json["shots"][number - 1]["continuity"]["opening"].clone();
    result["source_transition_evidence"] = json!(evidence);
    result["violations"] = json!(violations);
    result["decision"] = json!(decision);
    if decision == "uncertain" {
        result["failure_kind"] = json!("uncertain");
    }
    result["unobservable_facts"] = json!(
        expected
            .iter()
            .filter(|(key, _)| unknown(get(&observed, key).unwrap_or("unknown")))
            .map(|(key, _)| key)
            .collect::<Vec<_>>()
    );
    Ok(result)
}

pub fn review_shot(plan: &Value, number: usize, row: &Value) -> Result<Value> {
    review_shot_data(&decode(plan)?, plan, number, row)
}

pub fn review_sequence(
    plan_json: &Value,
    rows: &BTreeMap<usize, Value>,
) -> Result<BTreeMap<usize, Value>> {
    let plan: Plan = decode(plan_json)?;
    let mut reports = BTreeMap::new();
    for (&number, row) in rows {
        reports.insert(number, review_shot_data(&plan, plan_json, number, row)?);
    }
    let mut known: BTreeMap<FactKey, (usize, String)> = BTreeMap::new();
    for (&number, report) in &mut reports {
        if report["decision"] != "pass" {
            continue;
        }
        for (key, actual) in facts(&decode(&report["observed_opening_state"])?)? {
            if unknown(&actual) || !persistent(&key) {
                continue;
            }
            if let Some((earlier, value)) = known.get(&key) {
                let mut supported = value.as_str();
                for n in *earlier..=number {
                    for timing in ["across-cut", "during-shot"] {
                        if (n == *earlier && timing == "across-cut")
                            || (n == number && timing == "during-shot")
                        {
                            continue;
                        }
                        for event in &plan.shots[n - 1].continuity.transitions {
                            if event.timing == timing
                                && event.entity == key.0
                                && event.field == key.1
                                && (event.before == supported || unknown(&event.before))
                            {
                                supported = &event.after;
                            }
                        }
                    }
                }
                if actual != supported {
                    report["violations"].as_array_mut().ok_or("Invalid review violations.")?.push(json!(format!("Sequence contradiction with frame {earlier}: {} was {value}, now {actual}; source-backed intervening transitions support {supported}.", key_label(&key))));
                    report["decision"] = json!("repair-required");
                }
            }
            if report["decision"] == "pass" {
                known.insert(key, (number, actual));
            }
        }
    }
    Ok(reports)
}
