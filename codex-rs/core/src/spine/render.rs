use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TrimProjection;
use crate::spine::model::TrimResponseKind;
use crate::spine::model::TrimTarget;
use crate::spine::model::TrimTargetState;
use crate::spine::parse_stack::ParseStack;
use crate::spine::user_message_projection::anchored_user_message_item;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct VisibleItemRef {
    pub(super) context_index: usize,
    pub(super) source: VisibleItemSource,
}

#[derive(Clone, Debug)]
pub(super) enum VisibleItemSource {
    RawResponseItem {
        raw_ordinal: u64,
        from_user: bool,
        user_anchor: Option<u64>,
    },
    ToolCallSegment {
        kind: ToolCallSegmentKind,
        raw_ordinal: u64,
    },
    MemoryRef {
        memory: MemoryRef,
        require_live_raw: bool,
    },
    MemorySeg {
        memory_id: String,
        body_path: PathBuf,
    },
}

#[cfg(test)]
pub(super) fn render_parse_stack_to_context(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
) -> Result<Vec<ResponseItem>, SpineError> {
    render_parse_stack_to_context_with_memory_body_and_trim_projection(
        ps,
        raw_items,
        None,
        &TrimProjection::default(),
    )
}

pub(super) fn render_parse_stack_to_context_with_trim_projection(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
    trim_projection: &TrimProjection,
) -> Result<Vec<ResponseItem>, SpineError> {
    render_parse_stack_to_context_with_memory_body_and_trim_projection(
        ps,
        raw_items,
        None,
        trim_projection,
    )
}

pub(super) fn render_parse_stack_to_context_with_memory_body_and_trim_projection(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
    staged_memory_body: Option<(&str, &str)>,
    trim_projection: &TrimProjection,
) -> Result<Vec<ResponseItem>, SpineError> {
    let visible_refs = project_parse_stack_visible_items(ps)?;
    let mut context = Vec::new();
    for visible_ref in &visible_refs {
        context.extend(render_visible_ref_to_context_items(
            &visible_ref.source,
            raw_items,
            staged_memory_body,
            trim_projection,
        )?);
    }
    Ok(context)
}

pub(super) fn project_raw_history_with_trim_projection(
    raw_items: &[ResponseItem],
    trim_projection: &TrimProjection,
) -> Result<Vec<ResponseItem>, SpineError> {
    raw_items
        .iter()
        .enumerate()
        .map(|(raw_ordinal, item)| {
            let raw_ordinal = u64::try_from(raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            let Some(target) = trim_projection.target_for_raw_ordinal(raw_ordinal) else {
                return Ok(item.clone());
            };
            projected_tool_response_item_with_state(item, target, &target.state)
        })
        .collect()
}

pub(super) fn project_parse_stack_visible_items(
    ps: &ParseStack,
) -> Result<Vec<VisibleItemRef>, SpineError> {
    VisibleRefProjection {
        next_context_index: 0,
        refs: Vec::new(),
    }
    .project_symbols(&ps.symbols)
}

pub(super) fn project_spine_tree_nodes_visible_items(
    nodes: &[SpineTreeNode],
    context_start: usize,
) -> Result<Vec<VisibleItemRef>, SpineError> {
    let mut projection = VisibleRefProjection {
        next_context_index: context_start,
        refs: Vec::new(),
    };
    projection.project_nodes_in_place(nodes)?;
    Ok(projection.refs)
}

struct VisibleRefProjection {
    next_context_index: usize,
    refs: Vec<VisibleItemRef>,
}

impl VisibleRefProjection {
    fn project_symbols(mut self, symbols: &[Symbol]) -> Result<Vec<VisibleItemRef>, SpineError> {
        for symbol in symbols {
            match symbol {
                Symbol::Control(ControlSymbol::Init(_))
                | Symbol::Control(ControlSymbol::End)
                | Symbol::Control(ControlSymbol::Open(_))
                | Symbol::Control(ControlSymbol::Close(_))
                | Symbol::Control(ControlSymbol::Compact(_, _, _, _)) => {}
                Symbol::SpineTreeNode(node) => self.project_node(node)?,
                Symbol::SpineTreeNodes(nodes) => {
                    self.project_nodes_in_place(nodes)?;
                }
                Symbol::RootEpoches(root_epochs) => {
                    if let Some(root_epoch) = root_epochs.last() {
                        self.push(VisibleItemSource::MemoryRef {
                            memory: root_epoch.memory.clone(),
                            require_live_raw: false,
                        })?;
                    }
                }
            }
        }
        Ok(self.refs)
    }

    fn project_nodes_in_place(&mut self, nodes: &[SpineTreeNode]) -> Result<(), SpineError> {
        for node in nodes {
            self.project_node(node)?;
        }
        Ok(())
    }

    fn project_node(&mut self, node: &SpineTreeNode) -> Result<(), SpineError> {
        match node {
            SpineTreeNode::MsgAsLeafNode {
                msg,
                from_user,
                user_anchor,
            } => match msg {
                SegRef::ResponseItem { raw_ordinal, .. } => {
                    self.push(VisibleItemSource::RawResponseItem {
                        raw_ordinal: *raw_ordinal,
                        from_user: *from_user,
                        user_anchor: *user_anchor,
                    })
                }
                SegRef::Memory {
                    memory_id,
                    body_path,
                } => self.push(VisibleItemSource::MemorySeg {
                    memory_id: memory_id.clone(),
                    body_path: body_path.clone(),
                }),
            },
            SpineTreeNode::ToolCallAsLeafNode { segments } => {
                for segment in segments {
                    let SegRef::ResponseItem { raw_ordinal, .. } = &segment.seg else {
                        return Err(SpineError::InvalidEvent(
                            "visible toolcall segment must reference raw response item".to_string(),
                        ));
                    };
                    self.push(VisibleItemSource::ToolCallSegment {
                        kind: segment.kind,
                        raw_ordinal: *raw_ordinal,
                    })?;
                }
                Ok(())
            }
            SpineTreeNode::SpineTree { memory, .. } => self.push(VisibleItemSource::MemoryRef {
                memory: memory.clone(),
                require_live_raw: true,
            }),
        }
    }

    fn push(&mut self, source: VisibleItemSource) -> Result<(), SpineError> {
        let context_index = self.next_context_index;
        let item_count = visible_source_context_item_count(&source);
        self.next_context_index =
            self.next_context_index
                .checked_add(item_count)
                .ok_or_else(|| {
                    SpineError::InvalidEvent("visible context index overflow".to_string())
                })?;
        self.refs.push(VisibleItemRef {
            context_index,
            source,
        });
        Ok(())
    }
}

pub(super) fn visible_source_context_item_count(source: &VisibleItemSource) -> usize {
    match source {
        VisibleItemSource::MemoryRef { memory, .. } => {
            memory.rendered_context_item_count.unwrap_or(1)
        }
        VisibleItemSource::RawResponseItem { .. }
        | VisibleItemSource::ToolCallSegment { .. }
        | VisibleItemSource::MemorySeg { .. } => 1,
    }
}

fn render_visible_ref_to_context_items(
    source: &VisibleItemSource,
    raw_items: &[Option<ResponseItem>],
    staged_memory_body: Option<(&str, &str)>,
    trim_projection: &TrimProjection,
) -> Result<Vec<ResponseItem>, SpineError> {
    let body = match source {
        VisibleItemSource::RawResponseItem { raw_ordinal, .. }
        | VisibleItemSource::ToolCallSegment { raw_ordinal, .. } => {
            return render_raw_visible_ref(source, *raw_ordinal, raw_items, trim_projection)
                .map(|item| vec![item]);
        }
        VisibleItemSource::MemoryRef {
            memory,
            require_live_raw,
        } => {
            if *require_live_raw && !memory_ref_is_live(memory, raw_items)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    memory.compact_id
                )));
            }
            let body = read_memory_ref_body_with_staged(memory, staged_memory_body)?;
            return render_memory_ref_context_items(memory, &body);
        }
        VisibleItemSource::MemorySeg {
            memory_id,
            body_path,
        } => read_memory_body(memory_id, body_path, None)?,
    };
    Ok(vec![memory_response_item(&body)])
}

pub(super) fn render_memory_ref_context_items(
    memory: &MemoryRef,
    body: &str,
) -> Result<Vec<ResponseItem>, SpineError> {
    let Some(expected_count) = memory.rendered_context_item_count else {
        return Ok(vec![memory_response_item(body)]);
    };
    let items: Vec<ResponseItem> = serde_json::from_str(body).map_err(|err| {
        SpineError::InvalidStore(format!(
            "root epoch memory {} body is not valid ResponseItem JSON: {err}",
            memory.compact_id
        ))
    })?;
    if items.len() != expected_count {
        return Err(SpineError::InvalidStore(format!(
            "root epoch memory {} rendered item count {} does not match body item count {}",
            memory.compact_id,
            expected_count,
            items.len()
        )));
    }
    if items.is_empty() {
        return Err(SpineError::InvalidStore(format!(
            "root epoch memory {} has empty rendered context items",
            memory.compact_id
        )));
    }
    Ok(items)
}

fn render_raw_visible_ref(
    source: &VisibleItemSource,
    raw_ordinal: u64,
    raw_items: &[Option<ResponseItem>],
    trim_projection: &TrimProjection,
) -> Result<ResponseItem, SpineError> {
    let raw_index = usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
    let item = raw_items
        .get(raw_index)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            let missing_label = match source {
                VisibleItemSource::RawResponseItem { .. } => "visible Msg",
                VisibleItemSource::ToolCallSegment { .. } => "visible toolcall segment",
                _ => unreachable!("raw source label requested for non-raw source"),
            };
            SpineError::InvalidEvent(format!(
                "missing raw item for {missing_label} raw ordinal {raw_ordinal}"
            ))
        })?;
    let mut item = item.clone();
    if let VisibleItemSource::ToolCallSegment {
        kind: ToolCallSegmentKind::Response,
        raw_ordinal,
    } = source
        && let Some(target) = trim_projection.target_for_raw_ordinal(*raw_ordinal)
    {
        item = projected_tool_response_item_with_state(&item, target, &target.state)?;
    }
    if let VisibleItemSource::RawResponseItem {
        from_user: true,
        user_anchor: Some(user_anchor),
        ..
    } = source
    {
        item = anchored_user_message_item(&item, *user_anchor)?;
    }
    Ok(item)
}

fn projected_tool_response_item_with_state(
    item: &ResponseItem,
    target: &TrimTarget,
    state: &TrimTargetState,
) -> Result<ResponseItem, SpineError> {
    let mismatch_label = match state {
        TrimTargetState::Tagged => "text body item",
        TrimTargetState::Snipped | TrimTargetState::Sliced { .. } => "output payload",
    };
    let output_payload = matched_tool_output(item, target, mismatch_label)?;
    let body = FunctionCallOutputBody::Text(match state {
        TrimTargetState::Tagged => {
            let body = output_payload
                .text_content()
                .ok_or_else(|| trim_body_error(target))?;
            format!("[TRIM_ID: {}]\n{body}", target.trim_id)
        }
        TrimTargetState::Snipped => TOOL_RESULT_CLEARED_MESSAGE.to_string(),
        TrimTargetState::Sliced { visible_body } => visible_body.clone(),
    });
    let output = FunctionCallOutputPayload {
        body,
        success: output_payload.success,
    };
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => Ok(ResponseItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output,
        }),
        ResponseItem::CustomToolCallOutput { call_id, name, .. } => {
            Ok(ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                name: name.clone(),
                output,
            })
        }
        _ => Err(SpineError::SidecarCorruption(format!(
            "trim target {} does not match visible raw item for call_id={}",
            target.trim_id, target.call_id
        ))),
    }
}

pub(super) fn matched_tool_output<'a>(
    item: &'a ResponseItem,
    target: &TrimTarget,
    mismatch_label: &str,
) -> Result<&'a FunctionCallOutputPayload, SpineError> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, output }
            if target.response_kind == TrimResponseKind::FunctionCallOutput
                && call_id == &target.call_id =>
        {
            Ok(output)
        }
        ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } if target.response_kind == TrimResponseKind::CustomToolCallOutput
            && call_id == &target.call_id =>
        {
            Ok(output)
        }
        _ => Err(SpineError::SidecarCorruption(format!(
            "trim target {} does not match {mismatch_label} for call_id={}",
            target.trim_id, target.call_id,
        ))),
    }
}

pub(super) fn trim_body_error(target: &TrimTarget) -> SpineError {
    SpineError::SidecarCorruption(format!(
        "trim target {} references non-text tool response body",
        target.trim_id
    ))
}

pub(super) fn read_memory_ref_body(memory: &MemoryRef) -> Result<String, SpineError> {
    read_memory_ref_body_with_staged(memory, None)
}

fn read_memory_ref_body_with_staged(
    memory: &MemoryRef,
    staged_memory_body: Option<(&str, &str)>,
) -> Result<String, SpineError> {
    if let Some((memory_id, body)) = staged_memory_body
        && memory_id == memory.compact_id
    {
        let actual_hash = sha1_hex(body.as_bytes());
        if actual_hash != memory.body_hash {
            return Err(SpineError::InvalidStore(format!(
                "staged memory body hash mismatch for {memory_id}"
            )));
        }
        return Ok(body.to_string());
    }
    read_memory_body(
        &memory.compact_id,
        &memory.body_path,
        Some(memory.body_hash.as_str()),
    )
}

pub(super) fn read_memory_body(
    memory_id: &str,
    body_path: &std::path::Path,
    expected_hash: Option<&str>,
) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(body_path)?;
    if let Some(expected_hash) = expected_hash {
        let actual_hash = sha1_hex(body.as_bytes());
        if actual_hash != expected_hash {
            return Err(SpineError::InvalidStore(format!(
                "memory body hash mismatch for {memory_id}"
            )));
        }
    }
    Ok(body)
}

fn memory_ref_is_live(
    memory: &MemoryRef,
    raw_items: &[Option<ResponseItem>],
) -> Result<bool, SpineError> {
    let (start, end) = memory_source_raw_range_usize(memory)?;
    if let Some(expected) = memory.raw_live_hash.as_deref() {
        let Some(prefix) = raw_items.get(..end) else {
            return Ok(false);
        };
        let live: Vec<bool> = prefix.iter().map(Option::is_some).collect();
        return Ok(hash_raw_live(&live) == expected);
    }
    Ok(raw_items
        .get(start..end)
        .is_some_and(|items| items.iter().all(Option::is_some)))
}

fn memory_source_raw_range_usize(memory: &MemoryRef) -> Result<(usize, usize), SpineError> {
    let start = usize::try_from(memory.source_raw_range.start)
        .map_err(|_| SpineError::InvalidEvent("memory raw start overflow".to_string()))?;
    let end = usize::try_from(memory.source_raw_range.end)
        .map_err(|_| SpineError::InvalidEvent("memory raw end overflow".to_string()))?;
    Ok((start, end))
}

pub(super) fn memory_response_item(body: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: format!("<spine_memory>\n{body}\n</spine_memory>"),
        }],
        phase: None,
    }
}
