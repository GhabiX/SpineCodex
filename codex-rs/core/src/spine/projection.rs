use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_CLOSE;
use super::SPINE_TOOL_NEXT;
use super::SPINE_TOOL_OPEN;
use super::ids::NodeId;
use super::state::SpineState;
use super::state::SpineStateError;
use super::store::SpineOperation;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use std::collections::HashSet;
use thiserror::Error;

const ROOT_EPOCH_COMPACT_MESSAGE_PREFIX: &str = "Spine compacted root epoch ";

#[derive(Debug)]
pub(crate) struct SpineProjection {
    pub(crate) state: SpineState,
    pub(crate) response_item_count: u64,
    pub(crate) surviving_turn_ids: HashSet<String>,
}

impl SpineProjection {
    pub(crate) fn node_ids(&self) -> impl Iterator<Item = &NodeId> {
        self.state.nodes().keys()
    }
}

#[derive(Debug, Error)]
pub(crate) enum SpineProjectionError {
    #[error("failed to parse spine call arguments for {call_id}: {source}")]
    ArgsJson {
        call_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unknown legacy spine operation {op:?} for {call_id}")]
    UnknownLegacyOp { call_id: String, op: String },
    #[error("spine projection raw ordinal overflow")]
    RawOrdinalOverflow,
    #[error("spine projection saw duplicate pending transition {call_id}")]
    DuplicatePendingTransition { call_id: String },
    #[error(transparent)]
    State(#[from] SpineStateError),
}

pub(crate) fn project_spine_state_from_rollout(
    rollout_items: &[RolloutItem],
) -> Result<SpineProjection, SpineProjectionError> {
    let effective_items = effective_rollout_items(rollout_items);
    let mut state = SpineState::new();
    let mut raw_ordinal = 0_u64;
    let mut pending_transition: Option<PendingTransition> = None;
    let surviving_turn_ids = surviving_turn_ids(&effective_items);

    for item in effective_items {
        match item {
            RolloutItem::ResponseItem(response_item) => {
                let item_end = raw_ordinal
                    .checked_add(1)
                    .ok_or(SpineProjectionError::RawOrdinalOverflow)?;
                if let Some(transition) = spine_transition_from_response_item(response_item)? {
                    if pending_transition.is_some() {
                        return Err(SpineProjectionError::DuplicatePendingTransition {
                            call_id: transition.call_id,
                        });
                    }
                    pending_transition = Some(transition);
                }
                if let ResponseItem::FunctionCallOutput { call_id, .. } = response_item
                    && pending_transition
                        .as_ref()
                        .is_some_and(|transition| transition.call_id == *call_id)
                {
                    let transition = pending_transition
                        .take()
                        .expect("pending transition checked above");
                    let applied = transition.op.apply(&mut state, transition.summary)?;
                    state.set_raw_start_ordinal(&applied.to, item_end)?;
                }
                raw_ordinal = item_end;
            }
            RolloutItem::Compacted(compacted)
                if compacted
                    .message
                    .starts_with(ROOT_EPOCH_COMPACT_MESSAGE_PREFIX) =>
            {
                state.reset_root_epoch(raw_ordinal)?;
            }
            _ => {}
        }
    }

    Ok(SpineProjection {
        state,
        response_item_count: raw_ordinal,
        surviving_turn_ids,
    })
}

fn effective_rollout_items(items: &[RolloutItem]) -> Vec<&RolloutItem> {
    let mut effective: Vec<&RolloutItem> = Vec::new();
    for item in items {
        match item {
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
                if num_turns == 0 {
                    continue;
                }
                let mut remaining = num_turns;
                let mut cut_idx = None;
                for (idx, item) in effective.iter().enumerate().rev() {
                    if rollout_item_is_user_turn_boundary(item) {
                        let turn_start_idx = turn_start_index(&effective, idx);
                        remaining -= 1;
                        cut_idx = Some(turn_start_idx);
                        if remaining == 0 {
                            break;
                        }
                    }
                }
                match cut_idx {
                    Some(idx) => effective.truncate(idx),
                    None => effective.clear(),
                }
            }
            _ => effective.push(item),
        }
    }
    effective
}

fn surviving_turn_ids(items: &[&RolloutItem]) -> HashSet<String> {
    let mut turn_ids = HashSet::new();
    for item in items {
        match item {
            RolloutItem::TurnContext(context) => {
                if let Some(turn_id) = &context.turn_id {
                    turn_ids.insert(turn_id.clone());
                }
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                turn_ids.insert(event.turn_id.clone());
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                turn_ids.insert(event.turn_id.clone());
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                if let Some(turn_id) = &event.turn_id {
                    turn_ids.insert(turn_id.clone());
                }
            }
            _ => {}
        }
    }
    turn_ids
}

fn rollout_item_is_user_turn_boundary(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::ResponseItem(item) => crate::context_manager::is_user_turn_boundary(item),
        _ => false,
    }
}

fn turn_start_index(items: &[&RolloutItem], user_boundary_idx: usize) -> usize {
    let mut idx = user_boundary_idx;
    while idx > 0 {
        let previous_idx = idx - 1;
        match items[previous_idx] {
            RolloutItem::EventMsg(EventMsg::TurnStarted(_)) => return previous_idx,
            RolloutItem::EventMsg(EventMsg::TurnComplete(_))
            | RolloutItem::EventMsg(EventMsg::TurnAborted(_)) => return idx,
            RolloutItem::EventMsg(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::SessionMeta(_) => {
                idx = previous_idx;
            }
            RolloutItem::ResponseItem(_) | RolloutItem::Compacted(_) => return idx,
        }
    }
    idx
}

#[derive(Debug)]
struct PendingTransition {
    call_id: String,
    op: SpineOperation,
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespacedOpenArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespacedSummaryArgs {
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySpineArgs {
    op: String,
    summary: Option<String>,
}

fn spine_transition_from_response_item(
    item: &ResponseItem,
) -> Result<Option<PendingTransition>, SpineProjectionError> {
    let ResponseItem::FunctionCall {
        name,
        namespace,
        arguments,
        call_id,
        ..
    } = item
    else {
        return Ok(None);
    };

    if namespace.as_deref() == Some(SPINE_NAMESPACE) {
        let (op, summary) =
            match name.as_str() {
                SPINE_TOOL_OPEN => {
                    serde_json::from_str::<NamespacedOpenArgs>(arguments).map_err(|source| {
                        SpineProjectionError::ArgsJson {
                            call_id: call_id.clone(),
                            source,
                        }
                    })?;
                    (SpineOperation::Open, None)
                }
                SPINE_TOOL_NEXT => {
                    let args = serde_json::from_str::<NamespacedSummaryArgs>(arguments).map_err(
                        |source| SpineProjectionError::ArgsJson {
                            call_id: call_id.clone(),
                            source,
                        },
                    )?;
                    (SpineOperation::Next, Some(args.summary))
                }
                SPINE_TOOL_CLOSE => {
                    let args = serde_json::from_str::<NamespacedSummaryArgs>(arguments).map_err(
                        |source| SpineProjectionError::ArgsJson {
                            call_id: call_id.clone(),
                            source,
                        },
                    )?;
                    (SpineOperation::Close, Some(args.summary))
                }
                _ => return Ok(None),
            };
        return Ok(Some(PendingTransition {
            call_id: call_id.clone(),
            op,
            summary,
        }));
    }

    if namespace.is_none() && name == "spine" {
        let args = serde_json::from_str::<LegacySpineArgs>(arguments).map_err(|source| {
            SpineProjectionError::ArgsJson {
                call_id: call_id.clone(),
                source,
            }
        })?;
        let op = match args.op.as_str() {
            "open" => SpineOperation::Open,
            "next" => SpineOperation::Next,
            "close" => SpineOperation::Close,
            _ => {
                return Err(SpineProjectionError::UnknownLegacyOp {
                    call_id: call_id.clone(),
                    op: args.op,
                });
            }
        };
        return Ok(Some(PendingTransition {
            call_id: call_id.clone(),
            op,
            summary: args.summary,
        }));
    }

    Ok(None)
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
