use crate::context_manager::ContextManager;
use crate::context_manager::is_user_turn_boundary;
use crate::event_mapping::is_contextual_dev_message_content;
use crate::event_mapping::is_contextual_user_message_content;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_spine_core::ContextItem;
use codex_spine_core::MemorySlot;
use codex_spine_core::Message;
use codex_spine_core::MessageRole;
use codex_spine_core::NativeItemRef;
use codex_spine_core::NodeStatus;
use codex_spine_core::RawBoundary;
use codex_spine_core::RolloutEvent;
use codex_spine_core::SpineProjection;
use codex_spine_core::SpineReducer;
use codex_spine_core::ToolCallGroup;
use codex_spine_core::ToolOutcome;
use codex_spine_core::ToolUse;
use codex_spine_core::TrimEdit;
use codex_spine_core::TrimProjection;
use codex_spine_core::TrimRequest;

pub(crate) mod instructions;
pub(crate) mod memory_projection;
pub(crate) mod pressure;
pub(crate) mod spawn;
pub(crate) mod status;
pub(crate) mod tool_response;

pub(crate) const TOOL_RESULT_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineControlKind {
    Open,
    Close,
    Next,
}

impl SpineControlKind {
    pub(crate) fn requires_task(self) -> bool {
        matches!(self, Self::Close | Self::Next)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CodexSpineProjection {
    pub(crate) spine: SpineProjection,
    pub(crate) context: Vec<ResponseItem>,
}

pub(crate) fn closed_memory_projection_entries(
    rollout: &[RolloutItem],
    spawn_enabled: bool,
) -> Vec<memory_projection::SpinetreeMemoryProjectionEntry> {
    derive_from_rollout_with_features(rollout, true, false, spawn_enabled)
        .spine
        .nodes
        .into_iter()
        .filter_map(|node| {
            if node.kind != codex_spine_core::NodeKind::Task || node.status != NodeStatus::Closed {
                return None;
            }
            let node_id = node.id;
            let body = node.memory?.into_iter().find_map(|slot| match slot {
                MemorySlot::Summary {
                    owner_node, body, ..
                } if owner_node == node_id => Some(body),
                _ => None,
            })?;
            let node_id = node_id.to_string();
            Some(memory_projection::SpinetreeMemoryProjectionEntry {
                summary: node.summary.unwrap_or_else(|| "node".to_string()),
                body: render_memory_artifact(&node_id, &body),
                node_id,
            })
        })
        .collect()
}

pub(crate) fn user_message_projection_entries(
    rollout: &[RolloutItem],
) -> Vec<memory_projection::SpinetreeUserMessageProjectionEntry> {
    let mut next_anchor = 1;
    effective_rollout(rollout)
        .into_iter()
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

pub(crate) fn derive_from_rollout(rollout: &[RolloutItem]) -> CodexSpineProjection {
    derive_from_rollout_with_features(rollout, true, false, true)
}

pub(crate) fn derive_from_rollout_with_features(
    rollout: &[RolloutItem],
    jit_enabled: bool,
    trim_enabled: bool,
    spawn_enabled: bool,
) -> CodexSpineProjection {
    let effective = effective_rollout(rollout);
    projection_from_effective_rollout(
        &effective,
        rollout,
        jit_enabled,
        trim_enabled,
        spawn_enabled,
        None,
    )
}

pub(crate) fn derive_from_rollout_with_host_history(
    rollout: &[RolloutItem],
    jit_enabled: bool,
    trim_enabled: bool,
    spawn_enabled: bool,
    host_history: &ContextManager,
) -> CodexSpineProjection {
    let effective = effective_rollout(rollout);
    projection_from_effective_rollout(
        &effective,
        rollout,
        jit_enabled,
        trim_enabled,
        spawn_enabled,
        Some(host_history),
    )
}

fn projection_from_effective_rollout(
    effective: &[(usize, &RolloutItem)],
    rollout: &[RolloutItem],
    jit_enabled: bool,
    trim_enabled: bool,
    spawn_enabled: bool,
    host_history: Option<&ContextManager>,
) -> CodexSpineProjection {
    let events = lex_rollout(effective, spawn_enabled);
    let trim = trim_enabled.then(|| TrimProjection::derive(&events));
    let spine = if jit_enabled {
        SpineReducer::derive(&events)
    } else {
        SpineReducer::derive(&[])
    };
    let context = if jit_enabled {
        materialize_context(&spine.visible_context, rollout, trim.as_ref(), host_history)
    } else {
        materialize_trim_only_context(effective, rollout, trim.as_ref(), host_history)
    };
    CodexSpineProjection { spine, context }
}

pub(crate) fn validate_trim_request(
    rollout: &[RolloutItem],
    request: &TrimRequest,
) -> Result<(), String> {
    let effective = effective_rollout(rollout);
    TrimProjection::derive(&lex_rollout(&effective, true)).validate(request)
}

fn effective_rollout(rollout: &[RolloutItem]) -> Vec<(usize, &RolloutItem)> {
    let mut effective: Vec<(usize, &RolloutItem)> = Vec::new();
    let mut response_ordinal = 0;
    for item in rollout {
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
        if is_spine_source_item(item) {
            effective.push((response_ordinal, item));
            response_ordinal += 1;
        } else if matches!(item, RolloutItem::EventMsg(EventMsg::TokenCount(_))) {
            effective.push((response_ordinal, item));
        }
    }
    effective
}

fn is_spine_source_item(item: &RolloutItem) -> bool {
    matches!(
        item,
        RolloutItem::ResponseItem(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::Compacted(_)
    )
}

fn lex_rollout(effective: &[(usize, &RolloutItem)], spawn_enabled: bool) -> Vec<RolloutEvent> {
    let mut events = Vec::new();
    let mut index = 0;
    while index < effective.len() {
        let (raw_index, item) = effective[index];
        match item {
            RolloutItem::ResponseItem(response_item) => {
                if let Some((group, consumed)) =
                    completed_tool_group(effective, index, spawn_enabled)
                {
                    events.push(RolloutEvent::ToolCall(group));
                    index += consumed;
                    continue;
                }
                events.push(RolloutEvent::Message(message_from_response_item(
                    raw_index,
                    response_item,
                )));
            }
            RolloutItem::InterAgentCommunication(communication) => {
                events.push(RolloutEvent::Message(message_from_response_item(
                    raw_index,
                    &communication.to_model_input_item(),
                )));
            }
            RolloutItem::Compacted(compacted) => {
                let replacement_history = compacted
                    .replacement_history
                    .as_ref()
                    .map(|items| {
                        items
                            .iter()
                            .enumerate()
                            .map(|(replacement_index, _)| ContextItem::Native {
                                source: NativeItemRef::CompactReplacement {
                                    compact_boundary: RawBoundary(raw_index as u64),
                                    index: u32::try_from(replacement_index).unwrap_or(u32::MAX),
                                },
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        vec![ContextItem::Message {
                            message: Message {
                                boundary: RawBoundary(raw_index as u64),
                                role: MessageRole::Assistant,
                                content: compacted.message.clone(),
                            },
                            user_anchor: None,
                        }]
                    });
                events.push(RolloutEvent::Compact {
                    boundary: RawBoundary(raw_index as u64),
                    replacement_history,
                });
            }
            RolloutItem::SessionMeta(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::EventMsg(_) => {}
        }
        index += 1;
    }
    events
}

fn completed_tool_group(
    effective: &[(usize, &RolloutItem)],
    start: usize,
    spawn_enabled: bool,
) -> Option<(ToolCallGroup, usize)> {
    let mut cursor = start;
    let mut leading = Vec::new();
    while let Some((raw_index, RolloutItem::ResponseItem(item))) = effective.get(cursor).copied() {
        if !is_leading_assistant_item(item) {
            break;
        }
        leading.push(message_from_response_item(raw_index, item));
        cursor += 1;
    }

    let first_call = cursor;
    let mut calls = Vec::new();
    while let Some((
        _,
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        }),
    )) = effective.get(cursor).copied()
    {
        calls.push(ToolUse {
            call_id: call_id.clone(),
            name: qualified_tool_name(namespace.as_deref(), name),
            arguments: arguments.clone(),
            outcome: None,
            output: None,
            output_boundary: None,
        });
        cursor += 1;
    }
    if cursor == first_call {
        return None;
    }

    let mut last_group_index = cursor.saturating_sub(1);
    while let Some((
        raw_index,
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }),
    )) = effective.get(cursor).copied()
    {
        let Some(call) = calls.iter_mut().find(|call| call.call_id == *call_id) else {
            break;
        };
        call.outcome = Some(classify_tool_outcome(call, output, spawn_enabled));
        call.output = Some(output.body.to_text().unwrap_or_default());
        call.output_boundary = Some(RawBoundary(raw_index as u64));
        last_group_index = cursor;
        cursor += 1;
    }

    let raw_start = effective[start].0;
    let raw_end = effective[last_group_index].0;
    Some((
        ToolCallGroup {
            start: RawBoundary(raw_start as u64),
            end: RawBoundary(raw_end as u64),
            leading_assistant_messages: leading,
            calls,
        },
        last_group_index - start + 1,
    ))
}

fn classify_tool_outcome(
    call: &ToolUse,
    output: &codex_protocol::models::FunctionCallOutputPayload,
    spawn_enabled: bool,
) -> ToolOutcome {
    if call.name == "spine.spawn" {
        if output.success == Some(false) {
            return ToolOutcome::Failed;
        }
        return if spawn_enabled && is_valid_spawn_success_carrier(call, &output.body) {
            ToolOutcome::Succeeded
        } else {
            ToolOutcome::Unknown
        };
    }
    tool_response::SpineToolResponse::outcome(&call.name, output)
}

fn is_valid_spawn_success_carrier(call: &ToolUse, body: &FunctionCallOutputBody) -> bool {
    let FunctionCallOutputBody::Text(body) = body else {
        return false;
    };
    let Ok(tasks) = spawn::parse_tasks(&call.arguments) else {
        return false;
    };
    let Ok(receipt) = spawn::decode_receipt(body) else {
        return false;
    };
    receipt.validate_for(&tasks).is_ok()
}

fn is_leading_assistant_item(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message { role, .. } if role == "assistant"
    ) || matches!(item, ResponseItem::Reasoning { .. })
}

fn qualified_tool_name(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}.{name}"),
        _ => name.to_string(),
    }
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
    }
}

fn materialize_context(
    context: &[ContextItem],
    rollout: &[RolloutItem],
    trim: Option<&TrimProjection>,
    host_history: Option<&ContextManager>,
) -> Vec<ResponseItem> {
    let mut materialized = Vec::new();
    for item in context {
        match item {
            ContextItem::Message {
                message,
                user_anchor,
            } => {
                if let Some(mut item) = response_item_at(rollout, message.boundary, host_history) {
                    if let Some(anchor) = user_anchor {
                        tag_user_message(&mut item, *anchor);
                    }
                    materialized.push(item);
                } else {
                    materialized.push(text_message(message.role, message.content.clone()));
                }
            }
            ContextItem::ToolCall(group) => {
                for raw_index in group.start.0..=group.end.0 {
                    if let Some(item) =
                        response_item_at(rollout, RawBoundary(raw_index), host_history)
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
            } => materialized.push(text_message(
                MessageRole::Developer,
                format!(
                    "<spine_node id=\"{node_id}\" summary=\"{}\" status=\"{}\" />",
                    escape_attribute(summary),
                    status_name(*status),
                ),
            )),
            ContextItem::MemorySlot(slot) => match slot {
                MemorySlot::User {
                    message, anchor, ..
                } => {
                    // The reducer created this slot from the same immutable rollout.
                    let mut item = response_item_at(rollout, message.boundary, host_history)
                        .unwrap_or_else(|| {
                            panic!(
                                "memory user slot at raw boundary {} has no rollout source",
                                message.boundary.0
                            )
                        });
                    assert!(
                        matches!(&item, ResponseItem::Message { role, .. } if role == "user"),
                        "memory user slot at raw boundary {} resolved to a non-user item",
                        message.boundary.0
                    );
                    tag_user_message(&mut item, *anchor);
                    materialized.push(item);
                }
                MemorySlot::Summary {
                    owner_node, body, ..
                } => materialized.push(text_message(
                    MessageRole::ContextualUser,
                    format!("<spine_memory node_id=\"{owner_node}\">\n{body}\n</spine_memory>"),
                )),
                MemorySlot::SpawnEvidence {
                    owner_node,
                    task,
                    outcome,
                    diagnostic,
                    execution_ref,
                    ..
                } => materialized.push(text_message(
                    MessageRole::ContextualUser,
                    render_spawn_evidence(
                        owner_node,
                        task,
                        *outcome,
                        diagnostic.as_deref(),
                        execution_ref.as_deref(),
                    ),
                )),
            },
            ContextItem::Native { source } => match source {
                NativeItemRef::CompactReplacement {
                    compact_boundary,
                    index,
                } => {
                    if let Some(item) = compact_replacement_at(rollout, *compact_boundary, *index) {
                        materialized.push(item);
                    }
                }
            },
        }
    }
    materialized
}

fn materialize_trim_only_context(
    effective: &[(usize, &RolloutItem)],
    rollout: &[RolloutItem],
    trim: Option<&TrimProjection>,
    host_history: Option<&ContextManager>,
) -> Vec<ResponseItem> {
    let start = effective
        .iter()
        .rposition(|(_, item)| matches!(item, RolloutItem::Compacted(_)))
        .unwrap_or(0);
    let mut context = Vec::new();
    for (raw_index, item) in effective.iter().skip(start) {
        match item {
            RolloutItem::ResponseItem(item) => {
                let item = host_history
                    .map(|history| history.canonical_projected_item(item))
                    .unwrap_or_else(|| item.clone());
                context.push(project_trim_item(item, *raw_index, trim))
            }
            RolloutItem::InterAgentCommunication(communication) => {
                context.push(communication.to_model_input_item())
            }
            RolloutItem::Compacted(compacted) => {
                if let Some(replacement) = &compacted.replacement_history {
                    context.extend(replacement.iter().map(|item| {
                        host_history
                            .map(|history| history.canonical_projected_item(item))
                            .unwrap_or_else(|| item.clone())
                    }));
                } else {
                    context.push(text_message(
                        MessageRole::Assistant,
                        compacted.message.clone(),
                    ));
                }
            }
            _ => {}
        }
    }
    if context.is_empty() && !rollout.is_empty() {
        context.extend(
            rollout
                .iter()
                .filter_map(|item| match item {
                    RolloutItem::ResponseItem(item) => Some(
                        host_history
                            .map(|history| history.canonical_projected_item(item))
                            .unwrap_or_else(|| item.clone()),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        );
    }
    context
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
        TrimEdit::Tagged { trim_id, body } => format!("[TRIM_ID: {trim_id}]\n{body}"),
        TrimEdit::Snipped => TOOL_RESULT_CLEARED_MESSAGE.to_string(),
        TrimEdit::Sliced(value) => value.clone(),
    };
    *body = FunctionCallOutputBody::Text(visible_body);
    item
}

fn response_item_at(
    rollout: &[RolloutItem],
    boundary: RawBoundary,
    host_history: Option<&ContextManager>,
) -> Option<ResponseItem> {
    let index = usize::try_from(boundary.0).ok()?;
    match rollout
        .iter()
        .filter(|item| is_spine_source_item(item))
        .nth(index)?
    {
        RolloutItem::ResponseItem(item) => Some(
            host_history
                .map(|history| history.canonical_projected_item(item))
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
    rollout: &[RolloutItem],
    boundary: RawBoundary,
    replacement_index: u32,
) -> Option<ResponseItem> {
    let raw_index = usize::try_from(boundary.0).ok()?;
    let replacement_index = usize::try_from(replacement_index).ok()?;
    let RolloutItem::Compacted(compacted) = rollout
        .iter()
        .filter(|item| is_spine_source_item(item))
        .nth(raw_index)?
    else {
        return None;
    };
    compacted
        .replacement_history
        .as_ref()?
        .get(replacement_index)
        .cloned()
}

fn tag_user_message(item: &mut ResponseItem, anchor: u64) {
    let ResponseItem::Message { role, content, .. } = item else {
        return;
    };
    if role != "user" {
        return;
    }
    let prefix = format!("[U{anchor}]\n");
    if let Some(ContentItem::InputText { text }) = content
        .iter_mut()
        .find(|item| matches!(item, ContentItem::InputText { .. }))
    {
        text.insert_str(0, &prefix);
    } else {
        content.insert(0, ContentItem::InputText { text: prefix });
    }
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

fn render_memory_artifact(node_id: &str, body: &str) -> String {
    format!("# Spine Memory {node_id}\n\n## Node Memory\n{body}")
}

fn render_spawn_evidence(
    owner_node: &codex_spine_core::NodeId,
    task: &codex_spine_core::SpawnTask,
    outcome: codex_spine_core::SpawnOutcome,
    diagnostic: Option<&str>,
    execution_ref: Option<&str>,
) -> String {
    format!(
        "<spine_spawn_evidence node_id=\"{owner_node}\">\n{}\n</spine_spawn_evidence>",
        render_spawn_evidence_body(task, outcome, diagnostic, execution_ref)
    )
}

fn render_spawn_evidence_body(
    task: &codex_spine_core::SpawnTask,
    outcome: codex_spine_core::SpawnOutcome,
    diagnostic: Option<&str>,
    execution_ref: Option<&str>,
) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "summary": task.summary,
        "prompt": task.prompt,
        "outcome": outcome,
        "diagnostic": diagnostic,
        "execution_ref": execution_ref,
    }))
    .expect("spawn evidence fields serialize")
}

fn status_name(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "opened",
        NodeStatus::Closed => "closed",
        NodeStatus::Compacted => "compacted",
    }
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests;
