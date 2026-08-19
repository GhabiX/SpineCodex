use crate::context::ContextualUserFragment;
use crate::context::SpineMemoryFragment;
use crate::context::SpineNodeFragment;
use crate::context::SpineSpawnEvidenceFragment;
use crate::context::SpineUserAnchor;
use crate::context_manager::ContextManager;
use crate::context_manager::is_user_turn_boundary;
use crate::event_mapping::is_contextual_dev_message_content;
use crate::event_mapping::is_contextual_user_message_content;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use spine_core::host::ContextItem;
use spine_core::host::MemorySlot;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::NativeItemRef;
use spine_core::host::RawBoundary;
use spine_core::host::SpineOperationFact;
use spine_core::host::SpineProjection;
use spine_core::host::ToolOutcome;
use spine_core::host::ToolUse;
use spine_core::host::ToolValidation;
use spine_core::host::TrimEdit;
use spine_core::host::TrimProjection;
use spine_core::host::ValidatedTransition;
use std::collections::BTreeMap;

pub(crate) mod config;
pub(crate) mod context_handler;
pub(crate) mod context_plan;
#[cfg(test)]
#[path = "context_plan_tests.rs"]
mod context_plan_tests;
pub(crate) mod coordinator;
#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod coordinator_tests;
pub(crate) mod memory_projection;
pub(crate) mod observer;
#[cfg(test)]
#[path = "persistence_baseline_tests.rs"]
mod persistence_baseline_tests;
pub(crate) mod rollout_debug;
pub(crate) mod session_config;
pub(crate) mod session_runtime;
pub(crate) mod spawn;
pub(crate) mod spawn_gate;
pub(crate) mod tool_response;

pub(crate) const TOOL_RESULT_CLEARED_MESSAGE: &str = spine_core::host::TRIM_SNIPPED_BODY;

pub(crate) fn validated_control_fact(
    tool: spine_core::host::SpineTool,
    arguments: &str,
) -> Result<SpineOperationFact, spine_core::host::ToolValidationError> {
    match spine_core::host::validate_tool(tool, arguments)? {
        ToolValidation::Transition(ValidatedTransition::Open { summary }) => {
            Ok(SpineOperationFact::Open { summary })
        }
        ToolValidation::Transition(ValidatedTransition::Close { memory }) => {
            Ok(SpineOperationFact::Close { memory })
        }
        ToolValidation::Transition(ValidatedTransition::Next { summary, memory }) => {
            Ok(SpineOperationFact::Next {
                closed_memory: memory,
                next_summary: summary,
            })
        }
        ToolValidation::Transition(
            ValidatedTransition::Trim(_) | ValidatedTransition::Spawn { .. },
        )
        | ToolValidation::Ordinary => Err(spine_core::host::ToolValidationError::UnknownTool(
            tool.qualified_name(),
        )),
    }
}

pub(crate) fn canonical_projected_item(
    history: &ContextManager,
    source: &ResponseItem,
) -> ResponseItem {
    history
        .raw_items()
        .iter()
        .find(|candidate| same_projected_identity(candidate, source))
        .cloned()
        .unwrap_or_else(|| source.clone())
}

fn same_projected_identity(left: &ResponseItem, right: &ResponseItem) -> bool {
    if let (Some(left_id), Some(right_id)) = (left.id(), right.id()) {
        return left_id == right_id;
    }

    match (left, right) {
        (
            ResponseItem::FunctionCallOutput { call_id: left, .. },
            ResponseItem::FunctionCallOutput { call_id: right, .. },
        )
        | (
            ResponseItem::CustomToolCallOutput { call_id: left, .. },
            ResponseItem::CustomToolCallOutput { call_id: right, .. },
        ) => left == right,
        _ => false,
    }
}

pub(crate) fn closed_memory_projection_entries(
    projection: &SpineProjection,
) -> Vec<memory_projection::SpinetreeMemoryProjectionEntry> {
    spine_core::host::closed_memory_artifacts(projection)
        .into_iter()
        .map(
            |artifact| memory_projection::SpinetreeMemoryProjectionEntry {
                summary: artifact.summary,
                body: spine_core::host::render_memory_artifact(&artifact.node_id, &artifact.body),
                node_id: artifact.node_id.to_string(),
            },
        )
        .collect()
}

#[cfg(test)]
pub(crate) fn user_message_projection_entries(
    rollout: &[RolloutItem],
) -> Vec<memory_projection::SpinetreeUserMessageProjectionEntry> {
    user_message_projection_entries_from_effective(&effective_rollout(rollout))
}

#[cfg(test)]
pub(crate) fn user_message_projection_entries_from_effective(
    effective: &[(usize, &RolloutItem)],
) -> Vec<memory_projection::SpinetreeUserMessageProjectionEntry> {
    let mut next_anchor = 1;
    effective
        .iter()
        .copied()
        .filter_map(|(raw_index, item)| {
            let RolloutItem::ResponseItem(item) = item else {
                return None;
            };
            let message = message_from_response_item(raw_index, item);
            if message.role != MessageRole::User {
                return None;
            }
            let entry = memory_projection::SpinetreeUserMessageProjectionEntry {
                anchor: next_anchor,
                body: message.content,
            };
            next_anchor += 1;
            Some(entry)
        })
        .collect()
}

pub(crate) fn effective_rollout(rollout: &[RolloutItem]) -> Vec<(usize, &RolloutItem)> {
    let mut source = Vec::new();
    let mut response_ordinal = 0;
    for item in rollout {
        source.push((response_ordinal, item));
        if is_spine_source_item(item) {
            response_ordinal += 1;
        }
    }
    effective_rollout_from_source(&source)
}

pub(crate) fn is_canonical_rollout(
    rollout: &[RolloutItem],
) -> Result<bool, coordinator::CoordinatorError> {
    Ok(matches!(
        coordinator::replay_mode(&effective_rollout(rollout))?,
        coordinator::ReplayMode::Canonical { .. }
    ))
}

pub(crate) fn trim_to_current_sampling_start(items: &mut Vec<RolloutItem>) -> bool {
    let Some(start) = items
        .iter()
        .rposition(|item| matches!(item, RolloutItem::SpineSamplingStarted(_)))
    else {
        return false;
    };
    if items[start + 1..]
        .iter()
        .any(|item| matches!(item, RolloutItem::SpineTransition(_)))
    {
        return false;
    }
    items.truncate(start + 1);
    true
}

pub(crate) fn effective_rollout_from_source<'a>(
    source: &[(usize, &'a RolloutItem)],
) -> Vec<(usize, &'a RolloutItem)> {
    let mut effective: Vec<(usize, &RolloutItem)> = Vec::new();
    for (response_ordinal, item) in source.iter().copied() {
        if let RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) = item {
            let turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
            if turns == 0 {
                continue;
            }
            let user_boundaries: Vec<_> = effective
                .iter()
                .enumerate()
                .filter_map(|(effective_index, (_, item))| match item {
                    RolloutItem::ResponseItem(item) if is_user_turn_boundary(item) => {
                        Some(effective_index)
                    }
                    RolloutItem::InterAgentCommunication(_) => Some(effective_index),
                    _ => None,
                })
                .collect();
            if let Some(cut) = user_boundaries
                .len()
                .checked_sub(turns)
                .and_then(|position| user_boundaries.get(position))
                .copied()
                .or_else(|| user_boundaries.first().copied())
            {
                let first_user_boundary = user_boundaries.first().copied().unwrap_or(cut);
                effective.truncate(cut);
                // Native rollback trims contextual updates immediately above the removed
                // user-turn boundary. Keep the Spine selected prefix identical to that host
                // boundary so projection cannot reintroduce settings that rollback removed.
                let mut scan = effective.len();
                while scan > first_user_boundary {
                    let Some((_, item)) = effective.get(scan - 1) else {
                        break;
                    };
                    let trim = match item {
                        RolloutItem::ResponseItem(ResponseItem::Message {
                            role, content, ..
                        }) if role == "developer" => is_contextual_dev_message_content(content),
                        RolloutItem::ResponseItem(ResponseItem::Message {
                            role, content, ..
                        }) if role == "user" => is_contextual_user_message_content(content),
                        RolloutItem::EventMsg(EventMsg::TokenCount(_)) => {
                            scan -= 1;
                            continue;
                        }
                        _ => false,
                    };
                    if !trim {
                        break;
                    }
                    effective.remove(scan - 1);
                    scan -= 1;
                }
            }
            continue;
        }
        if is_spine_source_item(item)
            || matches!(
                item,
                RolloutItem::EventMsg(EventMsg::TokenCount(_))
                    | RolloutItem::SpineSamplingStarted(_)
                    | RolloutItem::SpineTransition(_)
            )
        {
            effective.push((response_ordinal, item));
        }
    }
    effective
}

pub(crate) fn is_spine_source_item(item: &RolloutItem) -> bool {
    matches!(
        item,
        RolloutItem::ResponseItem(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::Compacted(_)
    )
}

fn normalized_tool_request(item: &ResponseItem) -> Option<ToolUse> {
    let (name, namespace, arguments, call_id) = match item {
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        } => (name, namespace.as_deref(), arguments, call_id),
        ResponseItem::CustomToolCall {
            name,
            namespace,
            input,
            call_id,
            ..
        } => (name, namespace.as_deref(), input, call_id),
        _ => return None,
    };
    Some(ToolUse {
        call_id: call_id.clone(),
        name: match namespace {
            Some(namespace) if !namespace.is_empty() => format!("{namespace}.{name}"),
            _ => name.to_string(),
        },
        arguments: arguments.clone(),
        call_ordinal: None,
        outcome: None,
        output: None,
        output_boundary: None,
    })
}

fn normalized_tool_response(item: &ResponseItem) -> Option<NormalizedToolResponse<'_>> {
    match item {
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => Some(NormalizedToolResponse { call_id, output }),
        _ => None,
    }
}

struct NormalizedToolResponse<'a> {
    call_id: &'a str,
    output: &'a codex_protocol::models::FunctionCallOutputPayload,
}

fn classify_tool_outcome(
    call: &ToolUse,
    output: &codex_protocol::models::FunctionCallOutputPayload,
    spawn_enabled: bool,
) -> ToolOutcome {
    if call.name == "spine.spawn" {
        return if spawn_enabled && is_valid_spawn_success_carrier(call, &output.body) {
            ToolOutcome::Succeeded
        } else {
            ToolOutcome::Unknown
        };
    }
    tool_response::outcome(&call.name, output)
}

fn is_valid_spawn_success_carrier(call: &ToolUse, body: &FunctionCallOutputBody) -> bool {
    let FunctionCallOutputBody::Text(body) = body else {
        return false;
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SpawnArgs {
        tasks: Vec<spine_core::host::SpawnTask>,
    }

    let Ok(SpawnArgs { tasks }) = serde_json::from_str(&call.arguments) else {
        return false;
    };
    let Ok(receipt) = spine_core::host::SpawnReceipt::decode_json(body) else {
        return false;
    };
    receipt.validate_for(&tasks).is_ok()
}

fn message_from_response_item(raw_index: usize, item: &ResponseItem) -> Message {
    let (role, content) = match item {
        ResponseItem::Message { role, content, .. } => (
            match role.as_str() {
                "user" if is_contextual_user_message_content(content) => {
                    MessageRole::ContextualUser
                }
                "user" => MessageRole::User,
                "developer" => MessageRole::Developer,
                "system" => MessageRole::System,
                _ => MessageRole::Assistant,
            },
            content
                .iter()
                .filter_map(content_text)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ResponseItem::Reasoning { .. } => (MessageRole::Assistant, String::new()),
        _ => (
            MessageRole::Assistant,
            serde_json::to_string(item).unwrap_or_default(),
        ),
    };
    Message {
        boundary: RawBoundary(raw_index as u64),
        role,
        content,
    }
}

fn content_text(item: &ContentItem) -> Option<String> {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text.clone()),
        ContentItem::InputImage { .. } => Some("<image>".to_string()),
        ContentItem::InputAudio { .. } => Some("<audio>".to_string()),
    }
}

fn materialize_context(
    context: &[ContextItem],
    source: &[(usize, &RolloutItem)],
    trim: Option<&TrimProjection>,
    host_history: Option<&ContextManager>,
    node_context_costs: &BTreeMap<spine_core::host::NodeId, spine_core::host::NodeContextCost>,
    node_prompt: &str,
) -> Result<Vec<ResponseItem>, String> {
    let mut materialized = Vec::new();
    for item in context {
        match item {
            ContextItem::Message {
                message,
                user_anchor,
            } => {
                let mut item = response_item_at(source, message.boundary, host_history)
                    .ok_or_else(|| {
                        format!(
                            "message at raw boundary {} has no native rollout source",
                            message.boundary.0
                        )
                    })?;
                if let Some(anchor) = user_anchor {
                    SpineUserAnchor::new(*anchor).prepend_to(&mut item);
                }
                materialized.push(item);
            }
            ContextItem::SourceSpan { span } => {
                for raw_index in span.start.0..=span.end.0 {
                    if let Some(item) =
                        response_item_at(source, RawBoundary(raw_index), host_history)
                    {
                        materialized.push(project_trim_item(
                            item,
                            usize::try_from(raw_index).unwrap_or(usize::MAX),
                            trim,
                        ));
                    }
                }
            }
            ContextItem::SyntheticNode {
                node_id,
                summary,
                status,
            } => materialized.push(ContextualUserFragment::into(SpineNodeFragment::new(
                node_id,
                summary,
                *status,
                node_context_costs
                    .get(node_id)
                    .copied()
                    .unwrap_or(spine_core::host::NodeContextCost::Unavailable),
                node_prompt,
            )?)),
            ContextItem::MemorySlot(slot) => match slot {
                MemorySlot::User {
                    message, anchor, ..
                } => {
                    // The reducer created this slot from the same immutable rollout.
                    let mut item = response_item_at(source, message.boundary, host_history)
                        .ok_or_else(|| {
                            format!(
                                "memory user slot at raw boundary {} has no native rollout source",
                                message.boundary.0
                            )
                        })?;
                    if !matches!(&item, ResponseItem::Message { role, .. } if role == "user") {
                        return Err(format!(
                            "memory user slot at raw boundary {} resolved to a non-user item",
                            message.boundary.0
                        ));
                    }
                    SpineUserAnchor::new(*anchor).prepend_to(&mut item);
                    materialized.push(item);
                }
                MemorySlot::Summary {
                    owner_node, body, ..
                } => materialized.push(ContextualUserFragment::into(SpineMemoryFragment::new(
                    owner_node, body,
                )?)),
                MemorySlot::SpawnEvidence {
                    owner_node,
                    task,
                    outcome,
                    diagnostic,
                    execution_ref,
                    ..
                } => materialized.push(ContextualUserFragment::into(
                    SpineSpawnEvidenceFragment::new(
                        owner_node,
                        task,
                        *outcome,
                        diagnostic.as_deref(),
                        execution_ref.as_deref(),
                    )?,
                )),
            },
            ContextItem::Native { source: native_ref } => match native_ref {
                NativeItemRef::Rollout { ordinal } => {
                    let item =
                        response_item_at(source, *ordinal, host_history).ok_or_else(|| {
                            format!("native rollout source {} is unavailable", ordinal.0)
                        })?;
                    materialized.push(item);
                }
                NativeItemRef::CompactReplacement {
                    compact_boundary,
                    index,
                } => {
                    let item = compact_replacement_at(source, *compact_boundary, *index)
                        .ok_or_else(|| {
                            format!(
                                "compact replacement {}:{} is unavailable",
                                compact_boundary.0, index
                            )
                        })?;
                    materialized.push(item);
                }
            },
        }
    }
    Ok(materialized)
}

fn project_trim_item(
    mut item: ResponseItem,
    raw_ordinal: usize,
    trim: Option<&TrimProjection>,
) -> ResponseItem {
    let (call_id, body) = match &mut item {
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        } => (call_id, &mut output.body),
        ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => (call_id, &mut output.body),
        _ => return item,
    };
    let Some(edit) =
        trim.and_then(|projection| projection.edit(RawBoundary(raw_ordinal as u64), call_id))
    else {
        return item;
    };
    let visible_body = match edit {
        TrimEdit::Tagged { trim_id, body, .. } => format!("[TRIM_ID: {trim_id}]\n{body}"),
        TrimEdit::Snipped => TOOL_RESULT_CLEARED_MESSAGE.to_string(),
        TrimEdit::Sliced(value) => value.clone(),
    };
    *body = FunctionCallOutputBody::Text(visible_body);
    item
}

fn response_item_at(
    source: &[(usize, &RolloutItem)],
    boundary: RawBoundary,
    host_history: Option<&ContextManager>,
) -> Option<ResponseItem> {
    let index = usize::try_from(boundary.0).ok()?;
    match source
        .iter()
        .filter(|(_, item)| is_spine_source_item(item))
        .find_map(|(ordinal, item)| (*ordinal == index).then_some(*item))?
    {
        RolloutItem::ResponseItem(item) => Some(
            host_history
                .map(|history| canonical_projected_item(history, item))
                .unwrap_or_else(|| item.clone()),
        ),
        RolloutItem::InterAgentCommunication(communication) => {
            Some(communication.to_model_input_item())
        }
        RolloutItem::Compacted(compacted) => Some(text_message(
            MessageRole::Assistant,
            compacted.message.clone(),
        )),
        _ => None,
    }
}

fn compact_replacement_at(
    source: &[(usize, &RolloutItem)],
    boundary: RawBoundary,
    replacement_index: u32,
) -> Option<ResponseItem> {
    let raw_index = usize::try_from(boundary.0).ok()?;
    let replacement_index = usize::try_from(replacement_index).ok()?;
    let RolloutItem::Compacted(compacted) = source
        .iter()
        .filter(|(_, item)| is_spine_source_item(item))
        .find_map(|(ordinal, item)| (*ordinal == raw_index).then_some(*item))?
    else {
        return None;
    };
    compacted
        .replacement_history
        .as_ref()?
        .get(replacement_index)
        .cloned()
}

fn text_message(role: MessageRole, text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: match role {
            MessageRole::User => "user",
            MessageRole::ContextualUser => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Developer => "developer",
            MessageRole::System => "system",
        }
        .to_string(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[cfg(test)]
mod tests;
