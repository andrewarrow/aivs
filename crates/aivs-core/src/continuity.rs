//! Observable scene state, source-backed transitions, and sequence acceptance.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const POLICY: &str = "storyboard-continuity-v2";
pub type Result<T> = std::result::Result<T, String>;
pub type FactKey = (String, String);
// Preserve declaration order for unobservable_facts and Python's tuple-key adapter.
pub type Facts = Vec<(FactKey, String)>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub subject: String,
    pub relation: String,
    pub object: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterState {
    pub role: String,
    pub presence: String,
    pub location: String,
    pub posture: String,
    pub costume: String,
    pub appearance: Vec<Attribute>,
    pub morphology: Vec<Attribute>,
    pub visibility: Vec<Attribute>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PropState {
    pub id: String,
    pub owner: String,
    pub support: String,
    pub location: String,
    pub state: String,
    pub visibility: String,
    pub physical: Vec<Attribute>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevealState {
    pub id: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneState {
    pub characters: Vec<CharacterState>,
    pub props: Vec<PropState>,
    pub reveals: Vec<RevealState>,
    pub relationships: Vec<Relationship>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub entity: String,
    pub field: String,
    pub before: String,
    pub after: String,
    pub timing: String,
    pub mode: String,
    pub beat_ids: Vec<i64>,
    pub quote: String,
    pub motivation: String,
    pub physical_transformation: bool,
    pub story_time: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub opening: SceneState,
    pub ending: SceneState,
    pub transitions: Vec<Transition>,
}

#[derive(Deserialize)]
struct PhysicalEntity {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    shape: String,
    size: String,
    color: String,
    features: Vec<String>,
}

#[derive(Deserialize)]
struct CanonicalProp {
    #[serde(flatten)]
    entity: PhysicalEntity,
    scene_ids: Vec<i64>,
    components: Vec<PhysicalEntity>,
    relationships: Vec<Relationship>,
    reference_prompt: String,
}

#[derive(Deserialize)]
struct CanonicalCharacter {
    role: String,
    morphology: Vec<Attribute>,
    reference_prompt: String,
}

#[derive(Deserialize)]
struct Environment {
    scene_id: i64,
    reference_prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct World {
    props: Vec<CanonicalProp>,
    characters: Vec<CanonicalCharacter>,
    environments: Vec<Environment>,
}

#[derive(Deserialize)]
struct CastRole {
    role: String,
}

#[derive(Deserialize)]
struct Beat {
    id: i64,
    text: String,
}

#[derive(Deserialize)]
struct Source {
    cast: Vec<CastRole>,
    beats: Vec<Beat>,
}

#[derive(Deserialize)]
struct Shot {
    scene_id: i64,
    beat_ids: Vec<i64>,
    visible_roles: Vec<String>,
    opening_visible_roles: Vec<String>,
    continuity: Contract,
}

#[derive(Deserialize)]
struct Plan {
    source: Source,
    world: World,
    shots: Vec<Shot>,
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|e| format!("Invalid continuity data: {e}"))
}

pub fn text(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 1200 || value.contains('\0') {
        return Err(
            "Continuity facts must be nonempty bounded text; use unknown or not-visible.".into(),
        );
    }
    Ok(())
}

fn unknown(value: &str) -> bool {
    matches!(value, "unknown" | "not-visible")
}

fn get<'a>(facts: &'a Facts, key: &FactKey) -> Option<&'a str> {
    facts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn put(facts: &mut Facts, key: FactKey, value: String) {
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

fn attributes(facts: &mut Facts, entity: &str, category: &str, rows: &[Attribute]) -> Result<()> {
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

fn relationships(facts: &mut Facts, rows: &[Relationship]) -> Result<()> {
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

fn persistent(key: &FactKey) -> bool {
    physical_key(key) || key.1 == "owner" || key.0.starts_with("reveal:")
}

fn physical_fields(entity: &PhysicalEntity) -> [(&str, &String); 4] {
    [
        ("type", &entity.kind),
        ("shape", &entity.shape),
        ("size", &entity.size),
        ("color", &entity.color),
    ]
}

fn canonical_physical(world: &World) -> Facts {
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

fn validate_world(plan: &Plan) -> Result<BTreeSet<String>> {
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

fn source_evidence(
    ids: &[i64],
    quote: &str,
    beats: &[Beat],
    allowed: &BTreeSet<i64>,
) -> Result<()> {
    if ids.is_empty()
        || ids.iter().any(|id| !allowed.contains(id))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        || quote.trim().is_empty()
        || !ids
            .iter()
            .any(|id| beats.iter().any(|b| b.id == *id && b.text.contains(quote)))
    {
        return Err(
            "Continuity transition requires an exact quote from its authorized source beats."
                .into(),
        );
    }
    Ok(())
}

fn changes<'a>(before: &'a Facts, after: &'a Facts) -> Vec<(&'a FactKey, &'a str, &'a str)> {
    before
        .iter()
        .filter_map(|(key, a)| {
            get(after, key)
                .filter(|b| !unknown(a) && !unknown(b) && a.as_str() != *b)
                .map(|b| (key, a.as_str(), b))
        })
        .collect()
}

fn remember(known: &mut Facts, incoming: &Facts, retain_unknown: bool) {
    for (key, value) in incoming {
        if !unknown(value) || (retain_unknown && get(known, key).is_none()) {
            put(known, key.clone(), value.clone());
        }
    }
}

fn validate_plan_data(plan: &Plan) -> Result<()> {
    let roles: BTreeSet<_> = plan.source.cast.iter().map(|r| r.role.as_str()).collect();
    let prop_ids = validate_world(plan)?;
    let mut physical = canonical_physical(&plan.world);
    let mut known = physical.clone();
    let mut previous: Option<&Shot> = None;
    for (index, shot) in plan.shots.iter().enumerate() {
        let number = index + 1;
        let contract = &shot.continuity;
        let opening = facts(&contract.opening)?;
        let ending = facts(&contract.ending)?;
        for state in [&opening, &ending] {
            for (key, value) in &physical {
                let Some(actual) = get(state, key) else {
                    return Err(format!(
                        "Shot {number} must retain canonical physical facts even when concealed."
                    ));
                };
                if unknown(actual) && !unknown(value) {
                    return Err(format!(
                        "Shot {number} must not forget known physical identity when it leaves the crop."
                    ));
                }
            }
        }
        if index == 0 && !changes(&physical, &opening).is_empty() {
            return Err("The first opening must match canonical physical definitions.".into());
        }
        for state in [&contract.opening, &contract.ending] {
            if state
                .characters
                .iter()
                .map(|r| r.role.as_str())
                .collect::<BTreeSet<_>>()
                != roles
            {
                return Err(format!(
                    "Shot {number} must track presence of every source role, including offscreen roles."
                ));
            }
            for prop in &state.props {
                if !prop_ids.contains(&prop.id) {
                    return Err(format!(
                        "Shot {number} invents a prop/component identity: {}",
                        prop.id
                    ));
                }
                if !roles.contains(prop.owner.as_str())
                    && !unknown(&prop.owner)
                    && prop.owner != "none"
                {
                    return Err(format!(
                        "Shot {number} prop owners must be source roles, none or unknown."
                    ));
                }
            }
        }
        let visible: BTreeSet<_> = contract
            .opening
            .characters
            .iter()
            .filter(|r| r.presence == "visible")
            .map(|r| &r.role)
            .collect();
        let declared: BTreeSet<_> = shot.opening_visible_roles.iter().collect();
        if declared != visible
            || declared.len() != shot.opening_visible_roles.len()
            || !visible.is_subset(&shot.visible_roles.iter().collect())
        {
            return Err(format!(
                "Shot {number} opening_visible_roles must match time-zero presence."
            ));
        }
        if contract.transitions.len() > 128 {
            return Err("Continuity transitions must be a compact array.".into());
        }
        let history: Facts = known
            .iter()
            .filter(|(key, _)| {
                previous.is_none_or(|p| p.scene_id == shot.scene_id) || persistent(key)
            })
            .cloned()
            .collect();
        let mut current = history.clone();
        remember(&mut current, &opening, true);
        let mut expected = Vec::new();
        if previous.is_some() {
            expected.extend(
                changes(&history, &opening)
                    .into_iter()
                    .map(|(k, a, b)| ("across-cut", k, a, b)),
            );
        }
        expected.extend(
            changes(&current, &ending)
                .into_iter()
                .map(|(k, a, b)| ("during-shot", k, a, b)),
        );
        let mut recorded = BTreeSet::new();
        let mut paths: BTreeMap<(&str, FactKey), Vec<&Transition>> = BTreeMap::new();
        for event in &contract.transitions {
            for field in [
                &event.entity,
                &event.field,
                &event.before,
                &event.after,
                &event.motivation,
                &event.story_time,
            ] {
                text(field)?;
            }
            if !matches!(event.timing.as_str(), "during-shot" | "across-cut")
                || !matches!(event.mode.as_str(), "on-screen" | "offscreen")
            {
                return Err("Invalid transition timing or visibility.".into());
            }
            if event.timing == "across-cut" && (previous.is_none() || event.mode != "offscreen") {
                return Err("An across-cut transition needs a preceding shot and motivated offscreen action.".into());
            }
            let mut allowed: BTreeSet<_> = if event.timing == "during-shot" {
                &shot.beat_ids
            } else {
                &previous.ok_or("Missing preceding shot.")?.beat_ids
            }
            .iter()
            .copied()
            .collect();
            if event.timing == "across-cut" && previous.is_some_and(|p| p.scene_id != shot.scene_id)
            {
                allowed.extend(&shot.beat_ids);
            }
            source_evidence(&event.beat_ids, &event.quote, &plan.source.beats, &allowed)?;
            if (event.field.starts_with("morphology.") || event.field.starts_with("physical."))
                && !event.physical_transformation
            {
                return Err("Physical identity/body changes require a source-backed transformation or sufficient story time.".into());
            }
            let key = (event.entity.clone(), event.field.clone());
            if !recorded.insert((&event.timing, key.clone(), &event.before, &event.after)) {
                return Err("Duplicate continuity transition.".into());
            }
            paths.entry((&event.timing, key)).or_default().push(event);
        }
        for (timing, key, before, after) in expected {
            let mut cursor = before;
            if let Some(events) = paths.get(&(timing, key.clone())) {
                for event in events {
                    if !unknown(cursor) && event.before != cursor {
                        return Err(format!(
                            "Shot {number} transition starts from the wrong known state: {}",
                            key_label(key)
                        ));
                    }
                    cursor = &event.after;
                }
            }
            if cursor != after {
                return Err(format!(
                    "Shot {number} unexplained {timing} change: {} {before} -> {after}",
                    key_label(key)
                ));
            }
        }
        for ((timing, key), events) in paths {
            let (start, end) = if timing == "across-cut" {
                (&history, &opening)
            } else {
                (&current, &ending)
            };
            let (Some(mut cursor), Some(endpoint)) = (get(start, &key), get(end, &key)) else {
                return Err(format!(
                    "Shot {number} {timing} transition must name a tracked entity and field: {}. Use the exact entity ID and field key present in both endpoint states.",
                    key_label(&key)
                ));
            };
            for event in events {
                if !unknown(cursor) && event.before != cursor {
                    return Err(format!(
                        "Shot {number} transition starts from the wrong known state: {}",
                        key_label(&key)
                    ));
                }
                cursor = &event.after;
            }
            if !unknown(endpoint) && cursor != endpoint {
                return Err(format!(
                    "Shot {number} transition does not reach its recorded endpoint: {}",
                    key_label(&key)
                ));
            }
        }
        previous = Some(shot);
        known = current;
        remember(&mut known, &ending, false);
        for (key, value) in &ending {
            if physical_key(key) && !unknown(value) {
                put(&mut physical, key.clone(), value.clone());
            }
        }
    }
    Ok(())
}

pub fn validate_plan(plan: &Value) -> Result<()> {
    validate_plan_data(&decode(plan)?)
}

fn key_label(key: &FactKey) -> String {
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

fn review_shot_data(plan: &Plan, plan_json: &Value, number: usize, row: &Value) -> Result<Value> {
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

pub fn dependent_shots(boards: &Value, changed: BTreeSet<usize>) -> Result<BTreeSet<usize>> {
    let shots = boards["shots"]
        .as_array()
        .ok_or("Missing storyboard shots.")?;
    let mut affected = changed;
    loop {
        let assets: Vec<_> = shots
            .iter()
            .filter(|e| {
                e["shot"]
                    .as_u64()
                    .is_some_and(|n| affected.contains(&(n as usize)))
            })
            .map(|e| (&e["image"]["file"], &e["image"]["sha256"]))
            .collect();
        let mut added = Vec::new();
        for entry in shots {
            if entry["generation"]["references"]
                .as_array()
                .is_some_and(|refs| {
                    refs.iter()
                        .any(|r| assets.contains(&(&r["file"], &r["sha256"])))
                })
            {
                added.push(
                    entry["shot"]
                        .as_u64()
                        .ok_or("Invalid storyboard shot number.")? as usize,
                );
            }
        }
        let old_len = affected.len();
        affected.extend(added);
        if old_len == affected.len() {
            return Ok(affected);
        }
    }
}

/// Hashes are supplied by the existing Python codec, never reserialized in Rust.
pub fn validate_acceptance(
    plan: &Value,
    boards: &Value,
    report: &Value,
    binding: &Value,
    report_sha256: &Value,
) -> Result<()> {
    let parsed: Plan = decode(plan)?;
    validate_plan_data(&parsed)?;
    let fail = "Storyboard continuity acceptance is failed, stale or incompatible; stop before video inference.";
    let rows = report["shots"].as_array().ok_or(fail)?;
    let shots = plan["shots"].as_array().ok_or(fail)?;
    let references = boards["references"].as_array().ok_or(fail)?;
    let reference_ids: BTreeSet<_> = references.iter().map(|r| r["id"].as_str()).collect();
    if report["version"] != 1
        || report["policy"] != POLICY
        || &report["binding"] != binding
        || report["status"] != "accepted"
        || boards["continuity_acceptance"]["policy"] != POLICY
        || &boards["continuity_acceptance"]["review_sha256"] != report_sha256
        || rows.len() != shots.len()
    {
        return Err(fail.into());
    }
    let mut reconstructed = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let findings = row["reference_findings"].as_array().ok_or(fail)?;
        if row["shot"].as_u64() != Some((index + 1) as u64)
            || row["decision"] != "pass"
            || !row["violations"].as_array().is_some_and(Vec::is_empty)
            || !row["confidence"]
                .as_f64()
                .is_some_and(|c| c.is_finite() && c >= 0.8 && c <= 1.0)
            || findings
                .iter()
                .map(|f| f["id"].as_str())
                .collect::<BTreeSet<_>>()
                != reference_ids
            || findings.iter().any(|f| {
                f["decision"] != "pass"
                    || !f["confidence"]
                        .as_f64()
                        .is_some_and(|c| c.is_finite() && c >= 0.8)
            })
        {
            return Err(fail.into());
        }
        let mut review = row.clone();
        let object = review.as_object_mut().ok_or(fail)?;
        for key in ["shot", "expected_opening_state", "unobservable_facts"] {
            object.remove(key);
        }
        let candidate = json!({"continuity_review": review});
        if row["expected_opening_state"] != shots[index]["continuity"]["opening"]
            || review_shot_data(&parsed, plan, index + 1, &candidate)?["decision"] != "pass"
        {
            return Err(
                "Storyboard acceptance contradicts its state contract or pixel observations."
                    .into(),
            );
        }
        reconstructed.insert(index + 1, candidate);
    }
    if review_sequence(plan, &reconstructed)?
        .values()
        .any(|r| r["decision"] != "pass")
    {
        return Err(
            "Storyboard acceptance contains a sequence-level physical or ownership contradiction."
                .into(),
        );
    }
    Ok(())
}

pub fn dispatch(request: &Value) -> Result<Value> {
    let args = &request["args"];
    match request["operation"].as_str().unwrap_or("") {
        "facts" => Ok(json!(
            facts(&decode(&args["state"])?)?
                .into_iter()
                .map(|(key, value)| (key.0, key.1, value))
                .collect::<Vec<_>>()
        )),
        "validate_world" => Ok(json!(validate_world(&decode(&args["plan"])?)?)),
        "canonical_physical" => Ok(json!(
            canonical_physical(&decode(&args["world"])?)
                .into_iter()
                .map(|(key, value)| (key.0, key.1, value))
                .collect::<Vec<_>>()
        )),
        "validate_plan" => {
            validate_plan(&args["plan"])?;
            Ok(Value::Null)
        }
        "review_shot" => review_shot(
            &args["plan"],
            args["number"].as_u64().ok_or("Invalid shot number.")? as usize,
            &args["row"],
        ),
        "review_sequence" => Ok(json!(review_sequence(
            &args["plan"],
            &decode(&args["rows"])?
        )?)),
        "dependent_shots" => Ok(json!(dependent_shots(
            &args["boards"],
            decode(&args["changed"])?
        )?)),
        "validate_acceptance" => {
            validate_acceptance(
                &args["plan"],
                &args["boards"],
                &args["report"],
                &args["binding"],
                &args["report_sha256"],
            )?;
            Ok(Value::Null)
        }
        _ => Err("Unknown AIVS engine operation.".into()),
    }
}
