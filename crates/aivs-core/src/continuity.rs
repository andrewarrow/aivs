//! Observable scene state, source-backed transitions, and sequence acceptance.
mod acceptance;
mod artifacts;
mod dispatch;
mod facts;
mod plan;
mod review;
mod schemas;
mod types;
mod world;

pub use acceptance::{dependent_shots, validate_acceptance};
pub use dispatch::dispatch;
pub(crate) use facts::key_label;
pub use facts::{facts, physical_key, text};
pub use plan::validate_plan;
pub use review::{review_sequence, review_shot};
pub use types::{
    Attribute, CharacterState, Contract, PropState, Relationship, RevealState, SceneState,
    Transition,
};

use serde::de::DeserializeOwned;
use serde_json::Value;

pub const POLICY: &str = "storyboard-continuity-v2";
pub type Result<T> = std::result::Result<T, String>;
pub type FactKey = (String, String);
// Preserve declaration order for unobservable_facts and the Python tuple-key adapter.
pub type Facts = Vec<(FactKey, String)>;

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|e| format!("Invalid continuity data: {e}"))
}
