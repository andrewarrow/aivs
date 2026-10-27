//! Validate canonical world definitions and derive their physical facts.
use super::facts::{attributes, put, relationships, text};
use super::types::{PhysicalEntity, Plan, World};
use super::{Facts, Result};
use std::collections::BTreeSet;

fn physical_fields(entity: &PhysicalEntity) -> [(&str, &String); 4] {
    [
        ("type", &entity.kind),
        ("shape", &entity.shape),
        ("size", &entity.size),
        ("color", &entity.color),
    ]
}

pub(super) fn canonical_physical(world: &World) -> Facts {
    let mut result = Vec::new();
    for character in &world.characters {
        for a in &character.morphology {
            put(
                &mut result,
                (
                    format!("character:{}", character.role),
                    format!("morphology.{}", a.key),
                ),
                a.value.clone(),
            );
        }
    }
    for prop in &world.props {
        for entity in std::iter::once(&prop.entity).chain(&prop.components) {
            for (field, value) in physical_fields(entity) {
                put(
                    &mut result,
                    (format!("prop:{}", entity.id), format!("physical.{field}")),
                    value.clone(),
                );
            }
            if !entity.features.is_empty() {
                put(
                    &mut result,
                    (
                        format!("prop:{}", entity.id),
                        "physical.distinguishing_features".into(),
                    ),
                    entity.features.join("; "),
                );
            }
        }
        for relation in &prop.relationships {
            put(
                &mut result,
                (
                    format!("relationship:{}", relation.subject),
                    format!("{}:{}", relation.relation, relation.object),
                ),
                relation.value.clone(),
            );
        }
    }
    result
}

pub(super) fn validate_world(plan: &Plan) -> Result<BTreeSet<String>> {
    let roles: BTreeSet<_> = plan.source.cast.iter().map(|c| &c.role).collect();
    let world = &plan.world;
    if world
        .characters
        .iter()
        .map(|c| &c.role)
        .collect::<BTreeSet<_>>()
        != roles
        || world.characters.len() != roles.len()
    {
        return Err("Canonical morphology must bind each source role exactly once.".into());
    }
    for character in &world.characters {
        text(&character.role)?;
        text(&character.reference_prompt)?;
        attributes(
            &mut Vec::new(),
            &format!("character:{}", character.role),
            "morphology",
            &character.morphology,
        )?;
    }
    if world.props.len() > 64 {
        return Err("Canonical props must be a compact array.".into());
    }
    let scenes: BTreeSet<_> = plan.shots.iter().map(|s| s.scene_id).collect();
    let mut ids = BTreeSet::new();
    for prop in &world.props {
        text(&prop.reference_prompt)?;
        for entity in std::iter::once(&prop.entity).chain(&prop.components) {
            text(&entity.id)?;
            if !ids.insert(entity.id.clone()) {
                return Err("Canonical prop/component IDs must be globally unique.".into());
            }
            for (_, value) in physical_fields(entity) {
                text(value)?;
            }
            for feature in &entity.features {
                text(feature)?;
            }
        }
        if prop.scene_ids.is_empty() || prop.scene_ids.iter().any(|n| !scenes.contains(n)) {
            return Err("Canonical props need their relevant source scene IDs.".into());
        }
        relationships(&mut Vec::new(), &prop.relationships)?;
    }
    for prop in &world.props {
        for (index, relation) in prop.relationships.iter().enumerate() {
            let missing: BTreeSet<_> = [&relation.subject, &relation.object]
                .into_iter()
                .filter(|id| !ids.contains(*id))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "Canonical component relationships must bind real physical entity IDs. Prop {:?}, relationships[{index}] references undefined IDs: {}. Use exact declared prop/component IDs or declare the missing physical components.",
                    prop.entity.id,
                    missing.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    if world
        .environments
        .iter()
        .map(|e| e.scene_id)
        .collect::<BTreeSet<_>>()
        != scenes
        || world.environments.len() != scenes.len()
    {
        return Err("Each scene needs one reusable clean environment reference.".into());
    }
    for environment in &world.environments {
        text(&environment.reference_prompt)?;
    }
    Ok(ids)
}
