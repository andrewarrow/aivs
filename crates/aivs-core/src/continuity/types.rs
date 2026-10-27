//! Typed screenplay states, canonical entities, source beats, and shot contracts.
use serde::{Deserialize, Serialize};

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
pub(super) struct PhysicalEntity {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) shape: String,
    pub(super) size: String,
    pub(super) color: String,
    pub(super) features: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct CanonicalProp {
    #[serde(flatten)]
    pub(super) entity: PhysicalEntity,
    pub(super) scene_ids: Vec<i64>,
    pub(super) components: Vec<PhysicalEntity>,
    pub(super) relationships: Vec<Relationship>,
    pub(super) reference_prompt: String,
}

#[derive(Deserialize)]
pub(super) struct CanonicalCharacter {
    pub(super) role: String,
    pub(super) morphology: Vec<Attribute>,
    pub(super) reference_prompt: String,
}

#[derive(Deserialize)]
pub(super) struct Environment {
    pub(super) scene_id: i64,
    pub(super) reference_prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct World {
    pub(super) props: Vec<CanonicalProp>,
    pub(super) characters: Vec<CanonicalCharacter>,
    pub(super) environments: Vec<Environment>,
}

#[derive(Deserialize)]
pub(super) struct CastRole {
    pub(super) role: String,
}

#[derive(Deserialize)]
pub(super) struct Beat {
    pub(super) id: i64,
    pub(super) text: String,
}

#[derive(Deserialize)]
pub(super) struct Source {
    pub(super) cast: Vec<CastRole>,
    pub(super) beats: Vec<Beat>,
}

#[derive(Deserialize)]
pub(super) struct Shot {
    pub(super) scene_id: i64,
    pub(super) beat_ids: Vec<i64>,
    pub(super) visible_roles: Vec<String>,
    pub(super) opening_visible_roles: Vec<String>,
    pub(super) continuity: Contract,
}

#[derive(Deserialize)]
pub(super) struct Plan {
    pub(super) source: Source,
    pub(super) world: World,
    pub(super) shots: Vec<Shot>,
}
