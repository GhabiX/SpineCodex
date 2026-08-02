use codex_protocol::ThreadId;
use codex_protocol::protocol::SpineSpawnOutcome;
use codex_protocol::protocol::SpineSpawnProgressEvent;
use codex_protocol::protocol::SpineSpawnTaskProgress;
use codex_protocol::protocol::SpineTreeUpdateEvent;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(crate) mod listener;
mod mcp;
mod render;

pub(crate) use mcp::SpineUiMcpHandler;
pub(crate) use mcp::SpineUiTerminalOutcome;
pub(crate) use mcp::is_enabled;
pub(crate) use mcp::is_tree_affecting_item;
pub(crate) use mcp::snapshot_terminal_notification;
pub(crate) use mcp::snapshot_upsert_notification;

#[cfg(test)]
pub(crate) use mcp::read_resource;
#[cfg(test)]
pub(crate) use mcp::server_status;
#[cfg(test)]
pub(crate) use mcp::tool_call_response;

pub(crate) const ENABLE_ENV: &str = "CODEX_SPINE_APP_UI";
pub(crate) const SERVER_NAME: &str = "__codex_internal_spine_tree_ui__";
pub(crate) const TOOL_NAME: &str = "spine_tree";
pub(crate) const RESOURCE_URI: &str = "ui://spine/tree.html";
const CODE_MODE_SPINE_CARRIER_MARKER: &str = "spine.code_mode.output.v1";
const ITEM_ID_PREFIX: &str = "spine-ui-";
const RESOURCE_MIME_TYPE: &str = "text/html;profile=mcp-app";
const RESOURCE_HTML: &str = include_str!("spine_ui/tree.html");

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineUiSpawnTask {
    progress: SpineSpawnTaskProgress,
    result_node_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineUiSpawnCall {
    call_id: String,
    parent_node_id: Option<String>,
    tasks: Vec<SpineUiSpawnTask>,
}

#[derive(Clone, Debug)]
struct SpineUiAgentState {
    generation: u64,
    state: SpineUiState,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SpineUiState {
    revision: u64,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    snapshot: Option<SpineTreeUpdateEvent>,
    spawn_calls: Vec<SpineUiSpawnCall>,
    settled_spawn_call_ids: HashSet<String>,
    agent_subtrees: HashMap<ThreadId, SpineUiAgentState>,
    invalidated_agent_generations: HashMap<ThreadId, u64>,
}

impl SpineUiState {
    pub(crate) fn record_snapshot(&mut self, snapshot: SpineTreeUpdateEvent) -> bool {
        if let Some(current) = self.snapshot.as_ref()
            && (snapshot.snapshot_seq < current.snapshot_seq
                || (snapshot.snapshot_seq == current.snapshot_seq && snapshot == *current))
        {
            return false;
        }
        for call in &mut self.spawn_calls {
            if call.parent_node_id.is_none() {
                call.parent_node_id = Some(snapshot.active_node_id.clone());
            }
        }
        self.settled_spawn_call_ids
            .extend(snapshot.settled_spawn_call_ids.iter().cloned());
        self.reconcile_spawn_result_nodes(&snapshot);
        let visible_agent_thread_ids = self
            .spawn_calls
            .iter()
            .flat_map(|call| call.tasks.iter().map(|task| task.progress.thread_id))
            .collect::<HashSet<_>>();
        self.agent_subtrees
            .retain(|thread_id, _| visible_agent_thread_ids.contains(thread_id));
        self.invalidated_agent_generations
            .retain(|thread_id, _| visible_agent_thread_ids.contains(thread_id));
        self.settled_spawn_call_ids
            .retain(|call_id| self.spawn_calls.iter().any(|call| &call.call_id == call_id));
        self.snapshot = Some(snapshot);
        self.started_at_ms.get_or_insert_with(now_unix_timestamp_ms);
        self.bump_revision();
        true
    }

    pub(crate) fn record_spawn_progress(&mut self, progress: SpineSpawnProgressEvent) -> bool {
        if self.settled_spawn_call_ids.contains(&progress.call_id) {
            return false;
        }
        let parent_node_id = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.active_node_id.clone());
        if let Some(existing) = self
            .spawn_calls
            .iter_mut()
            .find(|call| call.call_id == progress.call_id)
        {
            let previous_call = existing.clone();
            let result_node_ids = existing
                .tasks
                .iter()
                .map(|task| (task.progress.ordinal, task.result_node_id.clone()))
                .collect::<HashMap<_, _>>();
            let previous = existing.tasks.clone();
            existing.tasks = progress
                .tasks
                .into_iter()
                .map(|mut progress| {
                    if let Some(old) = previous
                        .iter()
                        .find(|task| task.progress.ordinal == progress.ordinal)
                        && should_keep_agent_status(&old.progress.status, &progress.status)
                    {
                        progress.status = old.progress.status.clone();
                    }
                    SpineUiSpawnTask {
                        result_node_id: result_node_ids.get(&progress.ordinal).cloned().flatten(),
                        progress,
                    }
                })
                .collect();
            if existing.parent_node_id.is_none() {
                existing.parent_node_id = parent_node_id;
            }
            if *existing == previous_call {
                return false;
            }
        } else {
            self.spawn_calls.push(SpineUiSpawnCall {
                call_id: progress.call_id,
                parent_node_id,
                tasks: progress
                    .tasks
                    .into_iter()
                    .map(|progress| SpineUiSpawnTask {
                        progress,
                        result_node_id: None,
                    })
                    .collect(),
            });
        }
        self.started_at_ms.get_or_insert_with(now_unix_timestamp_ms);
        self.bump_revision();
        true
    }

    pub(crate) fn record_agent_state(
        &mut self,
        thread_id: ThreadId,
        generation: u64,
        state: SpineUiState,
    ) -> bool {
        let is_known_agent = self.spawn_calls.iter().any(|call| {
            call.tasks
                .iter()
                .any(|task| task.progress.thread_id == thread_id)
        });
        if !is_known_agent
            || self
                .invalidated_agent_generations
                .get(&thread_id)
                .is_some_and(|invalidated| generation <= *invalidated)
            || self.agent_subtrees.get(&thread_id).is_some_and(|current| {
                generation < current.generation
                    || (generation == current.generation
                        && state.revision <= current.state.revision)
            })
        {
            return false;
        }
        for call in &mut self.spawn_calls {
            if let Some(task) = call
                .tasks
                .iter_mut()
                .find(|task| task.progress.thread_id == thread_id)
                && matches!(
                    task.progress.status,
                    codex_protocol::protocol::AgentStatus::PendingInit
                )
            {
                task.progress.status = codex_protocol::protocol::AgentStatus::Running;
            }
        }
        self.agent_subtrees
            .insert(thread_id, SpineUiAgentState { generation, state });
        self.bump_revision();
        true
    }

    pub(crate) fn terminalize_incomplete_agents(
        &mut self,
        terminal_status: codex_protocol::protocol::AgentStatus,
    ) -> bool {
        let mut changed = false;
        for call in &mut self.spawn_calls {
            for task in &mut call.tasks {
                if matches!(
                    task.progress.status,
                    codex_protocol::protocol::AgentStatus::PendingInit
                        | codex_protocol::protocol::AgentStatus::Running
                ) {
                    task.progress.status = terminal_status.clone();
                    changed = true;
                }
            }
        }
        for agent in self.agent_subtrees.values_mut() {
            changed |= agent
                .state
                .terminalize_incomplete_agents(terminal_status.clone());
        }
        if changed {
            self.bump_revision();
        }
        changed
    }

    pub(crate) fn remove_agent_state(&mut self, thread_id: ThreadId, generation: u64) -> bool {
        self.invalidated_agent_generations
            .entry(thread_id)
            .and_modify(|invalidated| *invalidated = (*invalidated).max(generation))
            .or_insert(generation);
        let should_remove_subtree = self
            .agent_subtrees
            .get(&thread_id)
            .is_some_and(|current| current.generation <= generation);
        if !should_remove_subtree {
            return false;
        }
        if should_remove_subtree {
            self.agent_subtrees.remove(&thread_id);
        }
        self.bump_revision();
        true
    }

    pub(crate) fn latest_snapshot(&self) -> Option<&SpineTreeUpdateEvent> {
        self.snapshot.as_ref()
    }

    pub(crate) fn filtered_for_parent(&self, baseline_node_ids: &HashSet<String>) -> Self {
        let mut filtered = self.clone();
        if let Some(snapshot) = filtered.snapshot.as_mut() {
            snapshot
                .nodes
                .retain(|node| !baseline_node_ids.contains(&node.node_id));
            if !snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == snapshot.active_node_id)
                && let Some(node) = snapshot.nodes.last()
            {
                snapshot.active_node_id = node.node_id.clone();
            }
        }
        for call in &mut filtered.spawn_calls {
            if call
                .parent_node_id
                .as_ref()
                .is_some_and(|node_id| baseline_node_ids.contains(node_id))
            {
                call.parent_node_id = None;
            }
        }
        filtered
    }

    pub(crate) fn carry_forward(&self) -> Self {
        Self {
            revision: self.revision,
            started_at_ms: None,
            completed_at_ms: None,
            snapshot: None,
            spawn_calls: self.spawn_calls.clone(),
            settled_spawn_call_ids: self.settled_spawn_call_ids.clone(),
            agent_subtrees: self.agent_subtrees.clone(),
            invalidated_agent_generations: self.invalidated_agent_generations.clone(),
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    pub(crate) fn mark_completed(&mut self) {
        self.completed_at_ms
            .get_or_insert_with(now_unix_timestamp_ms);
    }

    pub(crate) fn started_at_ms(&self) -> Option<i64> {
        self.started_at_ms
    }

    pub(crate) fn completed_at_ms(&self) -> Option<i64> {
        self.completed_at_ms
    }

    pub(crate) fn structured_content(&self) -> Option<serde_json::Value> {
        render::structured_content(self)
    }

    fn reconcile_spawn_result_nodes(&mut self, snapshot: &SpineTreeUpdateEvent) {
        let mut claimed_node_ids = self
            .spawn_calls
            .iter()
            .flat_map(|call| call.tasks.iter())
            .filter_map(|task| task.result_node_id.clone())
            .collect::<HashSet<_>>();

        for call in &mut self.spawn_calls {
            if !self.settled_spawn_call_ids.contains(&call.call_id) {
                continue;
            }
            for task in &mut call.tasks {
                if task.result_node_id.is_some() {
                    continue;
                }
                let Some(node) = snapshot.nodes.iter().find(|node| {
                    node.spawn_outcome.is_some()
                        && node.parent_id == call.parent_node_id
                        && node.summary.as_deref() == Some(task.progress.summary.as_str())
                        && !claimed_node_ids.contains(&node.node_id)
                }) else {
                    continue;
                };
                let result_status = match node.spawn_outcome {
                    Some(SpineSpawnOutcome::Completed) => {
                        codex_protocol::protocol::AgentStatus::Completed(None)
                    }
                    Some(SpineSpawnOutcome::Errored) => {
                        codex_protocol::protocol::AgentStatus::Errored(
                            node.memory_summary
                                .clone()
                                .unwrap_or_else(|| "Agent failed".to_string()),
                        )
                    }
                    Some(SpineSpawnOutcome::Aborted) => {
                        codex_protocol::protocol::AgentStatus::Interrupted
                    }
                    None => continue,
                };
                if !should_keep_agent_status(&task.progress.status, &result_status) {
                    task.progress.status = result_status;
                }
                task.result_node_id = Some(node.node_id.clone());
                claimed_node_ids.insert(node.node_id.clone());
            }
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

fn status_rank(status: &codex_protocol::protocol::AgentStatus) -> u8 {
    match status {
        codex_protocol::protocol::AgentStatus::PendingInit => 0,
        codex_protocol::protocol::AgentStatus::Running => 1,
        codex_protocol::protocol::AgentStatus::Interrupted => 2,
        codex_protocol::protocol::AgentStatus::Completed(_)
        | codex_protocol::protocol::AgentStatus::Errored(_)
        | codex_protocol::protocol::AgentStatus::Shutdown
        | codex_protocol::protocol::AgentStatus::NotFound => 3,
    }
}

fn should_keep_agent_status(
    current: &codex_protocol::protocol::AgentStatus,
    incoming: &codex_protocol::protocol::AgentStatus,
) -> bool {
    status_rank(current) >= 2 || status_rank(current) > status_rank(incoming)
}

fn now_unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "spine_ui_tests.rs"]
mod tests;
