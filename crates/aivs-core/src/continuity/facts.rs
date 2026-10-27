//! Flatten observable states and classify persistent facts.
use super::types::{Attribute, Relationship, SceneState};
use super::{FactKey, Facts, Result};
use std::collections::BTreeSet;

pub fn text(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 1200 || value.contains('\0') {
        return Err(
            "Continuity facts must be nonempty bounded text; use unknown or not-visible.".into(),
        );
    }
    Ok(())
}

pub(super) fn unknown(value: &str) -> bool {
    matches!(value, "unknown" | "not-visible")
}

pub(super) fn get<'a>(facts: &'a Facts, key: &FactKey) -> Option<&'a str> {
    facts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

pub(super) fn put(facts: &mut Facts, key: FactKey, value: String) {
    if let Some((_, existing)) = facts.iter_mut().find(|(k, _)| k == &key) {
        *existing = value;
    } else {
        facts.push((key, value));
    }
}

fn insert(facts: &mut Facts, entity: &str, field: &str, value: &str) -> Result<()> {
    text(value)?;
    let key = (entity.into(), field.into());
    if get(facts, &key).is_some() {
        return Err("Duplicate appearance attribute or physical relationship.".into());
    }
    facts.push((key, value.into()));
    Ok(())
}

pub(super) fn attributes(
    facts: &mut Facts,
    entity: &str,
    category: &str,
    rows: &[Attribute],
) -> Result<()> {
    if rows.len() > 24 {
        return Err("Physical/visibility facts must be compact key/value arrays.".into());
    }
    for row in rows {
        text(&row.key)?;
        insert(
            facts,
            entity,
            &format!("{category}.{}", row.key),
            &row.value,
        )?;
    }
    Ok(())
}

pub(super) fn relationships(facts: &mut Facts, rows: &[Relationship]) -> Result<()> {
    if rows.len() > 128 {
        return Err("Physical relationships must be a compact array.".into());
    }
    for row in rows {
        text(&row.subject)?;
        text(&row.relation)?;
        text(&row.object)?;
        insert(
            facts,
            &format!("relationship:{}", row.subject),
            &format!("{}:{}", row.relation, row.object),
            &row.value,
        )?;
    }
    Ok(())
}

pub fn facts(state: &SceneState) -> Result<Facts> {
    if state.characters.len() > 64 || state.props.len() > 64 || state.reveals.len() > 64 {
        return Err("Continuity state arrays must contain at most 64 objects.".into());
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for row in &state.characters {
        text(&row.role)?;
        if !seen.insert(&row.role) {
            return Err(format!("Duplicate continuity entity: {}", row.role));
        }
        if !matches!(
            row.presence.as_str(),
            "visible" | "offscreen" | "absent" | "unknown" | "not-visible"
        ) {
            return Err("Invalid character presence.".into());
        }
        let entity = format!("character:{}", row.role);
        for (field, value) in [
            ("presence", &row.presence),
            ("location", &row.location),
            ("posture", &row.posture),
            ("costume", &row.costume),
        ] {
            insert(&mut result, &entity, field, value)?;
        }
        attributes(&mut result, &entity, "appearance", &row.appearance)?;
        attributes(&mut result, &entity, "morphology", &row.morphology)?;
        attributes(&mut result, &entity, "visibility", &row.visibility)?;
    }
    let mut seen = BTreeSet::new();
    for row in &state.props {
        text(&row.id)?;
        if !seen.insert(&row.id) {
            return Err(format!("Duplicate continuity entity: {}", row.id));
        }
        let entity = format!("prop:{}", row.id);
        for (field, value) in [
            ("owner", &row.owner),
            ("support", &row.support),
            ("location", &row.location),
            ("state", &row.state),
            ("visibility", &row.visibility),
        ] {
            insert(&mut result, &entity, field, value)?;
        }
        attributes(&mut result, &entity, "physical", &row.physical)?;
    }
    let mut seen = BTreeSet::new();
    for row in &state.reveals {
        text(&row.id)?;
        if !seen.insert(&row.id) {
            return Err(format!("Duplicate continuity entity: {}", row.id));
        }
        insert(
            &mut result,
            &format!("reveal:{}", row.id),
            "value",
            &row.value,
        )?;
    }
    relationships(&mut result, &state.relationships)?;
    Ok(result)
}

pub fn physical_key(key: &FactKey) -> bool {
    key.1.starts_with("morphology.")
        || key.1.starts_with("physical.")
        || key.0.starts_with("relationship:")
}

pub(super) fn persistent(key: &FactKey) -> bool {
    physical_key(key) || key.1 == "owner" || key.0.starts_with("reveal:")
}

pub(crate) fn key_label(key: &FactKey) -> String {
    // Match Python's tuple spelling in saved review evidence.
    format!("({}, {})", python_repr(&key.0), python_repr(&key.1))
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if c.is_control() => result.push_str(&format!("\\x{:02x}", c as u32)),
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}
