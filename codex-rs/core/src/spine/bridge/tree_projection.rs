use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineNodeContextProblem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;

use super::super::NodeId;
use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) struct TreeSnapshotProjection {
    snapshot: SpineTreeUpdateEvent,
    open_nodes: Vec<OpenNodeContextProjection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenNodeContextProjection {
    pub(crate) node_id: NodeId,
    pub(crate) provider_input_tokens: Option<i64>,
    pub(crate) baseline_source: Option<SpineNodeContextBaselineSource>,
    pub(crate) problem: Option<SpineNodeContextProblem>,
}

impl TreeSnapshotProjection {
    pub(crate) fn take_initial_snapshot(
        state: &mut SpineSessionState,
    ) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
        state.take_initial_tree_snapshot()
    }

    pub(crate) fn from_state(
        state: &SpineSessionState,
    ) -> Result<Option<TreeSnapshotProjection>, SpineError> {
        state
            .tree_snapshot_projection()
            .map(|projection| projection.map(TreeSnapshotProjection::from_runtime))
    }

    pub(crate) fn render_tree_with_context_annotations(
        state: &SpineSessionState,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<Option<String>, SpineError> {
        state.render_tree_with_context_annotations(annotations)
    }

    pub(super) fn from_runtime(
        (snapshot, open_nodes): (
            SpineTreeUpdateEvent,
            Vec<runtime::SpineOpenNodeContextProjection>,
        ),
    ) -> Self {
        Self {
            snapshot,
            open_nodes: open_nodes
                .into_iter()
                .map(OpenNodeContextProjection::from_runtime)
                .collect(),
        }
    }

    pub(crate) fn snapshot(&self) -> &SpineTreeUpdateEvent {
        &self.snapshot
    }

    pub(crate) fn open_nodes(&self) -> &[OpenNodeContextProjection] {
        &self.open_nodes
    }

    pub(crate) fn into_parts(self) -> (SpineTreeUpdateEvent, Vec<OpenNodeContextProjection>) {
        (self.snapshot, self.open_nodes)
    }
}

impl OpenNodeContextProjection {
    fn from_runtime(inner: runtime::SpineOpenNodeContextProjection) -> Self {
        Self {
            node_id: inner.node_id,
            provider_input_tokens: inner.provider_input_tokens,
            baseline_source: inner.baseline_source,
            problem: inner.problem,
        }
    }
}
