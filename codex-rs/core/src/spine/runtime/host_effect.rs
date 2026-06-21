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

impl SpineHostEffects {
    pub(crate) fn none() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    pub(crate) fn replace_history(update: SpineHistoryUpdate) -> Self {
        Self {
            effects: vec![SpineHostEffect::ReplaceHistory(update)],
        }
    }

    pub(crate) fn tree_update(
        snapshot: SpineTreeUpdateEvent,
        delivery: SpineTreeUpdateDelivery,
    ) -> Self {
        Self {
            effects: vec![SpineHostEffect::TreeUpdate { snapshot, delivery }],
        }
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

    pub(crate) fn into_effects(self) -> Vec<SpineHostEffect> {
        self.effects
    }
}

pub(crate) enum SpineHostEffect {
    ReplaceHistory(SpineHistoryUpdate),
    TreeUpdate {
        snapshot: SpineTreeUpdateEvent,
        delivery: SpineTreeUpdateDelivery,
    },
}

pub(crate) enum SpineTreeUpdateDelivery {
    Immediate,
    AfterRawOutputDurable,
}
