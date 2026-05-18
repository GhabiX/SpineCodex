use super::ids::NodeId;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use super::view::op_label;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

pub(crate) const SPINE_INITIAL_CONTEXT_OPEN_TAG: &str =
    "<spine_initial_context runtime_generated=\"true\">";
pub(crate) const SPINE_INITIAL_CONTEXT_CLOSE_TAG: &str = "</spine_initial_context>";
const SPINE_MEMORY_MARKER_PREFIX: &str = "<!-- codex-spine-memory:";
const SPINE_MEMORY_MARKER_SUFFIX: &str = " -->";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectiveItemSemantics {
    Raw1,
    Zero,
    Span { cut: u64, fold_end: u64 },
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderedSpineCarrierClassification {
    NotCarrier,
    Invalid,
    Semantics(EffectiveItemSemantics),
}

pub(crate) fn validate_spine_replacement_history_admissible(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    required_raw_ordinals: &[u64],
) -> CodexResult<()> {
    let mut raw_cursor = 0_u64;
    let mut span_cursor = 0_usize;
    for (index, item) in history.iter().enumerate() {
        match classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)
            .ok_or_else(|| {
                CodexErr::Fatal(format!(
                    "spine compact replacement history is not admissible: item {index} does not match raw cursor {raw_cursor} in the compact span ledger"
                ))
            })? {
            EffectiveItemSemantics::Raw1 => {
                raw_cursor = raw_cursor.checked_add(1).ok_or_else(|| {
                    CodexErr::Fatal(
                        "spine compact replacement history raw cursor overflowed".to_string(),
                    )
                })?;
            }
            EffectiveItemSemantics::Zero => {}
            EffectiveItemSemantics::Span { cut, fold_end } => {
                if cut != raw_cursor {
                    return Err(CodexErr::Fatal(format!(
                        "spine compact replacement history is not admissible: span at item {index} starts at raw ordinal {cut}, expected {raw_cursor}"
                    )));
                }
                if cut >= fold_end {
                    return Err(CodexErr::Fatal(format!(
                        "spine compact replacement history is not admissible: span at item {index} is empty or inverted [{cut}, {fold_end})"
                    )));
                }
                raw_cursor = fold_end;
            }
            EffectiveItemSemantics::Stop => break,
        }
    }

    for raw_ordinal in required_raw_ordinals {
        let index =
            effective_index_for_raw_ordinal_with_spans(history, *raw_ordinal, runtime_spans)
                .ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "spine compact replacement history is not admissible: required raw ordinal {raw_ordinal} does not map to an effective history index"
                    ))
                })?;
        let round_trip = raw_ordinal_for_effective_index_with_spans(history, index, runtime_spans)
            .ok_or_else(|| {
                CodexErr::Fatal(format!(
                    "spine compact replacement history is not admissible: effective index {index} for raw ordinal {raw_ordinal} does not map back to a raw ordinal"
                ))
            })?;
        if round_trip != *raw_ordinal {
            return Err(CodexErr::Fatal(format!(
                "spine compact replacement history is not admissible: raw ordinal {raw_ordinal} maps to effective index {index}, which maps back to {round_trip}"
            )));
        }
    }

    Ok(())
}

pub(crate) fn classify_effective_item(
    item: &ResponseItem,
    raw_cursor: u64,
    runtime_spans: &[InstalledCompactSpan],
    span_cursor: &mut usize,
) -> Option<EffectiveItemSemantics> {
    match classify_runtime_span_backed_spine_carrier(item, raw_cursor, runtime_spans, span_cursor) {
        RenderedSpineCarrierClassification::Semantics(semantics) => return Some(semantics),
        RenderedSpineCarrierClassification::Invalid => return None,
        RenderedSpineCarrierClassification::NotCarrier => {}
    }
    if is_spine_handoff_item(item) || parse_spine_initial_context_item(item).is_some() {
        return Some(EffectiveItemSemantics::Zero);
    }
    if is_non_spine_compact_item(item) {
        return Some(EffectiveItemSemantics::Stop);
    }
    Some(EffectiveItemSemantics::Raw1)
}

fn classify_runtime_span_backed_spine_carrier(
    item: &ResponseItem,
    raw_cursor: u64,
    runtime_spans: &[InstalledCompactSpan],
    span_cursor: &mut usize,
) -> RenderedSpineCarrierClassification {
    // Rendered Spine memory is a bridge carrier. It may collapse raw ordinals
    // only when the runtime compact ledger supplies and validates the span.
    if let Some(meta) = parse_current_spine_memory_metadata(item) {
        return match consume_runtime_span_for_memory(runtime_spans, span_cursor, &meta, raw_cursor)
        {
            Some(span) => {
                RenderedSpineCarrierClassification::Semantics(EffectiveItemSemantics::Span {
                    cut: span.cut_ordinal,
                    fold_end: span.fold_end_ordinal,
                })
            }
            None => RenderedSpineCarrierClassification::Invalid,
        };
    }
    RenderedSpineCarrierClassification::NotCarrier
}

pub(crate) fn raw_ordinal_for_effective_index_with_spans(
    history: &[ResponseItem],
    target_index: usize,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<u64> {
    let mut raw_cursor = 0_u64;
    let mut span_cursor = 0_usize;
    for (index, item) in history.iter().enumerate() {
        if index == target_index {
            return Some(raw_cursor);
        }
        match classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)? {
            EffectiveItemSemantics::Raw1 => {
                raw_cursor = raw_cursor.checked_add(1)?;
            }
            EffectiveItemSemantics::Zero => {}
            EffectiveItemSemantics::Span { cut: _, fold_end } => {
                raw_cursor = fold_end;
            }
            EffectiveItemSemantics::Stop => return None,
        }
    }
    (target_index == history.len()).then_some(raw_cursor)
}

pub(crate) fn effective_index_for_raw_ordinal_with_spans(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<usize> {
    let mut raw_cursor = 0_u64;
    let mut span_cursor = 0_usize;
    for (index, item) in history.iter().enumerate() {
        match classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)? {
            EffectiveItemSemantics::Raw1 => {
                if raw_cursor == target_raw_ordinal {
                    return Some(index);
                }
                raw_cursor = raw_cursor.checked_add(1)?;
            }
            EffectiveItemSemantics::Zero => {}
            EffectiveItemSemantics::Span { cut, fold_end } => {
                if target_raw_ordinal == cut {
                    return Some(index);
                }
                if target_raw_ordinal > cut && target_raw_ordinal < fold_end {
                    return None;
                }
                raw_cursor = fold_end;
            }
            EffectiveItemSemantics::Stop => {
                return (target_raw_ordinal == raw_cursor).then_some(index);
            }
        }
    }
    (target_raw_ordinal == raw_cursor).then_some(history.len())
}

pub(crate) fn is_spine_internal_render_item(item: &ResponseItem) -> bool {
    // Host/UI filtering only. This is not an admissibility check for mutable
    // compact planning; raw/effective mapping still requires runtime span data.
    parse_current_spine_memory_metadata(item).is_some()
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
        op_label(op)
    )
}

pub(crate) fn spine_memory_synthetic_id(node_id: &NodeId, op: SpineOperation) -> String {
    format!("spine-memory:{node_id}:{}", op_label(op))
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineMemoryMetadata {
    node_id: NodeId,
    op: SpineOperation,
}

fn runtime_span_matches_memory(span: &InstalledCompactSpan, meta: &SpineMemoryMetadata) -> bool {
    span.node_id == meta.node_id && span.op == meta.op
}

enum RuntimeMemorySpanMatch<'a> {
    NoMatch,
    Unique {
        index: usize,
        span: &'a InstalledCompactSpan,
    },
    Ambiguous,
}

fn lookup_runtime_span_for_memory<'a>(
    runtime_spans: &'a [InstalledCompactSpan],
    span_cursor: usize,
    meta: &SpineMemoryMetadata,
    cut_ordinal: u64,
) -> RuntimeMemorySpanMatch<'a> {
    let mut found = None;
    for (index, span) in runtime_spans.iter().enumerate().skip(span_cursor) {
        if span.cut_ordinal == cut_ordinal && runtime_span_matches_memory(span, meta) {
            if found.is_some() {
                return RuntimeMemorySpanMatch::Ambiguous;
            }
            found = Some((index, span));
        }
    }
    match found {
        Some((index, span)) => RuntimeMemorySpanMatch::Unique { index, span },
        None => RuntimeMemorySpanMatch::NoMatch,
    }
}

fn consume_runtime_span_for_memory<'a>(
    runtime_spans: &'a [InstalledCompactSpan],
    span_cursor: &mut usize,
    meta: &SpineMemoryMetadata,
    cut_ordinal: u64,
) -> Option<&'a InstalledCompactSpan> {
    match lookup_runtime_span_for_memory(runtime_spans, *span_cursor, meta, cut_ordinal) {
        RuntimeMemorySpanMatch::Unique { index, span } => {
            *span_cursor = index + 1;
            Some(span)
        }
        RuntimeMemorySpanMatch::NoMatch | RuntimeMemorySpanMatch::Ambiguous => None,
    }
}

fn parse_current_spine_memory_metadata(item: &ResponseItem) -> Option<SpineMemoryMetadata> {
    let (id, text) = match item {
        ResponseItem::Message {
            id, role, content, ..
        } if role == "assistant" => (
            id.as_deref(),
            content.iter().find_map(|content_item| match content_item {
                ContentItem::OutputText { text } => Some(text.as_str()),
                _ => None,
            })?,
        ),
        _ => return None,
    };

    if let Some(meta) = id.and_then(parse_spine_memory_id) {
        return Some(meta);
    }
    if let Some(meta) = parse_spine_memory_text_marker(text) {
        return Some(meta);
    }

    None
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

fn parse_spine_memory_id(id: &str) -> Option<SpineMemoryMetadata> {
    let rest = id.strip_prefix("spine-memory:")?;
    let (node_id, op) = rest.rsplit_once(':')?;
    Some(SpineMemoryMetadata {
        node_id: NodeId::parse(node_id).ok()?,
        op: parse_spine_operation_label(op)?,
    })
}

fn parse_spine_operation_label(value: &str) -> Option<SpineOperation> {
    match value {
        "open" => Some(SpineOperation::Open),
        "next" => Some(SpineOperation::Next),
        "close" => Some(SpineOperation::Close),
        "archive" => Some(SpineOperation::Archive),
        _ => None,
    }
}
