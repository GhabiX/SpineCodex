use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::SpineCompletedToolCallHostOutcome;
use super::SpineError;
use super::SpineToolcallHostAttempt;
use super::SpineToolcallHostCommitAttempt;
use super::session_state::SpineToolcallHostCommit;
use super::support::validate_no_orphan_tool_outputs;

#[derive(Debug)]
pub(crate) struct SpineHistoryUpdate {
    pub(crate) call_id: String,
    pub(crate) operation: &'static str,
    pub(crate) suffix_start: usize,
    pub(crate) expected_history: Vec<ResponseItem>,
    pub(crate) replacement: Vec<ResponseItem>,
    pub(crate) reference_context_item: Option<TurnContextItem>,
}

pub(crate) struct SpineHostEffects {
    effects: Vec<SpineHostEffect>,
}

pub(crate) struct SpineTreeHostUpdates {
    immediate: Vec<SpineTreeUpdateEvent>,
    after_raw_output_durable: Vec<SpineTreeUpdateEvent>,
}

pub(crate) struct SpineRootCompactHostPublish {
    publication_history: Vec<ResponseItem>,
}

impl SpineHostEffects {
    pub(crate) fn none() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    fn one(effect: SpineHostEffect) -> Self {
        Self {
            effects: vec![effect],
        }
    }

    fn many(effects: Vec<SpineHostEffect>) -> Self {
        Self { effects }
    }

    pub(crate) fn replace_history(update: SpineHistoryUpdate) -> Self {
        Self::one(SpineHostEffect::ReplaceHistory(update))
    }

    pub(crate) fn tree_update(
        snapshot: SpineTreeUpdateEvent,
        delivery: SpineTreeUpdateDelivery,
    ) -> Self {
        Self::one(SpineHostEffect::TreeUpdate { snapshot, delivery })
    }

    pub(crate) fn from_optional_history_update(update: Option<SpineHistoryUpdate>) -> Self {
        update.map_or_else(Self::none, Self::replace_history)
    }

    pub(crate) fn from_optional_tree_update(
        snapshot: Option<SpineTreeUpdateEvent>,
        delivery: SpineTreeUpdateDelivery,
    ) -> Self {
        snapshot.map_or_else(Self::none, |snapshot| Self::tree_update(snapshot, delivery))
    }

    pub(crate) fn publish_materialized_history_after_batch() -> Self {
        Self::one(SpineHostEffect::PublishVariableHistoryAfterBatch)
    }

    pub(crate) fn root_compact_history_publication(publication_history: Vec<ResponseItem>) -> Self {
        Self::one(SpineHostEffect::RootCompactHistoryPublication(
            SpineRootCompactHostPublish {
                publication_history,
            },
        ))
    }

    pub(crate) fn toolcall_host_commit(host_commit: SpineToolcallHostCommit) -> Self {
        Self::one(SpineHostEffect::ToolcallHostCommit(host_commit))
    }

    pub(crate) fn extend(&mut self, effects: Self) {
        self.effects.extend(effects.effects);
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
        let (publish_requests, remaining): (Vec<_>, Vec<_>) =
            self.effects.into_iter().partition(|effect| {
                matches!(effect, SpineHostEffect::PublishVariableHistoryAfterBatch)
            });
        apply_effects(Self::many(remaining)).await?;
        if !publish_requests.is_empty() {
            publish_materialized_history().await?;
        }
        Ok(())
    }

    fn into_only_root_compact_host_publish(
        self,
    ) -> Result<Option<SpineRootCompactHostPublish>, String> {
        let (effects, publication) = self.take_unique_effect(
            "multiple Spine root compact history publications in one hook",
            |effect| match effect {
                SpineHostEffect::RootCompactHistoryPublication(next) => Ok(next),
                effect => Err(effect),
            },
        )?;
        if !effects.effects.is_empty() {
            return Err("compact hook returned unsupported host effects".to_string());
        }
        Ok(publication)
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
        let Some(host_publish) = self
            .into_only_root_compact_host_publish()
            .map_err(invariant_error)?
        else {
            publish_history(native_items, false).await?;
            return Ok(None);
        };
        let published_variable_history_len = host_publish.publication_history.len();
        let published_history =
            host_publish.published_history_from_native_items(&native_items, is_fixed_prefix_item);
        publish_history(published_history, true).await?;
        let spine_tree_snapshot = finalize_after_publish(published_variable_history_len).await?;
        after_installed().await?;
        Ok(spine_tree_snapshot)
    }

    fn into_toolcall_host_commit(self) -> Result<(Self, Option<SpineToolcallHostCommit>), String> {
        self.take_unique_effect(
            "multiple Spine toolcall host commits in one hook",
            |effect| match effect {
                SpineHostEffect::ToolcallHostCommit(next) => Ok(next),
                effect => Err(effect),
            },
        )
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
        mut attempt_once: AttemptOnce,
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
        let (effects, host_commit) = self
            .into_toolcall_host_commit()
            .map_err(SpineError::Invariant)?;
        if !effects.effects.is_empty() {
            return Err(SpineError::Invariant(
                "toolcall hook returned unsupported host effects".to_string(),
            ));
        }
        let Some(mut host_commit) = host_commit else {
            return Ok(None);
        };
        let post_commit_effects = host_commit
            .run_host_commit_loop(
                call_id,
                current_turn_provider_input_tokens,
                |attempt_input| attempt_once(attempt_input),
                yield_retry,
                fail_closed,
                abort_pending,
            )
            .await?;
        Ok(post_commit_effects.map(|effects| host_commit.host_outcome(effects)))
    }

    pub(crate) fn apply_history_updates_or_keep(
        self,
        mut apply_history_update: impl FnMut(
            SpineHostEffect,
        ) -> Result<Result<(), SpineHostEffect>, String>,
    ) -> Result<Self, String> {
        let mut remaining = Vec::new();
        for effect in self.effects {
            match apply_history_update(effect)? {
                Ok(()) => {}
                Err(effect) => remaining.push(effect),
            }
        }
        Ok(Self::many(remaining))
    }

    fn take_unique_effect<T>(
        self,
        duplicate_error: &'static str,
        mut take: impl FnMut(SpineHostEffect) -> Result<T, SpineHostEffect>,
    ) -> Result<(Self, Option<T>), String> {
        let mut remaining = Vec::new();
        let mut found = None;
        for effect in self.effects {
            match take(effect) {
                Ok(next) => {
                    if found.is_some() {
                        return Err(duplicate_error.to_string());
                    }
                    found = Some(next);
                }
                Err(effect) => remaining.push(effect),
            }
        }
        Ok((Self::many(remaining), found))
    }

    pub(crate) fn into_tree_host_updates(self) -> SpineTreeHostUpdates {
        let mut updates = SpineTreeHostUpdates {
            immediate: Vec::new(),
            after_raw_output_durable: Vec::new(),
        };
        for effect in self.effects {
            updates.push_effect(effect);
        }
        updates
    }
}

pub(crate) enum SpineHostEffect {
    ReplaceHistory(SpineHistoryUpdate),
    TreeUpdate {
        snapshot: SpineTreeUpdateEvent,
        delivery: SpineTreeUpdateDelivery,
    },
    PublishVariableHistoryAfterBatch,
    RootCompactHistoryPublication(SpineRootCompactHostPublish),
    ToolcallHostCommit(SpineToolcallHostCommit),
}

impl SpineHostEffect {
    pub(crate) fn apply_history_update_or_self(
        self,
        current_history: &[ResponseItem],
        replace_history_suffix: impl FnOnce(
            std::ops::Range<usize>,
            Vec<ResponseItem>,
            Option<TurnContextItem>,
        ) -> Result<(), String>,
    ) -> Result<Result<(), Self>, String> {
        let update = match self {
            Self::ReplaceHistory(update) => update,
            effect => return Ok(Err(effect)),
        };
        if current_history != update.expected_history.as_slice() {
            Err(format!(
                "{} history changed before suffix replacement for call_id={}",
                update.operation, update.call_id
            ))
        } else if update.suffix_start > current_history.len() {
            Err(format!(
                "{} suffix start {} exceeds history length {} for call_id={}",
                update.operation,
                update.suffix_start,
                current_history.len(),
                update.call_id
            ))
        } else {
            let mut candidate_history = current_history[..update.suffix_start].to_vec();
            candidate_history.extend_from_slice(&update.replacement);
            validate_no_orphan_tool_outputs(update.operation, &update.call_id, &candidate_history)
                .map_err(|err| err.to_string())?;
            replace_history_suffix(
                update.suffix_start..current_history.len(),
                update.replacement,
                update.reference_context_item,
            )
            .map_err(|err| {
                format!(
                    "{} suffix replacement failed for call_id={}: {err}",
                    update.operation, update.call_id
                )
            })?;
            Ok(Ok(()))
        }
    }
}

impl SpineRootCompactHostPublish {
    pub(crate) fn published_history_from_native_items(
        &self,
        native_items: &[ResponseItem],
        is_fixed_prefix_item: impl Fn(&ResponseItem) -> bool,
    ) -> Vec<ResponseItem> {
        let mut published = native_items
            .iter()
            .filter(|item| is_fixed_prefix_item(item))
            .cloned()
            .collect::<Vec<_>>();
        published.extend_from_slice(&self.publication_history);
        published
    }
}

pub(crate) enum SpineTreeUpdateDelivery {
    Immediate,
    AfterRawOutputDurable,
}

impl SpineTreeHostUpdates {
    fn push_effect(&mut self, effect: SpineHostEffect) {
        let SpineHostEffect::TreeUpdate { snapshot, delivery } = effect else {
            return;
        };
        match delivery {
            SpineTreeUpdateDelivery::Immediate => self.immediate.push(snapshot),
            SpineTreeUpdateDelivery::AfterRawOutputDurable => {
                self.after_raw_output_durable.push(snapshot);
            }
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<SpineTreeUpdateEvent>, Vec<SpineTreeUpdateEvent>) {
        (self.immediate, self.after_raw_output_durable)
    }
}
