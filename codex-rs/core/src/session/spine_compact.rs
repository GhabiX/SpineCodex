use std::collections::HashSet;

use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::context_manager::ContextManager;
use crate::session::Session;
use crate::session::turn::built_tools;
use crate::session::turn_context::TurnContext;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineError;
use crate::stream_events_utils::last_assistant_message_from_item;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

impl Session {
    pub(crate) async fn spine_compact_close(
        &self,
        turn_context: &TurnContext,
        history: &ContextManager,
        node_id: String,
        suffix_start: usize,
        close_output: &ResponseItem,
        instruction: Option<String>,
    ) -> Result<SpineCloseCompact, SpineError> {
        let raw_items = history.raw_items();
        if suffix_start >= raw_items.len() {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close suffix start {suffix_start} is outside history length {}",
                raw_items.len()
            )));
        }
        let mut prompt_input = history
            .clone()
            .for_prompt(&turn_context.model_info.input_modalities);
        let ResponseItem::FunctionCallOutput { call_id, .. } = close_output else {
            return Err(SpineError::InvalidEvent(
                "spine.close output missing call_id".to_string(),
            ));
        };
        let close_call_id = call_id.as_str();
        let Some(close_request_index) = raw_items.iter().position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall {
                    call_id,
                    namespace,
                    name,
                    ..
                } if call_id == close_call_id
                    && namespace.as_deref() == Some(SPINE_NAMESPACE)
                    && name == SPINE_TOOL_CLOSE
            )
        }) else {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close compact missing request for call {close_call_id}"
            )));
        };
        if close_request_index < suffix_start {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close request index {close_request_index} precedes suffix start {suffix_start}"
            )));
        }
        if close_request_index == suffix_start {
            return Err(SpineError::InvalidEvent(
                "spine.close requires non-empty live suffix".to_string(),
            ));
        }
        let original_prompt_len = prompt_input.len();
        prompt_input.retain(|item| !is_current_spine_close_carrier(item, close_call_id));
        if prompt_input.len() == original_prompt_len {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close compact prompt missing carrier for call {close_call_id}"
            )));
        }
        if suffix_start > prompt_input.len() {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close compact suffix start {suffix_start} exceeds prompt length {}",
                prompt_input.len()
            )));
        }
        let suffix_items = prompt_input.split_off(suffix_start);
        let compact_instructions =
            spine_close_compact_instruction_text(&node_id, instruction.as_deref());
        prompt_input.push(spine_close_compact_suffix_boundary_message(&node_id));
        prompt_input.extend(suffix_items);
        prompt_input.push(spine_close_compact_system_message(&compact_instructions));
        let tool_router = built_tools(
            self,
            turn_context,
            &prompt_input,
            &HashSet::new(),
            /*skills_outcome*/ None,
            &CancellationToken::new(),
        )
        .await
        .map_err(|err| {
            SpineError::InvalidEvent(format!("spine.close compact tool build failed: {err}"))
        })?;
        let prompt = Prompt {
            input: prompt_input,
            tools: tool_router.model_visible_specs(),
            parallel_tool_calls: false,
            tool_choice: "none".to_string(),
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let output = self
            .spine_close_summary_items(turn_context, &prompt)
            .await?;
        let body = spine_close_compact_body(&node_id, &output)?;
        Ok(SpineCloseCompact {
            body,
            source_context_range: suffix_start..close_request_index,
        })
    }

    async fn spine_close_summary_items(
        &self,
        turn_context: &TurnContext,
        prompt: &Prompt,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
        let client_session = self.services.model_client.new_session();
        let mut stream = client_session
            .stream_responses_api(
                prompt,
                &turn_context.model_info,
                &turn_context.session_telemetry,
                turn_context.reasoning_effort,
                turn_context.reasoning_summary,
                turn_context.config.service_tier.clone(),
                turn_metadata_header.as_deref(),
                &InferenceTraceContext::disabled(),
            )
            .await
            .map_err(|err| {
                SpineError::InvalidEvent(format!("spine.close compact failed: {err}"))
            })?;

        let mut output = Vec::new();
        loop {
            let Some(event) = stream.next().await else {
                return Err(SpineError::InvalidEvent(
                    "spine.close compact stream closed before response.completed".to_string(),
                ));
            };
            match event.map_err(|err| {
                SpineError::InvalidEvent(format!("spine.close compact failed: {err}"))
            })? {
                ResponseEvent::OutputItemDone(item) => {
                    output.push(item);
                }
                ResponseEvent::ServerReasoningIncluded(included) => {
                    self.set_server_reasoning_included(included).await;
                }
                ResponseEvent::RateLimits(snapshot) => {
                    self.update_rate_limits(turn_context, snapshot).await;
                }
                ResponseEvent::Completed { token_usage, .. } => {
                    self.update_token_usage_info(turn_context, token_usage.as_ref())
                        .await;
                    return Ok(output);
                }
                _ => {}
            }
        }
    }
}

fn spine_close_compact_instruction_text(node_id: &str, instruction: Option<&str>) -> String {
    let mut text = format!(
        "---------- Spine Compact Directive ----------\n\n\
You are writing runtime Spine memory for a closed task node. Write a Markdown memory, not a conversation reply.\n\
This memory will replace the raw transcript for Spine node {node_id} in future context.\n\n\
Summarize only this node's suffix as a compact continuity record for its parent scope:\n\
- Scope: the user's question, intent, and what this node was trying to resolve.\n\
- Current State: what is now known, decided, changed, or verified.\n\
- Evidence: important files, functions, commands, tests, results, ids, and failures.\n\
- Carry Forward: the user's current intent, facts the parent or later sibling needs, and the concrete direction for continuing the work if this task should keep moving after close.\n\
- Open Questions: unresolved decisions, blockers, or user-facing conclusions that still matter.\n\n\
Use prefix context only to interpret names and constraints. Use the optional close instruction as a hint about what to preserve.\n\
If the folded node completed its user-facing work, make the memory read as completed history. If the task should continue after this node closes, state the user's current intent and continuation direction as carry-forward context rather than a generic instruction."
    );
    if let Some(instruction) = instruction
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty())
    {
        text.push_str("\n\n");
        text.push_str(instruction);
    }
    text
}

fn spine_close_compact_system_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn spine_close_compact_suffix_boundary_message(node_id: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "---------- Spine Suffix Begin ----------\n\n\
Messages below this boundary are the suffix for Spine node {node_id}. Messages above are prefix context."
            ),
        }],
        phase: None,
    }
}

fn is_current_spine_close_carrier(item: &ResponseItem, close_call_id: &str) -> bool {
    matches!(
        item,
        ResponseItem::FunctionCall {
            call_id,
            namespace,
            name,
            ..
        } if call_id == close_call_id
            && namespace.as_deref() == Some(SPINE_NAMESPACE)
            && name == SPINE_TOOL_CLOSE
    ) || matches!(
        item,
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == close_call_id
    )
}

fn spine_close_compact_body(node_id: &str, output: &[ResponseItem]) -> Result<String, SpineError> {
    if let Some(item) = output.iter().find(|item| {
        matches!(
            item,
            ResponseItem::FunctionCall { .. }
                | ResponseItem::LocalShellCall { .. }
                | ResponseItem::CustomToolCall { .. }
                | ResponseItem::ToolSearchCall { .. }
                | ResponseItem::WebSearchCall { .. }
                | ResponseItem::ImageGenerationCall { .. }
        )
    }) {
        return Err(SpineError::InvalidEvent(format!(
            "spine.close compact produced unexpected tool call: {item:?}"
        )));
    }
    let mut entries = Vec::new();
    for item in output {
        if let ResponseItem::Message { role, .. } = item
            && role == "assistant"
            && let Some(text) = last_assistant_message_from_item(item, /*plan_mode*/ false)
            && !text.trim().is_empty()
        {
            entries.push(text);
        }
    }
    if entries.is_empty() {
        let encrypted_only = output.iter().any(|item| {
            matches!(
                item,
                ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
            )
        });
        return Err(SpineError::InvalidEvent(if encrypted_only {
            "spine.close compact produced no readable memory body".to_string()
        } else {
            "spine.close compact produced no memory body".to_string()
        }));
    }
    let mut body = format!("# Spine Memory {node_id}\n\n");
    body.push_str(&entries.join("\n\n"));
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(body)
}

#[cfg(test)]
mod spine_close_compact_body_tests {
    use super::*;

    fn assistant_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    #[test]
    fn spine_close_compact_body_uses_only_readable_assistant_summary() {
        let body = spine_close_compact_body(
            "1.1",
            &[
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "PREFIX_ONLY_SHOULD_NOT_APPEAR_IN_MEMORY".to_string(),
                    }],
                    phase: None,
                },
                ResponseItem::Compaction {
                    encrypted_content: "gAAAAencrypted".to_string(),
                },
                assistant_message("readable suffix memory"),
            ],
        )
        .expect("readable assistant summary should be accepted");

        assert!(body.contains("readable suffix memory"));
        assert!(!body.contains("PREFIX_ONLY_SHOULD_NOT_APPEAR_IN_MEMORY"));
        assert!(!body.contains("gAAAAencrypted"));
    }

    #[test]
    fn spine_close_compact_body_rejects_encrypted_only_output() {
        let err = spine_close_compact_body(
            "1.1",
            &[ResponseItem::Compaction {
                encrypted_content: "gAAAAencrypted".to_string(),
            }],
        )
        .expect_err("encrypted-only compact output must not become memory");

        assert!(
            err.to_string()
                .contains("spine.close compact produced no readable memory body"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn spine_close_compact_body_rejects_tool_call_output() {
        let err = spine_close_compact_body(
            "1.1",
            &[ResponseItem::FunctionCall {
                id: None,
                name: "close".to_string(),
                namespace: Some(SPINE_NAMESPACE.to_string()),
                arguments: "{}".to_string(),
                call_id: "compact-tool-call".to_string(),
            }],
        )
        .expect_err("compact output must be readable memory, not another tool call");

        assert!(
            err.to_string()
                .contains("spine.close compact produced unexpected tool call"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn spine_close_compact_body_does_not_repair_missing_evidence() {
        let body = spine_close_compact_body(
            "1.1",
            &[assistant_message("summary omitted the required sentinel")],
        )
        .expect("readable assistant summary should be accepted");

        assert!(body.contains("summary omitted the required sentinel"));
        assert!(!body.contains("## Required Evidence"));
        assert!(!body.contains("NEXT_SUFFIX_CRITICAL_ID=SPINE_NEXT_CACHE_SENTINEL_77"));
    }
}
