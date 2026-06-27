use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::super::runtime;

pub(crate) struct InitEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
}

pub(crate) struct CompactEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) close_provider_input_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct MessageEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
}

impl<'a> InitEvidence<'a> {
    pub(in crate::spine) fn into_runtime(self) -> runtime::SpineInitEvidence<'a> {
        runtime::SpineInitEvidence {
            rollout_path: self.rollout_path,
        }
    }
}

impl<'a> CompactEvidence<'a> {
    pub(in crate::spine) fn into_runtime(self) -> runtime::SpineCompactEvidence<'a> {
        runtime::SpineCompactEvidence {
            rollout_path: self.rollout_path,
            compacted_history: self.compacted_history,
            raw_items: self.raw_items,
            close_provider_input_tokens: self.close_provider_input_tokens,
        }
    }
}

impl<'a> MessageEvidence<'a> {
    pub(in crate::spine) fn into_runtime(self) -> runtime::SpineMessageEvidence<'a> {
        runtime::SpineMessageEvidence {
            rollout_path: self.rollout_path,
            raw_ordinal: self.raw_ordinal,
            context_index: self.context_index,
            item: self.item,
            raw_items: self.raw_items,
        }
    }
}
