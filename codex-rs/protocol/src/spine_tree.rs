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
    pub plan: Option<SpineTreePlanSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpineTreeNodeStatus {
    Live,
    Opened,
    Finished,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreePlanSnapshot {
    pub revision: u64,
    pub explanation: Option<String>,
    pub scope_allocation: Option<SpineTreeScopeAllocationSnapshot>,
    pub items: Vec<SpineTreePlanItemSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreePlanItemSnapshot {
    pub stable_task_id: String,
    pub step: String,
    pub status: SpineTreePlanItemStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpineTreePlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreeScopeAllocationSnapshot {
    pub anchor_node_id: String,
    pub scopes: Vec<SpineTreeScopeAllocationScopeSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreeScopeAllocationScopeSnapshot {
    pub existing_node_id: Option<String>,
    pub summary: String,
    pub checkpoints: Vec<String>,
}
