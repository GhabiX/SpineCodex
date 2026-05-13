use super::ids::NodeId;
use super::store::SpineOperation;
use super::view::display_node_id;
use super::view::op_label;
use super::view::relative_node_trajs_path;
use super::view::relative_worklog_path;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::client_common::ResponseEvent;
use crate::session::session::Session;
use crate::session::turn::get_last_assistant_message_from_turn;
use crate::session::turn_context::TurnContext;
use crate::util::backoff;
use async_trait::async_trait;
use codex_async_utils::OrCancelExt;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactInput {
    pub(crate) op: SpineOperation,
    pub(crate) node_id: NodeId,
    pub(crate) scope_node_id: Option<NodeId>,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) spine_tree: String,
    pub(crate) prefix_items: Vec<ResponseItem>,
    pub(crate) suffix_items: Vec<ResponseItem>,
    pub(crate) transition_summary: String,
    pub(crate) compact_instruction: Option<String>,
    pub(crate) rollout_path: PathBuf,
    pub(crate) raw_mirror_path: PathBuf,
    pub(crate) sidecar_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCompactBoundary {
    pub(crate) op: SpineOperation,
    pub(crate) node_id: NodeId,
    pub(crate) scope_node_id: Option<NodeId>,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) transition_summary: String,
    pub(crate) compact_instruction: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactPlan {
    pub(crate) input: SpineCompactInput,
    pub(crate) cut_index: usize,
    pub(crate) fold_end_index: usize,
    pub(crate) replacement_tail: Vec<ResponseItem>,
    pub(crate) worklog_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactOutput {
    pub(crate) worklog_markdown: String,
    pub(crate) compact_message: String,
    pub(crate) strategy_name: &'static str,
}

#[async_trait]
pub(crate) trait SpineCompactStrategy: Send + Sync {
    async fn compact_suffix(&self, input: SpineCompactInput) -> CodexResult<SpineCompactOutput>;
}

pub(crate) const CODEX_BUILTIN_TEXT_STRATEGY: &str = "codex_builtin_fork_full_history";
const COMPACT_WORKLOG_OPEN_TAG: &str = "<spine_compact_worklog>";
const COMPACT_WORKLOG_CLOSE_TAG: &str = "</spine_compact_worklog>";

pub(crate) async fn compact_suffix_with_codex_builtin_text(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    prompt_envelope: &Prompt,
    input: SpineCompactInput,
    cancellation_token: &CancellationToken,
) -> CodexResult<SpineCompactOutput> {
    let prompt = build_codex_builtin_prompt(&input, turn_context.compact_prompt(), prompt_envelope);
    let max_retries = turn_context.provider.info().stream_max_retries();
    let mut retries = 0;
    let compacted_suffix = loop {
        match collect_compaction_response(
            sess,
            turn_context,
            client_session,
            &prompt,
            cancellation_token,
        )
        .await
        {
            Ok(text) => break text,
            Err(err) if err.is_retryable() && retries < max_retries => {
                retries += 1;
                let delay = backoff(retries);
                warn!("spine compact stream failed; retrying ({retries}/{max_retries})");
                sess.notify_stream_error(
                    turn_context,
                    format!("Reconnecting... {retries}/{max_retries}"),
                    err,
                )
                .await;
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    };

    let compacted_suffix = extract_spine_compact_worklog(&compacted_suffix)?;
    let worklog_markdown = render_auto_compact_worklog(&input, &compacted_suffix);
    Ok(SpineCompactOutput {
        compact_message: format!(
            "Spine compacted {} [{}, {})",
            input.node_id, input.cut_ordinal, input.fold_end_ordinal
        ),
        worklog_markdown,
        strategy_name: CODEX_BUILTIN_TEXT_STRATEGY,
    })
}

fn build_codex_builtin_prompt(
    input: &SpineCompactInput,
    compact_prompt: &str,
    prompt_envelope: &Prompt,
) -> Prompt {
    Prompt {
        input: build_codex_builtin_prompt_input(input, compact_prompt),
        tools: prompt_envelope.tools.clone(),
        parallel_tool_calls: prompt_envelope.parallel_tool_calls,
        base_instructions: prompt_envelope.base_instructions.clone(),
        personality: prompt_envelope.personality,
        // The internal compact response is parsed from the XML-like block below.
        // Carrying a user turn final-output schema would make that response invalid.
        output_schema: None,
        output_schema_strict: true,
    }
}

fn build_codex_builtin_prompt_input(
    input: &SpineCompactInput,
    compact_prompt: &str,
) -> Vec<ResponseItem> {
    let mut prompt_input =
        Vec::with_capacity(input.prefix_items.len() + input.suffix_items.len() + 1);
    prompt_input.extend(input.prefix_items.clone());
    prompt_input.extend(input.suffix_items.clone());
    let target_tree_node_id = display_node_id(&input.node_id);
    let compact_instruction = input
        .compact_instruction
        .as_deref()
        .map(|instruction| format!("\n\nAdditional compaction guidance: {instruction}"))
        .unwrap_or_default();
    prompt_input.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "{compact_prompt}\n\nCompact only the target suffix represented by node `{}` in this Spine Tree. Write a concise assistant worklog that records what the agent did, where it stopped, and the smallest facts needed to continue.\nUse temporal locality to keep the latest decisions, blockers, validation status, and next concrete step.\nUse spatial locality to keep only the relevant files, functions, tests, commands, errors, node ids, and paths. Drop chatter, duplicate instructions, and imperative continuation text.\n\nTarget tree node: {}\nInternal node id: {}\nTarget operation: {}\nSpine Tree summary label: {}\n\n<spine_tree>\n{}\n</spine_tree>{}\n\nReturn exactly one XML-like block and no text outside it:\n{}\n<dense Markdown compact for the target suffix only>\n{}",
                target_tree_node_id,
                target_tree_node_id,
                input.node_id,
                op_label(input.op),
                input.transition_summary,
                input.spine_tree,
                compact_instruction,
                COMPACT_WORKLOG_OPEN_TAG,
                COMPACT_WORKLOG_CLOSE_TAG
            ),
        }],
        phase: None,
    });
    prompt_input
}

async fn collect_compaction_response(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut crate::client::ModelClientSession,
    prompt: &Prompt,
    cancellation_token: &CancellationToken,
) -> CodexResult<String> {
    let mut stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            turn_context
                .turn_metadata_state
                .current_header_value()
                .as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .or_cancel(cancellation_token)
        .await??;
    let mut output_items = Vec::new();
    loop {
        let event = match stream.next().or_cancel(cancellation_token).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(CodexErr::Stream(
                    "stream closed before spine compact response.completed".into(),
                    None,
                ));
            }
            Err(codex_async_utils::CancelErr::Cancelled) => return Err(CodexErr::TurnAborted),
        };
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => output_items.push(item),
            Ok(ResponseEvent::ServerReasoningIncluded(included)) => {
                sess.set_server_reasoning_included(included).await;
            }
            Ok(ResponseEvent::RateLimits(snapshot)) => {
                sess.update_rate_limits(turn_context, snapshot).await;
            }
            Ok(ResponseEvent::Completed { token_usage, .. }) => {
                sess.update_token_usage_info(turn_context, token_usage.as_ref())
                    .await;
                return get_last_assistant_message_from_turn(&output_items).ok_or_else(|| {
                    CodexErr::Fatal("spine compact produced no assistant summary".to_string())
                });
            }
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
}

fn extract_spine_compact_worklog(text: &str) -> CodexResult<String> {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix(COMPACT_WORKLOG_OPEN_TAG) else {
        return Err(CodexErr::Fatal(format!(
            "spine compact response must start with {COMPACT_WORKLOG_OPEN_TAG}"
        )));
    };
    let Some(body) = after_open.strip_suffix(COMPACT_WORKLOG_CLOSE_TAG) else {
        return Err(CodexErr::Fatal(format!(
            "spine compact response must end with {COMPACT_WORKLOG_CLOSE_TAG}"
        )));
    };
    if body.contains(COMPACT_WORKLOG_OPEN_TAG) || body.contains(COMPACT_WORKLOG_CLOSE_TAG) {
        return Err(CodexErr::Fatal(
            "spine compact response contains nested compact worklog tags".to_string(),
        ));
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(CodexErr::Fatal(
            "spine compact response worklog is empty".to_string(),
        ));
    }
    Ok(body.to_string())
}

pub(crate) fn render_auto_compact_worklog(
    input: &SpineCompactInput,
    compacted_suffix: &str,
) -> String {
    let raw_mirror_path = input
        .raw_mirror_path
        .strip_prefix(&input.sidecar_root)
        .unwrap_or(input.raw_mirror_path.as_path());
    let node_trajs_path = relative_node_trajs_path(&input.node_id);
    format!(
        "\n\n## Auto Compact\n\nBase: {}\nFold: response ordinals [{}, {})\nNode trajs: {}\nRaw mirror: {}\nRollout: {}\n\n{}\n\n## Node Summary\n\n{}\n",
        input.sidecar_root.display(),
        input.cut_ordinal,
        input.fold_end_ordinal,
        node_trajs_path.display(),
        raw_mirror_path.display(),
        input.rollout_path.display(),
        compacted_suffix,
        input.transition_summary
    )
}

pub(crate) fn build_suffix_replacement_history(
    old_history: &[ResponseItem],
    cut_index: usize,
    fold_end_index: usize,
    ir_items: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    let mut replacement_history = Vec::with_capacity(
        cut_index + ir_items.len() + old_history.len().saturating_sub(fold_end_index),
    );
    replacement_history.extend_from_slice(&old_history[..cut_index]);
    replacement_history.extend(ir_items);
    replacement_history.extend_from_slice(&old_history[fold_end_index..]);
    replacement_history
}

pub(crate) fn plan_suffix_fold(
    history: &[ResponseItem],
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    input: SpineCompactInput,
) -> CodexResult<SpineCompactPlan> {
    let cut_index = effective_index_for_raw_ordinal(history, cut_ordinal).ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact cut ordinal {cut_ordinal} does not map to an effective history index"
        ))
    })?;
    let fold_end_index = effective_index_for_raw_ordinal(history, fold_end_ordinal).ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact fold_end ordinal {fold_end_ordinal} does not map to an effective history index"
        ))
    })?;
    if cut_index > fold_end_index {
        return Err(CodexErr::Fatal(format!(
            "spine compact cut index {cut_index} is after fold end index {fold_end_index}"
        )));
    }
    if cut_index == fold_end_index {
        return Err(CodexErr::Fatal(
            "spine compact fold range is empty after mapping".to_string(),
        ));
    }
    let cut_index = adjusted_cut_index_after_prefix_closure(history, cut_index, fold_end_index);
    let (cut_index, fold_end_index) =
        adjusted_range_for_tool_call_closure(history, cut_index, fold_end_index);
    let cut_ordinal = raw_ordinal_for_effective_index(history, cut_index).ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact adjusted cut index {cut_index} does not map to a raw ordinal"
        ))
    })?;
    let fold_end_ordinal = raw_ordinal_for_effective_index(history, fold_end_index).ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact adjusted fold end index {fold_end_index} does not map to a raw ordinal"
        ))
    })?;
    let mut input = input;
    input.cut_ordinal = cut_ordinal;
    input.fold_end_ordinal = fold_end_ordinal;
    input.prefix_items = history[..cut_index].to_vec();
    input.suffix_items = history[cut_index..fold_end_index].to_vec();

    Ok(SpineCompactPlan {
        worklog_path: input
            .sidecar_root
            .join(relative_worklog_path(&input.node_id)),
        replacement_tail: history[fold_end_index..].to_vec(),
        input,
        cut_index,
        fold_end_index,
    })
}

fn adjusted_cut_index_after_prefix_closure(
    history: &[ResponseItem],
    cut_index: usize,
    fold_end_index: usize,
) -> usize {
    if cut_index == 0
        || cut_index >= fold_end_index
        || !matches!(
            history.get(cut_index - 1),
            Some(ResponseItem::FunctionCallOutput { .. })
        )
    {
        return cut_index;
    }

    let mut first_user_index = None;
    for index in cut_index..fold_end_index {
        if matches!(
            history.get(index),
            Some(ResponseItem::Message { role, .. }) if role == "user"
        ) {
            first_user_index = Some(index);
            break;
        }
    }

    match first_user_index {
        Some(index) if index > cut_index => index,
        _ => cut_index,
    }
}

fn adjusted_range_for_tool_call_closure(
    history: &[ResponseItem],
    mut cut_index: usize,
    mut fold_end_index: usize,
) -> (usize, usize) {
    loop {
        let calls_in_range = call_ids_in(history, cut_index, fold_end_index);
        let outputs_in_range = output_call_ids_in(history, cut_index, fold_end_index);
        let mut changed = false;

        if let Some(index) = first_output_for_call_after(history, fold_end_index, &calls_in_range) {
            fold_end_index = index.saturating_add(1);
            changed = true;
        }

        if let Some(index) = last_call_for_output_before(history, cut_index, &outputs_in_range) {
            cut_index = index;
            changed = true;
        }

        if !changed {
            return (cut_index, fold_end_index);
        }
    }
}

fn call_ids_in(history: &[ResponseItem], start: usize, end: usize) -> HashSet<String> {
    history[start..end]
        .iter()
        .filter_map(tool_call_id)
        .collect()
}

fn output_call_ids_in(history: &[ResponseItem], start: usize, end: usize) -> HashSet<String> {
    history[start..end]
        .iter()
        .filter_map(tool_output_call_id)
        .collect()
}

fn first_output_for_call_after(
    history: &[ResponseItem],
    start: usize,
    call_ids: &HashSet<String>,
) -> Option<usize> {
    if call_ids.is_empty() {
        return None;
    }
    history
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, item)| {
            tool_output_call_id(item)
                .filter(|call_id| call_ids.contains(call_id))
                .map(|_| index)
        })
}

fn last_call_for_output_before(
    history: &[ResponseItem],
    end: usize,
    output_call_ids: &HashSet<String>,
) -> Option<usize> {
    if output_call_ids.is_empty() {
        return None;
    }
    history[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| {
            tool_call_id(item)
                .filter(|call_id| output_call_ids.contains(call_id))
                .map(|_| index)
        })
}

fn tool_call_id(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::FunctionCall { call_id, .. } => Some(call_id.clone()),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        }
        | ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.clone()),
        ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.clone()),
        _ => None,
    }
}

fn tool_output_call_id(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.clone()),
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            execution,
            ..
        } if execution != "server" => Some(call_id.clone()),
        _ => None,
    }
}

fn raw_ordinal_for_effective_index(history: &[ResponseItem], target_index: usize) -> Option<u64> {
    let mut raw_cursor = 0_u64;
    for (index, item) in history.iter().enumerate() {
        if index == target_index {
            return Some(raw_cursor);
        }
        if let Some(meta) = parse_spine_ir_metadata(item) {
            raw_cursor = meta.fold_end;
            continue;
        }
        if is_non_spine_compact_item(item) {
            return None;
        }
        raw_cursor = raw_cursor.checked_add(1)?;
    }
    (target_index == history.len()).then_some(raw_cursor)
}

pub(crate) fn render_spine_ir_item(
    node_id: &NodeId,
    op: SpineOperation,
    summary: &str,
    base_path: &Path,
    worklog_path: &Path,
    worklog_body: &str,
    fold_start: u64,
    fold_end: u64,
) -> ResponseItem {
    let synthetic_id = spine_ir_synthetic_id(node_id, op, fold_start, fold_end);
    ResponseItem::Message {
        id: Some(synthetic_id.clone()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: format!(
                "<spine_ir id=\"{}\" node=\"{}\" op=\"{}\" runtime_generated=\"true\" fold_start=\"{}\" fold_end=\"{}\">\nSummary: {}\nBase: {}\nWorklog path: {}\n\n<worklog>\n{}\n</worklog>\n</spine_ir>",
                synthetic_id,
                node_id,
                op_label(op),
                fold_start,
                fold_end,
                summary,
                base_path.display(),
                worklog_path.display(),
                worklog_body
            ),
        }],
        phase: None,
    }
}

fn spine_ir_synthetic_id(
    node_id: &NodeId,
    op: SpineOperation,
    fold_start: u64,
    fold_end: u64,
) -> String {
    format!(
        "spine-ir:{}:{}-{}:{}",
        node_id,
        fold_start,
        fold_end,
        op_label(op)
    )
}

pub(crate) fn render_context_compacted_outline(
    scope_node_id: &NodeId,
    scope_summary: &str,
    base_path: &Path,
    scope_worklog_path: &Path,
    child_rows: &[(String, String)],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    rendered.push_str(&format!("Base: {}\n", base_path.display()));
    rendered.push_str(&format!(
        "[{}] {} ({})\n",
        scope_node_id,
        scope_summary,
        scope_worklog_path.display()
    ));
    for (summary, path) in child_rows {
        rendered.push_str(&format!("|-- {} ({})\n", summary, path));
    }
    rendered
}

pub(crate) fn effective_index_for_raw_ordinal(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
) -> Option<usize> {
    let mut raw_cursor = 0_u64;
    for (index, item) in history.iter().enumerate() {
        if let Some(meta) = parse_spine_ir_metadata(item) {
            if target_raw_ordinal == meta.fold_start {
                return Some(index);
            }
            if target_raw_ordinal > meta.fold_start && target_raw_ordinal < meta.fold_end {
                return None;
            }
            raw_cursor = meta.fold_end;
            continue;
        }

        if is_non_spine_compact_item(item) {
            return (target_raw_ordinal == raw_cursor).then_some(index);
        }

        if raw_cursor == target_raw_ordinal {
            return Some(index);
        }
        raw_cursor = raw_cursor.checked_add(1)?;
    }
    (target_raw_ordinal == raw_cursor).then_some(history.len())
}

pub(crate) fn is_spine_ir_item(item: &ResponseItem) -> bool {
    parse_spine_ir_metadata(item).is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineIrMetadata {
    fold_start: u64,
    fold_end: u64,
}

fn parse_spine_ir_metadata(item: &ResponseItem) -> Option<SpineIrMetadata> {
    let (item_id, text) = match item {
        ResponseItem::Message {
            id, role, content, ..
        } if matches!(role.as_str(), "assistant" | "user") => (
            id.as_deref(),
            content.iter().find_map(|content_item| match content_item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })?,
        ),
        _ => return None,
    };

    let header = text.strip_prefix("<spine_ir ")?;
    let header = header.split_once('>')?.0;
    let text_id = parse_tag_string(header, "id")?;
    if !text_id.starts_with("spine-ir:") {
        return None;
    }
    if let Some(item_id) = item_id
        && item_id != text_id
    {
        return None;
    }
    let fold_start = parse_tag_value(header, "fold_start")?;
    let fold_end = parse_tag_value(header, "fold_end")?;
    Some(SpineIrMetadata {
        fold_start,
        fold_end,
    })
}

fn is_non_spine_compact_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. } => true,
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content.iter().any(|content_item| {
                matches!(
                    content_item,
                    ContentItem::InputText { text }
                        if crate::compact::is_summary_message(text)
                )
            })
        }
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Other => false,
    }
}

fn parse_tag_value(header: &str, key: &str) -> Option<u64> {
    parse_tag_string(header, key)?.parse().ok()
}

fn parse_tag_string<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=\"");
    let value = header.split_once(&needle)?.1;
    Some(value.split_once('"')?.0)
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
