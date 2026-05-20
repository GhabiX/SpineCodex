use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_CLOSE;
use super::SPINE_TOOL_OPEN;
use super::ids::NodeId;
use super::projection_epoch::ProjectionEpochError;
use super::projection_epoch::ProjectionEpochMetadata;
use super::projection_epoch::projection_epoch_metadata;
use super::state::SpineState;
use super::state::SpineStateError;
use super::store::SpineOperation;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineCompactedCheckpointKind;
use serde::Deserialize;
use serde::de;
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct SpineProjection {
    pub(crate) state: SpineState,
    pub(crate) response_item_count: u64,
    pub(crate) surviving_turn_ids: HashSet<String>,
    pub(crate) surviving_compact_ids: HashSet<String>,
    pub(crate) root_epoch_compact_ids: HashSet<String>,
    pub(crate) epoch: ProjectionEpochMetadata,
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
    #[error("spine projection raw ordinal overflow")]
    RawOrdinalOverflow,
    #[error("spine projection saw duplicate pending transition {call_id}")]
    DuplicatePendingTransition { call_id: String },
    #[error("unsupported spine tool {name} in rollout projection for {call_id}")]
    UnsupportedSpineTool { call_id: String, name: String },
    #[error("spine projection rollback turn count {num_turns} cannot fit in usize")]
    RollbackTurnCountOverflow { num_turns: u32 },
    #[error("failed to build projection epoch metadata: {0}")]
    Metadata(#[from] ProjectionEpochError),
    #[error(transparent)]
    State(#[from] SpineStateError),
}

#[cfg(test)]
pub(crate) fn project_spine_state_from_rollout(
    rollout_items: &[RolloutItem],
) -> Result<SpineProjection, SpineProjectionError> {
    project_spine_state_from_rollout_with_source("in_memory_rollout", rollout_items)
}

pub(crate) fn project_spine_state_from_rollout_with_source(
    source_rollout_ref: impl Into<String>,
    rollout_items: &[RolloutItem],
) -> Result<SpineProjection, SpineProjectionError> {
    let effective_items = effective_rollout_items(rollout_items)?;
    let mut state = SpineState::new();
    let mut raw_ordinal = 0_u64;
    let mut pending_transition: Option<PendingTransition> = None;
    let surviving_turn_ids = surviving_turn_ids(&effective_items);
    let surviving_compact_ids = surviving_compact_ids(&effective_items);
    let mut root_epoch_compact_ids = HashSet::new();

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
            RolloutItem::Compacted(compacted) => match compacted.spine.as_ref() {
                Some(spine) if spine.kind == SpineCompactedCheckpointKind::RootEpoch => {
                    root_epoch_compact_ids.insert(spine.compact_id.clone());
                    state.reset_root_epoch("Context compacted", raw_ordinal)?;
                }
                Some(_) => {}
                None => break,
            },
            _ => {}
        }
    }

    let epoch = projection_epoch_metadata(
        source_rollout_ref,
        rollout_items,
        &state,
        raw_ordinal,
        &surviving_turn_ids,
        &surviving_compact_ids,
    )?;

    Ok(SpineProjection {
        state,
        response_item_count: raw_ordinal,
        surviving_turn_ids,
        surviving_compact_ids,
        root_epoch_compact_ids,
        epoch,
    })
}

pub(crate) fn surviving_spine_compact_ids_from_rollout(
    rollout_items: &[RolloutItem],
) -> Result<HashSet<String>, SpineProjectionError> {
    let effective_items = effective_rollout_items(rollout_items)?;
    Ok(surviving_compact_ids(&effective_items))
}

fn effective_rollout_items(
    items: &[RolloutItem],
) -> Result<Vec<&RolloutItem>, SpineProjectionError> {
    let mut effective: Vec<&RolloutItem> = Vec::new();
    for item in items {
        match item {
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).map_err(|_| {
                    SpineProjectionError::RollbackTurnCountOverflow {
                        num_turns: rollback.num_turns,
                    }
                })?;
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
    Ok(effective)
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

fn surviving_compact_ids(items: &[&RolloutItem]) -> HashSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted
                .spine
                .as_ref()
                .map(|spine| spine.compact_id.clone()),
            _ => None,
        })
        .collect()
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
struct NamespacedCloseArgs {
    summary: String,
    #[serde(
        default,
        rename = "instruction",
        deserialize_with = "discard_optional_string"
    )]
    _instruction: (),
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
        let (op, summary) = match name.as_str() {
            SPINE_TOOL_OPEN => {
                serde_json::from_str::<NamespacedOpenArgs>(arguments).map_err(|source| {
                    SpineProjectionError::ArgsJson {
                        call_id: call_id.clone(),
                        source,
                    }
                })?;
                (SpineOperation::Open, None)
            }
            SPINE_TOOL_CLOSE => {
                let args =
                    serde_json::from_str::<NamespacedCloseArgs>(arguments).map_err(|source| {
                        SpineProjectionError::ArgsJson {
                            call_id: call_id.clone(),
                            source,
                        }
                    })?;
                let NamespacedCloseArgs {
                    summary,
                    _instruction: _,
                } = args;
                (SpineOperation::Close, Some(summary))
            }
            other => {
                return Err(SpineProjectionError::UnsupportedSpineTool {
                    call_id: call_id.clone(),
                    name: other.to_string(),
                });
            }
        };
        return Ok(Some(PendingTransition {
            call_id: call_id.clone(),
            op,
            summary,
        }));
    }

    Ok(None)
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;

#[cfg(test)]
mod instruction_projection_tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputPayload;

    fn user_message(text: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
        })
    }

    fn spine_call(call_id: &str, op: &str, arguments: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: op.to_string(),
            namespace: Some(SPINE_NAMESPACE.to_string()),
            arguments: arguments.to_string(),
            call_id: call_id.to_string(),
        })
    }

    fn call_output(call_id: &str) -> RolloutItem {
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("ok".to_string()),
                success: Some(true),
            },
        })
    }

    #[test]
    fn projection_rejects_unknown_namespaced_spine_tool() {
        let error = project_spine_state_from_rollout(&[
            user_message("start"),
            spine_call(
                "unknown-1",
                "unknown",
                r#"{"summary":"done","instruction":"keep failing test"}"#,
            ),
            call_output("unknown-1"),
        ])
        .expect_err("unknown namespaced spine tools must fail closed");

        assert!(matches!(
            error,
            SpineProjectionError::UnsupportedSpineTool { .. }
        ));
    }

    #[test]
    fn projection_accepts_runtime_valid_compact_instruction_on_close() {
        let projection = project_spine_state_from_rollout(&[
            user_message("start"),
            spine_call("open-1", SPINE_TOOL_OPEN, "{}"),
            call_output("open-1"),
            spine_call(
                "close-1",
                SPINE_TOOL_CLOSE,
                r#"{"summary":"done","instruction":"keep child context"}"#,
            ),
            call_output("close-1"),
        ])
        .expect("project");

        assert_eq!(projection.state.cursor().to_string(), "1.1");
        assert_eq!(
            projection
                .state
                .node(&NodeId::from_segments(vec![1, 1]))
                .expect("node")
                .summary
                .as_deref(),
            None
        );
        assert_eq!(
            projection
                .state
                .node(&NodeId::from_segments(vec![1, 1, 1]))
                .expect("child node")
                .summary
                .as_deref(),
            Some("done")
        );
    }

    #[test]
    fn projection_rejects_namespaced_open_with_arguments() {
        let error = project_spine_state_from_rollout(&[
            user_message("start"),
            spine_call(
                "open-1",
                SPINE_TOOL_OPEN,
                r#"{"summary":"scope","instruction":"bad"}"#,
            ),
        ])
        .expect_err("open arguments should remain strict");

        assert!(matches!(error, SpineProjectionError::ArgsJson { .. }));
    }
}

fn discard_optional_string<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: de::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|_| ())
}
