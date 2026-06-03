use crate::Prompt;
use crate::client::ModelClientSession;
use crate::client::ResponsesToolChoice;
use crate::client_common::ResponseEvent;
use crate::compact::InitialContextInjection;
use crate::context_manager::ContextManager;
use crate::session::Session;
use crate::session::turn::run_auto_compact;
use crate::session::turn_context::TurnContext;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineError;
use crate::spine::is_spine_close_like_tool_name;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::util::backoff;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use std::sync::Arc;

impl Session {
    pub(crate) async fn spine_compact_close(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        native_compact_client_session: &mut ModelClientSession,
        history: &ContextManager,
        node_id: String,
        suffix_start: usize,
        close_output: &ResponseItem,
        instruction: Option<String>,
    ) -> Result<SpineCloseCompactOutcome, SpineError> {
        let raw_items = history.raw_items();
        if suffix_start >= raw_items.len() {
            return Err(SpineError::Operation(format!(
                "spine.close suffix start {suffix_start} is outside history length {} for node {node_id}",
                raw_items.len()
            )));
        }
        let mut prompt_input = history
            .clone()
            .for_prompt(&turn_context.model_info.input_modalities);
        let ResponseItem::FunctionCallOutput { call_id, .. } = close_output else {
            return Err(SpineError::Operation(
                "spine.close output missing call_id".to_string(),
            ));
        };
        let close_call_id = call_id.as_str();
        let close_request_index = raw_items.iter().position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall {
                    call_id,
                    namespace,
                    name,
                    ..
                } if call_id == close_call_id
                    && namespace.as_deref() == Some(SPINE_NAMESPACE)
                    && is_spine_close_like_tool_name(name)
            )
        });
        let close_context_end = close_request_index.unwrap_or(raw_items.len());
        if let Some(close_request_index) = close_request_index
            && close_request_index < suffix_start
        {
            return Err(SpineError::Operation(format!(
                "spine.close request index {close_request_index} precedes suffix start {suffix_start} for node {node_id} call_id={close_call_id}"
            )));
        }
        if close_context_end == suffix_start {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node_id} call_id={close_call_id}"
            )));
        }
        let original_prompt_len = prompt_input.len();
        prompt_input.retain(|item| !is_current_spine_close_like_carrier(item, close_call_id));
        if close_request_index.is_some() && prompt_input.len() == original_prompt_len {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact prompt missing carrier for call {close_call_id} node {node_id}"
            )));
        }
        if suffix_start > prompt_input.len() {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact suffix start {suffix_start} exceeds prompt length {} for node {node_id} call_id={close_call_id}",
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
            tools: Vec::new(),
            parallel_tool_calls: false,
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let (output, token_usage) = match self
            .spine_close_summary_items(turn_context, native_compact_client_session, prompt)
            .await?
        {
            SpineCloseSummaryOutcome::Output {
                output,
                token_usage,
            } => (output, token_usage),
            SpineCloseSummaryOutcome::NativeCompacted {
                reset_client_session,
            } => {
                return Ok(SpineCloseCompactOutcome::NativeCompacted {
                    reset_client_session,
                });
            }
        };
        let body = spine_close_compact_body(&node_id, &output)?;
        Ok(SpineCloseCompactOutcome::Compact(SpineCloseCompact {
            body,
            source_context_range: suffix_start..close_context_end,
            memory_output_tokens: token_usage.map(|usage| usage.output_tokens),
        }))
    }

    async fn spine_close_summary_items(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        native_compact_client_session: &mut ModelClientSession,
        prompt: Prompt,
    ) -> Result<SpineCloseSummaryOutcome, SpineError> {
        let max_retries = turn_context.provider.info().stream_max_retries();
        let mut retries = 0;

        loop {
            match self
                .spine_close_summary_items_once(
                    turn_context,
                    native_compact_client_session,
                    &prompt,
                )
                .await
            {
                Ok(output) => {
                    return Ok(SpineCloseSummaryOutcome::Output {
                        output: output.0,
                        token_usage: output.1,
                    });
                }
                Err(CodexErr::Interrupted) => {
                    return Err(SpineError::CompactFailure(
                        "spine.close compact interrupted".to_string(),
                    ));
                }
                Err(CodexErr::ContextWindowExceeded) => {
                    self.set_total_tokens_full(turn_context).await;
                    let reset_client_session = Box::pin(run_auto_compact(
                        self,
                        turn_context,
                        native_compact_client_session,
                        InitialContextInjection::BeforeLastUserMessage,
                        CompactionReason::ContextLimit,
                        CompactionPhase::MidTurn,
                    ))
                    .await
                    .map_err(|err| {
                        SpineError::CompactFailure(format!(
                            "spine.close compact triggered native compact, but native compact failed: {err}"
                        ))
                    })?;
                    return Ok(SpineCloseSummaryOutcome::NativeCompacted {
                        reset_client_session,
                    });
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
                    return Err(SpineError::CompactFailure(format!(
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
                ResponsesToolChoice::None,
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

pub(crate) enum SpineCloseCompactOutcome {
    Compact(SpineCloseCompact),
    NativeCompacted { reset_client_session: bool },
}

enum SpineCloseSummaryOutcome {
    Output {
        output: Vec<ResponseItem>,
        token_usage: Option<TokenUsage>,
    },
    NativeCompacted {
        reset_client_session: bool,
    },
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
Write a compact handoff memory for Spine node {node_id}. Write Markdown memory, not a conversation reply.\n\n\
This memory will replace only this node's raw trajs in future context. Its job is to let the parent conversation continue correctly without replaying this node's raw trace.\n\n\
Write concise, concrete prose that preserves:\n\n\
1. User Intent\n\
Explain what the user wanted in this node, including explicit instructions, corrections, approvals, constraints, authorization boundaries, and any change of direction. If the latest user message changes the task, make that current intent clear.\n\n\
2. Work Done\n\
State what this node actually did or decided. Include the current state, conclusions, partial results, and what remains unresolved. Do not imply the overall task is complete unless the raw trace clearly proves it.\n\n\
3. Critical Details\n\
Preserve exact details needed for continuation: file paths, identifiers, commands, important command outputs, test names/results, errors, task/worklog paths, approvals, risks, and any values the close instruction says to preserve exactly. Lines containing `_CRITICAL_ID=`, `_FILE=`, sentinel values, source paths, or explicit exact-preservation instructions are mandatory evidence and must appear in the memory.\n\n\
4. Next Step\n\
State how the parent should continue after this memory. Be specific: continue implementation, review a decision, answer the user, wait for approval, run tests, switch back to a corrected user request, or return a subproblem result to the parent.\n\n\
Rules:\n\
- Prefer high-signal facts over chronology.\n\
- Do not archive routine progress updates, tool-call noise, or irrelevant implementation minutiae.\n\
- Use prefix context only to understand names, parent goal, and constraints; do not summarize unrelated prefix.\n\
- Use the optional close instruction as a preservation hint, not as a user message.\n\
- If there is conflict between an old plan and a later user correction, preserve the correction as the active intent.\n\
- If information is uncertain, say what is known and what still needs confirmation.\n\
- User-facing final-answer markers such as `TEST_WITH_LLM_*` are evidence only; never let such a marker be the whole memory."
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

fn is_current_spine_close_like_carrier(item: &ResponseItem, close_call_id: &str) -> bool {
    matches!(
        item,
        ResponseItem::FunctionCall {
            call_id,
            namespace,
            name,
            ..
        } if call_id == close_call_id
            && namespace.as_deref() == Some(SPINE_NAMESPACE)
            && is_spine_close_like_tool_name(name)
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
        return Err(SpineError::CompactFailure(format!(
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
        return Err(SpineError::CompactFailure(if encrypted_only {
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
    use crate::spine::SPINE_TOOL_CLOSE;
    use crate::spine::SPINE_TOOL_NEXT;
    use crate::spine::SPINE_TOOL_OPEN;
    use crate::spine::SPINE_TOOL_TREE;
    use codex_protocol::models::FunctionCallOutputPayload;

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

    fn spine_call(name: &str, call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: name.to_string(),
            namespace: Some(SPINE_NAMESPACE.to_string()),
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
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

    #[test]
    fn spine_close_like_carrier_filters_only_close_like_tools() {
        assert!(is_current_spine_close_like_carrier(
            &spine_call(SPINE_TOOL_CLOSE, "call-1"),
            "call-1"
        ));
        assert!(is_current_spine_close_like_carrier(
            &spine_call(SPINE_TOOL_NEXT, "call-1"),
            "call-1"
        ));
        assert!(!is_current_spine_close_like_carrier(
            &spine_call(SPINE_TOOL_OPEN, "call-1"),
            "call-1"
        ));
        assert!(!is_current_spine_close_like_carrier(
            &spine_call(SPINE_TOOL_TREE, "call-1"),
            "call-1"
        ));
        assert!(!is_current_spine_close_like_carrier(
            &spine_call(SPINE_TOOL_CLOSE, "other"),
            "call-1"
        ));
        assert!(is_current_spine_close_like_carrier(
            &ResponseItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload::from_text("ok".to_string()),
            },
            "call-1"
        ));
    }
}
