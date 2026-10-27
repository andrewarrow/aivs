//! Deterministic acceptance of canonical prop pixel observations.
use crate::continuity::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const POLICY: &str = "prop-structure-v2";

#[derive(Deserialize)]
struct Component {
    id: String,
}

#[derive(Deserialize)]
struct Definition {
    id: String,
    components: Vec<Component>,
    relationships: Vec<Connection>,
}

#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Connection {
    subject: String,
    relation: String,
    object: String,
    value: String,
}

#[derive(Deserialize)]
struct PartObservation {
    id: String,
    count: u64,
    matches_definition: bool,
    clearly_visible: bool,
    evidence: String,
}

#[derive(Deserialize)]
struct ConnectionObservation {
    #[serde(flatten)]
    connection: Connection,
    matches_definition: bool,
    clearly_visible: bool,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    parts: Vec<PartObservation>,
    relationships: Vec<ConnectionObservation>,
    coherent_component_state: bool,
    connections_clear: bool,
    extra_parts: Vec<String>,
    violations: Vec<String>,
    confidence: f64,
    evidence: String,
    regeneration_prompt: String,
}

fn evidence(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 1200 || value.contains('\0') {
        return Err("Prop inspection needs bounded, concrete pixel evidence.".into());
    }
    Ok(())
}

pub fn generation_prompt(definition: &Value, replacement: Option<&Value>) -> Result<String> {
    let (value, field) = replacement.map_or(
        (&definition["reference_prompt"], "reference_prompt"),
        |value| (value, "regeneration_prompt"),
    );
    let description = value
        .as_str()
        .filter(|text| {
            !text.trim().is_empty()
                && !text.contains('\0')
                && text.chars().count() <= 1200
                && text.split_whitespace().count() <= 120
        })
        .ok_or_else(|| {
            format!(
                "{field} must describe the complete prop in at most 120 words and 1200 characters."
            )
        })?;
    Ok(format!(
        "One product photograph on a plain gray surface, evenly lit and fully in frame. {} Show one coherent resting assembly, each component present exactly once, and every connection traceable end to end. Each part rests on the surface or its assembled support. Single view.",
        description.trim()
    ))
}

pub fn assess(definition: &Value, observation: &Value) -> Result<Vec<String>> {
    let definition: Definition = serde_json::from_value(definition.clone())
        .map_err(|error| format!("Invalid canonical prop definition: {error}"))?;
    let observation: Observation =
        serde_json::from_value(observation.clone()).map_err(|error| {
            format!("Prop inspection needs complete component and connection observations: {error}")
        })?;
    let expected: BTreeSet<_> = std::iter::once(&definition.id)
        .chain(definition.components.iter().map(|part| &part.id))
        .collect();
    if observation.parts.len() != expected.len()
        || observation
            .parts
            .iter()
            .map(|part| &part.id)
            .collect::<BTreeSet<_>>()
            != expected
    {
        return Err("Inspect every canonical prop/component ID exactly once.".into());
    }
    let mut failures = Vec::new();
    for part in &observation.parts {
        evidence(&part.evidence)?;
        if part.count != 1 || !part.matches_definition || !part.clearly_visible {
            failures.push(format!(
                "{}: count {}; {}",
                part.id, part.count, part.evidence
            ));
        }
    }
    let expected: BTreeSet<_> = definition.relationships.iter().collect();
    if observation.relationships.len() != expected.len()
        || observation
            .relationships
            .iter()
            .map(|row| &row.connection)
            .collect::<BTreeSet<_>>()
            != expected
    {
        return Err("Inspect every declared prop connection exactly once.".into());
    }
    for relation in &observation.relationships {
        evidence(&relation.evidence)?;
        if !relation.matches_definition || !relation.clearly_visible {
            failures.push(relation.evidence.clone());
        }
    }
    evidence(&observation.evidence)?;
    for (key, accepted) in [
        (
            "coherent_component_state",
            observation.coherent_component_state,
        ),
        ("connections_clear", observation.connections_clear),
    ] {
        if !accepted {
            failures.push(format!("{key}: {}", observation.evidence));
        }
    }
    for item in observation
        .extra_parts
        .iter()
        .chain(&observation.violations)
    {
        evidence(item)?;
        failures.push(item.clone());
    }
    if !observation.confidence.is_finite() || !(0.0..=1.0).contains(&observation.confidence) {
        return Err("Prop inspection confidence must be between zero and one.".into());
    }
    if observation.confidence < 0.8 {
        failures.push(format!(
            "Uncertain prop structure: {}",
            observation.evidence
        ));
    }
    if !failures.is_empty() || !observation.regeneration_prompt.is_empty() {
        generation_prompt(&Value::Null, Some(&json!(observation.regeneration_prompt)))?;
    }
    let mut seen = BTreeSet::new();
    failures.retain(|failure| seen.insert(failure.clone()));
    Ok(failures)
}

pub fn require_accepted(spec: &Value, entry: &Value, binding: &Value) -> Result<Value> {
    let report = &entry["prop_validation"];
    if !report.is_object()
        || report["binding"] != *binding
        || report["approved"] != true
        || !assess(&spec["definition"], &report["observation"])?.is_empty()
    {
        return Err(format!(
            "Canonical {} has no current, accepted structural inspection.",
            spec["id"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(report.clone())
}
