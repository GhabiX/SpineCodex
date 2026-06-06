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
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineCompactSourceEntryKind;
use crate::spine::SpineCompactSourcePlan;
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
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

const SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME: &str = "spine_compact_memory.md";
const SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER: &str = "{node_id}";
const SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER: &str = "{close_instruction}";
const SPINE_COMPACT_SLOT_MAP_BOUNDARY: &str = "----------- Spine Compact Slot Map ----------";

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
        let mut skeleton = SpineCompactMemorySkeleton::from_source_plan(&node_id, &source_plan)?;
        if instruction
            .as_deref()
            .map(str::trim)
            .is_some_and(|instruction| !instruction.is_empty())
        {
            skeleton.ensure_instruction_slot();
        }
        if !skeleton.has_generated_slots() {
            return Ok(SpineCloseCompactOutcome::Compact(SpineCloseCompact {
                body: skeleton.assemble(std::iter::empty())?,
                source_context_range: source_plan.source_context_range,
                memory_output_tokens: None,
            }));
        }

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
        );
        let prompt = Prompt {
            input: prompt_input,
            tools: Vec::new(),
            parallel_tool_calls: false,
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            output_schema: Some(spine_close_compact_slot_schema(&skeleton)),
            output_schema_strict: true,
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
        let body = spine_close_compact_body(&node_id, &output, &skeleton)?;
        Ok(SpineCloseCompactOutcome::Compact(SpineCloseCompact {
            body,
            source_context_range: source_plan.source_context_range,
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
Write compact handoff slot bodies for Spine node {node_id}. Return strict JSON matching the provided schema, not a conversation reply.\n\n\
The runtime will assemble the final Markdown memory from trusted exact user messages, trusted child memory bodies, and your generated slot bodies. These slots will replace only this node's raw trajs in future context. Their job is to let the parent conversation continue correctly without replaying this node's raw trace.\n\n\
For each generated Memory Slot, write concise, concrete prose that preserves:\n\n\
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
- Exact `## User Message` and `## Child Memory` blocks in the final slot map are runtime-owned evidence. Do not copy or rewrite them into generated slot bodies.\n\
- If no `## User Message` block appears in the slot map, do not invent one.\n\
- Compact only adjacent assistant/tool/runtime source ordinals into the requested `## Memory Slot` JSON bodies.\n\
- Omit no requested slots; every slot body must be non-empty.\n\n\
Rules:\n\
- Follow the final Spine Compact Slot Map appended at the end of this prompt.\n\
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
    close_target_projection: &str,
    compact_instructions: &str,
    skeleton: &SpineCompactMemorySkeleton,
) {
    let slot_map = skeleton.prompt_slot_map();
    let tail = format!(
        "{close_target_projection}\n\n\
---------- Spine Suffix Boundary ----------\n\
The raw suffix for Spine node {node_id} is already present immediately before this tail directive.\n\
Source context range: [{}..{}).\n\n\
{compact_instructions}\n\n\
{slot_map}",
        skeleton.source_context_range.start, skeleton.source_context_range.end
    );
    prompt_input.push(spine_close_compact_system_message(&tail));
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
    close_call_id: &str,
) -> Result<(), SpineError> {
    for (expected_ordinal, entry) in source_plan.entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} does not match expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        let expected_context_index = source_plan
            .source_context_range
            .start
            .checked_add(expected_ordinal)
            .ok_or_else(|| {
                SpineError::CompactFailure(
                    "spine.close compact source context index overflow".to_string(),
                )
            })?;
        if entry.context_index != expected_context_index {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} has context_index {}, expected {expected_context_index}",
                entry.source_ordinal, entry.context_index
            )));
        }
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
    for item in &raw_items[source_plan.source_context_range.end..] {
        if !is_current_spine_close_like_carrier(item, close_call_id) {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact found non-carrier item after source range for call_id={close_call_id}: {item:?}"
            )));
        }
    }
    Ok(())
}

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
                && namespace.as_deref() == Some(SPINE_NAMESPACE)
                && is_spine_close_like_tool_name(name)
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

    fn has_generated_slots(&self) -> bool {
        self.slot_count > 0
    }

    fn ensure_instruction_slot(&mut self) {
        if self.slot_count > 0 {
            return;
        }
        self.slot_count += 1;
        self.blocks.push(SpineCompactMemoryBlock::MemorySlot {
            slot_id: format!("slot_{}", self.slot_count),
            entry_ordinals: Vec::new(),
            instruction_only: true,
        });
    }

    fn prompt_slot_map(&self) -> String {
        let mut text = format!(
            "{SPINE_COMPACT_SLOT_MAP_BOUNDARY}\n\
Return strict JSON with exactly this shape: {{\"memory_slots\":[{{\"slot_id\":\"slot_1\",\"body\":\"...\"}}]}}.\n\
Only fill generated Memory Slot bodies. Exact User Message and Child Memory blocks are assembled by runtime from trusted source-plan provenance.\n\
If no User Message block appears below, the final memory must not contain a User Message block.\n\
Do not include Markdown headings, <spine_memory> tags, close target text, or exact user/child text in slot bodies.\n\n\
Memory skeleton for Spine node {}:\n",
            self.node_id
        );
        for block in &self.blocks {
            match block {
                SpineCompactMemoryBlock::UserMessage(body) => {
                    text.push_str("\n## User Message\n");
                    text.push_str("${\n");
                    text.push_str(body);
                    if !body.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("}\n");
                }
                SpineCompactMemoryBlock::ChildMemory {
                    node_id,
                    compact_id,
                    body_hash,
                    body,
                } => {
                    text.push_str("\n## Child Memory\n");
                    text.push_str(&format!(
                        "$ {{node_id={node_id} compact_id={compact_id} body_hash={body_hash}}}\n"
                    ));
                    text.push_str("${\n");
                    text.push_str(body);
                    if !body.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("}\n");
                }
                SpineCompactMemoryBlock::MemorySlot {
                    slot_id,
                    entry_ordinals,
                    instruction_only,
                } => {
                    text.push_str("\n## Memory Slot\n");
                    if *instruction_only {
                        text.push_str(&format!(
                            "<{slot_id}: preserve close instruction and resume focus>\n"
                        ));
                    } else {
                        text.push_str(&format!(
                            "<{slot_id}: compact source ordinals {:?}>\n",
                            entry_ordinals
                        ));
                    }
                }
            }
        }
        text
    }

    fn assemble<'a>(
        &self,
        slots: impl IntoIterator<Item = (&'a str, &'a str)>,
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
                    let Some(slot_body) = slot_values.remove(slot_id) else {
                        return Err(SpineError::CompactFailure(format!(
                            "spine.close compact missing memory slot {slot_id}"
                        )));
                    };
                    validate_generated_slot_body(slot_id, &slot_body)?;
                    push_memory_block(&mut body, "## Memory Slot", &slot_body);
                }
            }
        }
        if let Some(extra) = slot_values.keys().next() {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact produced unexpected memory slot {extra}"
            )));
        }
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

fn spine_close_compact_slot_schema(skeleton: &SpineCompactMemorySkeleton) -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["memory_slots"],
        "properties": {
            "memory_slots": {
                "type": "array",
                "minItems": skeleton.slot_count,
                "maxItems": skeleton.slot_count,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["slot_id", "body"],
                    "properties": {
                        "slot_id": { "type": "string" },
                        "body": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn spine_close_compact_body(
    node_id: &str,
    output: &[ResponseItem],
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<String, SpineError> {
    if skeleton.node_id != node_id {
        return Err(SpineError::CompactFailure(format!(
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
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact produced unexpected tool call: {item:?}"
        )));
    }
    let mut json_entries = Vec::new();
    for item in output {
        if let ResponseItem::Message { role, .. } = item
            && role == "assistant"
            && let Some(text) = last_assistant_message_from_item(item, /*plan_mode*/ false)
            && !text.trim().is_empty()
        {
            json_entries.push(text);
        }
    }
    if json_entries.is_empty() {
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
    let joined = json_entries.join("\n");
    let slot_output: SpineCompactSlotOutput =
        serde_json::from_str(joined.trim()).map_err(|err| {
            SpineError::CompactFailure(format!(
                "spine.close compact produced invalid slot JSON: {err}"
            ))
        })?;
    let slots = slot_output
        .memory_slots
        .iter()
        .map(|slot| (slot.slot_id.as_str(), slot.body.as_str()));
    skeleton.assemble(slots)
}

#[derive(Debug, Deserialize)]
struct SpineCompactSlotOutput {
    memory_slots: Vec<SpineCompactSlot>,
}

#[derive(Debug, Deserialize)]
struct SpineCompactSlot {
    slot_id: String,
    body: String,
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

#[cfg(test)]
mod spine_close_slot_map_tests {
    use super::*;
    use crate::spine::NodeId;
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

    #[test]
    fn skeleton_preserves_exact_blocks_and_fills_generated_slots() {
        let plan = source_plan(vec![
            source_entry(2, 0, user_message("USER_EXACT\nline 2"), true),
            source_entry(3, 1, assistant_message("assistant details"), false),
            child_memory_entry(4, 2, "# Spine Memory 1.1.1\n\nchild body\n"),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(skeleton.has_generated_slots());
        let slot_map = skeleton.prompt_slot_map();
        assert!(slot_map.starts_with(SPINE_COMPACT_SLOT_MAP_BOUNDARY));
        assert!(slot_map.contains("${\nUSER_EXACT\nline 2\n}"));
        assert!(slot_map.contains("# Spine Memory 1.1.1"));
        assert!(slot_map.contains("<slot_1: compact source ordinals [1]>"));

        let body = skeleton
            .assemble([("slot_1", "compact assistant facts")])
            .expect("assembled body");
        assert!(body.contains("# Spine Memory 1.1"));
        assert!(body.contains("## User Message\nUSER_EXACT\nline 2"));
        assert!(body.contains("## Memory Slot\ncompact assistant facts"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild body"));
    }

    #[test]
    fn zero_slot_skeleton_assembles_without_model_slots() {
        let plan = source_plan(vec![
            source_entry(2, 0, user_message("only user one"), true),
            child_memory_entry(3, 1, "# Spine Memory 1.1.1\n\nchild exact\n"),
            source_entry(4, 2, user_message("only user two"), true),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(!skeleton.has_generated_slots());
        let body = skeleton
            .assemble(std::iter::empty())
            .expect("assembled body");
        assert!(body.contains("## User Message\nonly user one"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## User Message\nonly user two"));
        assert!(!body.contains("## Memory Slot"));
    }

    #[test]
    fn instruction_slot_does_not_invent_user_message() {
        let plan = source_plan(vec![child_memory_entry(
            2,
            0,
            "# Spine Memory 1.1.1\n\nchild exact\n",
        )]);
        let mut skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        assert!(!skeleton.has_generated_slots());
        skeleton.ensure_instruction_slot();

        let slot_map = skeleton.prompt_slot_map();
        assert!(!slot_map.contains("## User Message"));
        assert!(slot_map.contains("<slot_1: preserve close instruction and resume focus>"));

        let body = skeleton
            .assemble([("slot_1", "preserved close instruction facts")])
            .expect("assembled body");
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## Memory Slot\npreserved close instruction facts"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn slot_json_rejects_structure_pollution() {
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
            &[assistant_message(
                r###"{"memory_slots":[{"slot_id":"slot_1","body":"## User Message\npolluted"}]}"###,
            )],
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
    fn slot_json_rejects_missing_extra_and_invalid_output() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let missing = spine_close_compact_body(
            "1.1",
            &[assistant_message(r#"{"memory_slots":[]}"#)],
            &skeleton,
        )
        .expect_err("missing slot must fail");
        assert!(missing.to_string().contains("missing memory slot slot_1"));

        let extra = spine_close_compact_body(
            "1.1",
            &[assistant_message(
                r#"{"memory_slots":[{"slot_id":"slot_1","body":"ok"},{"slot_id":"slot_2","body":"extra"}]}"#,
            )],
            &skeleton,
        )
        .expect_err("extra slot must fail");
        assert!(extra.to_string().contains("unexpected memory slot slot_2"));

        let invalid = spine_close_compact_body("1.1", &[assistant_message("not json")], &skeleton)
            .expect_err("invalid JSON must fail");
        assert!(invalid.to_string().contains("invalid slot JSON"));
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
        let slot_map = skeleton.prompt_slot_map();
        assert!(slot_map.contains("<slot_1: compact source ordinals [0]>"));
        assert!(
            !slot_map.contains("## User Message"),
            "multi-content user messages should be summarized by generated slots, not exact-preserved"
        );
        let body = skeleton
            .assemble([("slot_1", "compact multimodal user facts")])
            .expect("assembled body");
        assert!(body.contains("## Memory Slot\ncompact multimodal user facts"));
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
}
