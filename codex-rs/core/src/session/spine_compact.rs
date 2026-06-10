use crate::Prompt;
use crate::client::ModelClientSession;
use crate::client::ResponsesToolChoice;
use crate::client_common::ResponseEvent;
use crate::compact::InitialContextInjection;
use crate::context_manager::ContextManager;
use crate::context_manager::normalize_prompt_items;
use crate::session::Session;
use crate::session::turn::run_auto_compact;
use crate::session::turn_context::TurnContext;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineCompactSourceEntryKind;
use crate::spine::SpineCompactSourcePlan;
use crate::spine::SpineError;
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
use codex_tools::ToolSpec;
use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;

const SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME: &str = "spine_compact_memory.md";
const SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER: &str = "{node_id}";
const SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER: &str = "{close_instruction}";
const SPINE_COMPACT_SLOT_MAP_BOUNDARY: &str = "---------- Spine Compact Memory ----------";
const SPINE_CLOSE_COMPACT_MAX_FORMAT_REPAIRS: usize = 1;

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
        source_plan: SpineCompactSourcePlan,
        close_compact_tools: Vec<ToolSpec>,
    ) -> Result<SpineCloseCompactOutcome, SpineError> {
        let raw_items = history.raw_items();
        if suffix_start >= raw_items.len() {
            return Err(SpineError::Operation(format!(
                "spine.close suffix start {suffix_start} is outside history length {} for node {node_id}",
                raw_items.len()
            )));
        }
        let ResponseItem::FunctionCallOutput { call_id, .. } = close_output else {
            return Err(SpineError::Operation(
                "spine.close output missing call_id".to_string(),
            ));
        };
        let close_call_id = call_id.as_str();
        if source_plan.node_id.to_string() != node_id {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source plan node {} does not match close node {node_id}",
                source_plan.node_id
            )));
        }
        if source_plan.source_context_range.start != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source range starts at {}, expected suffix start {suffix_start} for node {node_id}",
                source_plan.source_context_range.start
            )));
        }
        if source_plan.source_context_range.end > raw_items.len() {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source range end {} exceeds history length {} for node {node_id}",
                source_plan.source_context_range.end,
                raw_items.len()
            )));
        }
        if source_plan.entries.is_empty() {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source plan is empty for node {node_id} call_id={close_call_id}"
            )));
        }
        validate_source_plan_against_history(&source_plan, raw_items, close_call_id)?;
        let skeleton = SpineCompactMemorySkeleton::from_source_plan(&node_id, &source_plan)?;

        let mut prompt_input = normalize_prompt_items(
            raw_items[..source_plan.source_context_range.end].to_vec(),
            &turn_context.model_info.input_modalities,
        );
        let compact_instructions = spine_close_compact_instruction_text(
            &node_id,
            instruction.as_deref(),
            turn_context.config.codex_home.as_path(),
            turn_context.config.dev_debug_prompt_overrides,
        );
        append_spine_close_compact_prompt_items(
            &mut prompt_input,
            &node_id,
            &close_target_projection,
            &compact_instructions,
            &skeleton,
        )?;
        let mut prompt = Prompt {
            input: prompt_input,
            tools: close_compact_tools,
            parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let mut repair_attempts = 0;
        let mut memory_output_tokens = None;
        let body = loop {
            let (output, token_usage) = match self
                .spine_close_summary_items(
                    turn_context,
                    native_compact_client_session,
                    prompt.clone(),
                    ResponsesToolChoice::None,
                )
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
            add_compact_output_tokens(&mut memory_output_tokens, token_usage.as_ref());
            match spine_close_compact_body_result(&node_id, &output, &skeleton) {
                Ok(body) => break body,
                Err(err)
                    if err.is_repairable()
                        && repair_attempts < SPINE_CLOSE_COMPACT_MAX_FORMAT_REPAIRS =>
                {
                    repair_attempts += 1;
                    prompt.input.push(spine_close_compact_developer_message(
                        &spine_close_compact_repair_text(&err, &skeleton),
                    ));
                }
                Err(err) => return Err(err.into_spine_error()),
            }
        };
        Ok(SpineCloseCompactOutcome::Compact(SpineCloseCompact {
            body,
            source_context_range: source_plan.source_context_range,
            source_raw_range: source_plan.source_raw_range,
            memory_output_tokens,
        }))
    }

    async fn spine_close_summary_items(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        native_compact_client_session: &mut ModelClientSession,
        prompt: Prompt,
        tool_choice: ResponsesToolChoice,
    ) -> Result<SpineCloseSummaryOutcome, SpineError> {
        let max_retries = turn_context.provider.info().stream_max_retries();
        let mut retries = 0;

        loop {
            match self
                .spine_close_summary_items_once(
                    turn_context,
                    native_compact_client_session,
                    &prompt,
                    tool_choice,
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
        tool_choice: ResponsesToolChoice,
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
                tool_choice,
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

/// Builds the close directive that turns a closed Spine node into durable memory.
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
Write compact handoff memory for Spine node {node_id}. Return only XML-like <SPINE_SLOT_N> blocks for optional evidence slots you choose to preserve and exactly one <SPINE_NODE_MEMORY> block for the whole node handoff. Do not return a conversation reply, JSON, code fences, or a tool call.\n\n\
The runtime will assemble the final Markdown memory from trusted exact user messages, trusted child memory bodies, your optional generated slot bodies, and your required node memory. The final memory will replace only this node's raw trajs in future context. Its job is to let the parent conversation continue correctly without replaying this node's raw trace.\n\n\
For optional Memory Slots and the required Node Memory, write concise, concrete prose that preserves:\n\n\
1. User Intent\n\
Explain what the user wanted in this node, including explicit instructions, corrections, approvals, constraints, authorization boundaries, and any change of direction. Preserve the latest user message, including its key wording and intent. If the latest user message changes the task, make that current intent clear.\n\n\
2. Work Done\n\
State what this node actually did or decided. Include the current state, conclusions, partial results, and what remains unresolved. Do not imply the overall task is complete unless the raw trace clearly proves it.\n\n\
3. Critical Details\n\
Preserve exact details needed for continuation: file paths, identifiers, commands, important command outputs, test names/results, errors, task/worklog paths, approvals, risks, and any values the close instruction says to preserve exactly. Lines containing `_CRITICAL_ID=`, `_FILE=`, sentinel values, source paths, or explicit exact-preservation instructions are mandatory evidence and must appear in the memory.\n\n\
4. Resume Focus\n\
State the parent-level focus after this memory: what is settled, what remains open, and what context should guide continuation.\n\n\
Slot Map Structure:\n\
- Walk the suffix in original order.\n\
- USER_MSG blocks and Child Memory blocks in the final slot map are runtime-owned evidence. Do not copy or rewrite them into generated slot bodies.\n\
- Optional SPINE_SLOT markers identify source spans that are not runtime-preserved as exact USER_MSG or Child Memory evidence. These spans can include assistant/tool/runtime work and non-exact user inputs such as multimodal user messages. Return an optional SPINE_SLOT only if it preserves important context not already clear from USER_MSG blocks, Child Memory blocks, or SPINE_NODE_MEMORY.\n\
- SPINE_NODE_MEMORY is mandatory even when no optional SPINE_SLOT is useful.\n\n\
Rules:\n\
- Follow the final Spine Compact Memory section appended at the end of this prompt.\n\
- Prefer high-signal facts over chronology.\n\
- Do not archive routine progress updates, tool-call noise, or irrelevant implementation minutiae.\n\
- Use prefix context only to understand names, parent goal, and constraints; do not summarize unrelated prefix.\n\
- Use the optional close instruction as a preservation hint, not as a user message.\n\
- If there is conflict between an old plan and a later user correction, preserve the correction as the active intent.\n\
- If information is uncertain, say what is known and what still needs confirmation.\n\
- User-facing final-answer markers such as `TEST_WITH_LLM_*` are evidence only; never let such a marker be the whole memory.\n\
- Return only <SPINE_SLOT_N> and <SPINE_NODE_MEMORY> blocks. Never return USER_MSG blocks.".to_string()
}

fn append_spine_close_compact_prompt_items(
    prompt_input: &mut Vec<ResponseItem>,
    node_id: &str,
    close_target_projection: &str,
    compact_instructions: &str,
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<(), SpineError> {
    let slot_map = skeleton.prompt_slot_map()?;
    let tail = format!(
        "{close_target_projection}\n\n\
---------- Spine Suffix Boundary ----------\n\
The raw suffix for Spine node {node_id} is already present immediately before this tail directive.\n\
Source context range: [{}..{}).\n\n\
{compact_instructions}\n\n\
{slot_map}",
        skeleton.source_context_range.start, skeleton.source_context_range.end
    );
    prompt_input.push(spine_close_compact_developer_message(&tail));
    Ok(())
}

fn spine_close_compact_developer_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
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

fn validate_source_plan_against_history(
    source_plan: &SpineCompactSourcePlan,
    raw_items: &[ResponseItem],
    _close_call_id: &str,
) -> Result<(), SpineError> {
    let mut previous_context_index = None;
    for (expected_ordinal, entry) in source_plan.entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} does not match expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        if entry.context_index < source_plan.source_context_range.start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} context_index {} precedes source range start {}",
                entry.source_ordinal, entry.context_index, source_plan.source_context_range.start
            )));
        }
        if entry.context_index >= source_plan.source_context_range.end {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} context_index {} is outside source context range [{}..{})",
                entry.source_ordinal,
                entry.context_index,
                source_plan.source_context_range.start,
                source_plan.source_context_range.end
            )));
        }
        if let Some(previous) = previous_context_index {
            if entry.context_index <= previous {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close compact source entry ordinal {} context_index {} is not strictly after previous context_index {previous}",
                    entry.source_ordinal, entry.context_index
                )));
            }
        }
        previous_context_index = Some(entry.context_index);
        let Some(host_item) = raw_items.get(entry.context_index) else {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} context_index {} exceeds history length {}",
                entry.source_ordinal,
                entry.context_index,
                raw_items.len()
            )));
        };
        let expected_item = entry.visible_response_item();
        if host_item != &expected_item {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry mismatch at ordinal {} context_index {} source_hash {}",
                entry.source_ordinal, entry.context_index, entry.source_hash
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
fn is_current_spine_close_like_carrier(item: &ResponseItem, close_call_id: &str) -> bool {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => call_id == close_call_id,
        ResponseItem::FunctionCall {
            call_id,
            namespace,
            name,
            ..
        } => {
            call_id == close_call_id
                && namespace.as_deref() == Some(crate::spine::SPINE_NAMESPACE)
                && crate::spine::is_spine_close_like_tool_name(name)
        }
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineCompactMemorySkeleton {
    node_id: String,
    source_context_range: std::ops::Range<usize>,
    blocks: Vec<SpineCompactMemoryBlock>,
    slot_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SpineCompactMemoryBlock {
    UserMessage(String),
    ChildMemory {
        node_id: String,
        compact_id: String,
        body_hash: String,
        body: String,
    },
    MemorySlot {
        slot_id: String,
        entry_ordinals: Vec<usize>,
        instruction_only: bool,
    },
}

impl SpineCompactMemorySkeleton {
    fn from_source_plan(
        node_id: &str,
        source_plan: &SpineCompactSourcePlan,
    ) -> Result<Self, SpineError> {
        let mut builder = SpineCompactMemorySkeletonBuilder::new(
            node_id.to_string(),
            source_plan.source_context_range.clone(),
        );
        for entry in &source_plan.entries {
            match &entry.kind {
                SpineCompactSourceEntryKind::RawResponseItem {
                    item,
                    from_user: true,
                    ..
                } => {
                    if let Some(text) = response_item_text(item) {
                        builder.flush_slot();
                        builder
                            .blocks
                            .push(SpineCompactMemoryBlock::UserMessage(text.to_string()));
                    } else {
                        builder.push_slot_entry(entry.source_ordinal);
                    }
                }
                SpineCompactSourceEntryKind::RawResponseItem {
                    from_user: false, ..
                } => {
                    builder.push_slot_entry(entry.source_ordinal);
                }
                SpineCompactSourceEntryKind::ChildMemory {
                    node_id,
                    compact_id,
                    body,
                    body_hash,
                    ..
                } => {
                    builder.flush_slot();
                    builder.blocks.push(SpineCompactMemoryBlock::ChildMemory {
                        node_id: node_id.to_string(),
                        compact_id: compact_id.clone(),
                        body_hash: body_hash.clone(),
                        body: body.clone(),
                    });
                }
            }
        }
        builder.flush_slot();
        Ok(builder.finish())
    }

    #[cfg(test)]
    fn has_generated_slots(&self) -> bool {
        self.slot_count > 0
    }

    fn slot_ids(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                SpineCompactMemoryBlock::MemorySlot { slot_id, .. } => Some(slot_id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn slot_xml_tag(slot_id: &str) -> Option<String> {
        slot_id
            .strip_prefix("slot_")
            .filter(|ordinal| !ordinal.is_empty() && ordinal.chars().all(|ch| ch.is_ascii_digit()))
            .map(|ordinal| format!("SPINE_SLOT_{ordinal}"))
    }

    fn optional_slot_tags(&self) -> Result<Vec<String>, SpineError> {
        self.slot_ids()
            .into_iter()
            .map(|slot_id| {
                Self::slot_xml_tag(slot_id).ok_or_else(|| {
                    SpineError::CompactFailure(format!(
                        "spine.close compact has invalid internal memory slot id {slot_id}"
                    ))
                })
            })
            .collect()
    }

    fn optional_slot_tags_text(&self) -> Result<String, SpineError> {
        let tags = self.optional_slot_tags()?;
        if tags.is_empty() {
            Ok("none".to_string())
        } else {
            Ok(tags.join(", "))
        }
    }

    fn required_node_memory_block() -> &'static str {
        "<SPINE_NODE_MEMORY>\nWrite the complete handoff for this Spine node: active user intent, decisions made, work done, files/tests/errors/results, remaining work, and constraints needed to continue. This block is required even if no optional slots are returned.\n</SPINE_NODE_MEMORY>"
    }

    fn prompt_slot_map(&self) -> Result<String, SpineError> {
        let optional_slot_tags = self.optional_slot_tags_text()?;
        let optional_slot_contract = if self.slot_ids().is_empty() {
            "No optional slot markers are available in this skeleton; return only <SPINE_NODE_MEMORY>.".to_string()
        } else {
            format!(
                "Optional slots may all be omitted; if returned, the tag must be one of: {optional_slot_tags}."
            )
        };
        let mut text = format!(
            "{SPINE_COMPACT_SLOT_MAP_BOUNDARY}\n\
Write the node handoff in <SPINE_NODE_MEMORY>. Optionally add selected <SPINE_SLOT_N> blocks only when a slot marker represents important source context that is not already preserved exactly by runtime.\n\n\
What the skeleton means:\n\
- USER_MSG_N blocks are exact user messages preserved by runtime. Use them as evidence; never return USER_MSG blocks.\n\
- Child memory evidence is trusted runtime memory preserved by runtime. Use it as evidence; do not copy it wholesale into slot bodies.\n\
- SPINE_NODE_MEMORY is the primary whole-node handoff. It must be useful on its own and should not merely point back to USER_MSG_N or optional slots.\n\
- The actual content for optional slots is in the raw suffix above; the skeleton only marks where non-preserved source spans sit between preserved evidence boundaries.\n\
- Each Optional memory slot marker represents source context at that point in the suffix that runtime cannot preserve as exact USER_MSG or child memory, including assistant/tool/runtime work and non-exact user inputs such as multimodal user messages.\n\
- Treat an optional slot as a small handoff for the active user intent around that marker: what changed, proved, implemented, failed, what non-exact user input contained, or what remains open between those preserved boundaries.\n\
- Return an optional slot only when that span contains durable user intent/details, decisions, command results, file paths, errors, partial implementation state, or unresolved work needed to continue correctly.\n\
- Omit optional slots for routine progress updates, transient tool noise, or facts already clear from USER_MSG_N, child memory, or SPINE_NODE_MEMORY.\n\n\
Output contract:\n\
- Return only <SPINE_SLOT_N> and <SPINE_NODE_MEMORY> blocks.\n\
- SPINE_NODE_MEMORY must appear exactly once and must be non-empty.\n\
- {optional_slot_contract}\n\
- Optional slot bodies must be non-empty, not duplicated, and not nested.\n\
- Tags must appear alone on their own line and must match exactly.\n\
- Text outside returned blocks is invalid.\n\n\
Memory skeleton for Spine node {}:\n",
            self.node_id
        );
        let mut anchor_labels = Vec::with_capacity(self.blocks.len());
        let mut user_msg_anchor_ordinal = 0usize;
        for block in &self.blocks {
            let anchor = match block {
                SpineCompactMemoryBlock::UserMessage(_) => {
                    user_msg_anchor_ordinal += 1;
                    Some(format!("USER_MSG_{user_msg_anchor_ordinal}"))
                }
                SpineCompactMemoryBlock::ChildMemory { node_id, .. } => {
                    Some(format!("child memory for node {node_id}"))
                }
                SpineCompactMemoryBlock::MemorySlot { .. } => None,
            };
            anchor_labels.push(anchor);
        }
        let mut previous_anchor = "node start".to_string();
        let mut user_msg_ordinal = 0usize;
        for (block_index, block) in self.blocks.iter().enumerate() {
            match block {
                SpineCompactMemoryBlock::UserMessage(body) => {
                    user_msg_ordinal += 1;
                    text.push_str(&format!("\n<USER_MSG_{user_msg_ordinal}>\n"));
                    text.push_str(body);
                    if !body.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&format!("</USER_MSG_{user_msg_ordinal}>\n"));
                    previous_anchor = format!("USER_MSG_{user_msg_ordinal}");
                }
                SpineCompactMemoryBlock::ChildMemory {
                    node_id,
                    compact_id,
                    body_hash,
                    body,
                } => {
                    text.push_str("\nChild memory evidence:\n");
                    text.push_str(&format!(
                        "node_id={node_id} compact_id={compact_id} body_hash={body_hash}\n"
                    ));
                    text.push_str("<spine_memory>\n");
                    text.push_str(body);
                    if !body.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("</spine_memory>\n");
                    previous_anchor = format!("child memory for node {node_id}");
                }
                SpineCompactMemoryBlock::MemorySlot {
                    slot_id,
                    entry_ordinals: _,
                    instruction_only: _,
                } => {
                    let tag = Self::slot_xml_tag(slot_id).ok_or_else(|| {
                        SpineError::CompactFailure(format!(
                            "spine.close compact has invalid internal memory slot id {slot_id}"
                        ))
                    })?;
                    let next_anchor = anchor_labels
                        .iter()
                        .skip(block_index + 1)
                        .find_map(|anchor| anchor.as_deref())
                        .unwrap_or("node end");
                    text.push_str(&format!(
                        "\nOptional memory slot: {tag}\n\
Span: source context not exact-preserved by runtime after {previous_anchor} and before {next_anchor}.\n\
Purpose: preserve only durable context from this span that changed task state for the surrounding user intent, including assistant/tool/runtime work or non-exact user input such as multimodal messages.\n"
                    ));
                }
            }
        }
        text.push_str(
            "\nRequired memory:\n<SPINE_NODE_MEMORY>\nWrite the complete handoff for this Spine node: active user intent, decisions made, work done, files/tests/errors/results, remaining work, and constraints needed to continue. This block is required even if no optional slots are returned.\n</SPINE_NODE_MEMORY>\n",
        );
        Ok(text)
    }

    fn assemble<'a>(
        &self,
        slots: impl IntoIterator<Item = (&'a str, &'a str)>,
        node_memory: &str,
    ) -> Result<String, SpineError> {
        let mut slot_values = std::collections::BTreeMap::new();
        for (slot_id, body) in slots {
            if slot_values
                .insert(slot_id.to_string(), body.to_string())
                .is_some()
            {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close compact produced duplicate memory slot {slot_id}"
                )));
            }
        }

        let mut body = format!("# Spine Memory {}\n", self.node_id);
        for block in &self.blocks {
            match block {
                SpineCompactMemoryBlock::UserMessage(text) => {
                    push_memory_block(&mut body, "## User Message", text);
                }
                SpineCompactMemoryBlock::ChildMemory { body: child, .. } => {
                    push_memory_block(&mut body, "## Child Memory", child);
                }
                SpineCompactMemoryBlock::MemorySlot { slot_id, .. } => {
                    if let Some(slot_body) = slot_values.remove(slot_id) {
                        validate_generated_slot_body(slot_id, &slot_body)?;
                        push_memory_block(&mut body, "## Memory Slot", &slot_body);
                    }
                }
            }
        }
        if let Some(extra) = slot_values.keys().next() {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact produced unexpected memory slot {extra}"
            )));
        }
        validate_generated_node_memory_body(node_memory)?;
        push_memory_block(&mut body, "## Node Memory", node_memory);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        Ok(body)
    }
}

struct SpineCompactMemorySkeletonBuilder {
    node_id: String,
    source_context_range: std::ops::Range<usize>,
    blocks: Vec<SpineCompactMemoryBlock>,
    pending_slot_ordinals: Vec<usize>,
    slot_count: usize,
}

impl SpineCompactMemorySkeletonBuilder {
    fn new(node_id: String, source_context_range: std::ops::Range<usize>) -> Self {
        Self {
            node_id,
            source_context_range,
            blocks: Vec::new(),
            pending_slot_ordinals: Vec::new(),
            slot_count: 0,
        }
    }

    fn push_slot_entry(&mut self, source_ordinal: usize) {
        self.pending_slot_ordinals.push(source_ordinal);
    }

    fn flush_slot(&mut self) {
        if self.pending_slot_ordinals.is_empty() {
            return;
        }
        self.slot_count += 1;
        self.blocks.push(SpineCompactMemoryBlock::MemorySlot {
            slot_id: format!("slot_{}", self.slot_count),
            entry_ordinals: std::mem::take(&mut self.pending_slot_ordinals),
            instruction_only: false,
        });
    }

    fn finish(self) -> SpineCompactMemorySkeleton {
        SpineCompactMemorySkeleton {
            node_id: self.node_id,
            source_context_range: self.source_context_range,
            blocks: self.blocks,
            slot_count: self.slot_count,
        }
    }
}

fn push_memory_block(body: &mut String, heading: &str, block_body: &str) {
    body.push('\n');
    body.push_str(heading);
    body.push('\n');
    body.push_str(block_body.trim_matches('\n'));
    body.push('\n');
}

#[cfg(test)]
fn spine_close_compact_body(
    node_id: &str,
    output: &[ResponseItem],
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<String, SpineError> {
    spine_close_compact_body_result(node_id, output, skeleton).map_err(|err| err.into_spine_error())
}

fn spine_close_compact_body_result(
    node_id: &str,
    output: &[ResponseItem],
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<String, SpineCloseCompactBodyError> {
    if skeleton.node_id != node_id {
        return Err(SpineCloseCompactBodyError::Fatal(format!(
            "spine.close compact skeleton node {} does not match output node {node_id}",
            skeleton.node_id
        )));
    }
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
        return Err(SpineCloseCompactBodyError::ToolCall(format!(
            "spine.close compact produced unexpected {} tool call",
            spine_close_compact_tool_call_kind(item)
        )));
    }
    let mut compact_block_entries = Vec::new();
    for item in output {
        if let ResponseItem::Message { role, .. } = item
            && role == "assistant"
            && let Some(text) = last_assistant_message_from_item(item, /*plan_mode*/ false)
            && !text.trim().is_empty()
        {
            compact_block_entries.push(text);
        }
    }
    if compact_block_entries.is_empty() {
        let encrypted_only = output.iter().any(|item| {
            matches!(
                item,
                ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
            )
        });
        return Err(SpineCloseCompactBodyError::Fatal(if encrypted_only {
            "spine.close compact produced no readable memory body".to_string()
        } else {
            "spine.close compact produced no memory body".to_string()
        }));
    }
    let joined = compact_block_entries.join("\n");
    let parsed_blocks = parse_spine_close_compact_blocks(&joined, skeleton)?;
    let slots = parsed_blocks
        .slots
        .iter()
        .map(|(slot_id, body)| (slot_id.as_str(), body.as_str()));
    skeleton
        .assemble(slots, &parsed_blocks.node_memory)
        .map_err(|err| SpineCloseCompactBodyError::Repairable(err.to_string()))
}

fn spine_close_compact_tool_call_kind(item: &ResponseItem) -> &'static str {
    match item {
        ResponseItem::FunctionCall { .. } => "FunctionCall",
        ResponseItem::LocalShellCall { .. } => "LocalShellCall",
        ResponseItem::CustomToolCall { .. } => "CustomToolCall",
        ResponseItem::ToolSearchCall { .. } => "ToolSearchCall",
        ResponseItem::WebSearchCall { .. } => "WebSearchCall",
        ResponseItem::ImageGenerationCall { .. } => "ImageGenerationCall",
        _ => "tool-like",
    }
}

#[derive(Debug)]
enum SpineCloseCompactBodyError {
    Fatal(String),
    Repairable(String),
    ToolCall(String),
}

impl SpineCloseCompactBodyError {
    fn is_repairable(&self) -> bool {
        matches!(self, Self::Repairable(_))
    }

    fn message(&self) -> &str {
        match self {
            Self::Fatal(message) | Self::Repairable(message) | Self::ToolCall(message) => message,
        }
    }

    fn into_spine_error(self) -> SpineError {
        match self {
            Self::Fatal(message) | Self::Repairable(message) | Self::ToolCall(message) => {
                SpineError::CompactFailure(message)
            }
        }
    }
}

fn spine_close_compact_repair_text(
    err: &SpineCloseCompactBodyError,
    skeleton: &SpineCompactMemorySkeleton,
) -> String {
    let optional_slot_tags = skeleton
        .optional_slot_tags_text()
        .unwrap_or_else(|error| format!("internal-error: {error}"));
    let required_node_memory_block = SpineCompactMemorySkeleton::required_node_memory_block();
    format!(
        "Your previous Spine compact output could not be used: {}\n\
Return only <SPINE_SLOT_N> and <SPINE_NODE_MEMORY> blocks.\n\
SPINE_NODE_MEMORY must appear exactly once and must be non-empty.\n\
Optional SPINE_SLOT_N blocks may all be omitted. Return a slot only if its marked span contains durable source context not exact-preserved by runtime and needed for continuation.\n\
Allowed optional slot tags: {}.\n\
Optional slot syntax, replacing N with an allowed slot number only when needed:\n\
<SPINE_SLOT_N>\n\
Compact durable source context for that marked span, including non-exact user input when relevant.\n\
</SPINE_SLOT_N>\n\
Required output block:\n\
{}\n\
Do not include explanations, code fences, JSON, USER_MSG blocks, or tool calls.",
        err.message(),
        optional_slot_tags,
        required_node_memory_block
    )
}

fn add_compact_output_tokens(
    memory_output_tokens: &mut Option<i64>,
    token_usage: Option<&TokenUsage>,
) {
    let Some(token_usage) = token_usage else {
        return;
    };
    *memory_output_tokens = Some(memory_output_tokens.unwrap_or(0) + token_usage.output_tokens);
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedSpineCompactBlocks {
    slots: Vec<(String, String)>,
    node_memory: String,
}

fn parse_spine_close_compact_blocks(
    text: &str,
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<ParsedSpineCompactBlocks, SpineCloseCompactBodyError> {
    let mut expected_tags = std::collections::BTreeMap::new();
    for slot_id in skeleton.slot_ids() {
        let Some(tag) = SpineCompactMemorySkeleton::slot_xml_tag(slot_id) else {
            return Err(SpineCloseCompactBodyError::Fatal(format!(
                "spine.close compact has invalid internal memory slot id {slot_id}"
            )));
        };
        expected_tags.insert(tag, slot_id.to_string());
    }

    let mut parsed_slots = std::collections::BTreeMap::<String, String>::new();
    let mut node_memory: Option<String> = None;
    let mut current: Option<OpenSpineCompactBlock> = None;
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        match current.as_mut() {
            Some(open) => match spine_compact_line_marker(line) {
                Some(SpineCompactLineMarker::Start(tag)) => {
                    if let SpineCompactBlockTag::UserMsg(tag) = tag {
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact returned evidence-only USER_MSG block <{tag}> at line {line_number}"
                        )));
                    }
                    let tag_name = tag.name();
                    return Err(SpineCloseCompactBodyError::Repairable(format!(
                        "spine.close compact produced nested tag <{tag_name}> at line {line_number}"
                    )));
                }
                Some(SpineCompactLineMarker::End(tag)) => {
                    if let SpineCompactBlockTag::UserMsg(tag) = tag {
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact returned evidence-only USER_MSG block </{tag}> at line {line_number}"
                        )));
                    }
                    if tag != open.tag {
                        let open_name = open.tag.name();
                        let close_name = tag.name();
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact produced mismatched tag: opened <{open_name}> but closed </{close_name}> at line {line_number}"
                        )));
                    }
                    let open = current
                        .take()
                        .expect("current compact block remains open while parsing closing tag");
                    let body = open.body_lines.join("\n").trim_matches('\n').to_string();
                    match open.tag {
                        SpineCompactBlockTag::Slot(tag) => {
                            let slot_id = open
                                .slot_id
                                .expect("slot compact block stores internal slot id");
                            if parsed_slots.insert(slot_id.clone(), body).is_some() {
                                return Err(SpineCloseCompactBodyError::Repairable(format!(
                                    "spine.close compact produced duplicate memory slot {slot_id}"
                                )));
                            }
                            if parsed_slots
                                .get(&slot_id)
                                .is_some_and(|body| body.trim().is_empty())
                            {
                                return Err(SpineCloseCompactBodyError::Repairable(format!(
                                    "spine.close compact produced empty memory slot {slot_id}"
                                )));
                            }
                            let _ = tag;
                        }
                        SpineCompactBlockTag::NodeMemory => {
                            if body.trim().is_empty() {
                                return Err(SpineCloseCompactBodyError::Repairable(
                                    "spine.close compact produced empty SPINE_NODE_MEMORY"
                                        .to_string(),
                                ));
                            }
                            if node_memory.replace(body).is_some() {
                                return Err(SpineCloseCompactBodyError::Repairable(
                                    "spine.close compact produced duplicate SPINE_NODE_MEMORY"
                                        .to_string(),
                                ));
                            }
                        }
                        SpineCompactBlockTag::UserMsg(_) => {
                            unreachable!("USER_MSG cannot be opened as a compact block")
                        }
                    }
                }
                Some(SpineCompactLineMarker::Malformed) => {
                    return Err(SpineCloseCompactBodyError::Repairable(format!(
                        "spine.close compact produced malformed Spine compact tag at line {line_number}"
                    )));
                }
                None => open.body_lines.push(line.to_string()),
            },
            None => {
                if line.trim().is_empty() {
                    continue;
                }
                match spine_compact_line_marker(line) {
                    Some(SpineCompactLineMarker::Start(tag)) => match tag {
                        SpineCompactBlockTag::Slot(tag) => {
                            let Some(slot_id) = expected_tags.get(&tag) else {
                                return Err(SpineCloseCompactBodyError::Repairable(format!(
                                    "spine.close compact produced unexpected memory slot tag <{tag}> at line {line_number}"
                                )));
                            };
                            current = Some(OpenSpineCompactBlock {
                                tag: SpineCompactBlockTag::Slot(tag),
                                slot_id: Some(slot_id.clone()),
                                body_lines: Vec::new(),
                            });
                        }
                        SpineCompactBlockTag::NodeMemory => {
                            current = Some(OpenSpineCompactBlock {
                                tag: SpineCompactBlockTag::NodeMemory,
                                slot_id: None,
                                body_lines: Vec::new(),
                            });
                        }
                        SpineCompactBlockTag::UserMsg(tag) => {
                            return Err(SpineCloseCompactBodyError::Repairable(format!(
                                "spine.close compact returned evidence-only USER_MSG block <{tag}> at line {line_number}"
                            )));
                        }
                    },
                    Some(SpineCompactLineMarker::End(tag)) => {
                        if let SpineCompactBlockTag::UserMsg(tag) = tag {
                            return Err(SpineCloseCompactBodyError::Repairable(format!(
                                "spine.close compact returned evidence-only USER_MSG block </{tag}> at line {line_number}"
                            )));
                        }
                        let tag_name = tag.name();
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact produced unexpected closing tag </{tag_name}> at line {line_number}"
                        )));
                    }
                    Some(SpineCompactLineMarker::Malformed) => {
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact produced malformed Spine compact tag at line {line_number}"
                        )));
                    }
                    None => {
                        return Err(SpineCloseCompactBodyError::Repairable(format!(
                            "spine.close compact produced text outside SPINE_SLOT or SPINE_NODE_MEMORY blocks at line {line_number}"
                        )));
                    }
                }
            }
        }
    }

    if let Some(open) = current {
        let tag_name = open.tag.name();
        return Err(SpineCloseCompactBodyError::Repairable(format!(
            "spine.close compact missing closing tag </{tag_name}>"
        )));
    }

    let Some(node_memory) = node_memory else {
        return Err(SpineCloseCompactBodyError::Repairable(
            "spine.close compact missing SPINE_NODE_MEMORY".to_string(),
        ));
    };

    let selected_slot_ids = skeleton
        .slot_ids()
        .into_iter()
        .filter(|slot_id| parsed_slots.contains_key(*slot_id))
        .map(str::to_string)
        .collect::<Vec<_>>();

    Ok(ParsedSpineCompactBlocks {
        slots: selected_slot_ids
            .into_iter()
            .map(|slot_id| {
                let body = parsed_slots.remove(&slot_id).expect("slot selected above");
                (slot_id, body)
            })
            .collect(),
        node_memory,
    })
}

#[derive(Debug)]
struct OpenSpineCompactBlock {
    tag: SpineCompactBlockTag,
    slot_id: Option<String>,
    body_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpineCompactBlockTag {
    Slot(String),
    NodeMemory,
    UserMsg(String),
}

impl SpineCompactBlockTag {
    fn name(&self) -> String {
        match self {
            Self::Slot(tag) | Self::UserMsg(tag) => tag.clone(),
            Self::NodeMemory => "SPINE_NODE_MEMORY".to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SpineCompactLineMarker {
    Start(SpineCompactBlockTag),
    End(SpineCompactBlockTag),
    Malformed,
}

fn spine_compact_line_marker(line: &str) -> Option<SpineCompactLineMarker> {
    let trimmed = line.trim();
    if !is_spine_compact_tag_like(trimmed) {
        return None;
    }
    if line != trimmed {
        return Some(SpineCompactLineMarker::Malformed);
    }
    if let Some(ordinal) = line
        .strip_prefix("<SPINE_SLOT_")
        .and_then(|rest| rest.strip_suffix('>'))
        && !ordinal.is_empty()
        && ordinal.chars().all(|ch| ch.is_ascii_digit())
    {
        return Some(SpineCompactLineMarker::Start(SpineCompactBlockTag::Slot(
            format!("SPINE_SLOT_{ordinal}"),
        )));
    }
    if let Some(ordinal) = line
        .strip_prefix("</SPINE_SLOT_")
        .and_then(|rest| rest.strip_suffix('>'))
        && !ordinal.is_empty()
        && ordinal.chars().all(|ch| ch.is_ascii_digit())
    {
        return Some(SpineCompactLineMarker::End(SpineCompactBlockTag::Slot(
            format!("SPINE_SLOT_{ordinal}"),
        )));
    }
    if line == "<SPINE_NODE_MEMORY>" {
        return Some(SpineCompactLineMarker::Start(
            SpineCompactBlockTag::NodeMemory,
        ));
    }
    if line == "</SPINE_NODE_MEMORY>" {
        return Some(SpineCompactLineMarker::End(
            SpineCompactBlockTag::NodeMemory,
        ));
    }
    if let Some(ordinal) = line
        .strip_prefix("<USER_MSG_")
        .and_then(|rest| rest.strip_suffix('>'))
        && !ordinal.is_empty()
        && ordinal.chars().all(|ch| ch.is_ascii_digit())
    {
        return Some(SpineCompactLineMarker::Start(
            SpineCompactBlockTag::UserMsg(format!("USER_MSG_{ordinal}")),
        ));
    }
    if let Some(ordinal) = line
        .strip_prefix("</USER_MSG_")
        .and_then(|rest| rest.strip_suffix('>'))
        && !ordinal.is_empty()
        && ordinal.chars().all(|ch| ch.is_ascii_digit())
    {
        return Some(SpineCompactLineMarker::End(SpineCompactBlockTag::UserMsg(
            format!("USER_MSG_{ordinal}"),
        )));
    }
    Some(SpineCompactLineMarker::Malformed)
}

fn is_spine_compact_tag_like(trimmed: &str) -> bool {
    trimmed.starts_with("<SPINE_SLOT_")
        || trimmed.starts_with("</SPINE_SLOT_")
        || trimmed.starts_with("<SPINE_NODE_MEMORY")
        || trimmed.starts_with("</SPINE_NODE_MEMORY")
        || trimmed.starts_with("<USER_MSG_")
        || trimmed.starts_with("</USER_MSG_")
}

fn validate_generated_slot_body(slot_id: &str, body: &str) -> Result<(), SpineError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact produced empty memory slot {slot_id}"
        )));
    }
    const FORBIDDEN: &[&str] = &[
        "# Spine Memory ",
        "---------- Spine Compact Directive ----------",
        "---------- Spine Close Target ----------",
        "## User Message",
        "## Child Memory",
        "## Memory Slot",
        "## Node Memory",
        "<SPINE_SLOT_",
        "</SPINE_SLOT_",
        "<SPINE_NODE_MEMORY>",
        "</SPINE_NODE_MEMORY>",
        "USER_MSG",
        "<spine_memory>",
        "</spine_memory>",
    ];
    if let Some(marker) = FORBIDDEN.iter().find(|marker| body.contains(**marker)) {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact memory slot {slot_id} contains forbidden structure marker {marker:?}"
        )));
    }
    Ok(())
}

fn validate_generated_node_memory_body(body: &str) -> Result<(), SpineError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(SpineError::CompactFailure(
            "spine.close compact produced empty SPINE_NODE_MEMORY".to_string(),
        ));
    }
    const FORBIDDEN: &[&str] = &[
        "# Spine Memory ",
        "---------- Spine Compact Directive ----------",
        "---------- Spine Close Target ----------",
        "## User Message",
        "## Child Memory",
        "## Memory Slot",
        "## Node Memory",
        "<SPINE_SLOT_",
        "</SPINE_SLOT_",
        "<SPINE_NODE_MEMORY>",
        "</SPINE_NODE_MEMORY>",
        "USER_MSG",
        "<spine_memory>",
        "</spine_memory>",
    ];
    if let Some(marker) = FORBIDDEN.iter().find(|marker| body.contains(**marker)) {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact SPINE_NODE_MEMORY contains forbidden structure marker {marker:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod spine_close_slot_map_tests {
    use super::*;
    use crate::spine::NodeId;
    use crate::spine::SPINE_NAMESPACE;
    use crate::spine::SPINE_TOOL_CLOSE;
    use crate::spine::SPINE_TOOL_NEXT;
    use crate::spine::SPINE_TOOL_OPEN;
    use crate::spine::SPINE_TOOL_TREE;
    use codex_protocol::models::FunctionCallOutputPayload;

    fn node_id(path: &[u32]) -> NodeId {
        serde_json::from_value(serde_json::json!(path)).expect("node id")
    }

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

    fn source_entry(
        context_index: usize,
        source_ordinal: usize,
        item: ResponseItem,
        from_user: bool,
    ) -> crate::spine::SpineCompactSourcePlanEntry {
        crate::spine::SpineCompactSourcePlanEntry {
            context_index,
            source_ordinal,
            source_hash: format!("hash-{source_ordinal}"),
            kind: SpineCompactSourceEntryKind::RawResponseItem {
                item,
                raw_ordinal: u64::try_from(context_index).expect("context index fits u64"),
                from_user,
            },
        }
    }

    fn child_memory_entry(
        context_index: usize,
        source_ordinal: usize,
        body: &str,
    ) -> crate::spine::SpineCompactSourcePlanEntry {
        crate::spine::SpineCompactSourcePlanEntry {
            context_index,
            source_ordinal,
            source_hash: format!("child-hash-{source_ordinal}"),
            kind: SpineCompactSourceEntryKind::ChildMemory {
                node_id: node_id(&[1, 1, 1]),
                compact_id: "mem-1-1-1".to_string(),
                source_raw_range: 2..3,
                body: body.to_string(),
                body_hash: "body-hash".to_string(),
            },
        }
    }

    fn source_plan(
        entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
    ) -> SpineCompactSourcePlan {
        SpineCompactSourcePlan {
            node_id: node_id(&[1, 1]),
            source_context_range: 2..2 + entries.len(),
            source_raw_range: 2..2 + u64::try_from(entries.len()).expect("entries len fits u64"),
            entries,
        }
    }

    fn source_plan_with_context_range(
        source_context_range: std::ops::Range<usize>,
        entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
    ) -> SpineCompactSourcePlan {
        SpineCompactSourcePlan {
            node_id: node_id(&[1, 1]),
            source_raw_range: u64::try_from(source_context_range.start)
                .expect("range start fits u64")
                ..u64::try_from(source_context_range.end).expect("range end fits u64"),
            source_context_range,
            entries,
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

    fn compact_output(optional_slots: &[(&str, &str)], node_memory: &str) -> String {
        let mut output = String::new();
        for (tag, body) in optional_slots {
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str(&format!("<{tag}>\n{body}\n</{tag}>"));
        }
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!(
            "<SPINE_NODE_MEMORY>\n{node_memory}\n</SPINE_NODE_MEMORY>"
        ));
        output
    }

    #[test]
    fn skeleton_preserves_exact_blocks_optional_slots_and_node_memory() {
        let plan = source_plan(vec![
            source_entry(2, 0, user_message("USER_EXACT\nline 2"), true),
            source_entry(3, 1, assistant_message("assistant details"), false),
            child_memory_entry(4, 2, "# Spine Memory 1.1.1\n\nchild body\n"),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(skeleton.has_generated_slots());
        let slot_map = skeleton.prompt_slot_map().expect("slot map");
        assert!(slot_map.starts_with(SPINE_COMPACT_SLOT_MAP_BOUNDARY));
        assert!(slot_map.contains("<USER_MSG_1>\nUSER_EXACT\nline 2\n</USER_MSG_1>"));
        assert!(slot_map.contains("# Spine Memory 1.1.1"));
        assert!(slot_map.contains("Optional memory slot: SPINE_SLOT_1"));
        assert!(!slot_map.contains("source ordinals"));
        assert!(slot_map.contains("<SPINE_NODE_MEMORY>"));

        let body = skeleton
            .assemble(
                [("slot_1", "compact assistant facts")],
                "node handoff facts",
            )
            .expect("assembled body");
        assert!(body.contains("# Spine Memory 1.1"));
        assert!(body.contains("## User Message\nUSER_EXACT\nline 2"));
        assert!(body.contains("## Memory Slot\ncompact assistant facts"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild body"));
        assert!(body.contains("## Node Memory\nnode handoff facts"));
    }

    #[test]
    fn slot_map_prompt_lists_optional_slots_and_repair_shape() {
        let plan = source_plan(vec![
            source_entry(2, 0, assistant_message("before user"), false),
            source_entry(3, 1, user_message("USER_EXACT"), true),
            source_entry(4, 2, assistant_message("after user"), false),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let slot_map = skeleton.prompt_slot_map().expect("slot map");
        assert!(slot_map.contains(
            "Optional slots may all be omitted; if returned, the tag must be one of: SPINE_SLOT_1, SPINE_SLOT_2."
        ));
        assert!(slot_map.contains("SPINE_NODE_MEMORY is the primary whole-node handoff"));
        assert!(
            slot_map.contains("The actual content for optional slots is in the raw suffix above")
        );
        assert!(slot_map.contains("non-exact user inputs such as multimodal user messages"));
        assert!(slot_map.contains(
            "Treat an optional slot as a small handoff for the active user intent around that marker"
        ));
        assert!(slot_map.contains("Optional memory slot: SPINE_SLOT_1"));
        assert!(
            slot_map.contains(
                "Span: source context not exact-preserved by runtime after node start and before USER_MSG_1."
            )
        );
        assert!(slot_map.contains("Optional memory slot: SPINE_SLOT_2"));
        assert!(
            slot_map.contains(
                "Span: source context not exact-preserved by runtime after USER_MSG_1 and before node end."
            )
        );
        assert!(slot_map.contains("<USER_MSG_1>\nUSER_EXACT\n</USER_MSG_1>"));
        assert!(!slot_map.contains("<SPINE_SLOT_1>"));
        assert!(!slot_map.contains("</SPINE_SLOT_1>"));
        assert!(!slot_map.contains("<SPINE_SLOT_2>"));
        assert!(!slot_map.contains("</SPINE_SLOT_2>"));
        assert!(!slot_map.contains("source ordinals"));

        let repair = spine_close_compact_repair_text(
            &SpineCloseCompactBodyError::Repairable(
                "spine.close compact missing SPINE_NODE_MEMORY".to_string(),
            ),
            &skeleton,
        );
        assert!(repair.contains("SPINE_NODE_MEMORY must appear exactly once"));
        assert!(repair.contains("SPINE_SLOT_1, SPINE_SLOT_2"));
        assert!(repair.contains("<SPINE_SLOT_N>"));
        assert!(repair.contains("</SPINE_SLOT_N>"));
        assert!(!repair.contains("<SPINE_SLOT_1>"));
        assert!(!repair.contains("</SPINE_SLOT_1>"));
        assert!(repair.contains("<SPINE_NODE_MEMORY>"));
        assert!(repair.contains("</SPINE_NODE_MEMORY>"));
    }

    #[test]
    fn zero_optional_slot_skeleton_requires_node_memory() {
        let plan = source_plan(vec![
            source_entry(2, 0, user_message("only user one"), true),
            child_memory_entry(3, 1, "# Spine Memory 1.1.1\n\nchild exact\n"),
            source_entry(4, 2, user_message("only user two"), true),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(!skeleton.has_generated_slots());
        let body = skeleton
            .assemble(std::iter::empty(), "node memory for exact-only suffix")
            .expect("assembled body");
        assert!(body.contains("## User Message\nonly user one"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## User Message\nonly user two"));
        assert!(!body.contains("## Memory Slot"));
        assert!(body.contains("## Node Memory\nnode memory for exact-only suffix"));
    }

    #[test]
    fn zero_slot_prompt_does_not_invent_user_message() {
        let plan = source_plan(vec![child_memory_entry(
            2,
            0,
            "# Spine Memory 1.1.1\n\nchild exact\n",
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(!skeleton.has_generated_slots());

        let slot_map = skeleton.prompt_slot_map().expect("slot map");
        assert!(!slot_map.contains("<USER_MSG_"));
        assert!(slot_map.contains(
            "No optional slot markers are available in this skeleton; return only <SPINE_NODE_MEMORY>."
        ));

        let body = skeleton
            .assemble(std::iter::empty(), "preserved close instruction facts")
            .expect("assembled body");
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## Node Memory\npreserved close instruction facts"));
        assert!(!body.contains("## Memory Slot"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn compact_xml_accepts_optional_slot_and_node_memory_markdown() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = spine_close_compact_body(
            "1.1",
            &[assistant_message(&compact_output(
                &[(
                    "SPINE_SLOT_1",
                    r#"Preserve path `scaffold/codex-home/codex`.

### Markdown heading is allowed inside a slot

```json
{"quoted":"json is content, not protocol"}
```"#,
                )],
                "Node memory can contain Markdown bullets:\n- next step remains open",
            ))],
            &skeleton,
        )
        .expect("compact XML blocks should parse");

        assert!(body.contains("## Memory Slot\nPreserve path"));
        assert!(body.contains("### Markdown heading is allowed inside a slot"));
        assert!(body.contains(r#"{"quoted":"json is content, not protocol"}"#));
        assert!(body.contains("## Node Memory\nNode memory can contain Markdown bullets"));
    }

    #[test]
    fn compact_xml_allows_omitting_optional_slot() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = spine_close_compact_body(
            "1.1",
            &[assistant_message(&compact_output(
                &[],
                "node memory covers the only durable facts",
            ))],
            &skeleton,
        )
        .expect("optional slot may be omitted");

        assert!(!body.contains("## Memory Slot"));
        assert!(body.contains("## Node Memory\nnode memory covers the only durable facts"));
    }

    #[test]
    fn compact_xml_rejects_structure_pollution() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
        let err = spine_close_compact_body(
            "1.1",
            &[assistant_message(&compact_output(
                &[("SPINE_SLOT_1", "## User Message\npolluted")],
                "node memory",
            ))],
            &skeleton,
        )
        .expect_err("slot pollution must be rejected");

        assert!(
            err.to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn compact_xml_rejects_text_and_fences_outside_blocks() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let wrapped = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"Here is the compact memory:
<SPINE_SLOT_1>
wrapped body
</SPINE_SLOT_1>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("wrapped slot blocks must fail");
        assert!(
            wrapped
                .to_string()
                .contains("text outside SPINE_SLOT or SPINE_NODE_MEMORY blocks")
        );

        let fenced = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"```xml
<SPINE_SLOT_1>
fenced body
</SPINE_SLOT_1>
<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>
```"#,
            )],
            &skeleton,
        )
        .expect_err("fenced slot blocks must fail");
        assert!(
            fenced
                .to_string()
                .contains("text outside SPINE_SLOT or SPINE_NODE_MEMORY blocks")
        );
    }

    #[test]
    fn compact_xml_rejects_missing_extra_duplicate_and_invalid_output() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let missing = spine_close_compact_body("1.1", &[assistant_message("")], &skeleton)
            .expect_err("missing body must fail");
        assert!(missing.to_string().contains("produced no memory body"));

        let extra = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
ok
</SPINE_SLOT_1>

<SPINE_SLOT_2>
extra
</SPINE_SLOT_2>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("extra slot must fail");
        assert!(
            extra
                .to_string()
                .contains("unexpected memory slot tag <SPINE_SLOT_2>")
        );

        let duplicate = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
first
</SPINE_SLOT_1>

<SPINE_SLOT_1>
second
</SPINE_SLOT_1>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("duplicate slot must fail");
        assert!(
            duplicate
                .to_string()
                .contains("duplicate memory slot slot_1")
        );

        let invalid = spine_close_compact_body("1.1", &[assistant_message("not xml")], &skeleton)
            .expect_err("invalid XML slot output must fail");
        assert!(
            invalid
                .to_string()
                .contains("text outside SPINE_SLOT or SPINE_NODE_MEMORY blocks")
        );
    }

    #[test]
    fn compact_xml_rejects_node_memory_missing_duplicate_empty_and_user_msg() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let missing_node = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
optional
</SPINE_SLOT_1>"#,
            )],
            &skeleton,
        )
        .expect_err("missing node memory must fail");
        assert!(
            missing_node
                .to_string()
                .contains("missing SPINE_NODE_MEMORY")
        );

        let duplicate_node = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_NODE_MEMORY>
first
</SPINE_NODE_MEMORY>

<SPINE_NODE_MEMORY>
second
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("duplicate node memory must fail");
        assert!(
            duplicate_node
                .to_string()
                .contains("duplicate SPINE_NODE_MEMORY")
        );

        let empty_node = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_NODE_MEMORY>

</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("empty node memory must fail");
        assert!(empty_node.to_string().contains("empty SPINE_NODE_MEMORY"));

        let user_msg = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<USER_MSG_1>
do not return this
</USER_MSG_1>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("returned USER_MSG must fail");
        assert!(user_msg.to_string().contains("evidence-only USER_MSG"));
    }

    #[test]
    fn compact_xml_rejects_mismatch_missing_end_nested_marker_and_empty_body() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let mismatch = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
body
</SPINE_SLOT_2>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("mismatched closing tag must fail");
        assert!(mismatch.to_string().contains("mismatched tag"));

        let missing_end = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
body"#,
            )],
            &skeleton,
        )
        .expect_err("missing closing tag must fail");
        assert!(missing_end.to_string().contains("missing closing tag"));

        let nested = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>
before
<SPINE_SLOT_1>
after
</SPINE_SLOT_1>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("nested tag must fail");
        assert!(nested.to_string().contains("nested tag"));

        let empty = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"<SPINE_SLOT_1>

</SPINE_SLOT_1>

<SPINE_NODE_MEMORY>
node
</SPINE_NODE_MEMORY>"#,
            )],
            &skeleton,
        )
        .expect_err("empty slot must fail");
        assert!(empty.to_string().contains("empty memory slot slot_1"));
    }

    #[test]
    fn multimodal_user_entry_becomes_generated_slot() {
        let plan = source_plan(vec![crate::spine::SpineCompactSourcePlanEntry {
            context_index: 2,
            source_ordinal: 0,
            source_hash: "hash-0".to_string(),
            kind: SpineCompactSourceEntryKind::RawResponseItem {
                item: ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![
                        ContentItem::InputText {
                            text: "text".to_string(),
                        },
                        ContentItem::InputText {
                            text: "second".to_string(),
                        },
                    ],
                    phase: None,
                },
                raw_ordinal: 2,
                from_user: true,
            },
        }]);

        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
        assert!(skeleton.has_generated_slots());
        let slot_map = skeleton.prompt_slot_map().expect("slot map");
        assert!(slot_map.contains("Optional memory slot: SPINE_SLOT_1"));
        assert!(slot_map.contains("non-exact user inputs such as multimodal user messages"));
        assert!(slot_map.contains(
            "Purpose: preserve only durable context from this span that changed task state"
        ));
        assert!(
            !slot_map.contains("<USER_MSG_"),
            "multi-content user messages should be summarized by generated slots, not exact-preserved"
        );
        let body = skeleton
            .assemble(
                [("slot_1", "compact multimodal user facts")],
                "node multimodal handoff",
            )
            .expect("assembled body");
        assert!(body.contains("## Memory Slot\ncompact multimodal user facts"));
        assert!(body.contains("## Node Memory\nnode multimodal handoff"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn close_like_carrier_filters_only_close_like_spine_tools() {
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
        assert!(!is_current_spine_close_like_carrier(
            &ResponseItem::FunctionCall {
                id: None,
                name: SPINE_TOOL_CLOSE.to_string(),
                namespace: Some("not-spine".to_string()),
                arguments: "{}".to_string(),
                call_id: "call-1".to_string(),
            },
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

    #[test]
    fn source_plan_validator_accepts_real_non_contiguous_context_indices() {
        let raw_items = vec![
            user_message("prefix 0"),
            user_message("prefix 1"),
            assistant_message("source 2"),
            assistant_message("gap 3"),
            assistant_message("source 4"),
        ];
        let plan = source_plan_with_context_range(
            2..5,
            vec![
                source_entry(2, 0, raw_items[2].clone(), false),
                source_entry(4, 1, raw_items[4].clone(), false),
            ],
        );

        validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect("non-contiguous real context indices should validate");
    }

    #[test]
    fn source_plan_validator_rejects_duplicate_context_indices() {
        let raw_items = vec![
            user_message("prefix 0"),
            user_message("prefix 1"),
            assistant_message("source 2"),
        ];
        let plan = source_plan_with_context_range(
            2..3,
            vec![
                source_entry(2, 0, raw_items[2].clone(), false),
                source_entry(2, 1, raw_items[2].clone(), false),
            ],
        );

        let err = validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect_err("duplicate context indices must fail");
        assert!(
            err.to_string()
                .contains("is not strictly after previous context_index 2"),
            "unexpected duplicate context error: {err}"
        );
    }
}
