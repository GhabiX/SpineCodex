use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::context_manager::ContextManager;
use crate::session::Session;
use crate::session::turn_context::TurnContext;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineError;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::util::backoff;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;

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
        let prompt = Prompt {
            input: prompt_input,
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let (output, token_usage) = self.spine_close_summary_items(turn_context, prompt).await?;
        let body = spine_close_compact_body(&node_id, &output)?;
        Ok(SpineCloseCompact {
            body,
            source_context_range: suffix_start..close_request_index,
            memory_output_tokens: token_usage.map(|usage| usage.output_tokens),
        })
    }

    async fn spine_close_summary_items(
        &self,
        turn_context: &TurnContext,
        mut prompt: Prompt,
    ) -> Result<(Vec<ResponseItem>, Option<TokenUsage>), SpineError> {
        let client_session = self.services.model_client.new_session();
        let max_retries = turn_context.provider.info().stream_max_retries();
        let mut retries = 0;

        loop {
            match self
                .spine_close_summary_items_once(turn_context, &client_session, &prompt)
                .await
            {
                Ok(output) => return Ok(output),
                Err(CodexErr::Interrupted) => {
                    return Err(SpineError::InvalidEvent(
                        "spine.close compact interrupted".to_string(),
                    ));
                }
                Err(e @ CodexErr::ContextWindowExceeded) => {
                    if prompt.input.len() > 1 {
                        // Keep the close path moving under window pressure. This is a
                        // last-resort recovery path: trim the oldest item first so the
                        // live suffix and compact directive stay intact, even if some
                        // prefix reuse has to be sacrificed.
                        prompt.input.remove(0);
                        retries = 0;
                        continue;
                    }
                    self.set_total_tokens_full(turn_context).await;
                    self.send_event(turn_context, EventMsg::Error(e.to_error_event(None)))
                        .await;
                    return Err(SpineError::InvalidEvent(format!(
                        "spine.close compact failed: {e}"
                    )));
                }
                Err(e) => {
                    if retries < max_retries {
                        retries += 1;
                        self.notify_stream_error(
                            turn_context,
                            format!("Reconnecting... {retries}/{max_retries}"),
                            e,
                        )
                        .await;
                        tokio::time::sleep(backoff(retries)).await;
                        continue;
                    }
                    self.send_event(turn_context, EventMsg::Error(e.to_error_event(None)))
                        .await;
                    return Err(SpineError::InvalidEvent(format!(
                        "spine.close compact failed: {e}"
                    )));
                }
            }
        }
    }

    async fn spine_close_summary_items_once(
        &self,
        turn_context: &TurnContext,
        client_session: &crate::client::ModelClientSession,
        prompt: &Prompt,
    ) -> Result<(Vec<ResponseItem>, Option<TokenUsage>), CodexErr> {
        let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
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
            .await?;

        let mut output = Vec::new();
        loop {
            let Some(event) = stream.next().await else {
                return Err(CodexErr::Fatal(
                    "spine.close compact stream closed before response.completed".to_string(),
                ));
            };
            match event? {
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
                    return Ok((output, token_usage));
                }
                _ => {}
            }
        }
    }
}

/// Builds the system directive that turns a closed Spine node into durable memory.
///
/// The close pass is a handoff artifact, not a conversation reply. The directive
/// therefore asks for readable Markdown and calls out exact identifiers, file
/// paths, sentinels, and test names so later turns and review can reconnect the
/// compacted memory to the archived suffix without replaying the raw trace.
fn spine_close_compact_instruction_text(node_id: &str, instruction: Option<&str>) -> String {
    let mut text = format!(
        "---------- Spine Compact Directive ----------\n\n\
Write a compact continuation memory for Spine node {node_id}. Write a Markdown memory, not a conversation reply.\n\
This memory will replace this node's raw trajs in future context.\n\n\
Write enough concrete continuity detail for the agent to continue naturally after this node's raw trajs are replaced by memory, and to trust the compressed history without replaying the full raw trace segment. Preserve decisions, constraints, exact identifiers, file paths, commands, test results, failures, approvals, and unresolved risks when they affect future work. Use the sections as guide rails: write concise, concrete prose, and prefer high-signal specifics over a chronological transcript.\n\n\
Use four sections:\n\n\
Motivation:\n\
Explain why this node existed, what the user was trying to accomplish, what constraints or authorization boundaries mattered, and what problem context is needed to understand the node.\n\n\
Judgment:\n\
Record the conclusions, decisions, accepted plan, or current technical understanding produced by this node. Include the main alternatives rejected only when they matter for future reasoning.\n\n\
Evidence:\n\
Preserve decisive facts that support the judgment or are needed to continue: important file paths, code locations, changed artifacts, key commands with outcomes, failed commands with reasons, review or user approval, task/worklog paths, and unresolved risks. Keep command output to the meaningful result, not full logs.\n\n\
Continuation:\n\
Explain how the parent conversation should naturally proceed from this memory. State the immediate next parent action. If this node closes because a subproblem finished, state the compact result returned to the parent. If the suffix ends with a direct user question, correction, or redirect, that latest user intent is the next parent action unless the user explicitly asked otherwise. State whether the parent work should continue, wait for the user, report status, or treat later questions as review of completed work. Do not imply the overall user goal is complete unless the raw trace clearly establishes that.\n\n\
Hard requirement: preserve exact critical identifiers, sentinel strings, file paths, commands, and test names from the suffix and from the optional close instruction. If the instruction says to preserve a value exactly, copy that value verbatim into the memory.\n\n\
Lines containing `_CRITICAL_ID=`, `_FILE=`, sentinel values, or source paths are mandatory evidence and must appear in the memory. User-facing final-answer markers such as `TEST_WITH_LLM_*` are evidence only; never let such a marker be the whole memory.\n\n\
Use prefix context only to interpret names and constraints. Use the optional close instruction as a hint about what to preserve.\n\
Do not write a chronological report. Do not include routine progress updates, tool-call noise, or implementation minutiae unless they are needed as evidence. The goal is to preserve enough concrete continuation context after replacing raw trajs with memory, not to archive every event."
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

/// Extracts the stored memory body from the compact response.
///
/// Only readable assistant text is persisted because the resulting memory must be
/// inspectable in later turns and survive as Markdown. Tool calls or encrypted-only
/// output are rejected instead of being silently folded into the archive.
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
