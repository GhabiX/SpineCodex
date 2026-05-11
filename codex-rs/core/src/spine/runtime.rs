use super::compact::SpineCompactBoundary;
use super::compact::render_context_compacted_outline;
use super::ids::NodeId;
use super::plan_bridge::PlanSnapshot;
use super::state::SpineState;
use super::state::SpineStateError;
use super::store::SpineOperation;
use super::store::SpineSidecarStore;
use super::store::SpineStoreError;
use super::trajs::RawOrdinalRange;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::UpdatePlanArgs;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct SpineRuntime {
    store: SpineSidecarStore,
    state: SpineState,
    next_raw_ordinal: u64,
    staged_transition: Option<StagedTransition>,
    last_committed_transition: Option<CommittedTransition>,
    pending_spine_call_starts: HashMap<String, u64>,
    non_spine_compacted_history: bool,
}

impl SpineRuntime {
    pub(crate) fn load_or_init(
        rollout_path: impl AsRef<Path>,
        next_raw_ordinal: u64,
    ) -> Result<Self, SpineRuntimeError> {
        let store = SpineSidecarStore::for_rollout(rollout_path)?;
        Self::load_or_create(store, next_raw_ordinal)
    }

    pub(crate) fn create(store: SpineSidecarStore) -> Result<Self, SpineRuntimeError> {
        let state = store.create()?;
        Ok(Self::from_parts(store, state, 0))
    }

    pub(crate) fn load_or_create(
        store: SpineSidecarStore,
        next_raw_ordinal: u64,
    ) -> Result<Self, SpineRuntimeError> {
        if store.tree_path().exists() {
            Self::load(store, next_raw_ordinal)
        } else {
            let state = store.create()?;
            Ok(Self::from_parts(store, state, next_raw_ordinal))
        }
    }

    pub(crate) fn load(
        store: SpineSidecarStore,
        next_raw_ordinal: u64,
    ) -> Result<Self, SpineRuntimeError> {
        let state = store.load()?;
        Ok(Self::from_parts(store, state, next_raw_ordinal))
    }

    pub(crate) fn from_parts(
        store: SpineSidecarStore,
        state: SpineState,
        next_raw_ordinal: u64,
    ) -> Self {
        Self {
            store,
            state,
            next_raw_ordinal,
            staged_transition: None,
            last_committed_transition: None,
            pending_spine_call_starts: HashMap::new(),
            non_spine_compacted_history: false,
        }
    }

    pub(crate) fn store(&self) -> &SpineSidecarStore {
        &self.store
    }

    pub(crate) fn state(&self) -> &SpineState {
        &self.state
    }

    pub(crate) fn cursor(&self) -> &NodeId {
        self.state.cursor()
    }

    pub(crate) fn current_ordinal(&self) -> u64 {
        self.next_raw_ordinal
    }

    pub(crate) fn staged_transition(&self) -> Option<&StagedTransition> {
        self.staged_transition.as_ref()
    }

    pub(crate) fn take_last_committed_transition(&mut self) -> Option<CommittedTransition> {
        self.last_committed_transition.take()
    }

    pub(crate) fn mark_non_spine_compacted_history(&mut self) {
        self.non_spine_compacted_history = true;
    }

    pub(crate) fn raw_start_ordinal(&self, node_id: &NodeId) -> Option<u64> {
        self.state.node(node_id)?.raw_start_ordinal
    }

    pub(crate) fn render_context_compacted_outline(
        &self,
        scope_node_id: &NodeId,
    ) -> Result<String, SpineRuntimeError> {
        let scope = self
            .state
            .node(scope_node_id)
            .ok_or_else(|| SpineRuntimeError::UnknownNode(scope_node_id.clone()))?;
        let scope_summary =
            scope
                .summary
                .as_deref()
                .ok_or_else(|| SpineRuntimeError::MissingSummary {
                    node_id: scope_node_id.clone(),
                })?;
        let scope_worklog_path = self.store.worklog_path(scope_node_id);
        let scope_worklog_path = scope_worklog_path
            .strip_prefix(self.store.root())
            .unwrap_or(scope_worklog_path.as_path())
            .to_path_buf();
        let mut child_rows = Vec::new();
        for child in self
            .state
            .nodes()
            .values()
            .filter(|node| node.parent_id.as_ref() == Some(scope_node_id))
        {
            let summary =
                child
                    .summary
                    .as_deref()
                    .ok_or_else(|| SpineRuntimeError::MissingSummary {
                        node_id: child.node_id.clone(),
                    })?;
            let worklog_path = self.store.worklog_path(&child.node_id);
            let worklog_path = worklog_path
                .strip_prefix(self.store.root())
                .unwrap_or(worklog_path.as_path())
                .to_string_lossy()
                .into_owned();
            child_rows.push((
                child.node_id.clone(),
                format!("[{}] {summary}", child.node_id),
                worklog_path,
            ));
        }
        child_rows.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
        let child_rows = child_rows
            .into_iter()
            .map(|(_, summary, path)| (summary, path))
            .collect::<Vec<_>>();
        Ok(render_context_compacted_outline(
            scope_node_id,
            scope_summary,
            &scope_worklog_path,
            &child_rows,
        ))
    }

    pub(crate) fn plan_compaction_after_transition(
        &self,
        committed: &CommittedTransition,
    ) -> Result<Option<SpineCompactBoundary>, SpineRuntimeError> {
        match committed.op {
            SpineOperation::Open => Ok(None),
            SpineOperation::Next => {
                self.ensure_spine_compaction_allowed()?;
                let cut_ordinal =
                    self.raw_start_ordinal(&committed.from_node)
                        .ok_or_else(|| SpineRuntimeError::MissingRawStartOrdinal {
                            node_id: committed.from_node.clone(),
                        })?;
                Ok(Some(SpineCompactBoundary {
                    op: committed.op,
                    node_id: committed.from_node.clone(),
                    scope_node_id: None,
                    cut_ordinal,
                    fold_end_ordinal: committed.boundary_end,
                    transition_summary: committed.summary.clone(),
                }))
            }
            SpineOperation::Close => {
                self.ensure_spine_compaction_allowed()?;
                let scope_node_id = self
                    .state
                    .node(&committed.from_node)
                    .and_then(|node| node.parent_id.clone())
                    .ok_or_else(|| SpineRuntimeError::MissingCloseScope {
                        node_id: committed.from_node.clone(),
                    })?;
                self.store
                    .validate_matching_open_for_scope(&scope_node_id, committed.boundary_end)?;
                let cut_ordinal = self.raw_start_ordinal(&scope_node_id).ok_or_else(|| {
                    SpineRuntimeError::MissingRawStartOrdinal {
                        node_id: scope_node_id.clone(),
                    }
                })?;
                Ok(Some(SpineCompactBoundary {
                    op: committed.op,
                    node_id: scope_node_id.clone(),
                    scope_node_id: Some(scope_node_id),
                    cut_ordinal,
                    fold_end_ordinal: committed.boundary_end,
                    transition_summary: committed.summary.clone(),
                }))
            }
        }
    }

    fn ensure_spine_compaction_allowed(&self) -> Result<(), SpineRuntimeError> {
        if self.non_spine_compacted_history {
            return Err(SpineRuntimeError::NonSpineCompactedHistory);
        }
        Ok(())
    }

    pub(crate) fn record_plan_update(
        &mut self,
        turn_id: impl Into<String>,
        args: UpdatePlanArgs,
    ) -> Result<PlanSnapshot, SpineRuntimeError> {
        let previous = self.store.read_plan_snapshot(self.cursor())?;
        let revision = self
            .store
            .read_plan_revision(self.cursor())?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SpineRuntimeError::PlanRevisionOverflow)?;
        let event_seq = self.store.next_tree_event_seq()?;
        let snapshot = PlanSnapshot::from_update(
            self.cursor(),
            revision,
            event_seq,
            turn_id,
            args,
            previous.as_ref(),
        );
        self.store.write_plan_snapshot(self.cursor(), &snapshot)?;
        Ok(snapshot)
    }

    pub(crate) fn after_response_items_recorded(
        &mut self,
        turn_id: impl Into<String>,
        items: &[ResponseItem],
        start_ordinal: u64,
        end_ordinal: u64,
    ) -> Result<Vec<RawOrdinalRange>, SpineRuntimeError> {
        let expected_end = start_ordinal
            .checked_add(
                u64::try_from(items.len()).map_err(|_| SpineRuntimeError::RawOrdinalOverflow)?,
            )
            .ok_or(SpineRuntimeError::RawOrdinalOverflow)?;
        if start_ordinal != self.next_raw_ordinal || end_ordinal != expected_end {
            return Err(SpineRuntimeError::RawOrdinalMismatch {
                expected_start: self.next_raw_ordinal,
                actual_start: start_ordinal,
                expected_end,
                actual_end: end_ordinal,
            });
        }

        let turn_id = turn_id.into();
        let mut ranges = Vec::new();
        let mut open_range: Option<OpenRange> = None;

        for item in items {
            let item_start = self.next_raw_ordinal;
            let item_end = item_start
                .checked_add(1)
                .ok_or(SpineRuntimeError::RawOrdinalOverflow)?;
            if open_range.is_none() {
                open_range = Some(OpenRange {
                    node_id: self.cursor().clone(),
                    start: item_start,
                });
            }
            if let ResponseItem::FunctionCall {
                name,
                namespace,
                call_id,
                ..
            } = item
                && namespace.is_none()
                && name == "spine"
            {
                self.pending_spine_call_starts
                    .insert(call_id.clone(), item_start);
            }
            if let Some(staged) = self.staged_transition.as_mut()
                && matches!(item, ResponseItem::FunctionCall { call_id, .. } if call_id == &staged.call_id)
            {
                staged.call_start_ordinal = Some(item_start);
            }
            self.next_raw_ordinal = item_end;

            if let Some(call_id) = staged_function_call_output_id(item, self.staged_transition()) {
                if let Some(range) = open_range.take() {
                    ranges.push(self.append_raw_range(turn_id.as_str(), range, item_end)?);
                }
                self.commit_staged_transition(&call_id, item_end)?;
            }
            if let ResponseItem::FunctionCallOutput { call_id, .. } = item {
                self.pending_spine_call_starts.remove(call_id);
            }
        }

        if let Some(range) = open_range {
            ranges.push(self.append_raw_range(turn_id.as_str(), range, self.next_raw_ordinal)?);
        }

        Ok(ranges)
    }

    pub(crate) fn stage_transition(
        &mut self,
        call_id: impl Into<String>,
        turn_id: impl Into<String>,
        op: SpineOperation,
        summary: impl Into<String>,
    ) -> Result<&StagedTransition, SpineRuntimeError> {
        if let Some(staged) = self.staged_transition.as_ref() {
            return Err(SpineRuntimeError::TransitionAlreadyStaged {
                call_id: staged.call_id.clone(),
            });
        }

        let call_id = call_id.into();
        let turn_id = turn_id.into();
        let summary = summary.into();
        let mut validation_state = self.state.clone();
        let transition = op.apply(&mut validation_state, summary.clone())?;

        let call_start_ordinal = self.pending_spine_call_starts.remove(&call_id);

        self.staged_transition = Some(StagedTransition {
            call_id,
            turn_id,
            op,
            from_node: transition.from,
            to_node: transition.to,
            visible_spine: validation_state.visible_spine(),
            summary,
            call_start_ordinal,
        });
        Ok(self
            .staged_transition
            .as_ref()
            .expect("staged transition set"))
    }

    pub(crate) fn commit_staged_transition(
        &mut self,
        call_id: &str,
        boundary_end_ordinal: u64,
    ) -> Result<CommittedTransition, SpineRuntimeError> {
        let staged = self
            .staged_transition
            .as_ref()
            .cloned()
            .ok_or(SpineRuntimeError::NoStagedTransition)?;
        if staged.call_id != call_id {
            return Err(SpineRuntimeError::StagedCallIdMismatch {
                expected: staged.call_id.clone(),
                actual: call_id.to_string(),
            });
        }
        if boundary_end_ordinal != self.next_raw_ordinal {
            return Err(SpineRuntimeError::TransitionBoundaryMismatch {
                expected: self.next_raw_ordinal,
                actual: boundary_end_ordinal,
            });
        }
        let call_start_ordinal = staged.call_start_ordinal.ok_or_else(|| {
            SpineRuntimeError::MissingCallStartOrdinal {
                call_id: staged.call_id.clone(),
            }
        })?;
        if call_start_ordinal >= boundary_end_ordinal {
            return Err(SpineRuntimeError::InvalidCallBoundary {
                call_id: staged.call_id.clone(),
                call_start_ordinal,
                boundary_end: boundary_end_ordinal,
            });
        }

        let mut validation_state = self.state.clone();
        let validation_transition = staged
            .op
            .apply(&mut validation_state, staged.summary.clone())?;
        if validation_transition.from != staged.from_node
            || validation_transition.to != staged.to_node
        {
            return Err(SpineRuntimeError::StagedTransitionMismatch {
                expected_from: staged.from_node.clone(),
                expected_to: staged.to_node.clone(),
                actual_from: validation_transition.from,
                actual_to: validation_transition.to,
            });
        }

        self.store.append_transition_committed(
            &staged.call_id,
            staged.op,
            &staged.from_node,
            &staged.to_node,
            call_start_ordinal,
            boundary_end_ordinal,
        )?;

        let mut next_state = self.state.clone();
        let transition = self.store.record_transition(
            &mut next_state,
            staged.op,
            staged.summary.clone(),
            boundary_end_ordinal,
        )?;
        if transition.from != staged.from_node || transition.to != staged.to_node {
            return Err(SpineRuntimeError::StagedTransitionMismatch {
                expected_from: staged.from_node.clone(),
                expected_to: staged.to_node.clone(),
                actual_from: transition.from,
                actual_to: transition.to,
            });
        }

        self.state = next_state;
        self.staged_transition = None;
        let committed = CommittedTransition {
            op: staged.op,
            call_id: call_id.to_string(),
            from_node: staged.from_node,
            to_node: staged.to_node,
            call_start_ordinal,
            boundary_end: boundary_end_ordinal,
            summary: staged.summary,
        };
        self.last_committed_transition = Some(committed.clone());
        Ok(committed)
    }

    fn append_raw_range(
        &self,
        turn_id: &str,
        range: OpenRange,
        end: u64,
    ) -> Result<RawOrdinalRange, SpineRuntimeError> {
        let range = RawOrdinalRange::new(range.node_id, range.start, end);
        self.store
            .append_raw_items_recorded(&range.node_id, turn_id, range.start, range.end)?;
        Ok(range)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenRange {
    node_id: NodeId,
    start: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StagedTransition {
    pub(crate) call_id: String,
    pub(crate) turn_id: String,
    pub(crate) op: SpineOperation,
    pub(crate) from_node: NodeId,
    pub(crate) to_node: NodeId,
    pub(crate) visible_spine: Vec<NodeId>,
    pub(crate) summary: String,
    pub(crate) call_start_ordinal: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommittedTransition {
    pub(crate) op: SpineOperation,
    pub(crate) call_id: String,
    pub(crate) from_node: NodeId,
    pub(crate) to_node: NodeId,
    pub(crate) call_start_ordinal: u64,
    pub(crate) boundary_end: u64,
    pub(crate) summary: String,
}

#[derive(Debug, Error)]
pub(crate) enum SpineRuntimeError {
    #[error("spine transition already staged for call_id {call_id}")]
    TransitionAlreadyStaged { call_id: String },
    #[error("no staged spine transition")]
    NoStagedTransition,
    #[error("staged spine transition call_id mismatch: expected {expected}, got {actual}")]
    StagedCallIdMismatch { expected: String, actual: String },
    #[error("spine raw ordinal overflow")]
    RawOrdinalOverflow,
    #[error(
        "spine raw ordinal mismatch: expected [{expected_start}, {expected_end}), got [{actual_start}, {actual_end})"
    )]
    RawOrdinalMismatch {
        expected_start: u64,
        actual_start: u64,
        expected_end: u64,
        actual_end: u64,
    },
    #[error("spine transition boundary mismatch: expected {expected}, got {actual}")]
    TransitionBoundaryMismatch { expected: u64, actual: u64 },
    #[error("spine transition {call_id} is missing FunctionCall start ordinal")]
    MissingCallStartOrdinal { call_id: String },
    #[error(
        "spine transition {call_id} has invalid call boundary: start {call_start_ordinal}, end {boundary_end}"
    )]
    InvalidCallBoundary {
        call_id: String,
        call_start_ordinal: u64,
        boundary_end: u64,
    },
    #[error("spine node {node_id} is missing raw_start_ordinal")]
    MissingRawStartOrdinal { node_id: NodeId },
    #[error("spine close transition from {node_id} has no parent scope")]
    MissingCloseScope { node_id: NodeId },
    #[error("spine node {node_id} is missing summary for compact outline")]
    MissingSummary { node_id: NodeId },
    #[error("spine compact cannot map raw ordinals after non-spine compacted history")]
    NonSpineCompactedHistory,
    #[error("unknown spine node {0}")]
    UnknownNode(NodeId),
    #[error("spine plan revision overflow")]
    PlanRevisionOverflow,
    #[error(
        "staged spine transition mismatch: expected {expected_from} -> {expected_to}, got {actual_from} -> {actual_to}"
    )]
    StagedTransitionMismatch {
        expected_from: NodeId,
        expected_to: NodeId,
        actual_from: NodeId,
        actual_to: NodeId,
    },
    #[error(transparent)]
    Store(#[from] SpineStoreError),
    #[error(transparent)]
    State(#[from] SpineStateError),
}

fn staged_function_call_output_id(
    item: &ResponseItem,
    staged: Option<&StagedTransition>,
) -> Option<String> {
    let staged = staged?;
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == &staged.call_id => {
            Some(call_id.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
