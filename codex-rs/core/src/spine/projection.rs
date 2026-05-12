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
use thiserror::Error;

const ROOT_EPOCH_COMPACT_MESSAGE_PREFIX: &str = "Spine compacted root epoch ";

#[derive(Debug)]
pub(crate) struct SpineProjection {
    pub(crate) state: SpineState,
    pub(crate) response_item_count: u64,
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
                let transition = state.archive_current_root_epoch("Context compacted")?;
                state.set_raw_start_ordinal(&transition.to, raw_ordinal)?;
            }
            _ => {}
        }
    }

    Ok(SpineProjection {
        state,
        response_item_count: raw_ordinal,
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
                        remaining -= 1;
                        cut_idx = Some(idx);
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

fn rollout_item_is_user_turn_boundary(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::ResponseItem(item) => crate::context_manager::is_user_turn_boundary(item),
        _ => false,
    }
}

#[derive(Debug)]
struct PendingTransition {
    call_id: String,
    op: SpineOperation,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct NamespacedSpineArgs {
    summary: String,
}

#[derive(Debug, Deserialize)]
struct LegacySpineArgs {
    op: String,
    summary: String,
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
        let op = match name.as_str() {
            SPINE_TOOL_OPEN => SpineOperation::Open,
            SPINE_TOOL_NEXT => SpineOperation::Next,
            SPINE_TOOL_CLOSE => SpineOperation::Close,
            _ => return Ok(None),
        };
        let args = serde_json::from_str::<NamespacedSpineArgs>(arguments).map_err(|source| {
            SpineProjectionError::ArgsJson {
                call_id: call_id.clone(),
                source,
            }
        })?;
        return Ok(Some(PendingTransition {
            call_id: call_id.clone(),
            op,
            summary: args.summary,
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
