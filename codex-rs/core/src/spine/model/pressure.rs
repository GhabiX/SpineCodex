use super::token::ContextBaselineSource;
use super::token::NodeId;
use crate::spine::io::hash_raw_live;
use serde::Deserialize;
use serde::Serialize;

impl LoggedPressureEvent {
    pub(in crate::spine) fn allowed_by(&self, raw_live: &[bool]) -> bool {
        let PressureEvent::OpenContextBaseline {
            observed_raw_ordinal,
            observed_raw_live_hash,
            ..
        } = &self.event;
        usize::try_from(*observed_raw_ordinal)
            .ok()
            .and_then(|end| raw_live.get(..end))
            .is_some_and(|raw_live_prefix| {
                observed_raw_live_hash
                    .as_deref()
                    .is_none_or(|hash| hash_raw_live(raw_live_prefix) == hash)
            })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(in crate::spine) enum PressureEvent {
    OpenContextBaseline {
        node: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_structural_seq: Option<u64>,
        observed_structural_seq: u64,
        observed_raw_ordinal: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_raw_live_hash: Option<String>,
        observed_context_index: usize,
        context_tokens: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<i64>,
        source: ContextBaselineSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_live_suffix_tokens: Option<i64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::spine) struct LoggedPressureEvent {
    pub(in crate::spine) pressure_seq: u64,
    #[serde(flatten)]
    pub(in crate::spine) event: PressureEvent,
}
