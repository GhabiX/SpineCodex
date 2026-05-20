use super::host_bridge::HostBridgeProjection;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::span;
use super::segment::validate_cover;
use super::store::InstalledCompactSpan;
use super::store::NotePlacement;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;

pub(crate) enum SpineHostRawSource<'a> {
    Direct(&'a [ResponseItem]),
    HostBridge {
        history: &'a [ResponseItem],
        runtime_spans: &'a [InstalledCompactSpan],
    },
}

pub(crate) struct SpineHostMaterializationInput<'a> {
    pub(crate) pi: &'a [Segment],
    pub(crate) artifacts: &'a SegmentArtifacts,
    pub(crate) raw_source: SpineHostRawSource<'a>,
    pub(crate) mem_items: &'a BTreeMap<String, ResponseItem>,
    pub(crate) note_items: &'a BTreeMap<String, Vec<ResponseItem>>,
}

pub(crate) fn materialize_spine_host_history(
    input: SpineHostMaterializationInput<'_>,
) -> CodexResult<Vec<ResponseItem>> {
    let raw_source = PreparedRawSource::new(input.raw_source)?;
    let covered_raw_len = validate_cover(input.pi, input.artifacts).map_err(pi_render_error)?;
    let raw_source_len = raw_source.raw_len()?;
    if covered_raw_len != raw_source_len {
        return Err(CodexErr::Fatal(format!(
            "Spine host materialization Pi covers raw length {covered_raw_len}, expected {raw_source_len}"
        )));
    }
    let mut rendered = Vec::new();
    for segment in input.pi {
        match segment {
            Segment::Raw(raw_span) => {
                rendered.extend(raw_source.items_for_raw_span(*raw_span)?);
            }
            Segment::Mem { compact_id } => {
                span(segment, input.artifacts).map_err(pi_render_error)?;
                let item = input.mem_items.get(compact_id).ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "Spine host materialization missing Mem item for {compact_id}"
                    ))
                })?;
                rendered.push(item.clone());
            }
            Segment::Note { kind } => {
                let items = input.note_items.get(kind).ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "Spine host materialization missing Note item for {kind}"
                    ))
                })?;
                rendered.extend(items.iter().cloned());
            }
        }
    }
    Ok(rendered)
}

pub(crate) fn insert_spine_host_note_segments(
    pi: Vec<Segment>,
    compact_id: &str,
    note_kinds: Vec<String>,
    placement: NotePlacement,
) -> CodexResult<Vec<Segment>> {
    if note_kinds.is_empty() {
        return Ok(pi);
    }
    let notes = note_kinds
        .into_iter()
        .map(Segment::note)
        .collect::<Vec<_>>();
    let Some(target_index) = pi
        .iter()
        .position(|segment| matches!(segment, Segment::Mem { compact_id: id } if id == compact_id))
    else {
        return Err(CodexErr::Fatal(format!(
            "Spine host materialization could not place notes {} Mem {compact_id}",
            match placement {
                NotePlacement::BeforeMem => "before",
                NotePlacement::AfterMem => "after",
            }
        )));
    };

    let mut result = Vec::with_capacity(pi.len() + notes.len());
    match placement {
        NotePlacement::BeforeMem => {
            result.extend(pi[..target_index].iter().cloned());
            result.extend(notes.iter().cloned());
            result.extend(pi[target_index..].iter().cloned());
        }
        NotePlacement::AfterMem => {
            result.extend(pi[..=target_index].iter().cloned());
            result.extend(notes.iter().cloned());
            result.extend(pi[target_index + 1..].iter().cloned());
        }
    }
    Ok(result)
}

fn pi_render_error(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Fatal(format!(
        "Spine host materialization failed to build canonical cover: {error}"
    ))
}

enum PreparedRawSource<'a> {
    Direct(&'a [ResponseItem]),
    HostBridge {
        history: &'a [ResponseItem],
        projection: HostBridgeProjection<'a>,
    },
}

impl<'a> PreparedRawSource<'a> {
    fn new(source: SpineHostRawSource<'a>) -> CodexResult<Self> {
        match source {
            SpineHostRawSource::Direct(items) => Ok(Self::Direct(items)),
            SpineHostRawSource::HostBridge {
                history,
                runtime_spans,
            } => Ok(Self::HostBridge {
                history,
                projection: HostBridgeProjection::build(history, runtime_spans)?,
            }),
        }
    }

    fn items_for_raw_span(
        &self,
        raw_span: super::segment::RawSpan,
    ) -> CodexResult<Vec<ResponseItem>> {
        match self {
            Self::Direct(items) => {
                let start = usize::try_from(raw_span.start).map_err(|_| {
                    CodexErr::Fatal(format!(
                        "Spine host materialization Raw {} start cannot fit usize",
                        raw_span
                    ))
                })?;
                let end = usize::try_from(raw_span.end).map_err(|_| {
                    CodexErr::Fatal(format!(
                        "Spine host materialization Raw {} end cannot fit usize",
                        raw_span
                    ))
                })?;
                if end > items.len() || start > end {
                    return Err(CodexErr::Fatal(format!(
                        "Spine host materialization Raw {} is outside raw item length {}",
                        raw_span,
                        items.len()
                    )));
                }
                Ok(items[start..end].to_vec())
            }
            Self::HostBridge {
                history,
                projection,
            } => {
                let start_index = projection
                    .effective_index_for_raw_boundary(raw_span.start)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "Spine host materialization Raw {} start does not map to an effective index",
                            raw_span
                        ))
                    })?;
                let end_index = projection
                    .effective_index_for_raw_boundary(raw_span.end)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "Spine host materialization Raw {} end does not map to an effective index",
                            raw_span
                        ))
                    })?;
                if start_index > end_index {
                    return Err(CodexErr::Fatal(format!(
                        "Spine host materialization Raw {} maps to inverted effective range [{start_index}, {end_index})",
                        raw_span
                    )));
                }
                Ok(history[start_index..end_index].to_vec())
            }
        }
    }

    fn raw_len(&self) -> CodexResult<u64> {
        match self {
            Self::Direct(items) => u64::try_from(items.len()).map_err(|_| {
                CodexErr::Fatal(format!(
                    "Spine host materialization raw item length {} cannot fit u64",
                    items.len()
                ))
            }),
            Self::HostBridge { projection, .. } => Ok(projection.raw_len()),
        }
    }
}

#[cfg(test)]
#[path = "host_materialization_tests.rs"]
mod tests;
