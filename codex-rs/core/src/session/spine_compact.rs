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
#[cfg(debug_assertions)]
use crate::spine::SpineStore;
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
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
#[cfg(debug_assertions)]
use codex_tools::create_tools_json_for_responses_api;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME: &str = "spine_compact_memory.md";
const SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER: &str = "{node_id}";
const SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER: &str = "{close_instruction}";
const SPINE_COMPACT_SOURCE_MAP_HEADER: &str = "Source map:";
const SPINE_CLOSE_COMPACT_MAX_FORMAT_REPAIRS: usize = 1;
const SPINE_CLOSE_COMPACT_REPAIR_EXCERPT_MAX_TOKENS: usize = 96;
const SPINE_CLOSE_COMPACT_JSON_SHAPE: &str = "{\"slots\":[],\"node_memory\":\"...\"}";
const SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME: &str = "submit_spine_memory";

fn spine_close_compact_memory_tool(skeleton: &SpineCompactMemorySkeleton) -> ToolSpec {
    let slot_ids = skeleton.slot_ids();
    let slot_id_schema = if slot_ids.is_empty() {
        JsonSchema::string(Some(
            "No slot ids are allowed for this node; submit an empty slots array.".to_string(),
        ))
    } else {
        JsonSchema::string_enum(
            slot_ids
                .iter()
                .map(|slot_id| serde_json::Value::String((*slot_id).to_string()))
                .collect(),
            Some("Allowed sparse memory slot id.".to_string()),
        )
    };
    let slot_item_schema = JsonSchema::object(
        BTreeMap::from([
            ("slot_id".to_string(), slot_id_schema),
            (
                "body".to_string(),
                JsonSchema::string(Some(
                    "Memory for the corresponding non-preserved span.".to_string(),
                )),
            ),
        ]),
        Some(vec!["slot_id".to_string(), "body".to_string()]),
        Some(false.into()),
    );
    let slots_schema = if slot_ids.is_empty() {
        JsonSchema::array_with_max_items(
            slot_item_schema,
            Some("No optional memory slots are allowed for this node; use [].".to_string()),
            0,
        )
    } else {
        JsonSchema::array(
            slot_item_schema,
            Some(
                "Sparse optional memory slots. Use [] unless a listed span must be carried forward."
                    .to_string(),
            ),
        )
    };
    let parameters = JsonSchema::object(
        BTreeMap::from([
            ("slots".to_string(), slots_schema),
            (
                "node_memory".to_string(),
                JsonSchema::string(Some(
                    "Required compact continuation record for this Spine node.".to_string(),
                )),
            ),
        ]),
        Some(vec!["slots".to_string(), "node_memory".to_string()]),
        Some(false.into()),
    );
    ToolSpec::Function(ResponsesApiTool {
        name: SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME.to_string(),
        description: "Submit the Spine close compact memory payload.".to_string(),
        strict: true,
        defer_loading: None,
        parameters,
        output_schema: None,
    })
}

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
            &compact_instructions,
            &skeleton,
        )?;
        let mut compact_tools = close_compact_tools;
        compact_tools.push(spine_close_compact_memory_tool(&skeleton));
        let mut prompt = Prompt {
            input: prompt_input,
            tools: compact_tools,
            parallel_tool_calls: false,
            base_instructions: self.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let mut repair_attempts = 0;
        let mut memory_output_tokens = None;
        #[cfg(debug_assertions)]
        let compact_debug_store =
            spine_close_compact_debug_store(self.as_ref(), turn_context.as_ref()).await;
        let body = loop {
            let compact_attempt = repair_attempts + 1;
            #[cfg(debug_assertions)]
            spine_close_compact_write_debug_request(
                compact_debug_store.as_ref(),
                turn_context.as_ref(),
                &node_id,
                close_call_id,
                compact_attempt,
                &source_plan,
                &prompt,
                ResponsesToolChoice::Function(SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME),
            );
            let summary_outcome = self
                .spine_close_summary_items(
                    turn_context,
                    native_compact_client_session,
                    prompt.clone(),
                    ResponsesToolChoice::Function(SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME),
                )
                .await?;
            let (output, token_usage) = match summary_outcome {
                SpineCloseSummaryOutcome::Output {
                    output,
                    token_usage,
                } => {
                    #[cfg(debug_assertions)]
                    spine_close_compact_write_debug_response(
                        compact_debug_store.as_ref(),
                        &node_id,
                        close_call_id,
                        compact_attempt,
                        &source_plan,
                        output.clone(),
                        token_usage.clone(),
                    );
                    (output, token_usage)
                }
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
                    let bad_output_excerpt = spine_close_compact_output_excerpt(&output);
                    prompt.input.push(spine_close_compact_developer_message(
                        &spine_close_compact_repair_text(
                            &err,
                            &skeleton,
                            bad_output_excerpt.as_deref(),
                        ),
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

#[cfg(debug_assertions)]
async fn spine_close_compact_debug_store(
    sess: &Session,
    turn_context: &TurnContext,
) -> Option<SpineStore> {
    if !turn_context.config.dev_debug_prompt_overrides {
        return None;
    }
    let rollout_path = match sess.current_rollout_path().await {
        Ok(Some(path)) => path,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!("skipping spine.close compact debug sidecar: {err}");
            return None;
        }
    };
    match SpineStore::for_rollout(&rollout_path) {
        Ok(store) => Some(store),
        Err(err) => {
            tracing::debug!(
                "skipping spine.close compact debug sidecar for {}: {err}",
                rollout_path.display()
            );
            None
        }
    }
}

#[cfg(debug_assertions)]
fn spine_close_compact_write_debug_request(
    store: Option<&SpineStore>,
    turn_context: &TurnContext,
    node_id: &str,
    call_id: &str,
    attempt: usize,
    source_plan: &SpineCompactSourcePlan,
    prompt: &Prompt,
    tool_choice: ResponsesToolChoice,
) {
    let Some(store) = store else {
        return;
    };
    let (tools, tools_serialization_error) =
        match create_tools_json_for_responses_api(&prompt.tools) {
            Ok(tools) => (Some(tools), None),
            Err(err) => (None, Some(err.to_string())),
        };
    let service_tier = turn_context
        .config
        .service_tier
        .clone()
        .filter(|service_tier| turn_context.model_info.supports_service_tier(service_tier));
    spine_close_compact_append_debug_record(
        store,
        &serde_json::json!({
            "event": "request",
            "context": spine_close_compact_debug_context(node_id, call_id, attempt, source_plan),
            "request": {
                "model": turn_context.model_info.slug,
                "instructions": prompt.base_instructions.text,
                "input": prompt.get_formatted_input(),
                "tools": tools,
                "tools_serialization_error": tools_serialization_error,
                "tool_choice": spine_close_compact_debug_tool_choice(tool_choice),
                "parallel_tool_calls": prompt.parallel_tool_calls,
                "reasoning_effort": turn_context.reasoning_effort.map(|effort| effort.to_string()),
                "reasoning_summary": turn_context.reasoning_summary.to_string(),
                "service_tier": service_tier,
                "turn_metadata_header": turn_context.turn_metadata_state.current_header_value(),
                "personality": prompt.personality.map(|personality| personality.to_string()),
                "output_schema": prompt.output_schema,
                "output_schema_strict": prompt.output_schema_strict,
            },
        }),
    );
}

#[cfg(debug_assertions)]
fn spine_close_compact_write_debug_response(
    store: Option<&SpineStore>,
    node_id: &str,
    call_id: &str,
    attempt: usize,
    source_plan: &SpineCompactSourcePlan,
    output: Vec<ResponseItem>,
    token_usage: Option<TokenUsage>,
) {
    let Some(store) = store else {
        return;
    };
    spine_close_compact_append_debug_record(
        store,
        &serde_json::json!({
            "event": "response",
            "context": spine_close_compact_debug_context(node_id, call_id, attempt, source_plan),
            "response": {
                "output": output,
                "token_usage": token_usage,
            },
        }),
    );
}

#[cfg(debug_assertions)]
fn spine_close_compact_debug_context(
    node_id: &str,
    call_id: &str,
    attempt: usize,
    source_plan: &SpineCompactSourcePlan,
) -> serde_json::Value {
    serde_json::json!({
        "node_id": node_id,
        "call_id": call_id,
        "attempt": attempt,
        "source_context_start": source_plan.source_context_range.start,
        "source_context_end": source_plan.source_context_range.end,
        "source_raw_start": source_plan.source_raw_range.start,
        "source_raw_end": source_plan.source_raw_range.end,
    })
}

#[cfg(debug_assertions)]
fn spine_close_compact_debug_tool_choice(tool_choice: ResponsesToolChoice) -> serde_json::Value {
    match tool_choice {
        ResponsesToolChoice::Auto => serde_json::json!("auto"),
        ResponsesToolChoice::Function(name) => serde_json::json!({
            "type": "function",
            "name": name,
        }),
    }
}

#[cfg(debug_assertions)]
fn spine_close_compact_append_debug_record(store: &SpineStore, record: &serde_json::Value) {
    if let Err(err) = store.append_compact_close_debug(record) {
        tracing::debug!("failed to write spine.close compact debug sidecar: {err}");
    }
}

/// Builds the close directive that turns a closed Spine node into durable memory.
///
/// The close pass is a compact continuation record, not a conversation reply.
/// The directive therefore asks for stable continuation state and calls out
/// exact identifiers, file paths, sentinels, and test names so later turns can
/// continue without replaying the raw trace.
fn spine_close_compact_instruction_text(
    node_id: &str,
    instruction: Option<&str>,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    let mut text =
        spine_close_compact_instruction_template(node_id, codex_home, dev_debug_prompt_overrides);
    text = text.replace(SPINE_COMPACT_MEMORY_NODE_ID_PLACEHOLDER, node_id);
    let instruction = instruction
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty());
    if text.contains(SPINE_COMPACT_MEMORY_CLOSE_INSTRUCTION_PLACEHOLDER) {
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
    node_id: &str,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if cfg!(debug_assertions) && dev_debug_prompt_overrides {
        let override_path = codex_home.join(SPINE_COMPACT_MEMORY_OVERRIDE_FILENAME);
        if let Ok(contents) = std::fs::read_to_string(override_path) {
            if !contents.is_empty() {
                return contents;
            }
        }
    }

    format!(
        "---------- SPINE MEMORY COMPACT ----------\n\
You are performing a CONTEXT CHECKPOINT COMPACTION for Spine node {node_id}.\n\
Write a compact continuation record for this Spine node.\n\
Do not answer the user.\n\n\
Call submit_spine_memory exactly once with this JSON payload shape:\n\
{SPINE_CLOSE_COMPACT_JSON_SHAPE}\n\n\
Contract:\n\n\
* node_memory is required. Keep only stable facts needed to continue: goal, key decisions, evidence, constraints, unresolved risks, and next actions.\n\
* When it helps user-facing continuation, start node_memory with a brief `User Intent Status:` block: latest intent, status, already said/done, continue from, do-not-repeat. Include it even without a new user message if this node changed an existing user intent. Omit it when it adds no information beyond ordinary work state. Keep the block short and keep node_memory concise.\n\
* slots is an array. Use [] unless a listed non-preserved span contains information needed later.\n\
* Each used slot must be exactly {{\"slot_id\":\"slot_N\",\"body\":\"...\"}}.\n\
* slot_id must be one of the allowed slot ids below.\n\
* Runtime already preserves exact user messages and child memory. Use them as evidence; do not copy them wholesale.\n\
* Do not narrate the full transcript or preserve stale exploration unless it changes the continuation state.\n\
* No extra fields. Do not emit prose outside the submit_spine_memory call."
    )
}

fn append_spine_close_compact_prompt_items(
    prompt_input: &mut Vec<ResponseItem>,
    compact_instructions: &str,
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<(), SpineError> {
    let slot_map = skeleton.prompt_slot_map()?;
    let tail = format!("{compact_instructions}\n\n{slot_map}");
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
    let expected_len = source_plan.source_context_range.len();
    if source_plan.entries.len() != expected_len {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact source entry count {} does not match source context range length {expected_len} for [{}..{})",
            source_plan.entries.len(),
            source_plan.source_context_range.start,
            source_plan.source_context_range.end
        )));
    }
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
                "spine.close compact source entry ordinal {} has context_index {}, expected {expected_context_index} for contiguous source context range [{}..{})",
                entry.source_ordinal,
                entry.context_index,
                source_plan.source_context_range.start,
                source_plan.source_context_range.end
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

    #[cfg(test)]
    fn output_schema(&self) -> serde_json::Value {
        let slot_ids = self.slot_ids();
        let slots_schema = if slot_ids.is_empty() {
            serde_json::json!({
                "type": "array",
                "maxItems": 0,
                "items": {
                    "type": "object",
                    "properties": {
                        "slot_id": {"type": "string"},
                        "body": {"type": "string"}
                    },
                    "required": ["slot_id", "body"],
                    "additionalProperties": false
                }
            })
        } else {
            serde_json::json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "slot_id": {
                            "type": "string",
                            "enum": slot_ids,
                        },
                        "body": {"type": "string"}
                    },
                    "required": ["slot_id", "body"],
                    "additionalProperties": false
                }
            })
        };
        serde_json::json!({
            "type": "object",
            "properties": {
                "slots": slots_schema,
                "node_memory": {"type": "string"}
            },
            "required": ["slots", "node_memory"],
            "additionalProperties": false
        })
    }

    fn prompt_slot_map(&self) -> Result<String, SpineError> {
        let optional_slot_ids = self.slot_ids();
        let allowed_slot_ids = if optional_slot_ids.is_empty() {
            "none".to_string()
        } else {
            optional_slot_ids.join(", ")
        };
        let mut text = format!(
            "{SPINE_COMPACT_SOURCE_MAP_HEADER}\n\
Node {} evidence order:\n\n",
            self.node_id
        );
        if !self
            .blocks
            .iter()
            .any(|block| matches!(block, SpineCompactMemoryBlock::UserMessage(_)))
        {
            text.push_str("* No preserved user messages exist in this node.\n");
        }
        let mut user_msg_ordinal = 0usize;
        for (block_index, block) in self.blocks.iter().enumerate() {
            match block {
                SpineCompactMemoryBlock::UserMessage(body) => {
                    user_msg_ordinal += 1;
                    text.push_str(&format!(
                        "* USER_MSG_{user_msg_ordinal} is preserved exactly:\n"
                    ));
                    push_indented_source_map_block(&mut text, body);
                }
                SpineCompactMemoryBlock::ChildMemory {
                    node_id,
                    compact_id,
                    body_hash,
                    body,
                } => {
                    text.push_str(&format!(
                        "* Child memory {node_id} is preserved exactly (compact_id={compact_id}, body_hash={body_hash}):\n"
                    ));
                    push_indented_source_map_block(&mut text, body);
                }
                SpineCompactMemoryBlock::MemorySlot {
                    slot_id,
                    entry_ordinals: _,
                    instruction_only: _,
                } => {
                    let label = slot_context_label(&self.blocks, block_index);
                    text.push_str(&format!("* {slot_id}: {label}\n"));
                }
            }
        }
        text.push_str(&format!("\nAllowed slot ids: {allowed_slot_ids}.\n"));
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

fn slot_context_label(blocks: &[SpineCompactMemoryBlock], slot_index: usize) -> String {
    let anchors = preserved_evidence_labels(blocks);
    let previous = anchors[..slot_index]
        .iter()
        .rev()
        .find_map(|anchor| anchor.as_deref());
    let next = anchors[slot_index + 1..]
        .iter()
        .find_map(|anchor| anchor.as_deref());
    match (previous, next) {
        (Some(previous), Some(next)) => {
            format!("optional non-preserved span after {previous} and before {next}.")
        }
        (Some(previous), None) => format!("optional non-preserved span after {previous}."),
        (None, Some(next)) => format!("optional non-preserved span before {next}."),
        (None, None) => "optional non-preserved span for the selected node body.".to_string(),
    }
}

fn preserved_evidence_labels(blocks: &[SpineCompactMemoryBlock]) -> Vec<Option<String>> {
    let mut labels = Vec::with_capacity(blocks.len());
    let mut user_ordinal = 0usize;
    for block in blocks {
        let label = match block {
            SpineCompactMemoryBlock::UserMessage(_) => {
                user_ordinal += 1;
                Some(format!("USER_MSG_{user_ordinal}"))
            }
            SpineCompactMemoryBlock::ChildMemory { node_id, .. } => {
                Some(format!("child memory {node_id}"))
            }
            SpineCompactMemoryBlock::MemorySlot { .. } => None,
        };
        labels.push(label);
    }
    labels
}

fn push_indented_source_map_block(text: &mut String, body: &str) {
    if body.is_empty() {
        text.push_str("  \n");
        return;
    }
    for line in body.lines() {
        text.push_str("  ");
        text.push_str(line);
        text.push('\n');
    }
    if body.ends_with('\n') {
        text.push_str("  \n");
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
    let mut carrier_arguments = None;
    let mut carrier_count = 0usize;
    for item in output {
        match item {
            ResponseItem::FunctionCall {
                name, arguments, ..
            } if name == SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME => {
                carrier_count += 1;
                carrier_arguments = Some(arguments.as_str());
            }
            ResponseItem::FunctionCall { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. } => {
                return Err(SpineCloseCompactBodyError::ToolCall(format!(
                    "spine.close compact produced unexpected {} tool call",
                    spine_close_compact_tool_call_kind(item)
                )));
            }
            _ => {}
        }
    }
    if carrier_count != 1 {
        let has_readable_assistant_message = output.iter().any(|item| {
            matches!(item, ResponseItem::Message { role, .. } if role == "assistant")
                && last_assistant_message_from_item(item, /*plan_mode*/ false)
                    .is_some_and(|text| !text.trim().is_empty())
        });
        if !has_readable_assistant_message
            && output.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
                )
            })
        {
            return Err(SpineCloseCompactBodyError::Fatal(
                "spine.close compact produced no readable memory body".to_string(),
            ));
        }
        return Err(SpineCloseCompactBodyError::Repairable(format!(
            "spine.close compact expected exactly one {SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME} function_call, got {carrier_count}"
        )));
    }
    let arguments = carrier_arguments.expect("carrier_count == 1 means arguments exists");
    let parsed_blocks = parse_spine_close_compact_json(arguments, skeleton)?;
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
    bad_output_excerpt: Option<&str>,
) -> String {
    let optional_slot_ids = skeleton.slot_ids();
    let optional_slots = if optional_slot_ids.is_empty() {
        "Allowed slot ids: none.".to_string()
    } else {
        format!("Allowed slot ids: {}.", optional_slot_ids.join(", "))
    };
    let bad_output_excerpt = bad_output_excerpt
        .map(str::trim)
        .filter(|excerpt| !excerpt.is_empty())
        .map(|excerpt| format!("Bad output excerpt (truncated): {excerpt}\n\n"))
        .unwrap_or_default();
    format!(
        "Format repair only. Do not answer the user.\n\
Rejected output: {}\n\
{}\
Call submit_spine_memory exactly once with this JSON payload shape:\n\
{SPINE_CLOSE_COMPACT_JSON_SHAPE}\n\n\
* node_memory is required and must be non-empty.\n\
* slots is an array. Use [] unless a listed non-preserved span must be carried forward.\n\
* Each slot must be exactly {{\"slot_id\":\"slot_N\",\"body\":\"...\"}}.\n\
* {optional_slots}\n\
* No extra fields. Do not emit prose outside the submit_spine_memory call.",
        err.message(),
        bad_output_excerpt,
    )
}

fn spine_close_compact_output_excerpt(output: &[ResponseItem]) -> Option<String> {
    let text = output
        .iter()
        .filter_map(|item| last_assistant_message_from_item(item, /*plan_mode*/ false))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    spine_close_compact_excerpt_text(&text)
}

fn spine_close_compact_excerpt_text(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.trim().is_empty() {
        return None;
    }
    let truncated = truncate_text(
        &collapsed,
        TruncationPolicy::Tokens(SPINE_CLOSE_COMPACT_REPAIR_EXCERPT_MAX_TOKENS),
    );
    let excerpt = truncated.trim();
    if excerpt.is_empty() {
        None
    } else {
        Some(excerpt.to_string())
    }
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpineCloseCompactJsonOutput {
    slots: Vec<SpineCloseCompactJsonSlot>,
    node_memory: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpineCloseCompactJsonSlot {
    slot_id: String,
    body: String,
}

fn parse_spine_close_compact_json(
    text: &str,
    skeleton: &SpineCompactMemorySkeleton,
) -> Result<ParsedSpineCompactBlocks, SpineCloseCompactBodyError> {
    let parsed: SpineCloseCompactJsonOutput = serde_json::from_str(text).map_err(|err| {
        SpineCloseCompactBodyError::Repairable(format!(
            "spine.close compact produced invalid JSON memory body: {err}"
        ))
    })?;
    if parsed.node_memory.trim().is_empty() {
        return Err(SpineCloseCompactBodyError::Repairable(
            "spine.close compact produced empty node_memory".to_string(),
        ));
    }
    let allowed_slots = skeleton
        .slot_ids()
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    let mut parsed_slots = std::collections::BTreeMap::<String, String>::new();
    for slot in parsed.slots {
        if !allowed_slots.contains(&slot.slot_id) {
            return Err(SpineCloseCompactBodyError::Repairable(format!(
                "spine.close compact produced unexpected memory slot {}",
                slot.slot_id
            )));
        }
        if slot.body.trim().is_empty() {
            return Err(SpineCloseCompactBodyError::Repairable(format!(
                "spine.close compact produced empty memory slot {}",
                slot.slot_id
            )));
        }
        if parsed_slots
            .insert(slot.slot_id.clone(), slot.body)
            .is_some()
        {
            return Err(SpineCloseCompactBodyError::Repairable(format!(
                "spine.close compact produced duplicate memory slot {}",
                slot.slot_id
            )));
        }
    }

    let ordered_slots = skeleton
        .slot_ids()
        .into_iter()
        .filter_map(|slot_id| {
            parsed_slots
                .remove(slot_id)
                .map(|body| (slot_id.to_string(), body))
        })
        .collect();

    Ok(ParsedSpineCompactBlocks {
        slots: ordered_slots,
        node_memory: parsed.node_memory,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpineCompactBlockTag {
    Slot(String),
    NodeMemory,
    UserMsg(String),
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
    if let Some(marker) = forbidden_generated_body_structure_marker(body) {
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
            "spine.close compact produced empty node_memory".to_string(),
        ));
    }
    if let Some(marker) = forbidden_generated_body_structure_marker(body) {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact node_memory contains forbidden structure marker {marker:?}"
        )));
    }
    Ok(())
}

fn forbidden_generated_body_structure_marker(body: &str) -> Option<String> {
    const FORBIDDEN_SUBSTRINGS: &[&str] = &[
        "# Spine Memory ",
        "---------- SPINE MEMORY COMPACT ----------",
        "---------- Spine Compact Directive ----------",
        "---------- Spine Close Target ----------",
        "## User Message",
        "## Child Memory",
        "## Memory Slot",
        "## Node Memory",
        "USER_MSG",
    ];
    if let Some(marker) = FORBIDDEN_SUBSTRINGS
        .iter()
        .find(|marker| body.contains(**marker))
    {
        return Some((*marker).to_string());
    }

    body.lines()
        .map(str::trim)
        .find(|line| is_forbidden_generated_body_tag_line(line))
        .map(str::to_string)
}

fn is_forbidden_generated_body_tag_line(line: &str) -> bool {
    if line == "<spine_memory>" || line == "</spine_memory>" {
        return true;
    }
    matches!(
        spine_compact_line_marker(line),
        Some(SpineCompactLineMarker::Start(_))
            | Some(SpineCompactLineMarker::End(_))
            | Some(SpineCompactLineMarker::Malformed)
    )
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

    fn spine_compact_carrier_call(arguments: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: SPINE_CLOSE_COMPACT_MEMORY_TOOL_NAME.to_string(),
            namespace: None,
            arguments: arguments.to_string(),
            call_id: "submit-spine-memory".to_string(),
        }
    }

    fn compact_json(optional_slots: &[(&str, &str)], node_memory: &str) -> String {
        let slots = optional_slots
            .iter()
            .map(|(slot_id, body)| {
                serde_json::json!({
                    "slot_id": slot_id,
                    "body": body,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "slots": slots,
            "node_memory": node_memory,
        })
        .to_string()
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

        assert!(text.contains("SPINE MEMORY COMPACT"));
        assert!(text.contains("Spine node 1.1"));
        assert!(text.ends_with("preserve this exact detail"));
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

        assert!(text.contains("SPINE MEMORY COMPACT"));
        assert!(!text.contains("SHOULD_NOT_APPEAR"));
    }

    #[test]
    fn spine_close_compact_instruction_empty_override_falls_back_to_builtin() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        std::fs::write(codex_home.path().join("spine_compact_memory.md"), "")
            .expect("write override");

        let text = spine_close_compact_instruction_text("1.4", None, codex_home.path(), true);

        assert!(text.contains("SPINE MEMORY COMPACT"));
        assert!(text.contains("Spine node 1.4"));
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
        assert!(slot_map.starts_with(SPINE_COMPACT_SOURCE_MAP_HEADER));
        assert!(slot_map.contains("* USER_MSG_1 is preserved exactly:\n  USER_EXACT\n  line 2\n"));
        assert!(slot_map.contains("# Spine Memory 1.1.1"));
        assert!(!slot_map.contains("----- BEGIN CHILD MEMORY -----"));
        assert!(slot_map.contains(
            "* slot_1: optional non-preserved span after USER_MSG_1 and before child memory 1.1.1."
        ));
        assert!(slot_map.contains("Allowed slot ids: slot_1."));
        assert!(!slot_map.contains("source ordinals"));
        assert!(!slot_map.contains("<SPINE_SLOT_1>"));
        assert!(!slot_map.contains("<SPINE_NODE_MEMORY>"));
        assert!(!slot_map.contains("<USER_MSG_1>"));
        assert!(!slot_map.contains("<spine_memory>"));

        let schema = skeleton.output_schema();
        assert_eq!(
            schema["properties"]["slots"]["items"]["properties"]["slot_id"]["enum"],
            serde_json::json!(["slot_1"])
        );
        assert_eq!(schema["properties"]["node_memory"]["type"], "string");
        assert_eq!(schema["additionalProperties"], false);

        let body = skeleton
            .assemble(
                [("slot_1", "compact assistant facts")],
                "node continuation facts",
            )
            .expect("assembled body");
        assert!(body.contains("# Spine Memory 1.1"));
        assert!(body.contains("## User Message\nUSER_EXACT\nline 2"));
        assert!(body.contains("## Memory Slot\ncompact assistant facts"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild body"));
        assert!(body.contains("## Node Memory\nnode continuation facts"));
    }

    #[test]
    fn slot_map_preserves_source_layout_without_xml_tags() {
        let plan = source_plan(vec![
            source_entry(2, 0, assistant_message("before user"), false),
            source_entry(3, 1, user_message("USER_EXACT"), true),
            source_entry(4, 2, assistant_message("after user"), false),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let slot_map = skeleton.prompt_slot_map().expect("slot map");
        assert!(slot_map.starts_with(SPINE_COMPACT_SOURCE_MAP_HEADER));
        assert!(slot_map.contains("* slot_1: optional non-preserved span before USER_MSG_1."));
        assert!(slot_map.contains("* slot_2: optional non-preserved span after USER_MSG_1."));
        assert!(slot_map.contains("* USER_MSG_1 is preserved exactly:\n  USER_EXACT\n"));
        assert!(
            !slot_map
                .lines()
                .any(|line| matches!(line, "<SPINE_SLOT_1>" | "</SPINE_SLOT_1>"))
        );
        assert!(
            !slot_map
                .lines()
                .any(|line| matches!(line, "<SPINE_SLOT_2>" | "</SPINE_SLOT_2>"))
        );
        assert!(!slot_map.contains("source ordinals"));
        assert!(!slot_map.contains("<USER_MSG_1>"));
        assert!(!slot_map.contains("<spine_memory>"));
    }

    #[test]
    fn compact_json_requires_slots_key_and_accepts_empty_slots_array() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let missing_slots = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(
                &serde_json::json!({
                    "node_memory": "node"
                })
                .to_string(),
            )],
            &skeleton,
        )
        .expect_err("missing slots key must be repairable");
        assert!(
            missing_slots
                .to_string()
                .contains("produced invalid JSON memory body"),
            "unexpected error: {missing_slots}"
        );

        let empty_slots = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(
                &serde_json::json!({
                    "slots": [],
                    "node_memory": "node"
                })
                .to_string(),
            )],
            &skeleton,
        )
        .expect("empty slots array should be accepted");
        assert!(!empty_slots.contains("## Memory Slot"));
        assert!(empty_slots.contains("## Node Memory\nnode"));
    }

    #[test]
    fn compact_repair_excerpt_collapses_and_truncates_bad_output() {
        let long_text = (0..200)
            .map(|index| format!("token-{index}"))
            .collect::<Vec<_>>()
            .join(" \n ");
        let excerpt = spine_close_compact_excerpt_text(&long_text).expect("excerpt");

        assert!(excerpt.contains("token-0"));
        assert!(excerpt.len() < long_text.len());
        assert!(!excerpt.contains('\n'));
    }

    #[test]
    fn compact_repair_text_includes_excerpt_and_short_shape() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
        let text = spine_close_compact_repair_text(
            &SpineCloseCompactBodyError::Repairable(
                "spine.close compact produced invalid JSON memory body: expected value at line 1 column 1".to_string(),
            ),
            &skeleton,
            Some("ordinary dialogue instead of JSON"),
        );

        assert!(text.contains("Format repair only."));
        assert!(text.contains("ordinary dialogue instead of JSON"));
        assert!(text.contains("Bad output excerpt (truncated):"));
        assert!(text.contains("Allowed slot ids: slot_1."));
        assert!(text.contains("{\"slots\":[],\"node_memory\":\"...\"}"));
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
        assert!(!slot_map.contains("<spine_memory>"));
        let schema = skeleton.output_schema();
        assert_eq!(schema["properties"]["slots"]["maxItems"], 0);
        assert_eq!(
            schema["required"],
            serde_json::json!(["slots", "node_memory"])
        );

        let body = skeleton
            .assemble(std::iter::empty(), "preserved close instruction facts")
            .expect("assembled body");
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## Node Memory\npreserved close instruction facts"));
        assert!(!body.contains("## Memory Slot"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn compact_json_accepts_optional_slot_and_node_memory_markdown() {
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
            &[spine_compact_carrier_call(&compact_json(
                &[(
                    "slot_1",
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
        .expect("compact JSON should parse");

        assert!(body.contains("## Memory Slot\nPreserve path"));
        assert!(body.contains("### Markdown heading is allowed inside a slot"));
        assert!(body.contains(r#"{"quoted":"json is content, not protocol"}"#));
        assert!(body.contains("## Node Memory\nNode memory can contain Markdown bullets"));
    }

    #[test]
    fn compact_json_allows_omitting_optional_slot() {
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
            &[spine_compact_carrier_call(&compact_json(
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
    fn compact_tool_carrier_ignores_incidental_assistant_message() {
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
            &[
                assistant_message("Incidental prose should not be parsed as compact memory."),
                spine_compact_carrier_call(&compact_json(
                    &[],
                    "carrier arguments are the compact memory source of truth",
                )),
            ],
            &skeleton,
        )
        .expect("valid forced carrier arguments should be accepted");

        assert!(!body.contains("Incidental prose"));
        assert!(
            body.contains(
                "## Node Memory\ncarrier arguments are the compact memory source of truth"
            )
        );
    }

    #[test]
    fn compact_json_allows_inline_protocol_marker_discussion() {
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
            &[spine_compact_carrier_call(&compact_json(
                &[(
                    "slot_1",
                    "The compact validator rejected <SPINE_SLOT_ markers in generated memory.",
                )],
                "The failure involved a literal <SPINE_SLOT_ substring in the node memory body.",
            ))],
            &skeleton,
        )
        .expect("inline protocol marker discussion should be accepted");

        assert!(body.contains("## Memory Slot\nThe compact validator rejected <SPINE_SLOT_"));
        assert!(
            body.contains("## Node Memory\nThe failure involved a literal <SPINE_SLOT_ substring")
        );
    }

    #[test]
    fn compact_json_rejects_structure_pollution() {
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
            &[spine_compact_carrier_call(&compact_json(
                &[("slot_1", "## User Message\npolluted")],
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
    fn compact_json_rejects_standalone_body_control_tags() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let nested_slot_tag = skeleton
            .assemble([], "before\n<SPINE_SLOT_1>\nafter")
            .expect_err("standalone slot tag inside node memory must fail");
        assert!(
            nested_slot_tag
                .to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {nested_slot_tag}"
        );

        let runtime_tag = skeleton
            .assemble([("slot_1", "before\n<spine_memory>\nafter")], "node memory")
            .expect_err("standalone runtime memory tag inside slot body must fail");
        assert!(
            runtime_tag
                .to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {runtime_tag}"
        );
    }

    #[test]
    fn compact_json_rejects_wrapped_xml_and_fences_as_invalid_json() {
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
            &[spine_compact_carrier_call(
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
                .contains("produced invalid JSON memory body")
        );

        let fenced = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(
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
                .contains("produced invalid JSON memory body")
        );
    }

    #[test]
    fn compact_json_rejects_missing_extra_duplicate_and_invalid_output() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let missing = spine_close_compact_body("1.1", &[assistant_message("")], &skeleton)
            .expect_err("missing carrier call must fail");
        assert!(
            missing
                .to_string()
                .contains("expected exactly one submit_spine_memory function_call"),
            "unexpected error: {missing}"
        );

        let extra = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(&compact_json(
                &[("slot_1", "ok"), ("slot_2", "extra")],
                "node",
            ))],
            &skeleton,
        )
        .expect_err("extra slot must fail");
        assert!(extra.to_string().contains("unexpected memory slot slot_2"));

        let duplicate = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(
                &serde_json::json!({
                    "slots": [
                        {"slot_id": "slot_1", "body": "first"},
                        {"slot_id": "slot_1", "body": "second"}
                    ],
                    "node_memory": "node"
                })
                .to_string(),
            )],
            &skeleton,
        )
        .expect_err("duplicate slot must fail");
        assert!(
            duplicate
                .to_string()
                .contains("duplicate memory slot slot_1")
        );

        let invalid =
            spine_close_compact_body("1.1", &[spine_compact_carrier_call("not json")], &skeleton)
                .expect_err("invalid JSON output must fail");
        assert!(
            invalid
                .to_string()
                .contains("produced invalid JSON memory body")
        );

        let unknown_field = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(
                &serde_json::json!({
                    "slots": [],
                    "node_memory": "node",
                    "extra": "field"
                })
                .to_string(),
            )],
            &skeleton,
        )
        .expect_err("extra JSON field must fail");
        assert!(
            unknown_field
                .to_string()
                .contains("produced invalid JSON memory body")
        );
    }

    #[test]
    fn compact_json_rejects_node_memory_missing_empty_and_user_msg() {
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
            &[spine_compact_carrier_call(
                &serde_json::json!({
                    "slots": [{"slot_id": "slot_1", "body": "optional"}]
                })
                .to_string(),
            )],
            &skeleton,
        )
        .expect_err("missing node memory must fail");
        assert!(
            missing_node
                .to_string()
                .contains("produced invalid JSON memory body")
        );

        let empty_node = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(&compact_json(&[], "  "))],
            &skeleton,
        )
        .expect_err("empty node memory must fail");
        assert!(empty_node.to_string().contains("empty node_memory"));

        let user_msg = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(&compact_json(
                &[],
                "before\n<USER_MSG_1>\ndo not return this\n</USER_MSG_1>\nafter",
            ))],
            &skeleton,
        )
        .expect_err("returned USER_MSG must fail");
        assert!(
            user_msg
                .to_string()
                .contains("contains forbidden structure marker")
        );

        let empty = spine_close_compact_body(
            "1.1",
            &[spine_compact_carrier_call(&compact_json(
                &[("slot_1", "   ")],
                "node",
            ))],
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
        assert!(
            slot_map.contains("* slot_1: optional non-preserved span for the selected node body.")
        );
        assert!(slot_map.contains("Allowed slot ids: slot_1."));
        assert!(
            !slot_map.contains("<USER_MSG_"),
            "multi-content user messages should be summarized by generated slots, not exact-preserved"
        );
        let body = skeleton
            .assemble(
                [("slot_1", "compact multimodal user facts")],
                "node multimodal continuation",
            )
            .expect("assembled body");
        assert!(body.contains("## Memory Slot\ncompact multimodal user facts"));
        assert!(body.contains("## Node Memory\nnode multimodal continuation"));
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
    fn source_plan_validator_rejects_non_contiguous_context_indices() {
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

        let err = validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect_err("non-contiguous real context indices must fail");
        assert!(
            err.to_string()
                .contains("source entry count 2 does not match source context range length 3"),
            "unexpected non-contiguous context error: {err}"
        );
    }

    #[test]
    fn source_plan_validator_rejects_duplicate_context_indices() {
        let raw_items = vec![
            user_message("prefix 0"),
            user_message("prefix 1"),
            assistant_message("source 2"),
            assistant_message("source 3"),
        ];
        let plan = source_plan_with_context_range(
            2..4,
            vec![
                source_entry(2, 0, raw_items[2].clone(), false),
                source_entry(2, 1, raw_items[2].clone(), false),
            ],
        );

        let err = validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect_err("duplicate context indices must fail");
        assert!(
            err.to_string().contains("has context_index 2, expected 3"),
            "unexpected duplicate context error: {err}"
        );
    }
}
