use codex_protocol::models::ResponseItem;

use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) struct RawObservationRuntime;

impl RawObservationRuntime {
    pub(crate) fn observe_raw_items(
        state: &mut SpineSessionState,
        count: usize,
    ) -> Result<(), SpineError> {
        state.observe_raw_items(count)
    }

    pub(crate) fn ensure_observable_context(state: &SpineSessionState) -> Result<(), SpineError> {
        state.ensure_observable_context()
    }

    pub(crate) fn observe_context_item(
        state: &mut SpineSessionState,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        state.observe_context_item(raw_ordinal, context_index, item)
    }
}
