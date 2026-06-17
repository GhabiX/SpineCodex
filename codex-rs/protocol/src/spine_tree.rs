use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreeUpdateEvent {
    pub snapshot_seq: u64,
    pub active_node_id: String,
    pub nodes: Vec<SpineTreeNodeSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreeNodeSnapshot {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub summary: Option<String>,
    pub status: SpineTreeNodeStatus,
    pub accounting: Option<SpineTreeNodeAccountingSnapshot>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreeNodeAccountingSnapshot {
    pub current_node_context_tokens: Option<i64>,
    pub current_node_context_problem: Option<SpineNodeContextProblem>,
    pub current_node_context_baseline_source: Option<SpineNodeContextBaselineSource>,
    pub closed_source_suffix_tokens: Option<i64>,
    pub closed_memory_context_tokens: Option<i64>,
    pub memory_output_tokens: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SpineNodeContextProblem {
    MissingCurrentUsage,
    MissingOpenContextBaseline,
    CoordinateMismatch,
    CorruptPressureMetadata,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SpineNodeContextBaselineSource {
    ProviderAtOpen,
    RootCompactHandoff,
    EstimatedFromLiveSuffix,
    CheckpointReplay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpineTreeNodeStatus {
    Live,
    Opened,
    Closed,
    Compacted,
}
