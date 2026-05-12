use super::ids::NodeId;
use super::ids::NodeIdParseError;
use super::plan_bridge::PlanSnapshot;
use super::plan_bridge::PlanSnapshotItem;
use super::state::NodeStatus;
use super::state::SpineState;
use super::state::SpineStateError;
use super::state::Transition;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

const TREE_FILE: &str = "tree.jsonl";
const STATE_FILE: &str = "state.json";
const NODES_DIR: &str = "nodes";
const WORKLOG_FILE: &str = "worklog.md";
const NODE_TRAJS_FILE: &str = "trajs.jsonl";
const PLAN_FILE: &str = "plan.json";
const TRAJS_INDEX_FILE: &str = "trajs.index.jsonl";
const COMPACT_INDEX_FILE: &str = "compact.index.jsonl";
const RAW_DIR: &str = "raw";
const RAW_ROLLOUT_FILE: &str = "rollout.raw.jsonl";
const GENERATED_WORKLOG_SECTION_MARKER: &str = "\n\n<!-- spine:auto-compact-generated -->\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineSidecarStore {
    root: PathBuf,
}

impl SpineSidecarStore {
    pub(crate) fn for_rollout(rollout_path: impl AsRef<Path>) -> Result<Self, SpineStoreError> {
        Ok(Self {
            root: Self::sidecar_dir_for_rollout(rollout_path.as_ref())?,
        })
    }

    pub(crate) fn sidecar_dir_for_rollout(rollout_path: &Path) -> Result<PathBuf, SpineStoreError> {
        if rollout_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("jsonl")
        {
            return Err(SpineStoreError::InvalidRolloutPath {
                path: rollout_path.to_path_buf(),
                reason: "rollout path must use the .jsonl extension",
            });
        }

        let parent = rollout_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| SpineStoreError::InvalidRolloutPath {
                path: rollout_path.to_path_buf(),
                reason: "rollout path must include a parent directory",
            })?;
        let stem = rollout_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| SpineStoreError::InvalidRolloutPath {
                path: rollout_path.to_path_buf(),
                reason: "rollout path must include a valid UTF-8 file stem",
            })?;

        Ok(parent.join(format!("spine-{stem}")))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn tree_path(&self) -> PathBuf {
        self.root.join(TREE_FILE)
    }

    pub(crate) fn state_path(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }

    pub(crate) fn trajs_index_path(&self) -> PathBuf {
        self.root.join(TRAJS_INDEX_FILE)
    }

    pub(crate) fn compact_index_path(&self) -> PathBuf {
        self.root.join(COMPACT_INDEX_FILE)
    }

    pub(crate) fn raw_rollout_path(&self) -> PathBuf {
        self.root.join(RAW_DIR).join(RAW_ROLLOUT_FILE)
    }

    pub(crate) fn node_dir(&self, node_id: &NodeId) -> PathBuf {
        let mut path = self.root.join(NODES_DIR);
        for segment in node_id.segments() {
            path.push(segment.to_string());
        }
        path
    }

    pub(crate) fn worklog_path(&self, node_id: &NodeId) -> PathBuf {
        self.node_dir(node_id).join(WORKLOG_FILE)
    }

    pub(crate) fn node_trajs_path(&self, node_id: &NodeId) -> PathBuf {
        self.node_dir(node_id).join(NODE_TRAJS_FILE)
    }

    pub(crate) fn plan_path(&self, node_id: &NodeId) -> PathBuf {
        self.node_dir(node_id).join(PLAN_FILE)
    }

    pub(crate) fn create(&self) -> Result<SpineState, SpineStoreError> {
        let tree_path = self.tree_path();
        if tree_path.exists() {
            return Err(SpineStoreError::AlreadyInitialized {
                path: self.root.clone(),
            });
        }

        self.ensure_sidecar_dir()?;
        self.ensure_node_dir(&NodeId::root())?;
        self.create_trajs_index_file()?;
        self.create_compact_index_file()?;
        self.create_raw_rollout_file()?;

        let event = TreeEvent::NodeCreated {
            seq: 1,
            node_id: NodeId::root().to_string(),
            parent_id: None,
            raw_start_ordinal: 0,
        };
        self.append_json_line(&tree_path, &event)?;

        let state = SpineState::new();
        self.write_state_cache(&state)?;
        Ok(state)
    }

    pub(crate) fn load(&self) -> Result<SpineState, SpineStoreError> {
        let state = self.replay_tree()?;
        self.validate_compact_index()?;
        let state_path = self.state_path();
        if state_path.exists() {
            let cached = self.read_state_cache(&state_path)?;
            let replayed = StateSnapshot::from_state(&state);
            if cached != replayed {
                return Err(SpineStoreError::StateCacheMismatch { path: state_path });
            }
        }
        Ok(state)
    }

    pub(crate) fn record_transition(
        &self,
        state: &mut SpineState,
        op: SpineOperation,
        summary: impl Into<String>,
        raw_start_ordinal: u64,
    ) -> Result<Transition, SpineStoreError> {
        let summary = summary.into();
        let mut next_state = state.clone();
        let transition = op.apply(&mut next_state, summary.clone())?;
        next_state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
        let to_parent_id = next_state
            .node(&transition.to)
            .ok_or_else(|| SpineStoreError::InvalidLedger("transition target missing".to_string()))?
            .parent_id
            .as_ref()
            .map(ToString::to_string);

        self.ensure_node_dir(&transition.to)?;

        let event = TreeEvent::TransitionApplied {
            seq: self.next_tree_seq()?,
            op,
            from_node: transition.from.to_string(),
            to_node: transition.to.to_string(),
            to_parent_id,
            summary,
            raw_start_ordinal,
        };
        self.append_json_line(&self.tree_path(), &event)?;

        *state = next_state;
        self.write_state_cache(state)?;
        Ok(transition)
    }

    pub(crate) fn record_root_epoch_archive(
        &self,
        state: &mut SpineState,
        summary: impl Into<String>,
        raw_start_ordinal: u64,
        compact_id: impl Into<String>,
        source_turn_id: impl Into<String>,
    ) -> Result<Transition, SpineStoreError> {
        let summary = summary.into();
        let compact_id = compact_id.into();
        let source_turn_id = source_turn_id.into();
        let mut next_state = state.clone();
        let transition = next_state.archive_current_root_epoch(summary.clone())?;
        next_state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
        let to_parent_id = next_state
            .node(&transition.to)
            .ok_or_else(|| {
                SpineStoreError::InvalidLedger("root epoch archive target missing".to_string())
            })?
            .parent_id
            .as_ref()
            .map(ToString::to_string);

        self.ensure_node_dir(&transition.to)?;

        let event = TreeEvent::RootEpochArchived {
            seq: self.next_tree_seq()?,
            archived_root_id: transition.from.to_string(),
            next_root_id: transition.to.to_string(),
            next_parent_id: to_parent_id,
            summary,
            raw_start_ordinal,
            compact_id,
            source_turn_id,
        };
        self.append_json_line(&self.tree_path(), &event)?;

        *state = next_state;
        self.write_state_cache(state)?;
        Ok(transition)
    }

    pub(crate) fn write_plan<T: Serialize>(
        &self,
        node_id: &NodeId,
        plan: &T,
    ) -> Result<PathBuf, SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.plan_path(node_id);
        let contents =
            serde_json::to_string_pretty(plan).map_err(|source| SpineStoreError::Json {
                path: path.clone(),
                source,
            })? + "\n";
        std::fs::write(&path, contents).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub(crate) fn write_plan_snapshot(
        &self,
        node_id: &NodeId,
        snapshot: &PlanSnapshot,
    ) -> Result<PathBuf, SpineStoreError> {
        let event = TreeEvent::TaskPlanUpdated {
            seq: snapshot.event_seq,
            node_id: node_id.to_string(),
            revision: snapshot.revision,
            explanation: snapshot.explanation.clone(),
            items: snapshot.items.clone(),
            source_turn_id: snapshot.source_turn_id.clone(),
        };
        self.append_json_line(&self.tree_path(), &event)?;
        self.write_plan(node_id, snapshot)
    }

    pub(crate) fn read_plan_snapshot(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<PlanSnapshot>, SpineStoreError> {
        let path = self.plan_path(node_id);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SpineStoreError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let snapshot = serde_json::from_str::<PlanSnapshot>(&contents).map_err(|source| {
            SpineStoreError::Json {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Some(snapshot))
    }

    pub(crate) fn read_plan_revision(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<u64>, SpineStoreError> {
        let node_id = node_id.to_string();
        let mut latest_revision = None;
        for event in self.read_tree_events()? {
            let TreeEvent::TaskPlanUpdated {
                node_id: event_node_id,
                revision,
                ..
            } = event
            else {
                continue;
            };
            if event_node_id == node_id {
                latest_revision = Some(revision);
            }
        }
        Ok(latest_revision)
    }

    pub(crate) fn append_raw_items_recorded(
        &self,
        node_id: &NodeId,
        turn_id: impl Into<String>,
        start: u64,
        end: u64,
    ) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        let event = TrajsIndexEvent::RawItemsRecorded {
            seq: self.next_jsonl_seq(&path)?,
            node_id: node_id.to_string(),
            turn_id: turn_id.into(),
            start,
            end,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_transition_committed(
        &self,
        call_id: impl Into<String>,
        op: SpineOperation,
        from_node: &NodeId,
        to_node: &NodeId,
        call_start_ordinal: u64,
        boundary_end: u64,
    ) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        let call_id = call_id.into();
        #[cfg(test)]
        if call_id == "__spine_fail_transition_commit__" {
            return Err(SpineStoreError::InvalidLedger(
                "injected transition commit failure".to_string(),
            ));
        }
        let event = TrajsIndexEvent::TransitionCommitted {
            seq: self.next_jsonl_seq(&path)?,
            call_id,
            op,
            from_node: from_node.to_string(),
            to_node: to_node.to_string(),
            call_start_ordinal,
            boundary_end,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_raw_mirror_items(
        &self,
        items: &[RolloutItem],
    ) -> Result<(), SpineStoreError> {
        #[cfg(test)]
        if items.iter().any(is_raw_mirror_failure_test_item) {
            return Err(SpineStoreError::InvalidLedger(
                "injected raw mirror failure".to_string(),
            ));
        }
        for item in items {
            self.append_json_line(&self.raw_rollout_path(), item)?;
        }
        Ok(())
    }

    pub(crate) fn append_node_trajs_items(
        &self,
        node_id: &NodeId,
        items: &[RolloutItem],
    ) -> Result<(), SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.node_trajs_path(node_id);
        for item in items {
            self.append_json_line(&path, item)?;
        }
        Ok(())
    }

    pub(crate) fn validate_matching_open_for_scope(
        &self,
        scope_node_id: &NodeId,
        close_boundary_end: u64,
    ) -> Result<(), SpineStoreError> {
        for event in self.read_trajs_index_events()? {
            let TrajsIndexEvent::TransitionCommitted {
                op,
                from_node,
                to_node,
                boundary_end,
                ..
            } = event
            else {
                continue;
            };
            if op != SpineOperation::Open || boundary_end > close_boundary_end {
                continue;
            }
            let from_node = NodeId::parse(&from_node)?;
            let to_node = NodeId::parse(&to_node)?;
            if &from_node == scope_node_id && is_direct_child_of(&to_node, scope_node_id) {
                return Ok(());
            }
        }

        Err(SpineStoreError::InvalidLedger(format!(
            "close compact scope {} has no matching open transition",
            scope_node_id.bracketed()
        )))
    }

    pub(crate) fn append_raw_mirror_compact_checkpoint(
        &self,
        compact_id: impl Into<String>,
        message_hash: impl Into<String>,
        replacement_history_len: usize,
    ) -> Result<(), SpineStoreError> {
        let event = RawMirrorEvent::RawMirrorEvent {
            compact_id: compact_id.into(),
            message_hash: message_hash.into(),
            replacement_history_len,
        };
        self.append_json_line(&self.raw_rollout_path(), &event)
    }

    pub(crate) fn append_compact_started(
        &self,
        compact_id: impl Into<String>,
        node_id: &NodeId,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: impl Into<String>,
        rollout: impl Into<String>,
    ) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactStarted {
            seq: self.next_jsonl_seq(&path)?,
            compact_id: compact_id.into(),
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            strategy: strategy.into(),
            raw_trajs: format!("{RAW_DIR}/{RAW_ROLLOUT_FILE}"),
            rollout: rollout.into(),
        };
        self.append_json_line(&path, &event)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_compact_installed(
        &self,
        compact_id: impl Into<String>,
        node_id: &NodeId,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        replacement_history_len: usize,
        worklog_path: impl Into<String>,
        message_hash: impl Into<String>,
    ) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactInstalled {
            seq: self.next_jsonl_seq(&path)?,
            compact_id: compact_id.into(),
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            replacement_history_len,
            worklog_path: worklog_path.into(),
            message_hash: message_hash.into(),
        };
        self.append_json_line(&path, &event)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_compact_failed(
        &self,
        compact_id: impl Into<String>,
        node_id: &NodeId,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactFailed {
            seq: self.next_jsonl_seq(&path)?,
            compact_id: compact_id.into(),
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            strategy: strategy.into(),
            error: error.into(),
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_worklog_section(
        &self,
        node_id: &NodeId,
        section: &str,
    ) -> Result<(), SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.worklog_path(node_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(GENERATED_WORKLOG_SECTION_MARKER.as_bytes())
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(section.as_bytes())
            .map_err(|source| SpineStoreError::Io { path, source })
    }

    pub(crate) fn worklog_with_appended_section(
        &self,
        node_id: &NodeId,
        section: &str,
    ) -> Result<String, SpineStoreError> {
        let mut worklog = match self.read_worklog_file(node_id) {
            Ok(worklog) => worklog,
            Err(SpineStoreError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
                String::new()
            }
            Err(err) => return Err(err),
        };
        worklog.push_str(GENERATED_WORKLOG_SECTION_MARKER);
        worklog.push_str(section);
        Ok(worklog)
    }

    pub(crate) fn read_worklog(&self, node_id: &NodeId) -> Result<String, SpineStoreError> {
        self.read_worklog_file(node_id)
    }

    fn replay_tree(&self) -> Result<SpineState, SpineStoreError> {
        let events = self.read_tree_events()?;
        let mut state = None;

        for event in events {
            match event {
                TreeEvent::NodeCreated {
                    node_id,
                    parent_id,
                    raw_start_ordinal,
                    ..
                } => {
                    if state.is_some() {
                        return Err(SpineStoreError::InvalidLedger(
                            "root node was created more than once".to_string(),
                        ));
                    }
                    let node_id = NodeId::parse(&node_id)?;
                    if node_id != NodeId::root() || parent_id.is_some() {
                        return Err(SpineStoreError::InvalidLedger(
                            "first node_created event must create the root node".to_string(),
                        ));
                    }
                    let mut root_state = SpineState::new();
                    root_state.set_raw_start_ordinal(&node_id, raw_start_ordinal)?;
                    state = Some(root_state);
                }
                TreeEvent::TransitionApplied {
                    op,
                    from_node,
                    to_node,
                    to_parent_id,
                    summary,
                    raw_start_ordinal,
                    ..
                } => {
                    let state = state.as_mut().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "transition appeared before root node creation".to_string(),
                        )
                    })?;
                    let from_node = NodeId::parse(&from_node)?;
                    let to_node = NodeId::parse(&to_node)?;
                    let to_parent_id = to_parent_id.as_deref().map(NodeId::parse).transpose()?;
                    let transition = op.apply(state, summary)?;
                    if transition.from != from_node || transition.to != to_node {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "transition replay mismatch: expected {} -> {}, got {} -> {}",
                            from_node.bracketed(),
                            to_node.bracketed(),
                            transition.from.bracketed(),
                            transition.to.bracketed()
                        )));
                    }
                    let actual_parent_id = state
                        .node(&transition.to)
                        .and_then(|node| node.parent_id.clone());
                    if actual_parent_id != to_parent_id {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "transition target parent mismatch for {}",
                            transition.to.bracketed()
                        )));
                    }
                    state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
                }
                TreeEvent::TaskPlanUpdated {
                    node_id, revision, ..
                } => {
                    if revision == 0 {
                        return Err(SpineStoreError::InvalidLedger(
                            "task_plan_updated revision must be non-zero".to_string(),
                        ));
                    }
                    let state = state.as_ref().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "task_plan_updated appeared before root node creation".to_string(),
                        )
                    })?;
                    let node_id = NodeId::parse(&node_id)?;
                    if state.node(&node_id).is_none() {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "task_plan_updated references unknown node {}",
                            node_id.bracketed()
                        )));
                    }
                }
                TreeEvent::RootEpochArchived {
                    archived_root_id,
                    next_root_id,
                    next_parent_id,
                    summary,
                    raw_start_ordinal,
                    ..
                } => {
                    let state = state.as_mut().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "root_epoch_archived appeared before root node creation".to_string(),
                        )
                    })?;
                    let archived_root_id = NodeId::parse(&archived_root_id)?;
                    let next_root_id = NodeId::parse(&next_root_id)?;
                    let next_parent_id =
                        next_parent_id.as_deref().map(NodeId::parse).transpose()?;
                    let transition = state.archive_current_root_epoch(summary)?;
                    if transition.from != archived_root_id || transition.to != next_root_id {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "root epoch archive replay mismatch: expected {} -> {}, got {} -> {}",
                            archived_root_id.bracketed(),
                            next_root_id.bracketed(),
                            transition.from.bracketed(),
                            transition.to.bracketed()
                        )));
                    }
                    let actual_parent_id = state
                        .node(&transition.to)
                        .and_then(|node| node.parent_id.clone());
                    if actual_parent_id != next_parent_id {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "root epoch archive target parent mismatch for {}",
                            transition.to.bracketed()
                        )));
                    }
                    state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
                }
            }
        }

        state.ok_or_else(|| {
            SpineStoreError::InvalidLedger("tree.jsonl does not create a root node".to_string())
        })
    }

    fn read_tree_events(&self) -> Result<Vec<TreeEvent>, SpineStoreError> {
        let path = self.tree_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "tree.jsonl line {} is empty",
                    index + 1
                )));
            }
            let event: TreeEvent =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger("tree.jsonl has too many events".to_string())
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "tree.jsonl line {} has seq {}, expected {}",
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
            events.push(event);
        }

        Ok(events)
    }

    fn read_trajs_index_events(&self) -> Result<Vec<TrajsIndexEvent>, SpineStoreError> {
        let path = self.trajs_index_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "trajs.index.jsonl line {} is empty",
                    index + 1
                )));
            }
            let event: TrajsIndexEvent =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger("trajs.index.jsonl has too many events".to_string())
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "trajs.index.jsonl line {} has seq {}, expected {}",
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
            events.push(event);
        }

        Ok(events)
    }

    fn read_compact_index_events(&self) -> Result<Vec<CompactIndexEvent>, SpineStoreError> {
        let path = self.compact_index_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl line {} is empty",
                    index + 1
                )));
            }
            let event: CompactIndexEvent =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger(
                    "compact.index.jsonl has too many events".to_string(),
                )
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl line {} has seq {}, expected {}",
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
            events.push(event);
        }

        Ok(events)
    }

    fn validate_compact_index(&self) -> Result<(), SpineStoreError> {
        let mut attempts: HashMap<String, CompactAttemptState> = HashMap::new();

        for event in self.read_compact_index_events()? {
            match event {
                CompactIndexEvent::CompactStarted {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    ..
                } => {
                    if attempts
                        .insert(
                            compact_id.clone(),
                            CompactAttemptState {
                                node_id,
                                op,
                                cut_ordinal,
                                fold_end_ordinal,
                                terminal: None,
                            },
                        )
                        .is_some()
                    {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "compact.index.jsonl has duplicate compact_started for {compact_id}"
                        )));
                    }
                }
                CompactIndexEvent::CompactInstalled {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    ..
                } => {
                    record_compact_terminal(
                        &mut attempts,
                        compact_id,
                        node_id,
                        op,
                        cut_ordinal,
                        fold_end_ordinal,
                        "compact_installed",
                    )?;
                }
                CompactIndexEvent::CompactFailed {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    ..
                } => {
                    record_compact_terminal(
                        &mut attempts,
                        compact_id,
                        node_id,
                        op,
                        cut_ordinal,
                        fold_end_ordinal,
                        "compact_failed",
                    )?;
                }
            }
        }

        for (compact_id, attempt) in attempts {
            if attempt.terminal.is_none() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl has dangling compact_started for {compact_id}; explicit spine compact repair is required"
                )));
            }
        }

        Ok(())
    }

    fn next_tree_seq(&self) -> Result<u64, SpineStoreError> {
        let len = self.read_tree_events()?.len();
        u64::try_from(len + 1)
            .map_err(|_| SpineStoreError::InvalidLedger("tree.jsonl has too many events".into()))
    }

    pub(crate) fn next_tree_event_seq(&self) -> Result<u64, SpineStoreError> {
        self.next_tree_seq()
    }

    fn next_jsonl_seq(&self, path: &Path) -> Result<u64, SpineStoreError> {
        if !path.exists() {
            return Ok(1);
        }

        let file = File::open(path).map_err(|source| SpineStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut count = 0_u64;
        for line in reader.lines() {
            line.map_err(|source| SpineStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            count = count.checked_add(1).ok_or_else(|| {
                SpineStoreError::InvalidLedger(format!("{} has too many events", path.display()))
            })?;
        }
        Ok(count + 1)
    }

    fn append_json_line<T: Serialize>(
        &self,
        path: &Path,
        value: &T,
    ) -> Result<(), SpineStoreError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| SpineStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        serde_json::to_writer(&mut file, value).map_err(|source| SpineStoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        file.write_all(b"\n").map_err(|source| SpineStoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn ensure_sidecar_dir(&self) -> Result<(), SpineStoreError> {
        std::fs::create_dir_all(&self.root).map_err(|source| SpineStoreError::Io {
            path: self.root.clone(),
            source,
        })
    }

    fn ensure_node_dir(&self, node_id: &NodeId) -> Result<(), SpineStoreError> {
        let path = self.node_dir(node_id);
        std::fs::create_dir_all(&path).map_err(|source| SpineStoreError::Io { path, source })
    }

    fn create_trajs_index_file(&self) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn create_compact_index_file(&self) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn create_raw_rollout_file(&self) -> Result<(), SpineStoreError> {
        let path = self.raw_rollout_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn write_worklog_file(&self, node_id: &NodeId, worklog: &str) -> Result<(), SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.worklog_path(node_id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(Some)
            .or_else(|source| {
                if source.kind() == std::io::ErrorKind::AlreadyExists {
                    let existing = std::fs::read_to_string(&path)?;
                    if existing == worklog {
                        return Ok(None);
                    }
                }
                Err(source)
            })
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        let Some(file) = file.as_mut() else {
            return Ok(());
        };
        file.write_all(worklog.as_bytes())
            .map_err(|source| SpineStoreError::Io { path, source })
    }

    fn read_worklog_file(&self, node_id: &NodeId) -> Result<String, SpineStoreError> {
        let path = self.worklog_path(node_id);
        std::fs::read_to_string(&path).map_err(|source| SpineStoreError::Io { path, source })
    }

    fn write_state_cache(&self, state: &SpineState) -> Result<(), SpineStoreError> {
        let path = self.state_path();
        let contents =
            serde_json::to_string_pretty(&StateSnapshot::from_state(state)).map_err(|source| {
                SpineStoreError::Json {
                    path: path.clone(),
                    source,
                }
            })? + "\n";
        std::fs::write(&path, contents).map_err(|source| SpineStoreError::Io { path, source })
    }

    fn read_state_cache(&self, path: &Path) -> Result<StateSnapshot, SpineStoreError> {
        let contents = std::fs::read_to_string(path).map_err(|source| SpineStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&contents).map_err(|source| SpineStoreError::Json {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpineOperation {
    Open,
    Next,
    Close,
}

impl SpineOperation {
    pub(crate) fn apply(
        self,
        state: &mut SpineState,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        match self {
            SpineOperation::Open => state.open(summary),
            SpineOperation::Next => state.next(summary),
            SpineOperation::Close => state.close(summary),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TreeEvent {
    NodeCreated {
        seq: u64,
        node_id: String,
        parent_id: Option<String>,
        raw_start_ordinal: u64,
    },
    TransitionApplied {
        seq: u64,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        to_parent_id: Option<String>,
        summary: String,
        raw_start_ordinal: u64,
    },
    TaskPlanUpdated {
        seq: u64,
        node_id: String,
        revision: u64,
        explanation: Option<String>,
        items: Vec<PlanSnapshotItem>,
        source_turn_id: String,
    },
    RootEpochArchived {
        seq: u64,
        archived_root_id: String,
        next_root_id: String,
        next_parent_id: Option<String>,
        summary: String,
        raw_start_ordinal: u64,
        compact_id: String,
        source_turn_id: String,
    },
}

impl TreeEvent {
    fn seq(&self) -> u64 {
        match self {
            TreeEvent::NodeCreated { seq, .. }
            | TreeEvent::TransitionApplied { seq, .. }
            | TreeEvent::TaskPlanUpdated { seq, .. }
            | TreeEvent::RootEpochArchived { seq, .. } => *seq,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TrajsIndexEvent {
    RawItemsRecorded {
        seq: u64,
        node_id: String,
        turn_id: String,
        start: u64,
        end: u64,
    },
    TransitionCommitted {
        seq: u64,
        call_id: String,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        call_start_ordinal: u64,
        boundary_end: u64,
    },
}

impl TrajsIndexEvent {
    fn seq(&self) -> u64 {
        match self {
            TrajsIndexEvent::RawItemsRecorded { seq, .. }
            | TrajsIndexEvent::TransitionCommitted { seq, .. } => *seq,
        }
    }
}

#[derive(Debug)]
struct CompactAttemptState {
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    terminal: Option<&'static str>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CompactIndexEvent {
    CompactStarted {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        raw_trajs: String,
        rollout: String,
    },
    CompactInstalled {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        replacement_history_len: usize,
        worklog_path: String,
        message_hash: String,
    },
    CompactFailed {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        error: String,
    },
}

impl CompactIndexEvent {
    fn seq(&self) -> u64 {
        match self {
            CompactIndexEvent::CompactStarted { seq, .. }
            | CompactIndexEvent::CompactInstalled { seq, .. }
            | CompactIndexEvent::CompactFailed { seq, .. } => *seq,
        }
    }
}

fn record_compact_terminal(
    attempts: &mut HashMap<String, CompactAttemptState>,
    compact_id: String,
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    terminal: &'static str,
) -> Result<(), SpineStoreError> {
    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has {terminal} without matching compact_started for {compact_id}"
        )));
    };
    if attempt.terminal.is_some() {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has duplicate terminal event for {compact_id}"
        )));
    }
    if attempt.node_id != node_id
        || attempt.op != op
        || attempt.cut_ordinal != cut_ordinal
        || attempt.fold_end_ordinal != fold_end_ordinal
    {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl {terminal} does not match compact_started for {compact_id}"
        )));
    }
    attempt.terminal = Some(terminal);
    Ok(())
}

#[cfg(test)]
fn is_raw_mirror_failure_test_item(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::ResponseItem(codex_protocol::models::ResponseItem::Message {
            content,
            ..
        }) => content.iter().any(|content_item| {
            matches!(
                content_item,
                codex_protocol::models::ContentItem::InputText { text }
                    | codex_protocol::models::ContentItem::OutputText { text }
                    if text == "__spine_fail_raw_mirror__"
            )
        }),
        RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::Warning(warning)) => {
            warning.message == "__spine_fail_raw_mirror__"
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawMirrorEvent {
    RawMirrorEvent {
        compact_id: String,
        message_hash: String,
        replacement_history_len: usize,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct StateSnapshot {
    cursor: String,
    nodes: Vec<NodeSnapshot>,
}

impl StateSnapshot {
    fn from_state(state: &SpineState) -> Self {
        Self {
            cursor: state.cursor().to_string(),
            nodes: state
                .nodes()
                .values()
                .map(|node| NodeSnapshot {
                    node_id: node.node_id.to_string(),
                    parent_id: node.parent_id.as_ref().map(ToString::to_string),
                    raw_start_ordinal: node.raw_start_ordinal,
                    status: status_label(&node.status).to_string(),
                    summary: node.summary.clone(),
                    worklog_path: Some(relative_worklog_path(&node.node_id)),
                    plan_path: Some(relative_plan_path(&node.node_id)),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct NodeSnapshot {
    node_id: String,
    parent_id: Option<String>,
    raw_start_ordinal: Option<u64>,
    status: String,
    summary: Option<String>,
    worklog_path: Option<String>,
    plan_path: Option<String>,
}

#[derive(Debug, Error)]
pub(crate) enum SpineStoreError {
    #[error("invalid spine rollout path {path}: {reason}")]
    InvalidRolloutPath { path: PathBuf, reason: &'static str },
    #[error("spine sidecar already initialized at {path}")]
    AlreadyInitialized { path: PathBuf },
    #[error("failed to access spine sidecar file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse spine sidecar JSON {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid spine ledger: {0}")]
    InvalidLedger(String),
    #[error("spine state cache mismatch at {path}")]
    StateCacheMismatch { path: PathBuf },
    #[error("spine worklog hash mismatch for {node_id}")]
    WorklogHashMismatch { node_id: NodeId },
    #[error(transparent)]
    State(#[from] SpineStateError),
    #[error(transparent)]
    NodeId(#[from] NodeIdParseError),
}

fn status_label(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "opened",
        NodeStatus::Finished => "finished",
        NodeStatus::Closed => "closed",
    }
}

fn relative_worklog_path(node_id: &NodeId) -> String {
    relative_node_file_path(node_id, WORKLOG_FILE)
}

fn relative_plan_path(node_id: &NodeId) -> String {
    relative_node_file_path(node_id, PLAN_FILE)
}

fn relative_node_file_path(node_id: &NodeId, file_name: &str) -> String {
    let mut parts = vec![NODES_DIR.to_string()];
    parts.extend(node_id.segments().iter().map(ToString::to_string));
    parts.push(file_name.to_string());
    parts.join("/")
}

fn is_direct_child_of(node_id: &NodeId, parent_id: &NodeId) -> bool {
    node_id.segments().len() == parent_id.segments().len() + 1
        && node_id.segments().starts_with(parent_id.segments())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
