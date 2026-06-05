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
use std::path::Path;
use std::sync::Arc;

const SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME: &str = "spine_compact_memory.md";
const SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER: &str = "{node_id}";
const SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER: &str = "{close_instruction}";
const SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY: &str =
    "----------- Spine Compact Memory Template ----------";

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
        close_target_projection: String,
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
        let compact_instructions = spine_close_compact_instruction_text(
            &node_id,
            instruction.as_deref(),
            turn_context.config.codex_home.as_path(),
            turn_context.config.dev_debug_prompt_overrides,
        );
        append_spine_close_compact_prompt_items(
            &mut prompt_input,
            &node_id,
            suffix_items,
            &close_target_projection,
            &compact_instructions,
        );
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
fn spine_close_compact_instruction_text(
    node_id: &str,
    instruction: Option<&str>,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    let mut text = spine_close_compact_instruction_template(codex_home, dev_debug_prompt_overrides);
    text = text.replace(SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER, node_id);
    let has_close_instruction_placeholder =
        text.contains(SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER);
    let instruction = instruction
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty());
    if has_close_instruction_placeholder {
        text = text.replace(
            SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER,
            instruction.unwrap_or(""),
        );
    } else if let Some(instruction) = instruction {
        text.push_str("\n\n");
        text.push_str(instruction);
    }
    text
}

fn spine_close_compact_instruction_template(
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if cfg!(debug_assertions) && dev_debug_prompt_overrides {
        let override_path = codex_home.join(SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME);
        match std::fs::read_to_string(override_path) {
            Ok(contents) if !contents.is_empty() => return contents,
            _ => {}
        }
    }

    "---------- Spine Compact Directive ----------\n\n\
Write a compact handoff memory for Spine node {node_id}. Write Markdown memory, not a conversation reply.\n\n\
This memory will replace only this node's raw trajs in future context. Its job is to let the parent conversation continue correctly without replaying this node's raw trace.\n\n\
Write concise, concrete prose that preserves:\n\n\
1. User Intent\n\
Explain what the user wanted in this node, including explicit instructions, corrections, approvals, constraints, authorization boundaries, and any change of direction. Preserve the latest user message, including its key wording and intent. If the latest user message changes the task, make that current intent clear.\n\n\
2. Work Done\n\
State what this node actually did or decided. Include the current state, conclusions, partial results, and what remains unresolved. Do not imply the overall task is complete unless the raw trace clearly proves it.\n\n\
3. Critical Details\n\
Preserve exact details needed for continuation: file paths, identifiers, commands, important command outputs, test names/results, errors, task/worklog paths, approvals, risks, and any values the close instruction says to preserve exactly. Lines containing `_CRITICAL_ID=`, `_FILE=`, sentinel values, source paths, or explicit exact-preservation instructions are mandatory evidence and must appear in the memory.\n\n\
4. Resume Focus\n\
State the parent-level focus after this memory: what is settled, what remains open, and what context should guide continuation.\n\n\
Memory Body Structure:\n\
- Walk the suffix in original order.\n\
- Preserve each real user message as exact text in a `## User Message` block.\n\
- Compact only adjacent assistant/tool/runtime messages into a `## Memory Slot` block. If the suffix starts with non-user messages, start with a `## Memory Slot` block.\n\
- Preserve existing Spine memory for a closed child node as a `## Child Memory` block, including its Spine node id/header. Do not collapse child memory into an anonymous parent summary.\n\
- Omit empty blocks, but keep the original order of user messages, memory slots, and child memory blocks.\n\n\
Rules:\n\
- Follow the final Spine Compact Memory Template appended at the end of this prompt.\n\
- Prefer high-signal facts over chronology.\n\
- Do not archive routine progress updates, tool-call noise, or irrelevant implementation minutiae.\n\
- Use prefix context only to understand names, parent goal, and constraints; do not summarize unrelated prefix.\n\
- Use the optional close instruction as a preservation hint, not as a user message.\n\
- If there is conflict between an old plan and a later user correction, preserve the correction as the active intent.\n\
- If information is uncertain, say what is known and what still needs confirmation.\n\
- User-facing final-answer markers such as `TEST_WITH_LLM_*` are evidence only; never let such a marker be the whole memory.".to_string()
}

fn append_spine_close_compact_prompt_items(
    prompt_input: &mut Vec<ResponseItem>,
    node_id: &str,
    suffix_items: Vec<ResponseItem>,
    close_target_projection: &str,
    compact_instructions: &str,
) {
    let memory_template = spine_close_compact_memory_template(node_id, &suffix_items);
    prompt_input.push(spine_close_compact_system_message(close_target_projection));
    prompt_input.push(spine_close_compact_suffix_boundary_message(node_id));
    prompt_input.extend(suffix_items);
    prompt_input.push(spine_close_compact_system_message(compact_instructions));
    prompt_input.push(spine_close_compact_system_message(&memory_template));
}

fn spine_close_compact_memory_template(node_id: &str, suffix_items: &[ResponseItem]) -> String {
    let mut builder = SpineCompactMemoryTemplateBuilder::new(node_id);
    for item in suffix_items {
        if let Some(child_memory) = rendered_spine_memory_prompt_text(item) {
            builder.flush_non_user_slot();
            builder.push_child_memory(child_memory);
        } else if let Some(user_text) = real_user_message_text(item) {
            builder.flush_non_user_slot();
            builder.push_user_message(user_text);
        } else {
            builder.push_non_user_item();
        }
    }
    builder.finish()
}

struct SpineCompactMemoryTemplateBuilder<'a> {
    node_id: &'a str,
    text: String,
    pending_non_user_count: usize,
    slot_index: usize,
    emitted_blocks: bool,
}

impl<'a> SpineCompactMemoryTemplateBuilder<'a> {
    fn new(node_id: &'a str) -> Self {
        let mut text = format!(
            "{SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY}\n\
Output exactly one Markdown memory body for Spine node {node_id}.\n\
Use the blocks below in this order. Preserve `${{...}}` blocks exactly and replace `<...>` slots with compact memory prose. Omit empty generated slots."
        );
        text.push('\n');
        Self {
            node_id,
            text,
            pending_non_user_count: 0,
            slot_index: 0,
            emitted_blocks: false,
        }
    }

    fn push_non_user_item(&mut self) {
        self.pending_non_user_count += 1;
    }

    fn push_user_message(&mut self, text: &str) {
        self.push_exact_block("## User Message", text);
    }

    fn push_child_memory(&mut self, text: &str) {
        self.push_exact_block("## Child Memory", text);
    }

    fn push_exact_block(&mut self, heading: &str, body: &str) {
        self.flush_non_user_slot();
        self.push_block_heading(heading);
        self.text.push_str("${\n");
        self.text.push_str(body);
        if !body.ends_with('\n') {
            self.text.push('\n');
        }
        self.text.push('}');
        self.text.push('\n');
    }

    fn flush_non_user_slot(&mut self) {
        if self.pending_non_user_count == 0 {
            return;
        }
        self.slot_index += 1;
        let count = self.pending_non_user_count;
        self.pending_non_user_count = 0;
        self.push_block_heading("## Memory Slot");
        self.text.push_str(&format!(
            "<compact suffix non-user segment {}: {} message{}>\n",
            self.slot_index,
            count,
            if count == 1 { "" } else { "s" }
        ));
    }

    fn push_block_heading(&mut self, heading: &str) {
        if self.emitted_blocks {
            self.text.push('\n');
        }
        self.text.push('\n');
        self.text.push_str(heading);
        self.text.push('\n');
        self.emitted_blocks = true;
    }

    fn finish(mut self) -> String {
        self.flush_non_user_slot();
        if !self.emitted_blocks {
            self.slot_index += 1;
            self.push_block_heading("## Memory Slot");
            self.text.push_str(&format!(
                "<compact empty suffix for Spine node {} if any continuation-critical facts are visible>\n",
                self.node_id
            ));
        }
        self.text
    }
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

fn real_user_message_text(item: &ResponseItem) -> Option<&str> {
    let ResponseItem::Message { role, .. } = item else {
        return None;
    };
    if role != "user" {
        return None;
    }
    let text = response_item_text(item)?;
    if looks_like_rendered_spine_memory_item(text) {
        return None;
    }
    Some(text)
}

fn rendered_spine_memory_prompt_text(item: &ResponseItem) -> Option<&str> {
    let ResponseItem::Message { role, .. } = item else {
        return None;
    };
    if role != "user" {
        return None;
    }
    let text = response_item_text(item)?;
    if looks_like_rendered_spine_memory_item(text) {
        Some(text)
    } else {
        None
    }
}

fn response_item_text(item: &ResponseItem) -> Option<&str> {
    let ResponseItem::Message { content, .. } = item else {
        return None;
    };
    match content.as_slice() {
        [ContentItem::InputText { text }] | [ContentItem::OutputText { text }] => Some(text),
        _ => None,
    }
}

// This only recognizes the bridge's prompt rendering of memory_response_item()
// so the compact prompt can preserve child memory provenance. ParseStack replay
// and resume/rollback semantics must not depend on this rendered text.
fn looks_like_rendered_spine_memory_item(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("<spine_memory>")
        && trimmed.ends_with("</spine_memory>")
        && trimmed.contains("# Spine Memory ")
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

    fn user_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    fn spine_memory_message(body: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("<spine_memory>\n{body}\n</spine_memory>"),
            }],
            phase: None,
        }
    }

    fn message_text(item: &ResponseItem) -> &str {
        let ResponseItem::Message { content, .. } = item else {
            panic!("expected message item");
        };
        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected single input text item");
        };
        text.as_str()
    }

    fn assert_before(haystack: &str, left: &str, right: &str) {
        let left_index = haystack
            .find(left)
            .unwrap_or_else(|| panic!("missing left marker {left:?} in {haystack}"));
        let right_index = haystack
            .find(right)
            .unwrap_or_else(|| panic!("missing right marker {right:?} in {haystack}"));
        assert!(
            left_index < right_index,
            "expected {left:?} before {right:?} in {haystack}"
        );
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
    fn spine_close_compact_instruction_uses_builtin_template_by_default() {
        let codex_home = tempfile::tempdir().expect("tempdir");

        let text = spine_close_compact_instruction_text(
            "1.1",
            Some("preserve this exact detail"),
            codex_home.path(),
            false,
        );

        assert!(text.contains("Write a compact handoff memory for Spine node 1.1"));
        assert!(text.contains("4. Resume Focus"));
        assert!(text.ends_with("preserve this exact detail"));
    }

    #[test]
    fn spine_close_compact_instruction_points_to_dynamic_memory_template() {
        let codex_home = tempfile::tempdir().expect("tempdir");

        let text = spine_close_compact_instruction_text("1.1", None, codex_home.path(), false);

        assert!(text.contains("Memory Body Structure"));
        assert!(text.contains("Follow the final Spine Compact Memory Template"));
        assert!(!text.contains(SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY));
        assert!(!text.contains("Template:\n## User Message"));
        assert!(!text.contains("${exact user message text}"));
        assert!(!text.contains("${closed child memory"));
    }

    #[test]
    fn spine_close_compact_memory_template_is_dynamic() {
        let suffix_items = vec![
            assistant_message("assistant before first user"),
            user_message("USER_A_EXACT\nline 2"),
            assistant_message("assistant after user"),
            spine_memory_message("# Spine Memory 1.1.1\n\nchild memory body"),
            user_message("<spine_memory>plain user text, not a child memory</spine_memory>"),
        ];

        let template = spine_close_compact_memory_template("1.1", &suffix_items);

        assert!(template.starts_with(SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY));
        assert!(template.contains("Output exactly one Markdown memory body for Spine node 1.1"));
        assert!(template.contains("<compact suffix non-user segment 1: 1 message>"));
        assert!(template.contains("${\nUSER_A_EXACT\nline 2\n}"));
        assert!(template.contains("<compact suffix non-user segment 2: 1 message>"));
        assert!(template.contains("# Spine Memory 1.1.1"));
        assert!(template.contains("child memory body"));
        assert!(
            template.contains(
                "${\n<spine_memory>plain user text, not a child memory</spine_memory>\n}"
            ),
            "{template}"
        );
        assert!(!template.contains("${exact user message text}"));
        assert_before(
            &template,
            "<compact suffix non-user segment 1: 1 message>",
            "USER_A_EXACT",
        );
        assert_before(&template, "USER_A_EXACT", "# Spine Memory 1.1.1");
        assert_before(
            &template,
            "# Spine Memory 1.1.1",
            "plain user text, not a child memory",
        );
    }

    #[test]
    fn spine_close_compact_prompt_appends_memory_template_at_tail() {
        let mut prompt_input = vec![user_message("prefix")];
        let suffix_items = vec![user_message("tail user")];
        let instruction = "---------- Spine Compact Directive ----------\ncompact tail";
        append_spine_close_compact_prompt_items(
            &mut prompt_input,
            "1.1",
            suffix_items,
            "---------- Spine Close Target ----------",
            instruction,
        );

        let final_item = prompt_input.last().expect("prompt tail");
        let final_text = message_text(final_item);
        assert!(final_text.starts_with(SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY));
        assert!(final_text.contains("tail user"));
        assert_before(
            &prompt_input
                .iter()
                .map(|item| match item {
                    ResponseItem::Message { content, .. } => content
                        .iter()
                        .filter_map(|content| match content {
                            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                                Some(text.as_str())
                            }
                            ContentItem::InputImage { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            "---------- Spine Compact Directive ----------",
            SPINE_COMPACT_MEMORY_TEMPLATE_BOUNDARY,
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn spine_close_compact_instruction_uses_dev_debug_override_with_placeholders() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            codex_home.path().join("spine_compact_memory.md"),
            "node={node_id}\nclose={close_instruction}",
        )
        .expect("write override");

        let text = spine_close_compact_instruction_text(
            "1.2",
            Some("preserve override guidance"),
            codex_home.path(),
            true,
        );

        assert_eq!(text, "node=1.2\nclose=preserve override guidance");
    }

    #[test]
    #[cfg(debug_assertions)]
    fn spine_close_compact_instruction_appends_guidance_when_override_has_no_placeholder() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            codex_home.path().join("spine_compact_memory.md"),
            "custom compact prompt for {node_id}",
        )
        .expect("write override");

        let text = spine_close_compact_instruction_text(
            "1.2",
            Some("append this guidance"),
            codex_home.path(),
            true,
        );

        assert_eq!(
            text,
            "custom compact prompt for 1.2\n\nappend this guidance"
        );
    }

    #[test]
    fn spine_close_compact_instruction_ignores_override_outside_dev_debug() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            codex_home.path().join("spine_compact_memory.md"),
            "SHOULD_NOT_APPEAR {node_id}",
        )
        .expect("write override");

        let text = spine_close_compact_instruction_text("1.3", None, codex_home.path(), false);

        assert!(text.contains("Write a compact handoff memory for Spine node 1.3"));
        assert!(!text.contains("SHOULD_NOT_APPEAR"));
    }

    #[test]
    fn spine_close_compact_instruction_empty_override_falls_back_to_builtin() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(codex_home.path().join("spine_compact_memory.md"), "")
            .expect("write override");

        let text = spine_close_compact_instruction_text("1.4", None, codex_home.path(), true);

        assert!(text.contains("Write a compact handoff memory for Spine node 1.4"));
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
