use super::compact::SpineCompactBoundary;
use super::compact::render_context_compacted_outline;
use super::compact::render_slim_context_compacted_outline;
use super::ids::NodeId;
use super::is_legacy_spine_transition_tool;
use super::is_spine_transition_tool;
use super::plan_bridge::PlanSnapshot;
use super::plan_bridge::PlanTreeCheckpointDraft;
use super::plan_bridge::PlanTreeDraft;
use super::plan_bridge::PlanTreeScopeDraft;
use super::plan_bridge::PlanTreeSnapshot;
use super::projection_epoch::ProjectionEpochMetadata;
use super::state::SpineState;
use super::state::SpineStateError;
use super::store::SpineOperation;
use super::store::SpineSidecarStore;
use super::store::SpineStoreError;
use super::store::TransitionSummaryArg;
use super::trajs::RawOrdinalRange;
use super::view::display_node_id;
use super::view::parse_display_node_id;
use super::view::render_tree;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::SpineUpdatePlanArgs;
use codex_protocol::plan_tool::TaskProjectionArg;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use codex_protocol::spine_tree::SpineTreePlanCheckpointSnapshot;
use codex_protocol::spine_tree::SpineTreePlanItemSnapshot;
use codex_protocol::spine_tree::SpineTreePlanItemStatus;
use codex_protocol::spine_tree::SpineTreePlanSnapshot;
use codex_protocol::spine_tree::SpineTreePlanTreeScopeSnapshot;
use codex_protocol::spine_tree::SpineTreePlanTreeSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(test)]
use std::path::Path;
use thiserror::Error;

const SPINE_HINT_FIRST_THRESHOLD_TOKENS: u64 = 50_000;
const SPINE_HINT_STEP_TOKENS: u64 = 30_000;
pub(crate) const NON_SPINE_COMPACT_STOP_REASON: &str = "non-Spine compact Stop boundary";

#[derive(Debug)]
pub(crate) struct SpineRuntime {
    store: SpineSidecarStore,
    state: SpineState,
    next_raw_ordinal: u64,
    staged_transition: Option<StagedTransition>,
    last_committed_transition: Option<CommittedTransition>,
    pending_spine_call_starts: HashMap<String, u64>,
    mode: SpineRuntimeMode,
    surviving_turn_ids: Option<HashSet<String>>,
    surviving_compact_hashes: Option<HashSet<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpineRuntimeMode {
    Mutable,
    ArchivedReadOnly { reason: String },
}

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn load_or_init(
        rollout_path: impl AsRef<Path>,
        next_raw_ordinal: u64,
    ) -> Result<Self, SpineRuntimeError> {
        let rollout_path = rollout_path.as_ref();
        let store = if SpineSidecarStore::has_sidecar_for_rollout(rollout_path)? {
            SpineSidecarStore::for_rollout(rollout_path)?
        } else {
            SpineSidecarStore::create_for_rollout(rollout_path)?
        };
        Self::load_or_create(store, next_raw_ordinal)
    }

    #[cfg(test)]
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
            mode: SpineRuntimeMode::Mutable,
            surviving_turn_ids: None,
            surviving_compact_hashes: None,
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

    pub(crate) fn surviving_compact_hashes(&self) -> Option<&HashSet<String>> {
        self.surviving_compact_hashes.as_ref()
    }

    pub(crate) fn record_surviving_compact_hash(&mut self, message_hash: String) {
        if let Some(surviving_compact_hashes) = self.surviving_compact_hashes.as_mut() {
            surviving_compact_hashes.insert(message_hash);
        }
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

    pub(crate) fn is_mutable(&self) -> bool {
        matches!(self.mode, SpineRuntimeMode::Mutable)
    }

    pub(crate) fn mark_archived_read_only(&mut self, reason: impl Into<String>) {
        self.mode = SpineRuntimeMode::ArchivedReadOnly {
            reason: reason.into(),
        };
    }

    pub(crate) fn mark_non_spine_compacted_history(&mut self) {
        self.mark_archived_read_only(NON_SPINE_COMPACT_STOP_REASON);
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
        let scope_memory_path = self.store.memory_path(scope_node_id);
        let scope_memory_path = scope_memory_path
            .strip_prefix(self.store.root())
            .unwrap_or(scope_memory_path.as_path())
            .to_path_buf();
        let mut child_rows = Vec::new();
        for child in self
            .state
            .nodes()
            .values()
            .filter(|node| node.parent_id.as_ref() == Some(scope_node_id))
        {
            let summary = child
                .summary
                .clone()
                .unwrap_or_else(|| compact_outline_status_label(&child.status).to_string());
            let memory_path = self.store.memory_path(&child.node_id);
            let memory_path = memory_path
                .strip_prefix(self.store.root())
                .unwrap_or(memory_path.as_path())
                .to_string_lossy()
                .into_owned();
            child_rows.push((
                child.node_id.clone(),
                format!("[{}] {}", child.node_id, summary),
                memory_path,
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
            self.store.root(),
            &scope_memory_path,
            &child_rows,
        ))
    }

    pub(crate) fn render_model_context_compacted_outline(
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
        let mut child_rows = Vec::new();
        for child in self
            .state
            .nodes()
            .values()
            .filter(|node| node.parent_id.as_ref() == Some(scope_node_id))
        {
            let summary = child
                .summary
                .clone()
                .unwrap_or_else(|| compact_outline_status_label(&child.status).to_string());
            child_rows.push((
                child.node_id.clone(),
                format!("[{}] {}", child.node_id, summary),
            ));
        }
        child_rows.sort_by(|(left, _), (right, _)| left.cmp(right));
        let child_rows = child_rows
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        Ok(render_slim_context_compacted_outline(
            scope_node_id,
            scope_summary,
            &child_rows,
        ))
    }

    pub(crate) fn render_tree_for_prompt(&self) -> Result<String, SpineRuntimeError> {
        let cursor = self.cursor();
        if self.state.node(cursor).is_none() {
            return Err(SpineRuntimeError::UnknownNode(cursor.clone()));
        }
        Ok(render_tree(&self.state, cursor))
    }

    pub(crate) fn maybe_emit_size_hint(
        &mut self,
        source: impl Into<String>,
    ) -> Result<Option<SpineRuntimeHint>, SpineRuntimeError> {
        self.size_hint_for_cursor(source)
    }

    fn size_hint_for_cursor(
        &mut self,
        source: impl Into<String>,
    ) -> Result<Option<SpineRuntimeHint>, SpineRuntimeError> {
        let node_id = self.cursor().clone();
        let start = self.raw_start_ordinal(&node_id).ok_or_else(|| {
            SpineRuntimeError::MissingRawStartOrdinal {
                node_id: node_id.clone(),
            }
        })?;
        let estimated_tokens = self
            .store
            .estimate_raw_response_tokens(start, self.next_raw_ordinal)?;
        let Some(threshold_tokens) = size_hint_threshold(estimated_tokens) else {
            return Ok(None);
        };
        if self
            .store
            .has_size_hint_emitted(&node_id, threshold_tokens)?
        {
            return Ok(None);
        }
        self.store.append_size_hint_emitted(
            &node_id,
            threshold_tokens,
            estimated_tokens,
            source,
        )?;
        Ok(Some(SpineRuntimeHint {
            node_id,
            estimated_tokens,
            threshold_tokens,
        }))
    }

    pub(crate) fn plan_compaction_after_transition(
        &self,
        committed: &CommittedTransition,
    ) -> Result<Vec<SpineCompactBoundary>, SpineRuntimeError> {
        match committed.op {
            SpineOperation::Open => Ok(Vec::new()),
            SpineOperation::Next => {
                self.ensure_spine_mutation_allowed()?;
                let cut_ordinal =
                    self.raw_start_ordinal(&committed.from_node)
                        .ok_or_else(|| SpineRuntimeError::MissingRawStartOrdinal {
                            node_id: committed.from_node.clone(),
                        })?;
                Ok(vec![SpineCompactBoundary {
                    op: committed.op,
                    node_id: committed.from_node.clone(),
                    scope_node_id: None,
                    cut_ordinal,
                    fold_end_ordinal: committed.boundary_end,
                    transition_summary: committed.summary.clone().ok_or_else(|| {
                        SpineRuntimeError::MissingSummary {
                            node_id: committed.from_node.clone(),
                        }
                    })?,
                    compact_instruction: committed.compact_instruction.clone(),
                }])
            }
            SpineOperation::Close => {
                self.ensure_spine_mutation_allowed()?;
                let scope_node_id = self
                    .state
                    .node(&committed.from_node)
                    .and_then(|node| node.parent_id.clone())
                    .ok_or_else(|| SpineRuntimeError::MissingCloseScope {
                        node_id: committed.from_node.clone(),
                    })?;
                self.store
                    .validate_matching_open_for_scope(&scope_node_id, committed.boundary_end)?;
                let child_cut_ordinal =
                    self.raw_start_ordinal(&committed.from_node)
                        .ok_or_else(|| SpineRuntimeError::MissingRawStartOrdinal {
                            node_id: committed.from_node.clone(),
                        })?;
                let scope_cut_ordinal =
                    self.raw_start_ordinal(&scope_node_id).ok_or_else(|| {
                        SpineRuntimeError::MissingRawStartOrdinal {
                            node_id: scope_node_id.clone(),
                        }
                    })?;
                let child_summary = self
                    .state
                    .node(&committed.from_node)
                    .ok_or_else(|| SpineRuntimeError::UnknownNode(committed.from_node.clone()))?
                    .summary
                    .clone()
                    .or_else(|| committed.child_summary.clone())
                    .ok_or_else(|| SpineRuntimeError::MissingSummary {
                        node_id: committed.from_node.clone(),
                    })?;
                let scope_summary = self
                    .state
                    .node(&scope_node_id)
                    .ok_or_else(|| SpineRuntimeError::UnknownNode(scope_node_id.clone()))?
                    .summary
                    .clone()
                    .ok_or_else(|| SpineRuntimeError::MissingSummary {
                        node_id: scope_node_id.clone(),
                    })?;
                Ok(vec![
                    SpineCompactBoundary {
                        op: committed.op,
                        node_id: committed.from_node.clone(),
                        scope_node_id: Some(scope_node_id.clone()),
                        cut_ordinal: child_cut_ordinal,
                        fold_end_ordinal: committed.boundary_end,
                        transition_summary: child_summary,
                        compact_instruction: committed.compact_instruction.clone(),
                    },
                    SpineCompactBoundary {
                        op: committed.op,
                        node_id: scope_node_id.clone(),
                        scope_node_id: Some(scope_node_id),
                        cut_ordinal: scope_cut_ordinal,
                        fold_end_ordinal: committed.boundary_end,
                        transition_summary: scope_summary,
                        compact_instruction: committed.compact_instruction.clone(),
                    },
                ])
            }
            SpineOperation::Archive => Err(SpineRuntimeError::ArchiveIsInternal),
        }
    }

    pub(crate) fn plan_root_epoch_archive(
        &self,
    ) -> Result<SpineCompactBoundary, SpineRuntimeError> {
        self.ensure_spine_mutation_allowed()?;
        let node_id = self.state.root_epoch_archive_target()?;
        let cut_ordinal = self.state.root_epoch_cut_ordinal()?;
        let transition_summary = "Context compacted".to_string();
        Ok(SpineCompactBoundary {
            op: SpineOperation::Archive,
            node_id,
            scope_node_id: None,
            cut_ordinal,
            fold_end_ordinal: self.next_raw_ordinal,
            transition_summary,
            compact_instruction: None,
        })
    }

    pub(crate) fn record_root_epoch_archive(
        &mut self,
        summary: impl Into<String>,
        raw_start_ordinal: u64,
        compact_id: impl Into<String>,
        source_turn_id: impl Into<String>,
    ) -> Result<(), SpineRuntimeError> {
        self.ensure_spine_mutation_allowed()?;
        self.store.record_root_epoch_archive(
            &mut self.state,
            summary,
            raw_start_ordinal,
            compact_id,
            source_turn_id,
        )?;
        Ok(())
    }

    pub(crate) fn after_prelude_items_recorded(
        &mut self,
        turn_id: impl Into<String>,
        items: &[ResponseItem],
        start_ordinal: u64,
        end_ordinal: u64,
    ) -> Result<(), SpineRuntimeError> {
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
        if items.is_empty() {
            return Ok(());
        }

        // Fixed prompt/prelude items belong to the hidden root sentinel, while the root epoch and
        // active leaf start at `end_ordinal`; root archive must not fold this prelude range.
        let turn_id = turn_id.into();
        self.next_raw_ordinal = end_ordinal;
        self.append_raw_range(
            turn_id.as_str(),
            OpenRange {
                node_id: NodeId::root(),
                start: start_ordinal,
            },
            end_ordinal,
        )?;
        let root_epoch = self.state.current_root_epoch()?;
        self.store.record_raw_start_ordinal(
            &mut self.state,
            &root_epoch,
            end_ordinal,
            turn_id.clone(),
        )?;
        let cursor = self.cursor().clone();
        self.store
            .record_raw_start_ordinal(&mut self.state, &cursor, end_ordinal, turn_id)?;
        Ok(())
    }

    pub(crate) fn record_projection_reset(
        &mut self,
        state: SpineState,
        next_raw_ordinal: u64,
        surviving_turn_ids: HashSet<String>,
        surviving_compact_hashes: HashSet<String>,
        epoch: ProjectionEpochMetadata,
        reason: impl Into<String>,
        source_turn_id: Option<String>,
    ) -> Result<(), SpineRuntimeError> {
        self.store
            .record_projection_reset(state.clone(), reason, source_turn_id, epoch)?;
        self.state = state;
        self.next_raw_ordinal = next_raw_ordinal;
        self.surviving_turn_ids = Some(surviving_turn_ids);
        self.surviving_compact_hashes = Some(surviving_compact_hashes);
        self.staged_transition = None;
        self.last_committed_transition = None;
        self.pending_spine_call_starts.clear();
        Ok(())
    }

    pub(crate) fn record_projection_survivors(
        &mut self,
        surviving_turn_ids: HashSet<String>,
        surviving_compact_hashes: HashSet<String>,
    ) {
        self.surviving_turn_ids = Some(surviving_turn_ids);
        self.surviving_compact_hashes = Some(surviving_compact_hashes);
    }

    fn ensure_spine_mutation_allowed(&self) -> Result<(), SpineRuntimeError> {
        if let SpineRuntimeMode::ArchivedReadOnly { reason } = &self.mode {
            return Err(SpineRuntimeError::ArchivedReadOnly {
                reason: reason.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn record_plan_update(
        &mut self,
        turn_id: impl Into<String>,
        args: SpineUpdatePlanArgs,
    ) -> Result<PlanSnapshot, SpineRuntimeError> {
        self.ensure_spine_mutation_allowed()?;
        let turn_id = turn_id.into();
        let SpineUpdatePlanArgs {
            mut flat,
            task_projection,
        } = args;
        flat.plan = task_projection.current.checklist.clone();
        let mut plantree = self.task_projection_to_plantree(task_projection)?;
        let revision = self
            .store
            .read_plan_revision(self.cursor())?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SpineRuntimeError::PlanRevisionOverflow)?;
        let event_seq = self.store.next_tree_event_seq()?;
        let anchor_node_id = plantree.anchor.clone();
        self.validate_plantree(&anchor_node_id, &plantree)?;
        normalize_plantree_node_ids(&mut plantree)?;
        let spine_plantree = Some(PlanTreeSnapshot::from_update(&anchor_node_id, plantree));
        let snapshot = PlanSnapshot::from_update(
            self.cursor(),
            revision,
            event_seq,
            turn_id,
            flat,
            spine_plantree,
        );
        self.store.write_plan_snapshot(self.cursor(), &snapshot)?;
        Ok(snapshot)
    }

    fn task_projection_to_plantree(
        &self,
        task_projection: TaskProjectionArg,
    ) -> Result<PlanTreeDraft, SpineRuntimeError> {
        let current_node =
            parse_display_node_id(&task_projection.current.node_id).map_err(|_| {
                SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "invalid task_projection current node id {}",
                        task_projection.current.node_id
                    ),
                }
            })?;
        if &current_node != self.cursor() {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: format!(
                    "task_projection current node {} must match cursor {}",
                    current_node.bracketed(),
                    self.cursor().bracketed()
                ),
            });
        }
        self.ensure_editable_plantree_node(&current_node, "task_projection current")?;

        let draft_scopes = self.normalize_task_projection_drafts(task_projection.draft_nodes)?;
        let mut anchor_candidates = vec![current_node.clone()];
        for draft in &draft_scopes {
            anchor_candidates.push(draft.nearest_real_parent.clone());
        }
        let anchor = lowest_common_ancestor(&anchor_candidates).ok_or_else(|| {
            SpineRuntimeError::InvalidPlanTree {
                message: "task_projection could not resolve an editable anchor".to_string(),
            }
        })?;
        self.ensure_editable_plantree_node(&anchor, "task_projection anchor")?;

        let mut root = PlanTreeScopeDraft {
            node: Some(anchor.to_string()),
            summary: format!("Task projection {}", display_node_id(&anchor)),
            status: None,
            checkpoints: Vec::new(),
            children: Vec::new(),
        };
        self.populate_task_projection_children(
            &mut root.children,
            &anchor,
            &current_node,
            &draft_scopes,
        );

        Ok(PlanTreeDraft { anchor, root })
    }

    fn normalize_task_projection_drafts(
        &self,
        drafts: Vec<codex_protocol::plan_tool::TaskProjectionDraftNodeArg>,
    ) -> Result<Vec<ResolvedTaskProjectionDraft>, SpineRuntimeError> {
        let mut known_draft_ids = HashSet::new();
        let mut resolved = Vec::with_capacity(drafts.len());
        for draft in drafts {
            if draft.summary.trim().is_empty() {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: "task_projection draft summary must not be empty".to_string(),
                });
            }
            let parent = if is_task_projection_draft_id(&draft.parent) {
                if !known_draft_ids.contains(&draft.parent) {
                    return Err(SpineRuntimeError::InvalidPlanTree {
                        message: format!(
                            "task_projection draft parent {} must reference an earlier draft_id",
                            draft.parent
                        ),
                    });
                }
                TaskProjectionParent::Draft(draft.parent.clone())
            } else {
                let real_parent = parse_display_node_id(&draft.parent).map_err(|_| {
                    SpineRuntimeError::InvalidPlanTree {
                        message: format!("invalid task_projection draft parent {}", draft.parent),
                    }
                })?;
                self.ensure_editable_plantree_node(&real_parent, "task_projection parent")?;
                TaskProjectionParent::Real(real_parent)
            };
            if let Some(draft_id) = &draft.draft_id {
                if !is_task_projection_draft_id(draft_id) {
                    return Err(SpineRuntimeError::InvalidPlanTree {
                        message: format!("task_projection draft_id {draft_id} must start with '~'"),
                    });
                }
                if !known_draft_ids.insert(draft_id.clone()) {
                    return Err(SpineRuntimeError::InvalidPlanTree {
                        message: format!("task_projection draft_id {draft_id} is duplicated"),
                    });
                }
            }
            let nearest_real_parent = match &parent {
                TaskProjectionParent::Real(parent) => parent.clone(),
                TaskProjectionParent::Draft(parent) => resolved
                    .iter()
                    .find(|resolved: &&ResolvedTaskProjectionDraft| {
                        resolved.draft_id.as_deref() == Some(parent.as_str())
                    })
                    .map(|resolved| resolved.nearest_real_parent.clone())
                    .ok_or_else(|| SpineRuntimeError::InvalidPlanTree {
                        message: format!(
                            "task_projection draft parent {parent} must reference an earlier draft_id"
                        ),
                    })?,
            };
            let checkpoints = draft
                .checklist
                .into_iter()
                .map(plan_item_to_plantree_checkpoint)
                .collect();
            resolved.push(ResolvedTaskProjectionDraft {
                draft_id: draft.draft_id,
                parent,
                nearest_real_parent,
                summary: draft.summary,
                checkpoints,
            });
        }
        Ok(resolved)
    }

    fn populate_task_projection_children(
        &self,
        children: &mut Vec<PlanTreeScopeDraft>,
        parent: &NodeId,
        current_node: &NodeId,
        drafts: &[ResolvedTaskProjectionDraft],
    ) {
        if let Some(next_existing) = next_child_on_path(parent, current_node) {
            let mut existing = PlanTreeScopeDraft {
                node: Some(next_existing.to_string()),
                summary: format!("Existing node {}", display_node_id(&next_existing)),
                status: None,
                checkpoints: Vec::new(),
                children: Vec::new(),
            };
            self.populate_task_projection_children(
                &mut existing.children,
                &next_existing,
                current_node,
                drafts,
            );
            children.push(existing);
        }
        for draft in drafts.iter().filter(|draft| {
            matches!(&draft.parent, TaskProjectionParent::Real(real_parent) if real_parent == parent)
        }) {
            let mut child = PlanTreeScopeDraft {
                node: None,
                summary: draft.summary.clone(),
                status: None,
                checkpoints: draft.checkpoints.clone(),
                children: Vec::new(),
            };
            self.populate_task_projection_draft_children(&mut child.children, draft, drafts);
            children.push(child);
        }
    }

    fn populate_task_projection_draft_children(
        &self,
        children: &mut Vec<PlanTreeScopeDraft>,
        parent: &ResolvedTaskProjectionDraft,
        drafts: &[ResolvedTaskProjectionDraft],
    ) {
        let Some(parent_draft_id) = parent.draft_id.as_deref() else {
            return;
        };
        for draft in drafts.iter().filter(|draft| {
            matches!(&draft.parent, TaskProjectionParent::Draft(draft_parent) if draft_parent == parent_draft_id)
        }) {
            let mut child = PlanTreeScopeDraft {
                node: None,
                summary: draft.summary.clone(),
                status: None,
                checkpoints: draft.checkpoints.clone(),
                children: Vec::new(),
            };
            self.populate_task_projection_draft_children(&mut child.children, draft, drafts);
            children.push(child);
        }
    }

    pub(crate) fn build_tree_snapshot(&self) -> Result<SpineTreeUpdateEvent, SpineRuntimeError> {
        let snapshot_seq = self.store.next_tree_event_seq()?.saturating_sub(1);
        let mut nodes = Vec::with_capacity(self.state.nodes().len());
        for (node_id, node) in self.state.nodes() {
            if node_id == &NodeId::root() {
                continue;
            }
            let plan = if node_id == self.cursor() {
                self.store
                    .read_projected_plan_snapshot(node_id, self.surviving_turn_ids.as_ref())?
                    .map(spine_tree_plan_snapshot)
                    .transpose()?
            } else {
                None
            };
            nodes.push(SpineTreeNodeSnapshot {
                node_id: display_node_id(&node.node_id),
                parent_id: visible_parent_id(node.parent_id.as_ref()),
                summary: node.summary.clone(),
                status: match node.status {
                    super::state::NodeStatus::Live => SpineTreeNodeStatus::Live,
                    super::state::NodeStatus::Opened => SpineTreeNodeStatus::Opened,
                    super::state::NodeStatus::Finished => SpineTreeNodeStatus::Finished,
                    super::state::NodeStatus::Closed => SpineTreeNodeStatus::Closed,
                },
                plan,
            });
        }

        Ok(SpineTreeUpdateEvent {
            snapshot_seq,
            active_node_id: display_node_id(self.cursor()),
            nodes,
        })
    }

    fn validate_plantree(
        &self,
        anchor: &NodeId,
        plantree: &PlanTreeDraft,
    ) -> Result<(), SpineRuntimeError> {
        let mut existing_scope_nodes = HashSet::new();
        self.validate_plantree_scope(anchor, &plantree.root, &mut existing_scope_nodes)
    }

    fn validate_plantree_scope(
        &self,
        anchor: &NodeId,
        scope: &PlanTreeScopeDraft,
        existing_scope_nodes: &mut HashSet<NodeId>,
    ) -> Result<(), SpineRuntimeError> {
        if scope.summary.trim().is_empty() {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: "plan tree scope summary must not be empty".to_string(),
            });
        }
        for checkpoint in &scope.checkpoints {
            if checkpoint.task.trim().is_empty() {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: "plan tree checkpoint task must not be empty".to_string(),
                });
            }
        }
        if let Some(existing_node_id) = &scope.node {
            let existing_node_id = parse_display_node_id(existing_node_id).map_err(|_| {
                SpineRuntimeError::InvalidPlanTree {
                    message: format!("invalid plantree scope node id {existing_node_id}"),
                }
            })?;
            self.ensure_editable_plantree_node(&existing_node_id, "scope")?;
            if !existing_scope_nodes.insert(existing_node_id.clone()) {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "plantree scope {} is duplicated",
                        existing_node_id.bracketed()
                    ),
                });
            }
            if !is_node_within_anchor(&existing_node_id, anchor) {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "plantree scope {} is outside anchor {}",
                        existing_node_id.bracketed(),
                        anchor.bracketed()
                    ),
                });
            }
        }
        for child in &scope.children {
            self.validate_plantree_scope(anchor, child, existing_scope_nodes)?;
        }
        Ok(())
    }

    fn ensure_editable_plantree_node(
        &self,
        node_id: &NodeId,
        role: &str,
    ) -> Result<(), SpineRuntimeError> {
        let node = self
            .state
            .node(node_id)
            .ok_or_else(|| SpineRuntimeError::UnknownNode(node_id.clone()))?;
        match node.status {
            super::state::NodeStatus::Live | super::state::NodeStatus::Opened => Ok(()),
            super::state::NodeStatus::Finished | super::state::NodeStatus::Closed => {
                Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "plantree {role} {} is read-only because it is {}",
                        node_id.bracketed(),
                        match node.status {
                            super::state::NodeStatus::Finished => "finished",
                            super::state::NodeStatus::Closed => "closed",
                            _ => unreachable!("handled editable states above"),
                        }
                    ),
                })
            }
        }
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
                && (is_spine_transition_tool(name, namespace.as_deref())
                    || is_legacy_spine_transition_tool(name, namespace.as_deref()))
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
        summary: impl TransitionSummaryArg,
        compact_instruction: Option<String>,
    ) -> Result<&StagedTransition, SpineRuntimeError> {
        self.stage_transition_with_child_summary(
            call_id,
            turn_id,
            op,
            summary,
            None::<String>,
            compact_instruction,
        )
    }

    pub(crate) fn stage_transition_with_child_summary(
        &mut self,
        call_id: impl Into<String>,
        turn_id: impl Into<String>,
        op: SpineOperation,
        summary: impl TransitionSummaryArg,
        child_summary: impl TransitionSummaryArg,
        compact_instruction: Option<String>,
    ) -> Result<&StagedTransition, SpineRuntimeError> {
        self.ensure_spine_mutation_allowed()?;
        if let Some(staged) = self.staged_transition.as_ref() {
            return Err(SpineRuntimeError::TransitionAlreadyStaged {
                call_id: staged.call_id.clone(),
            });
        }

        let call_id = call_id.into();
        let turn_id = turn_id.into();
        let summary = summary.into_transition_summary();
        let child_summary = child_summary.into_transition_summary();
        let mut validation_state = self.state.clone();
        let transition = op.apply_with_child_summary(
            &mut validation_state,
            summary.clone(),
            child_summary.clone(),
        )?;
        if op == SpineOperation::Close {
            let scope_node_id = self
                .state
                .node(&transition.from)
                .and_then(|node| node.parent_id.clone())
                .ok_or_else(|| SpineRuntimeError::MissingCloseScope {
                    node_id: transition.from.clone(),
                })?;
            self.store
                .validate_matching_open_for_scope(&scope_node_id, self.next_raw_ordinal)?;
        }

        let call_start_ordinal = self.pending_spine_call_starts.remove(&call_id);

        self.staged_transition = Some(StagedTransition {
            call_id,
            turn_id,
            op,
            from_node: transition.from,
            to_node: transition.to,
            visible_spine: validation_state.visible_spine(),
            summary,
            child_summary,
            compact_instruction,
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
        let validation_transition = staged.op.apply_with_child_summary(
            &mut validation_state,
            staged.summary.clone(),
            staged.child_summary.clone(),
        )?;
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
        let transition = if staged.child_summary.is_some() {
            self.store.record_transition_with_child_summary(
                &mut next_state,
                staged.op,
                staged.summary.clone(),
                staged.child_summary.clone(),
                boundary_end_ordinal,
                staged.turn_id.clone(),
            )?
        } else {
            self.store.record_transition(
                &mut next_state,
                staged.op,
                staged.summary.clone(),
                boundary_end_ordinal,
                staged.turn_id.clone(),
            )?
        };
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
            child_summary: staged.child_summary,
            compact_instruction: staged.compact_instruction,
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

fn visible_parent_id(parent_id: Option<&NodeId>) -> Option<String> {
    match parent_id {
        Some(parent) if parent == &NodeId::root() => None,
        Some(parent) => Some(display_node_id(parent)),
        None => None,
    }
}

#[derive(Clone, Debug)]
struct ResolvedTaskProjectionDraft {
    draft_id: Option<String>,
    parent: TaskProjectionParent,
    nearest_real_parent: NodeId,
    summary: String,
    checkpoints: Vec<PlanTreeCheckpointDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskProjectionParent {
    Real(NodeId),
    Draft(String),
}

fn plan_item_to_plantree_checkpoint(item: PlanItemArg) -> PlanTreeCheckpointDraft {
    PlanTreeCheckpointDraft {
        task: item.step,
        status: item.status,
    }
}

fn is_task_projection_draft_id(value: &str) -> bool {
    value.starts_with('~')
}

fn lowest_common_ancestor(nodes: &[NodeId]) -> Option<NodeId> {
    let first = nodes.first()?;
    let mut prefix = first.segments().to_vec();
    for node in &nodes[1..] {
        let common_len = prefix
            .iter()
            .zip(node.segments())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(common_len);
    }
    if prefix.is_empty() {
        None
    } else {
        Some(NodeId::from_segments(prefix))
    }
}

fn next_child_on_path(parent: &NodeId, descendant: &NodeId) -> Option<NodeId> {
    let parent_len = parent.segments().len();
    let descendant_segments = descendant.segments();
    if descendant_segments.len() <= parent_len {
        return None;
    }
    if !descendant_segments.starts_with(parent.segments()) {
        return None;
    }
    Some(NodeId::from_segments(
        descendant_segments[..=parent_len].to_vec(),
    ))
}

fn normalize_plantree_node_ids(plantree: &mut PlanTreeDraft) -> Result<(), SpineRuntimeError> {
    normalize_plantree_scope_node_ids(&mut plantree.root)
}

fn normalize_plantree_scope_node_ids(
    scope: &mut PlanTreeScopeDraft,
) -> Result<(), SpineRuntimeError> {
    if let Some(node) = &mut scope.node {
        *node = parse_display_node_id(node)
            .map_err(|_| SpineRuntimeError::InvalidPlanTree {
                message: format!("invalid plantree scope node id {node}"),
            })?
            .to_string();
    }
    for child in &mut scope.children {
        normalize_plantree_scope_node_ids(child)?;
    }
    Ok(())
}

fn compact_outline_status_label(status: &super::state::NodeStatus) -> &'static str {
    match status {
        super::state::NodeStatus::Live | super::state::NodeStatus::Opened => "live",
        super::state::NodeStatus::Finished => "finished",
        super::state::NodeStatus::Closed => "closed",
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
    pub(crate) summary: Option<String>,
    pub(crate) child_summary: Option<String>,
    pub(crate) compact_instruction: Option<String>,
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
    pub(crate) summary: Option<String>,
    pub(crate) child_summary: Option<String>,
    pub(crate) compact_instruction: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineRuntimeHint {
    pub(crate) node_id: NodeId,
    pub(crate) estimated_tokens: u64,
    pub(crate) threshold_tokens: u64,
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
    #[error("spine task tree is archived read-only: {reason}")]
    ArchivedReadOnly { reason: String },
    #[error("archive is an internal spine compact operation")]
    ArchiveIsInternal,
    #[error("unknown spine node {0}")]
    UnknownNode(NodeId),
    #[error("spine plan revision overflow")]
    PlanRevisionOverflow,
    #[error("unknown spine plan item status {0}")]
    UnknownPlanItemStatus(String),
    #[error("invalid spine plantree: {message}")]
    InvalidPlanTree { message: String },
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

pub(crate) fn size_hint_threshold(estimated_tokens: u64) -> Option<u64> {
    if estimated_tokens < SPINE_HINT_FIRST_THRESHOLD_TOKENS {
        return None;
    }
    let offset = estimated_tokens - SPINE_HINT_FIRST_THRESHOLD_TOKENS;
    let steps = offset / SPINE_HINT_STEP_TOKENS;
    Some(SPINE_HINT_FIRST_THRESHOLD_TOKENS + steps * SPINE_HINT_STEP_TOKENS)
}

fn spine_tree_plan_snapshot(
    snapshot: PlanSnapshot,
) -> Result<SpineTreePlanSnapshot, SpineRuntimeError> {
    Ok(SpineTreePlanSnapshot {
        revision: snapshot.revision,
        explanation: snapshot.explanation,
        spine_plantree: snapshot.spine_plantree.map(spine_tree_plantree_snapshot),
        items: snapshot
            .items
            .into_iter()
            .map(|item| {
                let status = match item.status.as_str() {
                    "pending" => SpineTreePlanItemStatus::Pending,
                    "in_progress" => SpineTreePlanItemStatus::InProgress,
                    "completed" => SpineTreePlanItemStatus::Completed,
                    _ => {
                        return Err(SpineRuntimeError::UnknownPlanItemStatus(item.status));
                    }
                };
                Ok(SpineTreePlanItemSnapshot {
                    stable_task_id: item.stable_task_id,
                    step: item.step,
                    status,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn spine_tree_plantree_snapshot(snapshot: PlanTreeSnapshot) -> SpineTreePlanTreeSnapshot {
    SpineTreePlanTreeSnapshot {
        anchor_node_id: display_node_id_from_str(&snapshot.anchor_node_id),
        root: spine_tree_plantree_scope_snapshot(snapshot.root),
    }
}

fn spine_tree_plantree_scope_snapshot(
    scope: super::plan_bridge::PlanTreeScope,
) -> SpineTreePlanTreeScopeSnapshot {
    SpineTreePlanTreeScopeSnapshot {
        existing_node_id: scope
            .existing_node_id
            .as_deref()
            .map(display_node_id_from_str),
        summary: scope.summary,
        status: scope.status.and_then(spine_tree_plan_item_status),
        checkpoints: scope
            .checkpoints
            .into_iter()
            .filter_map(|checkpoint| {
                Some(SpineTreePlanCheckpointSnapshot {
                    task: checkpoint.task,
                    status: spine_tree_plan_item_status(checkpoint.status)?,
                })
            })
            .collect(),
        children: scope
            .children
            .into_iter()
            .map(spine_tree_plantree_scope_snapshot)
            .collect(),
    }
}

fn display_node_id_from_str(node_id: &str) -> String {
    NodeId::parse(node_id)
        .map(|node_id| display_node_id(&node_id))
        .unwrap_or_else(|_| node_id.to_string())
}

fn spine_tree_plan_item_status(status: impl AsRef<str>) -> Option<SpineTreePlanItemStatus> {
    match status.as_ref() {
        "pending" => Some(SpineTreePlanItemStatus::Pending),
        "in_progress" => Some(SpineTreePlanItemStatus::InProgress),
        "completed" => Some(SpineTreePlanItemStatus::Completed),
        _ => None,
    }
}

fn is_node_within_anchor(node_id: &NodeId, anchor: &NodeId) -> bool {
    node_id.segments().starts_with(anchor.segments())
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
