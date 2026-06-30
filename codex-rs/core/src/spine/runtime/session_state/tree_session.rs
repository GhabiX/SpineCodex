use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;

use super::super::SpineError;
use super::super::SpineOpenNodeContextProjection;
use super::SpineSessionState;
use crate::spine::model::NodeId;

impl SpineSessionState {
    pub(crate) fn take_initial_tree_snapshot(
        &mut self,
    ) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
        self.ensure_valid()?;
        if self.initial_tree_snapshot_emitted {
            return Ok(None);
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        if !runtime.jit_enabled() {
            return Ok(None);
        }
        let snapshot = runtime.build_tree_snapshot()?;
        self.initial_tree_snapshot_emitted = true;
        Ok(Some(snapshot))
    }

    pub(crate) fn tree_snapshot_projection(
        &self,
    ) -> Result<Option<(SpineTreeUpdateEvent, Vec<SpineOpenNodeContextProjection>)>, SpineError>
    {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some((
            runtime.build_tree_snapshot()?,
            runtime.open_node_context_projections(),
        )))
    }

    pub(crate) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<Option<String>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        runtime
            .render_tree_with_context_annotations(annotations)
            .map(Some)
    }
}
