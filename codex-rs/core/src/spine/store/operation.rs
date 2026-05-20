use crate::spine::state::SpineOperationName;
use crate::spine::state::SpineState;
use crate::spine::state::SpineStateError;
use crate::spine::state::Transition;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpineOperation {
    Open,
    Close,
    Archive,
}

pub(crate) trait TransitionSummaryArg {
    fn into_transition_summary(self) -> Option<String>;
}

impl TransitionSummaryArg for Option<String> {
    fn into_transition_summary(self) -> Option<String> {
        self
    }
}

impl TransitionSummaryArg for String {
    fn into_transition_summary(self) -> Option<String> {
        Some(self)
    }
}

impl TransitionSummaryArg for &str {
    fn into_transition_summary(self) -> Option<String> {
        Some(self.to_string())
    }
}

impl SpineOperation {
    pub(crate) fn apply(
        self,
        state: &mut SpineState,
        summary: Option<String>,
    ) -> Result<Transition, SpineStateError> {
        match self {
            SpineOperation::Open => {
                if summary.is_some() {
                    return Err(SpineStateError::UnexpectedSummary(SpineOperationName::Open));
                }
                state.open()
            }
            SpineOperation::Close => state
                .close(summary.ok_or(SpineStateError::MissingSummary(SpineOperationName::Close))?),
            SpineOperation::Archive => Err(SpineStateError::ArchiveIsInternal),
        }
    }
}
