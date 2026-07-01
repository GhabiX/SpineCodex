use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::super::LiveRootCompact;
#[cfg(test)]
use super::super::SpineRootCompactResult;
use super::super::SpineRuntime;
use super::super::prepared::SpinePreparedRootCompact;
use crate::spine::model::TrimBodyUpdate;

pub(crate) struct PreparedSpineReplayRuntime {
    pub(super) raw_len: u64,
    pub(super) runtime: Option<SpineRuntime>,
    pub(super) variable_context: Option<Vec<ResponseItem>>,
    pub(super) live_root_compacts: Vec<LiveRootCompact>,
}

pub(crate) struct SpineInitEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
}

pub(crate) struct SpineCompactEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) close_provider_input_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineMessageEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
}

pub(in crate::spine) struct SpineSingleToolcallOutputRecordingPlan {
    pub(in crate::spine) raw_len: u64,
    pub(in crate::spine) prerecord_output_before_reduce: bool,
}

pub(in crate::spine) struct SpineGroupedToolcallOutputRecordingPlan {
    pub(in crate::spine) raw_ordinals: Vec<Option<u64>>,
}

pub(super) struct CommittedSpineToolcall {
    pub(super) installed_commit: bool,
    pub(super) delivery: super::super::SpineTreeUpdateDelivery,
    pub(super) trim_body_updates: Vec<TrimBodyUpdate>,
}

impl PreparedSpineReplayRuntime {
    pub(super) fn new(
        raw_len: u64,
        runtime: Option<SpineRuntime>,
        variable_context: Option<Vec<ResponseItem>>,
        live_root_compacts: Vec<LiveRootCompact>,
    ) -> Self {
        Self {
            raw_len,
            runtime,
            variable_context,
            live_root_compacts,
        }
    }

    pub(crate) fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }

    pub(crate) fn into_variable_context(self) -> Option<Vec<ResponseItem>> {
        self.variable_context
    }

    pub(in crate::spine) fn live_root_compacts(&self) -> &[LiveRootCompact] {
        &self.live_root_compacts
    }
}

#[derive(Debug)]
pub(crate) struct SpineRootCompactHostInstall {
    pub(super) prepared: SpinePreparedRootCompact,
}

impl SpineRootCompactHostInstall {
    #[cfg(test)]
    pub(crate) fn variable_context_publication_for_test(&self) -> SpineRootCompactResult {
        self.prepared.clone_variable_context_publication_for_test()
    }
}
