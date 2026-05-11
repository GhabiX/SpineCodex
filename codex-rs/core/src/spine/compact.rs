use super::ids::NodeId;
use super::store::SpineOperation;
use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::session::session::Session;
use crate::session::turn::get_last_assistant_message_from_turn;
use crate::session::turn_context::TurnContext;
use crate::util::backoff;
use async_trait::async_trait;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use std::path::Path;
use std::path::PathBuf;
use tracing::warn;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactInput {
    pub(crate) op: SpineOperation,
    pub(crate) node_id: NodeId,
    pub(crate) scope_node_id: Option<NodeId>,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) prefix_items: Vec<ResponseItem>,
    pub(crate) suffix_items: Vec<ResponseItem>,
    pub(crate) transition_summary: String,
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
    input: SpineCompactInput,
) -> CodexResult<SpineCompactOutput> {
    let prompt_input = build_codex_builtin_prompt_input(&input, turn_context.compact_prompt());
    let prompt = Prompt {
        input: prompt_input,
        base_instructions: sess.get_base_instructions().await,
        personality: turn_context.personality,
        ..Default::default()
    };
    let mut client_session = sess.services.model_client.new_session();
    let max_retries = turn_context.provider.info().stream_max_retries();
    let mut retries = 0;
    let compacted_suffix = loop {
        match collect_compaction_response(sess, turn_context, &mut client_session, &prompt).await {
            Ok(text) => break text,
            Err(err) if err.is_retryable() && retries < max_retries => {
                retries += 1;
                let delay = backoff(retries);
                warn!("spine compact stream failed; retrying ({retries}/{max_retries})");
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

fn build_codex_builtin_prompt_input(
    input: &SpineCompactInput,
    compact_prompt: &str,
) -> Vec<ResponseItem> {
    let suffix_item_count = input.suffix_items.len();
    let suffix_signature = response_item_signature(&input.suffix_items);
    let mut prompt_input =
        Vec::with_capacity(input.prefix_items.len() + input.suffix_items.len() + 1);
    prompt_input.extend(input.prefix_items.clone());
    prompt_input.extend(input.suffix_items.clone());
    prompt_input.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "{compact_prompt}\n\nSpineJIT suffix compaction request.\n\nYou are compacting a SpineJIT suffix so the runtime can replace raw transcript tokens with compact worklog IR while preserving enough context for the next turn to continue correctly.\n\nTarget node: {}\nTarget operation: {}\nTarget response ordinal range: [{}, {})\nSpine Tree summary label: {}\nTarget suffix item count: {}\nTarget suffix item signature: {}\n\nThe target suffix is exactly the immediately preceding {} ResponseItem(s) in this prompt, corresponding to response ordinals [{}, {}). The earlier prompt prefix is preserved verbatim in the runtime context and must not be summarized or rewritten.\n\nPreserve information with high locality:\n- Preserve temporal locality from the suffix: latest decisions, current goal, next actions, unresolved risks, verification status, and failed attempts.\n- Preserve spatial locality from the suffix: relevant files, functions, tests, commands, errors, node relationships, worklog/traj paths, and neighboring scope context needed to resume.\n\nDrop low-value transcript detail, repeated chatter, and tool-output noise. Keep exact identifiers, paths, commands, errors, and test results when they affect future work. Do not mention prefix-only content unless it is repeated or changed inside the target suffix.\n\nReturn exactly one XML-like block and no text outside it:\n{}\n<dense Markdown compact for the target suffix only>\n{}",
                input.node_id,
                op_label(input.op),
                input.cut_ordinal,
                input.fold_end_ordinal,
                input.transition_summary,
                suffix_item_count,
                suffix_signature,
                suffix_item_count,
                input.cut_ordinal,
                input.fold_end_ordinal,
                COMPACT_WORKLOG_OPEN_TAG,
                COMPACT_WORKLOG_CLOSE_TAG
            ),
        }],
        phase: None,
    });
    prompt_input
}

fn response_item_signature(items: &[ResponseItem]) -> String {
    items
        .iter()
        .map(response_item_label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn response_item_label(item: &ResponseItem) -> &'static str {
    match item {
        ResponseItem::Message { .. } => "message",
        ResponseItem::Reasoning { .. } => "reasoning",
        ResponseItem::LocalShellCall { .. } => "local_shell_call",
        ResponseItem::FunctionCall { .. } => "function_call",
        ResponseItem::ToolSearchCall { .. } => "tool_search_call",
        ResponseItem::FunctionCallOutput { .. } => "function_call_output",
        ResponseItem::CustomToolCall { .. } => "custom_tool_call",
        ResponseItem::CustomToolCallOutput { .. } => "custom_tool_call_output",
        ResponseItem::ToolSearchOutput { .. } => "tool_search_output",
        ResponseItem::WebSearchCall { .. } => "web_search_call",
        ResponseItem::ImageGenerationCall { .. } => "image_generation_call",
        ResponseItem::Compaction { .. } => "compaction",
        ResponseItem::ContextCompaction { .. } => "context_compaction",
        ResponseItem::Other => "other",
    }
}

async fn collect_compaction_response(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut crate::client::ModelClientSession,
    prompt: &Prompt,
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
        .await?;
    let mut output_items = Vec::new();
    loop {
        let Some(event) = stream.next().await else {
            return Err(CodexErr::Stream(
                "stream closed before spine compact response.completed".into(),
                None,
            ));
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

fn render_auto_compact_worklog(input: &SpineCompactInput, compacted_suffix: &str) -> String {
    let raw_mirror_path = input
        .raw_mirror_path
        .strip_prefix(&input.sidecar_root)
        .unwrap_or(input.raw_mirror_path.as_path());
    format!(
        "\n\n## Auto Compact\n\nStrategy: {CODEX_BUILTIN_TEXT_STRATEGY}\nFold: response ordinals [{}, {})\nRaw trajs: {}\nRollout: {}\nIndex: trajs.index.jsonl\n\n{}\n\n## Node Summary\n\n{}\n",
        input.cut_ordinal,
        input.fold_end_ordinal,
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
    let mut input = input;
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

pub(crate) fn render_spine_ir_item(
    node_id: &NodeId,
    op: SpineOperation,
    summary: &str,
    worklog_path: &Path,
    worklog_body: &str,
    fold_start: u64,
    fold_end: u64,
) -> ResponseItem {
    let synthetic_id = spine_ir_synthetic_id(node_id, op, fold_start, fold_end);
    ResponseItem::Message {
        id: Some(synthetic_id.clone()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "<spine_ir id=\"{}\" node=\"{}\" op=\"{}\" runtime_generated=\"true\" fold_start=\"{}\" fold_end=\"{}\">\nSummary: {}\nWorklog path: {}\n\n<worklog>\n{}\n</worklog>\n</spine_ir>",
                synthetic_id,
                node_id,
                op_label(op),
                fold_start,
                fold_end,
                summary,
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
    scope_worklog_path: &Path,
    child_rows: &[(String, String)],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
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
        } if role == "user" => (
            id.as_deref(),
            content.iter().find_map(|content_item| match content_item {
                ContentItem::InputText { text } => Some(text.as_str()),
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

fn op_label(op: SpineOperation) -> &'static str {
    match op {
        SpineOperation::Open => "open",
        SpineOperation::Next => "next",
        SpineOperation::Close => "close",
    }
}

fn relative_worklog_path(node_id: &NodeId) -> PathBuf {
    let mut path = PathBuf::from("nodes");
    for segment in node_id.segments() {
        path.push(segment.to_string());
    }
    path.push("worklog.md");
    path
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
