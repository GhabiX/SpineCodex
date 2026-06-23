use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;

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

pub(crate) struct SpineRootCompactHistoryPublication {
    materialized: Vec<ResponseItem>,
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
        Self::one(SpineHostEffect::PublishMaterializedHistoryAfterBatch)
    }

    pub(crate) fn root_compact_history_publication(materialized: Vec<ResponseItem>) -> Self {
        Self::one(SpineHostEffect::RootCompactHistoryPublication(
            SpineRootCompactHistoryPublication::new(materialized),
        ))
    }

    pub(crate) fn extend(&mut self, effects: Self) {
        self.effects.extend(effects.effects);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub(crate) fn into_after_batch_materialized_history_request(self) -> (Self, bool) {
        let mut remaining = Vec::new();
        let mut requested = false;
        for effect in self.effects {
            if matches!(
                effect,
                SpineHostEffect::PublishMaterializedHistoryAfterBatch
            ) {
                requested = true;
            } else {
                remaining.push(effect);
            }
        }
        (Self::many(remaining), requested)
    }

    pub(crate) fn into_root_compact_history_publication(
        self,
    ) -> Result<(Self, Option<SpineRootCompactHistoryPublication>), String> {
        let mut remaining = Vec::new();
        let mut publication = None;
        for effect in self.effects {
            match effect {
                SpineHostEffect::RootCompactHistoryPublication(next) => {
                    if publication.is_some() {
                        return Err(
                            "multiple Spine root compact history publications in one hook"
                                .to_string(),
                        );
                    }
                    publication = Some(next);
                }
                effect => remaining.push(effect),
            }
        }
        Ok((Self::many(remaining), publication))
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

    pub(crate) fn into_tree_host_updates(self) -> SpineTreeHostUpdates {
        let mut updates = SpineTreeHostUpdates::new();
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
    PublishMaterializedHistoryAfterBatch,
    RootCompactHistoryPublication(SpineRootCompactHistoryPublication),
}

impl SpineHostEffect {
    fn into_history_update_or_self(self) -> Result<SpineHistoryUpdate, Self> {
        match self {
            Self::ReplaceHistory(update) => Ok(update),
            effect => Err(effect),
        }
    }

    pub(crate) fn apply_history_update_or_self(
        self,
        current_history: &[ResponseItem],
        replace_history_suffix: impl FnOnce(
            std::ops::Range<usize>,
            Vec<ResponseItem>,
            Option<TurnContextItem>,
        ) -> Result<(), String>,
    ) -> Result<Result<(), Self>, String> {
        let update = match self.into_history_update_or_self() {
            Ok(update) => update,
            Err(effect) => return Ok(Err(effect)),
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

    pub(crate) fn into_tree_update(
        self,
    ) -> Option<(SpineTreeUpdateEvent, SpineTreeUpdateDelivery)> {
        match self {
            Self::ReplaceHistory(_) => None,
            Self::TreeUpdate { snapshot, delivery } => Some((snapshot, delivery)),
            Self::PublishMaterializedHistoryAfterBatch => None,
            Self::RootCompactHistoryPublication(_) => None,
        }
    }
}

impl SpineRootCompactHistoryPublication {
    fn new(materialized: Vec<ResponseItem>) -> Self {
        Self { materialized }
    }

    pub(crate) fn materialized_len(&self) -> usize {
        self.materialized.len()
    }

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
        published.extend_from_slice(&self.materialized);
        published
    }
}

pub(crate) enum SpineTreeUpdateDelivery {
    Immediate,
    AfterRawOutputDurable,
}

impl SpineTreeHostUpdates {
    fn new() -> Self {
        Self {
            immediate: Vec::new(),
            after_raw_output_durable: Vec::new(),
        }
    }

    fn push_effect(&mut self, effect: SpineHostEffect) {
        let Some((snapshot, delivery)) = effect.into_tree_update() else {
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
