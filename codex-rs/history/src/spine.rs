use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// Durable pre-sampling boundary, retaining the versioned Spine archive payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct SpineSamplingStartedItem {
    pub version: u32,
    pub payload: Value,
    /// Resolved SDK configuration used by this sampling transaction, never sent as history text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_config: Option<String>,
    /// Exact host initialization before the first canonical sampling, including native context edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_seed: Option<Value>,
}

/// A committed sampling transaction, decoded by the Spine archive owner.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct SpineTransitionItem {
    pub version: u32,
    pub payload: Value,
}

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;
