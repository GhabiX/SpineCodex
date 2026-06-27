use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::super::LiveRootCompact;
use super::super::SpineHostEffects;
#[cfg(test)]
use super::super::SpineRootCompactResult;
use super::super::SpineRuntime;
use super::super::SpineTreeUpdateDelivery;
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

pub(crate) enum SpineToolcallOutputRecordingRequest<'a> {
    Single {
        call_id: &'a str,
        raw_items: &'a [Option<ResponseItem>],
    },
    Grouped {
        output_items: &'a [ResponseItem],
    },
}

pub(crate) enum SpineToolcallOutputRecordingPlan {
    Single(Option<SpineSingleToolcallOutputRecordingPlan>),
    Grouped(SpineGroupedToolcallOutputRecordingPlan),
}

pub(crate) struct SpinePostApplyEffectPolicy {
    pub(super) delivery: SpineTreeUpdateDelivery,
}

pub(crate) struct CommittedSpineToolcall {
    pub(super) installed_commit: bool,
    pub(super) post_apply_effect_policy: SpinePostApplyEffectPolicy,
    pub(super) trim_body_updates: Vec<TrimBodyUpdate>,
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
        .combine(SpineHostEffects::trim_body_updates(self.trim_body_updates))
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

    pub(crate) fn live_root_compacts(&self) -> &[LiveRootCompact] {
        &self.live_root_compacts
    }

    pub(crate) fn into_variable_context(self) -> Option<Vec<ResponseItem>> {
        self.variable_context
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

    pub(crate) fn variable_context(&self) -> &[ResponseItem] {
        self.prepared.variable_context()
    }

    #[cfg(test)]
    pub(crate) fn variable_context_len(&self) -> usize {
        self.variable_context().len()
    }

    pub(crate) fn validate_published_variable_context_len(
        &self,
        published_variable_context_len: usize,
    ) -> Result<(), super::super::SpineError> {
        self.prepared
            .validate_published_variable_context_len(published_variable_context_len)
    }

    #[cfg(test)]
    pub(crate) fn variable_context_publication_for_test(&self) -> SpineRootCompactResult {
        self.prepared.clone_variable_context_publication_for_test()
    }

    pub(super) fn into_prepared(self) -> SpinePreparedRootCompact {
        self.prepared
    }
}
