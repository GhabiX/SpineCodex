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
    pub spine_plantree: Option<SpineTreePlanTreeSnapshot>,
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
pub struct SpineTreePlanTreeSnapshot {
    pub anchor_node_id: String,
    pub root: SpineTreePlanTreeScopeSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreePlanTreeScopeSnapshot {
    pub existing_node_id: Option<String>,
    pub summary: String,
    pub status: Option<SpineTreePlanItemStatus>,
    pub checkpoints: Vec<SpineTreePlanCheckpointSnapshot>,
    pub children: Vec<SpineTreePlanTreeScopeSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpineTreePlanCheckpointSnapshot {
    pub task: String,
    pub status: SpineTreePlanItemStatus,
}
