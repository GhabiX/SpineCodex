use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;
use std::path::Path;

use super::runtime::SpineCompletedToolCallHostOutcome;
use super::runtime::SpineError;
use super::runtime::SpineSessionState;
use super::runtime::SpineToolcallHostAttempt;
use super::runtime::SpineToolcallHostCommitAttempt;

pub(crate) use super::runtime::SpineToolcallHookEvidence as ToolcallHookEvidence;

pub(crate) struct HostEffects {
    inner: super::runtime::SpineHostEffects,
}

pub(crate) struct TreeHostUpdates {
    inner: super::runtime::SpineTreeHostUpdates,
}

pub(crate) struct HistoryHostEffect {
    inner: super::runtime::SpineHostEffect,
}

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

pub(crate) struct ToolCallEvidence<'a> {
    kind: ToolCallEvidenceKind<'a>,
    force_ordinary: bool,
}

enum ToolCallEvidenceKind<'a> {
    Single {
        item: &'a ResponseItem,
    },
    Grouped {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    },
    #[cfg(test)]
    Runtime(super::runtime::SpineToolCallEvidence<'a>),
}

impl<'a> ToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Single { item },
            force_ordinary: false,
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, false)
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, true)
    }

    fn grouped_with_policy(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        force_ordinary: bool,
    ) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            },
            force_ordinary,
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<super::runtime::SpineCompletedToolCallOutputEvidence<'a>>, SpineError> {
        let runtime_evidence = match &self.kind {
            ToolCallEvidenceKind::Single { item } => {
                super::runtime::SpineToolCallEvidence::single(item)
            }
            ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } if self.force_ordinary => super::runtime::SpineToolCallEvidence::grouped_as_ordinary(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
            ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } => super::runtime::SpineToolCallEvidence::grouped(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(evidence) => return evidence.completed_output(),
        };
        runtime_evidence.completed_output()
    }
}

impl HostEffects {
    pub(crate) fn none() -> Self {
        Self::from_runtime(super::runtime::SpineHostEffects::none())
    }

    pub(crate) fn from_runtime(inner: super::runtime::SpineHostEffects) -> Self {
        Self { inner }
    }

    pub(crate) fn extend(&mut self, effects: Self) {
        self.inner.extend(effects.inner);
    }

    pub(crate) async fn apply_after_batch_materialized_history_request<
        E,
        ApplyEffects,
        ApplyEffectsFuture,
        PublishMaterializedHistory,
        PublishMaterializedHistoryFuture,
    >(
        self,
        apply_effects: ApplyEffects,
        publish_materialized_history: PublishMaterializedHistory,
    ) -> Result<(), E>
    where
        ApplyEffects: FnOnce(Self) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Result<(), E>>,
        PublishMaterializedHistory: FnOnce() -> PublishMaterializedHistoryFuture,
        PublishMaterializedHistoryFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_after_batch_materialized_history_request(
                |effects| apply_effects(Self::from_runtime(effects)),
                publish_materialized_history,
            )
            .await
    }

    pub(crate) async fn apply_toolcall_host_commit<
        AttemptOnce,
        AttemptOnceFuture,
        YieldRetry,
        YieldRetryFuture,
        FailClosed,
        FailClosedFuture,
        AbortPending,
        AbortPendingFuture,
    >(
        self,
        call_id: &str,
        current_turn_provider_input_tokens: Option<i64>,
        attempt_once: AttemptOnce,
        yield_retry: YieldRetry,
        fail_closed: FailClosed,
        abort_pending: AbortPending,
    ) -> Result<Option<SpineCompletedToolCallHostOutcome>, SpineError>
    where
        AttemptOnce: FnMut(SpineToolcallHostCommitAttempt) -> AttemptOnceFuture,
        AttemptOnceFuture: Future<Output = Result<SpineToolcallHostAttempt, SpineError>>,
        YieldRetry: FnMut() -> YieldRetryFuture,
        YieldRetryFuture: Future<Output = ()>,
        FailClosed: FnMut(&'static str) -> FailClosedFuture,
        FailClosedFuture: Future<Output = ()>,
        AbortPending: FnMut(&'static str) -> AbortPendingFuture,
        AbortPendingFuture: Future<Output = ()>,
    {
        self.inner
            .apply_toolcall_host_commit(
                call_id,
                current_turn_provider_input_tokens,
                attempt_once,
                yield_retry,
                fail_closed,
                abort_pending,
            )
            .await
    }

    pub(crate) fn apply_history_updates_or_keep(
        self,
        mut apply_history_update: impl FnMut(
            HistoryHostEffect,
        ) -> Result<Result<(), HistoryHostEffect>, String>,
    ) -> Result<Self, String> {
        self.inner
            .apply_history_updates_or_keep(|effect| {
                apply_history_update(HistoryHostEffect { inner: effect })
                    .map(|result| result.map_err(|effect| effect.inner))
            })
            .map(Self::from_runtime)
    }

    pub(crate) fn into_tree_host_updates(self) -> TreeHostUpdates {
        TreeHostUpdates {
            inner: self.inner.into_tree_host_updates(),
        }
    }

    pub(crate) async fn apply_root_compact_history_publication<
        E,
        PublishHistory,
        PublishHistoryFuture,
        FinalizeAfterPublish,
        FinalizeAfterPublishFuture,
        AfterInstalled,
        AfterInstalledFuture,
    >(
        self,
        native_items: Vec<ResponseItem>,
        is_fixed_prefix_item: impl Fn(&ResponseItem) -> bool,
        invariant_error: impl Fn(String) -> E,
        publish_history: PublishHistory,
        finalize_after_publish: FinalizeAfterPublish,
        after_installed: AfterInstalled,
    ) -> Result<Option<SpineTreeUpdateEvent>, E>
    where
        PublishHistory: FnOnce(Vec<ResponseItem>, bool) -> PublishHistoryFuture,
        PublishHistoryFuture: Future<Output = Result<(), E>>,
        FinalizeAfterPublish: FnOnce(usize) -> FinalizeAfterPublishFuture,
        FinalizeAfterPublishFuture: Future<Output = Result<Option<SpineTreeUpdateEvent>, E>>,
        AfterInstalled: FnOnce() -> AfterInstalledFuture,
        AfterInstalledFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_root_compact_history_publication(
                native_items,
                is_fixed_prefix_item,
                invariant_error,
                publish_history,
                finalize_after_publish,
                after_installed,
            )
            .await
    }
}

impl TreeHostUpdates {
    pub(crate) fn into_parts(self) -> (Vec<SpineTreeUpdateEvent>, Vec<SpineTreeUpdateEvent>) {
        self.inner.into_parts()
    }
}

impl HistoryHostEffect {
    pub(crate) fn apply_history_update_or_self(
        self,
        current_history: &[ResponseItem],
        replace_history_suffix: impl FnOnce(
            std::ops::Range<usize>,
            Vec<ResponseItem>,
            Option<TurnContextItem>,
        ) -> Result<(), String>,
    ) -> Result<Result<(), Self>, String> {
        self.inner
            .apply_history_update_or_self(current_history, replace_history_suffix)
            .map(|result| result.map_err(|inner| Self { inner }))
    }
}

#[cfg(test)]
impl<'a> From<super::runtime::SpineToolCallEvidence<'a>> for ToolCallEvidence<'a> {
    fn from(evidence: super::runtime::SpineToolCallEvidence<'a>) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Runtime(evidence),
            force_ordinary: false,
        }
    }
}

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .on_init(super::runtime::SpineInitEvidence {
            rollout_path: evidence.rollout_path,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .observe_non_toolcall_msg_with_host_effects(super::runtime::SpineMessageEvidence {
            rollout_path: evidence.rollout_path,
            raw_ordinal: evidence.raw_ordinal,
            context_index: evidence.context_index,
            item: evidence.item,
            raw_items: evidence.raw_items,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_native_root_compact_from_history_with_checkpoint(
            super::runtime::SpineCompactEvidence {
                rollout_path: evidence.rollout_path,
                compacted_history: evidence.compacted_history,
                raw_items: evidence.raw_items,
                close_provider_input_tokens: evidence.close_provider_input_tokens,
            },
        )
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_completed_toolcall_for_commit(evidence)
        .map(HostEffects::from_runtime)
}
