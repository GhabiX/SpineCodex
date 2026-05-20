use super::ids::NodeId;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

pub(crate) const SPINE_INITIAL_CONTEXT_OPEN_TAG: &str =
    "<spine_initial_context runtime_generated=\"true\">";
pub(crate) const SPINE_INITIAL_CONTEXT_CLOSE_TAG: &str = "</spine_initial_context>";
const SPINE_MEMORY_MARKER_PREFIX: &str = "<!-- codex-spine-memory:";
const SPINE_MEMORY_MARKER_SUFFIX: &str = " -->";

#[derive(Debug)]
pub(crate) struct HostBridgeProjection<'a> {
    history: &'a [ResponseItem],
    entries: Vec<BridgeEntry>,
    raw_len: u64,
    stopped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BridgeEntry {
    pub(crate) index: usize,
    pub(crate) raw_before: u64,
    pub(crate) width: BridgeWidth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BridgeWidth {
    // Host bridge only: classify materialized `replacement_history` items back
    // into raw-width effects until native `R/E/N/M -> Pi -> ContextSnapshot`
    // host replay replaces this bridge.
    Raw1,
    Zero,
    Span {
        compact_id: String,
        start: u64,
        end: u64,
    },
    Stop,
}

impl<'a> HostBridgeProjection<'a> {
    pub(crate) fn build(
        history: &'a [ResponseItem],
        runtime_spans: &[InstalledCompactSpan],
    ) -> CodexResult<Self> {
        let mut raw_cursor = 0_u64;
        let mut span_cursor = 0_usize;
        let mut entries = Vec::with_capacity(history.len());
        let mut stopped = false;

        for (index, item) in history.iter().enumerate() {
            let raw_before = raw_cursor;
            let width = classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)
                .ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "spine host bridge projection is not admissible: item {index} does not match raw cursor {raw_cursor} in the compact span ledger"
                    ))
                })?;
            match &width {
                BridgeWidth::Raw1 => {
                    raw_cursor = raw_cursor.checked_add(1).ok_or_else(|| {
                        CodexErr::Fatal(
                            "spine host bridge projection raw cursor overflowed".to_string(),
                        )
                    })?;
                }
                BridgeWidth::Zero => {}
                BridgeWidth::Span { start, end, .. } => {
                    if *start != raw_cursor {
                        return Err(CodexErr::Fatal(format!(
                            "spine host bridge projection is not admissible: span at item {index} starts at raw ordinal {start}, expected {raw_cursor}"
                        )));
                    }
                    if start >= end {
                        return Err(CodexErr::Fatal(format!(
                            "spine host bridge projection is not admissible: span at item {index} is empty or inverted [{start}, {end})"
                        )));
                    }
                    raw_cursor = *end;
                }
                BridgeWidth::Stop => {
                    stopped = true;
                }
            };
            entries.push(BridgeEntry {
                index,
                raw_before,
                width,
            });
            if stopped {
                break;
            }
        }

        Ok(Self {
            history,
            entries,
            raw_len: raw_cursor,
            stopped,
        })
    }

    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn raw_for_effective_index(&self, index: usize) -> Option<u64> {
        if let Some(entry) = self.entries.iter().find(|entry| entry.index == index) {
            return Some(entry.raw_before);
        }
        if !self.stopped && index == self.history.len() {
            return Some(self.raw_len);
        }
        None
    }

    pub(crate) fn effective_index_for_raw_boundary(&self, raw: u64) -> Option<usize> {
        for entry in &self.entries {
            match &entry.width {
                BridgeWidth::Raw1 => {
                    if entry.raw_before == raw {
                        return Some(entry.index);
                    }
                }
                BridgeWidth::Zero => {}
                BridgeWidth::Span { start, end, .. } => {
                    if raw == *start {
                        return Some(entry.index);
                    }
                    if raw > *start && raw < *end {
                        return None;
                    }
                }
                BridgeWidth::Stop => {
                    return (raw == entry.raw_before).then_some(entry.index);
                }
            }
        }
        if !self.stopped && raw == self.raw_len {
            return Some(self.history.len());
        }
        None
    }

    pub(crate) fn first_span_in_prefix(&self, prefix_index: usize) -> Option<(u64, usize)> {
        self.entries.iter().find_map(|entry| {
            if entry.index >= prefix_index {
                return None;
            }
            match entry.width {
                BridgeWidth::Span { start, .. } => Some((start, entry.index)),
                BridgeWidth::Raw1 | BridgeWidth::Zero | BridgeWidth::Stop => None,
            }
        })
    }

    pub(crate) fn memory_item_for_span(&self, compact_id: &str) -> CodexResult<ResponseItem> {
        let mut found_index = None;
        for entry in &self.entries {
            let BridgeWidth::Span {
                compact_id: entry_compact_id,
                ..
            } = &entry.width
            else {
                continue;
            };
            if entry_compact_id == compact_id {
                if found_index.is_some() {
                    return Err(CodexErr::Fatal(format!(
                        "spine host bridge projection found duplicate Mem item for {compact_id}"
                    )));
                }
                found_index = Some(entry.index);
            }
        }
        let index = found_index.ok_or_else(|| {
            CodexErr::Fatal(format!(
                "spine host bridge projection missing Mem item for {compact_id}"
            ))
        })?;
        self.history.get(index).cloned().ok_or_else(|| {
            CodexErr::Fatal(format!(
                "spine host bridge projection Mem {compact_id} mapped past history at index {index}"
            ))
        })
    }
}

pub(crate) fn classify_effective_item(
    item: &ResponseItem,
    raw_cursor: u64,
    runtime_spans: &[InstalledCompactSpan],
    span_cursor: &mut usize,
) -> Option<BridgeWidth> {
    if let Some(meta) = parse_current_spine_memory_metadata(item) {
        let mut found = None;
        for (index, span) in runtime_spans.iter().enumerate().skip(*span_cursor) {
            if span.cut_ordinal == raw_cursor && span.node_id == meta.node_id && span.op == meta.op
            {
                if found.is_some() {
                    return None;
                }
                found = Some((index, span));
            }
        }
        let (index, span) = found?;
        *span_cursor = index + 1;
        return Some(BridgeWidth::Span {
            compact_id: span.compact_id.clone(),
            start: span.cut_ordinal,
            end: span.fold_end_ordinal,
        });
    }
    if is_spine_handoff_item(item) || parse_spine_initial_context_item(item).is_some() {
        return Some(BridgeWidth::Zero);
    }
    if is_non_spine_compact_item(item) {
        return Some(BridgeWidth::Stop);
    }
    Some(BridgeWidth::Raw1)
}

pub(crate) fn raw_ordinal_for_effective_index_with_spans(
    history: &[ResponseItem],
    target_index: usize,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<u64> {
    HostBridgeProjection::build(history, runtime_spans)
        .ok()?
        .raw_for_effective_index(target_index)
}

pub(crate) fn effective_index_for_raw_ordinal_with_spans(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<usize> {
    HostBridgeProjection::build(history, runtime_spans)
        .ok()?
        .effective_index_for_raw_boundary(target_raw_ordinal)
}

pub(crate) fn is_spine_internal_render_item(item: &ResponseItem) -> bool {
    // Host/UI filtering only. This is not an admissibility check for mutable
    // compact planning; raw/effective mapping still requires runtime span data.
    parse_current_spine_memory_metadata(item).is_some()
}

fn parse_current_spine_memory_metadata(item: &ResponseItem) -> Option<SpineMemoryMetadata> {
    let text = match item {
        ResponseItem::Message { role, content, .. } if role == "assistant" => {
            content.iter().find_map(|content_item| match content_item {
                ContentItem::OutputText { text } => Some(text.as_str()),
                _ => None,
            })?
        }
        _ => return None,
    };
    parse_spine_memory_text_marker(text)
}

pub(crate) fn parse_spine_initial_context_item(item: &ResponseItem) -> Option<Vec<ResponseItem>> {
    let ResponseItem::Message { role, content, .. } = item else {
        return None;
    };
    if role != "developer" || content.len() != 1 {
        return None;
    }
    let ContentItem::InputText { text } = &content[0] else {
        return None;
    };
    let body = text
        .strip_prefix(SPINE_INITIAL_CONTEXT_OPEN_TAG)?
        .strip_prefix('\n')?
        .strip_suffix(SPINE_INITIAL_CONTEXT_CLOSE_TAG)?
        .strip_suffix('\n')?;
    serde_json::from_str(body).ok()
}

pub(crate) fn spine_memory_text_marker(node_id: &NodeId, op: SpineOperation) -> String {
    format!(
        "{SPINE_MEMORY_MARKER_PREFIX}{node_id}:{}{SPINE_MEMORY_MARKER_SUFFIX}",
        super::view::op_label(op)
    )
}

pub(crate) fn is_non_spine_compact_item(item: &ResponseItem) -> bool {
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

fn is_spine_handoff_item(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    if role != "developer" || content.len() != 1 {
        return false;
    }
    let ContentItem::InputText { text } = &content[0] else {
        return false;
    };
    text.starts_with("<spine_handoff>") && text.ends_with("</spine_handoff>")
}

fn parse_spine_memory_text_marker(text: &str) -> Option<SpineMemoryMetadata> {
    let marker = text
        .lines()
        .next()?
        .strip_prefix(SPINE_MEMORY_MARKER_PREFIX)?
        .strip_suffix(SPINE_MEMORY_MARKER_SUFFIX)?;
    let (node_id, op) = marker.rsplit_once(':')?;
    Some(SpineMemoryMetadata {
        node_id: NodeId::parse(node_id).ok()?,
        op: parse_spine_operation_label(op)?,
    })
}

fn parse_spine_operation_label(value: &str) -> Option<SpineOperation> {
    match value {
        "open" => Some(SpineOperation::Open),
        "close" => Some(SpineOperation::Close),
        "archive" => Some(SpineOperation::Archive),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineMemoryMetadata {
    node_id: NodeId,
    op: SpineOperation,
}

#[cfg(test)]
#[path = "host_bridge_tests.rs"]
mod tests;
