use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::AgentPath;
use crate::ThreadId;
use crate::protocol::AgentStatus;

/// Transient progress for an experimental `spine.spawn` transaction.
///
/// This event is delivered to live clients only.  It is deliberately not a
/// rollout item: the completed typed receipt remains the sole durable parent
/// input and the only source used by replay.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpineSpawnProgressEvent {
    pub call_id: String,
    pub tasks: Vec<SpineSpawnTaskProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpineSpawnTaskProgress {
    pub ordinal: u32,
    pub summary: String,
    pub thread_id: ThreadId,
    pub agent_path: Option<AgentPath>,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpineTreeUpdateEvent {
    pub snapshot_seq: u64,
    pub active_node_id: String,
    pub nodes: Vec<SpineTreeNodeSnapshot>,
    #[serde(default)]
    pub settled_spawn_call_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpineTreeNodeSnapshot {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub kind: SpineTreeNodeKind,
    pub status: SpineTreeNodeStatus,
    pub summary: Option<String>,
    pub memory_summary: Option<String>,
    #[serde(default)]
    pub spawn_outcome: Option<SpineSpawnOutcome>,
    pub start: u64,
    pub end: Option<u64>,
    pub context_pressure: Option<SpineNodeContextPressureSnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "v2/")]
pub enum SpineSpawnOutcome {
    Completed,
    Errored,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpineNodeContextPressureSnapshot {
    pub open_input_tokens: Option<i64>,
    pub current_input_tokens: Option<i64>,
    pub context_tokens: Option<i64>,
    pub problem: Option<SpineNodeContextPressureProblem>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SpineNodeContextPressureProblem {
    MissingCurrentUsage,
    MissingOpenContextBaseline,
    CoordinateMismatch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SpineTreeNodeKind {
    RootEpoch,
    Task,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SpineTreeNodeStatus {
    Live,
    Opened,
    Closed,
    Compacted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn spine_spawn_progress_round_trips_child_thread_identity() {
        let child_thread_id = ThreadId::new();
        let event = SpineSpawnProgressEvent {
            call_id: "spawn-1".to_string(),
            tasks: vec![SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "child".to_string(),
                thread_id: child_thread_id,
                agent_path: None,
                status: AgentStatus::Running,
            }],
        };

        let json = serde_json::to_value(&event).expect("serialize Spine spawn progress");
        assert_eq!(json["tasks"][0]["threadId"], child_thread_id.to_string());
        assert_eq!(
            serde_json::from_value::<SpineSpawnProgressEvent>(json)
                .expect("deserialize Spine spawn progress"),
            event
        );
    }

    #[test]
    fn spine_tree_update_round_trips_with_stable_wire_names() {
        let event = SpineTreeUpdateEvent {
            snapshot_seq: 9,
            active_node_id: "2.1".to_string(),
            settled_spawn_call_ids: vec![],
            nodes: vec![
                SpineTreeNodeSnapshot {
                    node_id: "2".to_string(),
                    parent_id: None,
                    kind: SpineTreeNodeKind::RootEpoch,
                    status: SpineTreeNodeStatus::Opened,
                    summary: None,
                    memory_summary: None,
                    spawn_outcome: None,
                    start: 7,
                    end: None,
                    context_pressure: None,
                },
                SpineTreeNodeSnapshot {
                    node_id: "2.1".to_string(),
                    parent_id: Some("2".to_string()),
                    kind: SpineTreeNodeKind::Task,
                    status: SpineTreeNodeStatus::Live,
                    summary: Some("verify TUI".to_string()),
                    memory_summary: Some("prior memory".to_string()),
                    spawn_outcome: None,
                    start: 8,
                    end: Some(9),
                    context_pressure: Some(SpineNodeContextPressureSnapshot {
                        open_input_tokens: Some(10_000),
                        current_input_tokens: Some(42_000),
                        context_tokens: Some(32_000),
                        problem: None,
                    }),
                },
            ],
        };

        let json = serde_json::to_value(&event).expect("serialize Spine tree update");
        assert_eq!(
            json,
            json!({
                "snapshotSeq": 9,
                "activeNodeId": "2.1",
                "settledSpawnCallIds": [],
                "nodes": [
                    {
                        "nodeId": "2",
                        "parentId": null,
                        "kind": "root_epoch",
                        "status": "opened",
                        "summary": null,
                        "memorySummary": null,
                        "spawnOutcome": null,
                        "start": 7,
                        "end": null,
                        "contextPressure": null
                    },
                    {
                        "nodeId": "2.1",
                        "parentId": "2",
                        "kind": "task",
                        "status": "live",
                        "summary": "verify TUI",
                        "memorySummary": "prior memory",
                        "spawnOutcome": null,
                        "start": 8,
                        "end": 9,
                        "contextPressure": {
                            "openInputTokens": 10000,
                            "currentInputTokens": 42000,
                            "contextTokens": 32000,
                            "problem": null
                        }
                    }
                ]
            })
        );
        assert_eq!(
            serde_json::from_value::<SpineTreeUpdateEvent>(json)
                .expect("deserialize Spine tree update"),
            event
        );
    }
}
