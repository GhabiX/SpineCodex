use super::ids::NodeId;
use super::store::SpineOperation;
use async_trait::async_trait;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::path::Path;
use std::path::PathBuf;

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
    pub(crate) transition_worklog: String,
    pub(crate) rollout_path: PathBuf,
    pub(crate) raw_mirror_path: PathBuf,
    pub(crate) sidecar_root: PathBuf,
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
    pub(crate) rendered_ir_items: Vec<ResponseItem>,
    pub(crate) compact_message: String,
    pub(crate) strategy_name: &'static str,
}

#[async_trait]
pub(crate) trait SpineCompactStrategy: Send + Sync {
    async fn compact_suffix(
        &self,
        input: SpineCompactInput,
    ) -> CodexResult<SpineCompactOutput>;
}

pub(crate) fn build_suffix_replacement_history(
    old_history: &[ResponseItem],
    cut_index: usize,
    fold_end_index: usize,
    ir_items: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    let mut replacement_history =
        Vec::with_capacity(cut_index + ir_items.len() + old_history.len().saturating_sub(fold_end_index));
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

    Ok(SpineCompactPlan {
        worklog_path: input.sidecar_root.join(relative_worklog_path(&input.node_id)),
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
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "<spine_ir node=\"{}\" op=\"{}\" runtime_generated=\"true\" fold_start=\"{}\" fold_end=\"{}\">\nSummary: {}\nWorklog path: {}\n\n<worklog>\n{}\n</worklog>\n</spine_ir>",
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
        scope_node_id, scope_summary, scope_worklog_path.display()
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
            if target_raw_ordinal >= meta.fold_start && target_raw_ordinal < meta.fold_end {
                return Some(index);
            }
            raw_cursor = meta.fold_end;
            continue;
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
    let text = match item {
        ResponseItem::Message { role, content, .. } if role == "user" => content
            .iter()
            .find_map(|content_item| match content_item {
                ContentItem::InputText { text } => Some(text.as_str()),
                _ => None,
            })?,
        _ => return None,
    };

    let header = text.strip_prefix("<spine_ir ")?;
    let header = header.split_once('>')?.0;
    let fold_start = parse_tag_value(header, "fold_start")?;
    let fold_end = parse_tag_value(header, "fold_end")?;
    Some(SpineIrMetadata {
        fold_start,
        fold_end,
    })
}

fn parse_tag_value(header: &str, key: &str) -> Option<u64> {
    let needle = format!("{key}=\"");
    let value = header.split_once(&needle)?.1;
    let value = value.split_once('"')?.0;
    value.parse().ok()
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
