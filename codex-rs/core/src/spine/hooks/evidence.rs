use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::super::runtime;

pub(crate) struct InitEvidence<'a> {
    rollout_path: &'a Path,
}

pub(crate) struct CompactEvidence<'a> {
    rollout_path: &'a Path,
    compacted_history: &'a [ResponseItem],
    raw_items: &'a [Option<ResponseItem>],
    close_provider_input_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct MessageEvidence<'a> {
    rollout_path: &'a Path,
    raw_ordinal: u64,
    context_index: usize,
    item: &'a ResponseItem,
    raw_items: &'a [Option<ResponseItem>],
}

impl<'a> InitEvidence<'a> {
    pub(in crate::spine) fn new(rollout_path: &'a Path) -> Self {
        Self { rollout_path }
    }

    pub(in crate::spine) fn into_runtime(self) -> runtime::SpineInitEvidence<'a> {
        runtime::SpineInitEvidence {
            rollout_path: self.rollout_path,
        }
    }
}

impl<'a> CompactEvidence<'a> {
    pub(in crate::spine) fn new(
        rollout_path: &'a Path,
        compacted_history: &'a [ResponseItem],
        raw_items: &'a [Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Self {
        Self {
            rollout_path,
            compacted_history,
            raw_items,
            close_provider_input_tokens,
        }
    }

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
    pub(in crate::spine) fn new(
        rollout_path: &'a Path,
        raw_ordinal: u64,
        context_index: usize,
        item: &'a ResponseItem,
        raw_items: &'a [Option<ResponseItem>],
    ) -> Self {
        Self {
            rollout_path,
            raw_ordinal,
            context_index,
            item,
            raw_items,
        }
    }

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
