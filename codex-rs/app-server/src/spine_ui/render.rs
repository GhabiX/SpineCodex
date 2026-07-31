use super::SpineUiState;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SpineSpawnOutcome;
use codex_protocol::protocol::SpineTreeNodeKind;
use codex_protocol::protocol::SpineTreeNodeStatus;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineUiAgentSubtree {
    thread_id: ThreadId,
    #[serde(flatten)]
    content: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineUiRenderSnapshot<'a> {
    active_node_id: &'a str,
    nodes: Vec<SpineUiRenderNode<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineUiRenderNode<'a> {
    node_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
    kind: SpineTreeNodeKind,
    status: SpineTreeNodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spawn_outcome: Option<SpineSpawnOutcome>,
    start: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineUiRenderSpawnCall<'a> {
    call_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_node_id: Option<&'a str>,
    tasks: Vec<SpineUiRenderSpawnTask<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineUiRenderSpawnTask<'a> {
    ordinal: u32,
    summary: &'a str,
    thread_id: ThreadId,
    status: SpineUiRenderAgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_node_id: Option<&'a str>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SpineUiRenderAgentStatus {
    Pending,
    Running,
    Interrupted,
    Completed,
    Error,
    Shutdown,
    NotFound,
}

impl From<&AgentStatus> for SpineUiRenderAgentStatus {
    fn from(status: &AgentStatus) -> Self {
        match status {
            AgentStatus::PendingInit => Self::Pending,
            AgentStatus::Running => Self::Running,
            AgentStatus::Interrupted => Self::Interrupted,
            AgentStatus::Completed(_) => Self::Completed,
            AgentStatus::Errored(_) => Self::Error,
            AgentStatus::Shutdown => Self::Shutdown,
            AgentStatus::NotFound => Self::NotFound,
        }
    }
}

pub(super) fn structured_content(state: &SpineUiState) -> Option<serde_json::Value> {
    let snapshot = state.snapshot.as_ref()?;
    let visible_node_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<HashSet<_>>();
    let mut seen_agent_threads = HashSet::new();
    let agent_subtrees = state
        .spawn_calls
        .iter()
        .flat_map(|call| call.tasks.iter())
        .filter_map(|task| {
            let thread_id = task.progress.thread_id;
            let child_state = &state.agent_subtrees.get(&thread_id)?.state;
            let content = structured_content(child_state)?;
            seen_agent_threads
                .insert(thread_id)
                .then_some(SpineUiAgentSubtree { thread_id, content })
        })
        .collect::<Vec<_>>();
    let snapshot = SpineUiRenderSnapshot {
        active_node_id: &snapshot.active_node_id,
        nodes: snapshot
            .nodes
            .iter()
            .map(|node| SpineUiRenderNode {
                node_id: &node.node_id,
                parent_id: node.parent_id.as_deref(),
                kind: node.kind,
                status: node.status,
                summary: node.summary.as_deref(),
                spawn_outcome: node.spawn_outcome,
                start: node.start,
            })
            .collect(),
    };
    let spawn_calls = state
        .spawn_calls
        .iter()
        .map(|call| SpineUiRenderSpawnCall {
            call_id: &call.call_id,
            parent_node_id: call
                .parent_node_id
                .as_deref()
                .filter(|parent_node_id| visible_node_ids.contains(parent_node_id)),
            tasks: call
                .tasks
                .iter()
                .map(|task| SpineUiRenderSpawnTask {
                    ordinal: task.progress.ordinal,
                    summary: &task.progress.summary,
                    thread_id: task.progress.thread_id,
                    status: if state
                        .agent_sync_timeout_generations
                        .contains_key(&task.progress.thread_id)
                    {
                        SpineUiRenderAgentStatus::Error
                    } else {
                        (&task.progress.status).into()
                    },
                    result_node_id: task.result_node_id.as_deref(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    Some(json!({
        "schemaVersion": 1,
        "uiRevision": state.revision,
        "snapshot": snapshot,
        "spawnCalls": spawn_calls,
        "agentSubtrees": agent_subtrees,
    }))
}
