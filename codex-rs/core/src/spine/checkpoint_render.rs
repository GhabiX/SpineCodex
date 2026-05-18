use super::host_bridge::HostBridgeProjection;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::span;
use super::store::InstalledCompactSpan;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderPiOrigin {
    Raw(RawSpan),
    Mem(String),
    Note(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderedPiItem {
    pub(crate) origin: RenderPiOrigin,
    pub(crate) item: ResponseItem,
}

pub(crate) fn render_pi_bridge_replacement_history(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<ResponseItem>> {
    Ok(
        render_pi_bridge_items(history, runtime_spans, pi, artifacts, mem_items, note_items)?
            .into_iter()
            .map(|rendered| rendered.item)
            .collect(),
    )
}

pub(crate) fn render_pi_bridge_items(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<RenderedPiItem>> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let mut rendered = Vec::new();
    for segment in pi {
        match segment {
            Segment::Raw(raw_span) => {
                let start_index = projection
                    .effective_index_for_raw_boundary(raw_span.start)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} start does not map to an effective index",
                            raw_span
                        ))
                    })?;
                let end_index = projection
                    .effective_index_for_raw_boundary(raw_span.end)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} end does not map to an effective index",
                            raw_span
                        ))
                    })?;
                if start_index > end_index {
                    return Err(CodexErr::Fatal(format!(
                        "render(Pi) Raw {} maps to inverted effective range [{start_index}, {end_index})",
                        raw_span
                    )));
                }
                rendered.extend(history[start_index..end_index].iter().cloned().map(|item| {
                    RenderedPiItem {
                        origin: RenderPiOrigin::Raw(*raw_span),
                        item,
                    }
                }));
            }
            Segment::Mem { compact_id } => {
                span(segment, artifacts).map_err(pi_render_error)?;
                let item = mem_items.get(compact_id).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Mem item for {compact_id}"))
                })?;
                rendered.push(RenderedPiItem {
                    origin: RenderPiOrigin::Mem(compact_id.clone()),
                    item: item.clone(),
                });
            }
            Segment::Note { kind } => {
                let items = note_items.get(kind).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Note item for {kind}"))
                })?;
                rendered.extend(items.iter().cloned().map(|item| RenderedPiItem {
                    origin: RenderPiOrigin::Note(kind.clone()),
                    item,
                }));
            }
        }
    }
    Ok(rendered)
}

fn pi_render_error(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Fatal(format!(
        "render(Pi) failed to build canonical cover: {error}"
    ))
}
