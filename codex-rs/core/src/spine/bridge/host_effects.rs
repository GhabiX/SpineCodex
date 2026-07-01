use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::super::hooks::HostEffects;
use super::super::runtime::SpineSessionState;
use crate::spine::model::TrimBodyUpdate;

struct RootCompactVariableContextPublication {
    published_items: Vec<ResponseItem>,
    replacement_history: Option<Vec<ResponseItem>>,
}

impl RootCompactVariableContextPublication {
    fn native_only(published_items: Vec<ResponseItem>) -> Self {
        Self {
            published_items,
            replacement_history: None,
        }
    }

    fn spine_installed(published_items: Vec<ResponseItem>) -> Self {
        Self {
            replacement_history: Some(published_items.clone()),
            published_items,
        }
    }

    fn into_compacted_rollout_item(
        self,
        mut compacted_item: CompactedItem,
    ) -> (Vec<ResponseItem>, CompactedItem) {
        if let Some(replacement_history) = self.replacement_history {
            compacted_item.replacement_history = Some(replacement_history);
        }
        (self.published_items, compacted_item)
    }
}

impl HostEffects {
    pub(crate) async fn apply_history_publication<
        E,
        PublishHistory,
        PublishHistoryFuture,
        FinalizeInstallFailure,
        FinalizeInstallFailureFuture,
        AfterInstalled,
        AfterInstalledFuture,
    >(
        self,
        state: Option<&tokio::sync::Mutex<SpineSessionState>>,
        native_items: Vec<ResponseItem>,
        is_fixed_prefix_item: impl Fn(&ResponseItem) -> bool,
        invariant_error: impl Fn(String) -> E,
        compacted_item: CompactedItem,
        publish_history: PublishHistory,
        finalize_install_failure: FinalizeInstallFailure,
        after_installed: AfterInstalled,
    ) -> Result<Option<SpineTreeUpdateEvent>, E>
    where
        PublishHistory: FnOnce(Vec<ResponseItem>, CompactedItem) -> PublishHistoryFuture,
        PublishHistoryFuture: Future<Output = Result<(), E>>,
        FinalizeInstallFailure: FnOnce(String) -> FinalizeInstallFailureFuture,
        FinalizeInstallFailureFuture: Future<Output = E>,
        AfterInstalled: FnOnce() -> AfterInstalledFuture,
        AfterInstalledFuture: Future<Output = Result<(), E>>,
    {
        self.apply_root_compact_variable_context_publication(
            state,
            native_items,
            is_fixed_prefix_item,
            invariant_error,
            |publication| {
                let (published_items, compacted_item) =
                    publication.into_compacted_rollout_item(compacted_item);
                publish_history(published_items, compacted_item)
            },
            finalize_install_failure,
            after_installed,
        )
        .await
    }

    pub(crate) async fn apply_after_batch_variable_context_request<
        E,
        ApplyEffects,
        ApplyEffectsFuture,
        PublishVariableContext,
        PublishVariableContextFuture,
    >(
        self,
        apply_effects: ApplyEffects,
        publish_variable_context: PublishVariableContext,
    ) -> Result<(), E>
    where
        ApplyEffects: FnOnce(Self) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Result<(), E>>,
        PublishVariableContext: FnOnce() -> PublishVariableContextFuture,
        PublishVariableContextFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_after_batch_variable_context_request(
                |effects| apply_effects(Self::from_runtime(effects)),
                publish_variable_context,
            )
            .await
    }

    pub(crate) async fn apply_after_batch_variable_context_request_from_state<
        E,
        ApplyEffects,
        ApplyEffectsFuture,
        CurrentHistory,
        CurrentHistoryFuture,
        ApplyPublishedEffects,
        ApplyPublishedEffectsFuture,
    >(
        self,
        state: Option<&tokio::sync::Mutex<SpineSessionState>>,
        raw_items: &[Option<ResponseItem>],
        invariant_error: impl Fn(String) -> E,
        apply_effects: ApplyEffects,
        current_history: CurrentHistory,
        apply_published_effects: ApplyPublishedEffects,
    ) -> Result<(), E>
    where
        ApplyEffects: FnOnce(Self) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Result<(), E>>,
        CurrentHistory: FnOnce() -> CurrentHistoryFuture,
        CurrentHistoryFuture: Future<Output = (Vec<ResponseItem>, Option<TurnContextItem>)>,
        ApplyPublishedEffects: FnOnce(Self) -> ApplyPublishedEffectsFuture,
        ApplyPublishedEffectsFuture: Future<Output = Result<(), E>>,
    {
        self.apply_after_batch_variable_context_request(apply_effects, || async move {
            let effects = match state {
                Some(state) => {
                    let (expected_history, reference_context_item) = current_history().await;
                    let guard = state.lock().await;
                    guard
                        .variable_context_host_effects_if_no_pending_tool_request(
                            raw_items,
                            expected_history,
                            reference_context_item,
                        )
                        .map(Self::from_runtime)
                        .map_err(|err| invariant_error(err.to_string()))?
                }
                None => Self::none(),
            };
            apply_published_effects(effects).await
        })
        .await
    }

    pub(crate) fn apply_history_updates_or_keep(
        self,
        current_history: &[ResponseItem],
        mut replace_history_suffix: impl FnMut(
            std::ops::Range<usize>,
            Vec<ResponseItem>,
            Option<TurnContextItem>,
        ) -> Result<(), String>,
    ) -> Result<Self, String> {
        self.inner
            .apply_history_updates_or_keep(|effect| {
                effect.apply_history_update_or_self(
                    current_history,
                    |range, replacement, reference| {
                        replace_history_suffix(range, replacement, reference)
                    },
                )
            })
            .map(Self::from_runtime)
    }

    pub(crate) fn into_tree_host_updates(
        self,
    ) -> (Vec<SpineTreeUpdateEvent>, Vec<SpineTreeUpdateEvent>) {
        self.inner.into_tree_host_updates().into_parts()
    }

    pub(crate) fn apply_trim_body_updates_or_keep(
        self,
        apply_updates: impl FnMut(Vec<TrimBodyUpdate>) -> Result<(), String>,
    ) -> Result<Self, String> {
        self.inner
            .apply_trim_body_updates_or_keep(apply_updates)
            .map(Self::from_runtime)
    }

    async fn apply_root_compact_variable_context_publication<
        E,
        PublishHistory,
        PublishHistoryFuture,
        FinalizeInstallFailure,
        FinalizeInstallFailureFuture,
        AfterInstalled,
        AfterInstalledFuture,
    >(
        self,
        state: Option<&tokio::sync::Mutex<SpineSessionState>>,
        native_items: Vec<ResponseItem>,
        is_fixed_prefix_item: impl Fn(&ResponseItem) -> bool,
        invariant_error: impl Fn(String) -> E,
        publish_history: PublishHistory,
        finalize_install_failure: FinalizeInstallFailure,
        after_installed: AfterInstalled,
    ) -> Result<Option<SpineTreeUpdateEvent>, E>
    where
        PublishHistory: FnOnce(RootCompactVariableContextPublication) -> PublishHistoryFuture,
        PublishHistoryFuture: Future<Output = Result<(), E>>,
        FinalizeInstallFailure: FnOnce(String) -> FinalizeInstallFailureFuture,
        FinalizeInstallFailureFuture: Future<Output = E>,
        AfterInstalled: FnOnce() -> AfterInstalledFuture,
        AfterInstalledFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_root_compact_variable_context_publication(
                native_items,
                is_fixed_prefix_item,
                invariant_error,
                |published_items, installed_spine_root_compact| {
                    let publication = if installed_spine_root_compact {
                        RootCompactVariableContextPublication::spine_installed(published_items)
                    } else {
                        RootCompactVariableContextPublication::native_only(published_items)
                    };
                    publish_history(publication)
                },
                |published_variable_context_len| async move {
                    let install_result = match state {
                        Some(state) => {
                            let mut guard = state.lock().await;
                            guard
                                .take_pending_root_compact_after_history_publish(
                                    published_variable_context_len,
                                )
                                .map(Some)
                                .map_err(|err| err.to_string())
                        }
                        None => {
                            Err("spine runtime missing before root compact PS install".to_string())
                        }
                    };
                    match install_result {
                        Ok(snapshot) => Ok(snapshot),
                        Err(reason) => Err(finalize_install_failure(reason).await),
                    }
                },
                after_installed,
            )
            .await
    }
}
