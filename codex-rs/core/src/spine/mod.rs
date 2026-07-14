use crate::context_manager::is_user_turn_boundary;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_spine_core::ContextItem;
use codex_spine_core::MemoryPart;
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

pub(crate) mod instructions;

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

pub(crate) fn derive_from_rollout(rollout: &[RolloutItem]) -> CodexSpineProjection {
    let effective = effective_rollout(rollout);
    let events = lex_rollout(&effective);
    let spine = SpineReducer::derive(&events);
    let context = materialize_context(&spine.visible_context, rollout);
    CodexSpineProjection { spine, context }
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
                effective.truncate(cut);
            }
            continue;
        }
        if is_spine_source_item(item) {
            effective.push((response_ordinal, item));
            response_ordinal += 1;
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

fn lex_rollout(effective: &[(usize, &RolloutItem)]) -> Vec<RolloutEvent> {
    let mut events = Vec::new();
    let mut index = 0;
    while index < effective.len() {
        let (raw_index, item) = effective[index];
        match item {
            RolloutItem::ResponseItem(response_item) => {
                if let Some((group, consumed)) = completed_tool_group(effective, index) {
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
        });
        cursor += 1;
    }
    if cursor == first_call {
        return None;
    }

    let mut last_group_index = cursor.saturating_sub(1);
    while let Some((
        _,
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }),
    )) = effective.get(cursor).copied()
    {
        let Some(call) = calls.iter_mut().find(|call| call.call_id == *call_id) else {
            break;
        };
        call.outcome = Some(match output.success {
            Some(true) => ToolOutcome::Succeeded,
            Some(false) => ToolOutcome::Failed,
            None => ToolOutcome::Unknown,
        });
        call.output = Some(output.body.to_text().unwrap_or_default());
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

fn materialize_context(context: &[ContextItem], rollout: &[RolloutItem]) -> Vec<ResponseItem> {
    let mut materialized = Vec::new();
    for item in context {
        match item {
            ContextItem::Message {
                message,
                user_anchor,
            } => {
                if let Some(mut item) = response_item_at(rollout, message.boundary) {
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
                    if let Some(item) = response_item_at(rollout, RawBoundary(raw_index)) {
                        materialized.push(item);
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
            ContextItem::Memory { node_id, parts } => materialized.push(text_message(
                MessageRole::User,
                format!(
                    "<spine_memory>\n{}\n</spine_memory>",
                    render_memory(node_id.to_string().as_str(), parts)
                ),
            )),
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

fn response_item_at(rollout: &[RolloutItem], boundary: RawBoundary) -> Option<ResponseItem> {
    let index = usize::try_from(boundary.0).ok()?;
    match rollout
        .iter()
        .filter(|item| is_spine_source_item(item))
        .nth(index)?
    {
        RolloutItem::ResponseItem(item) => Some(item.clone()),
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

fn render_memory(node_id: &str, parts: &[MemoryPart]) -> String {
    let mut blocks = vec![format!("# Spine Memory {node_id}")];
    for part in parts {
        match part {
            MemoryPart::User { anchor, content } => {
                blocks.push(format!("## User Message [U{anchor}]\n{content}"));
            }
            MemoryPart::Child { node_id, parts } => {
                blocks.push(format!(
                    "## Child Memory\n{}",
                    render_memory(node_id.to_string().as_str(), parts)
                ));
            }
            MemoryPart::Model(memory) => {
                blocks.push(format!("## Node Memory\n{memory}"));
            }
        }
    }
    blocks.join("\n\n")
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
