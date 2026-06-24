use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::super::LiveRootCompact;
use super::super::SpineHostEffects;
use super::super::SpinePreparedRootCompact;
#[cfg(test)]
use super::super::SpineRootCompactResult;
use super::super::SpineRuntime;
use super::super::SpineTreeUpdateDelivery;

pub(crate) struct PreparedSpineReplayRuntime {
    pub(crate) runtime: Option<SpineRuntime>,
    pub(crate) materialized: Option<Vec<ResponseItem>>,
    pub(crate) live_root_compacts: Vec<LiveRootCompact>,
}

pub(crate) struct SpineInitEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
}

pub(crate) struct SpineNativeCompactEvidence<'a> {
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) native_items: &'a [ResponseItem],
}

pub(crate) struct SpineCompactEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) close_provider_input_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineObservedContextItem<'a> {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineMessageEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
}

pub(crate) struct SpineSingleToolcallOutputRecordingPlan {
    pub(super) raw_len: u64,
    pub(super) prerecord_output_before_reduce: bool,
}

pub(crate) struct SpineGroupedToolcallOutputRecordingPlan {
    pub(super) raw_ordinals: Vec<Option<u64>>,
}

pub(crate) struct SpinePostApplyEffectPolicy {
    pub(super) delivery: SpineTreeUpdateDelivery,
}

pub(crate) struct CommittedSpineToolcall {
    pub(super) installed_commit: bool,
    pub(super) post_apply_effect_policy: SpinePostApplyEffectPolicy,
}

impl CommittedSpineToolcall {
    pub(super) fn installed_commit(&self) -> bool {
        self.installed_commit
    }

    pub(super) fn post_apply_host_effects(
        self,
        snapshot: Option<SpineTreeUpdateEvent>,
    ) -> SpineHostEffects {
        SpineHostEffects::from_optional_tree_update(
            snapshot,
            self.post_apply_effect_policy.delivery,
        )
    }
}

impl SpineSingleToolcallOutputRecordingPlan {
    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn prerecord_output_before_reduce(&self) -> bool {
        self.prerecord_output_before_reduce
    }
}

impl SpineGroupedToolcallOutputRecordingPlan {
    pub(crate) fn into_raw_ordinals(self) -> Vec<Option<u64>> {
        self.raw_ordinals
    }
}

#[derive(Debug)]
pub(crate) struct SpineRootCompactHostInstall {
    pub(super) prepared: SpinePreparedRootCompact,
}

impl SpineRootCompactHostInstall {
    pub(super) fn new(prepared: SpinePreparedRootCompact) -> Self {
        Self { prepared }
    }

    pub(crate) fn publication_history(&self) -> &[ResponseItem] {
        &self.prepared.result().materialized
    }

    #[cfg(test)]
    pub(crate) fn publication_history_len(&self) -> usize {
        self.publication_history().len()
    }

    pub(crate) fn validate_published_history_len(
        &self,
        published_history_len: usize,
    ) -> Result<(), super::super::SpineError> {
        self.prepared
            .validate_published_history_len(published_history_len)
    }

    #[cfg(test)]
    pub(crate) fn result(&self) -> SpineRootCompactResult {
        self.prepared.result().clone()
    }

    pub(super) fn into_prepared(self) -> SpinePreparedRootCompact {
        self.prepared
    }
}
