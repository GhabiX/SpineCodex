use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineNodeContextProblem;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
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

    pub(crate) fn into_annotated_snapshot(
        self,
        current_provider_input_tokens: Option<i64>,
    ) -> SpineTreeUpdateEvent {
        let (mut snapshot, open_nodes) = self.into_parts();
        let open_nodes_by_id = open_nodes
            .iter()
            .map(|node| (node.node_id.to_string(), node))
            .collect::<BTreeMap<_, _>>();
        for node in &mut snapshot.nodes {
            let Some(open_node) = open_nodes_by_id.get(node.node_id.as_str()) else {
                continue;
            };
            let accounting = node
                .accounting
                .get_or_insert_with(SpineTreeNodeAccountingSnapshot::default);
            let (current_node_context_tokens, problem) =
                open_node.context_state(current_provider_input_tokens);
            accounting.current_node_context_tokens = current_node_context_tokens;
            accounting.current_node_context_baseline_source = open_node.baseline_source;
            accounting.current_node_context_problem = problem;
        }
        snapshot
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

    pub(crate) fn context_state(
        &self,
        current_provider_input_tokens: Option<i64>,
    ) -> (Option<i64>, Option<SpineNodeContextProblem>) {
        if let Some(problem) = self.problem {
            return (None, Some(problem));
        }
        match current_provider_input_tokens
            .zip(self.provider_input_tokens)
            .map(|(current, open_context_tokens)| current - open_context_tokens)
        {
            Some(tokens) if tokens >= 0 => (Some(tokens), None),
            Some(_) => (None, Some(SpineNodeContextProblem::CoordinateMismatch)),
            None if current_provider_input_tokens.is_none() => {
                (None, Some(SpineNodeContextProblem::MissingCurrentUsage))
            }
            None => (
                None,
                Some(SpineNodeContextProblem::MissingOpenContextBaseline),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_node(provider_input_tokens: Option<i64>) -> OpenNodeContextProjection {
        OpenNodeContextProjection {
            node_id: NodeId::root_epoch(1),
            provider_input_tokens,
            baseline_source: None,
            problem: None,
        }
    }

    #[test]
    fn context_state_reports_context_delta() {
        assert_eq!(open_node(Some(10)).context_state(Some(15)), (Some(5), None));
    }

    #[test]
    fn context_state_reports_missing_inputs() {
        assert_eq!(
            open_node(Some(10)).context_state(None),
            (None, Some(SpineNodeContextProblem::MissingCurrentUsage))
        );
        assert_eq!(
            open_node(None).context_state(Some(15)),
            (
                None,
                Some(SpineNodeContextProblem::MissingOpenContextBaseline)
            )
        );
    }

    #[test]
    fn context_state_reports_coordinate_mismatch() {
        assert_eq!(
            open_node(Some(20)).context_state(Some(15)),
            (None, Some(SpineNodeContextProblem::CoordinateMismatch))
        );
    }
}
