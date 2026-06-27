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
}
