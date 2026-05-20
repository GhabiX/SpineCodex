use std::collections::BTreeMap;
use std::fmt;
use thiserror::Error;

pub(crate) type SegmentArtifacts = BTreeMap<String, RawSpan>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RawSpan {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl RawSpan {
    pub(crate) fn new(start: u64, end: u64) -> Result<Self, SegmentError> {
        if start >= end {
            return Err(SegmentError::EmptySpan {
                index: None,
                span: Self { start, end },
            });
        }
        Ok(Self { start, end })
    }

    fn contains_interior(self, raw_boundary: u64) -> bool {
        self.start < raw_boundary && raw_boundary < self.end
    }

    fn contains_span(self, other: RawSpan) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    fn overlaps(self, other: RawSpan) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl fmt::Display for RawSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{},{})", self.start, self.end)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    Raw(RawSpan),
    Mem { compact_id: String },
    Note { kind: String },
}

impl Segment {
    #[cfg(test)]
    pub(crate) fn raw(start: u64, end: u64) -> Result<Self, SegmentError> {
        Ok(Self::Raw(RawSpan::new(start, end)?))
    }

    pub(crate) fn mem(compact_id: impl Into<String>) -> Self {
        Self::Mem {
            compact_id: compact_id.into(),
        }
    }

    pub(crate) fn note(kind: impl Into<String>) -> Self {
        Self::Note { kind: kind.into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentPosition {
    pub(crate) segment_index: usize,
    pub(crate) offset: u64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum SegmentError {
    #[error(
        "I5 canonical segment cover ordered/gap-free/non-overlap: segment {index:?} has empty span {span}"
    )]
    EmptySpan { index: Option<usize>, span: RawSpan },
    #[error(
        "I5 canonical segment cover ordered/gap-free/non-overlap: segment {index} creates raw gap, expected start {expected_start}, got {actual_start}"
    )]
    CoverGap {
        index: usize,
        expected_start: u64,
        actual_start: u64,
    },
    #[error(
        "I5 canonical segment cover ordered/gap-free/non-overlap: segment {index} creates raw overlap, expected start {expected_start}, got {actual_start}"
    )]
    CoverOverlap {
        index: usize,
        expected_start: u64,
        actual_start: u64,
    },
    #[error("I7 structured Mem evidence: Mem {compact_id} has no surviving artifact")]
    MissingMemArtifact { compact_id: String },
    #[error(
        "I9 no future live_start inside Mem interior: live raw_start {raw_start} lies inside Mem {compact_id} {span}"
    )]
    LiveStartInsideMem {
        raw_start: u64,
        compact_id: String,
        span: RawSpan,
    },
    #[error(
        "I8 future live boundaries round-trip through f/g: raw boundary {raw_boundary} does not map through f"
    )]
    BoundaryNotMapped { raw_boundary: u64 },
    #[error(
        "I8 future live boundaries round-trip through f/g: raw boundary {raw_boundary} maps to {position:?}, but g returns {mapped:?}"
    )]
    BoundaryRoundTrip {
        raw_boundary: u64,
        position: SegmentPosition,
        mapped: Option<u64>,
    },
    #[cfg(test)]
    #[error("I6 exact Cover_Pi replacement: cannot replace empty span {span}")]
    ReplacementEmpty { span: RawSpan },
    #[cfg(test)]
    #[error(
        "I6 exact Cover_Pi replacement: replacement {replacement} cuts through existing segment {existing}"
    )]
    ReplacementCutsSpan {
        replacement: RawSpan,
        existing: RawSpan,
    },
    #[cfg(test)]
    #[error("I6 exact Cover_Pi replacement: replacement {span} matched no raw-consuming cover")]
    ReplacementMatchedNoCover { span: RawSpan },
    #[error(
        "I5 canonical segment cover: Mem {compact_id} span {span} extends past raw_len {raw_len}"
    )]
    CanonicalMemPastRawLen {
        compact_id: String,
        span: RawSpan,
        raw_len: u64,
    },
    #[error(
        "I5 canonical segment cover: Mem {compact_id} span {span} overlaps prior selected span {prior}"
    )]
    CanonicalMemOverlap {
        compact_id: String,
        span: RawSpan,
        prior: RawSpan,
    },
}

pub(crate) fn span(
    segment: &Segment,
    artifacts: &SegmentArtifacts,
) -> Result<Option<RawSpan>, SegmentError> {
    match segment {
        Segment::Raw(raw_span) => Ok(Some(*raw_span)),
        Segment::Mem { compact_id } => mem_artifact_span(compact_id, artifacts).map(Some),
        Segment::Note { .. } => Ok(None),
    }
}

pub(crate) fn validate_cover(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
) -> Result<u64, SegmentError> {
    let mut cursor = 0;
    for (index, segment) in segments.iter().enumerate() {
        let Some(segment_span) = span(segment, artifacts)? else {
            continue;
        };
        if segment_span.start >= segment_span.end {
            return Err(SegmentError::EmptySpan {
                index: Some(index),
                span: segment_span,
            });
        }
        if segment_span.start > cursor {
            return Err(SegmentError::CoverGap {
                index,
                expected_start: cursor,
                actual_start: segment_span.start,
            });
        }
        if segment_span.start < cursor {
            return Err(SegmentError::CoverOverlap {
                index,
                expected_start: cursor,
                actual_start: segment_span.start,
            });
        }
        cursor = segment_span.end;
    }
    Ok(cursor)
}

pub(crate) fn validate_future_live_boundaries(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
    live_starts: &[u64],
) -> Result<(), SegmentError> {
    validate_cover(segments, artifacts)?;
    let mem_spans = segments
        .iter()
        .filter_map(|segment| {
            if let Segment::Mem { compact_id } = segment {
                Some(compact_id)
            } else {
                None
            }
        })
        .map(|compact_id| mem_artifact_span(compact_id, artifacts).map(|span| (compact_id, span)))
        .collect::<Result<Vec<_>, _>>()?;

    for raw_boundary in live_starts {
        for (compact_id, mem_span) in &mem_spans {
            if mem_span.contains_interior(*raw_boundary) {
                return Err(SegmentError::LiveStartInsideMem {
                    raw_start: *raw_boundary,
                    compact_id: (*compact_id).clone(),
                    span: *mem_span,
                });
            }
        }
        let position = f_boundary(segments, artifacts, *raw_boundary)?.ok_or(
            SegmentError::BoundaryNotMapped {
                raw_boundary: *raw_boundary,
            },
        )?;
        let mapped = g_boundary(segments, artifacts, position)?;
        if mapped != Some(*raw_boundary) {
            return Err(SegmentError::BoundaryRoundTrip {
                raw_boundary: *raw_boundary,
                position,
                mapped,
            });
        }
    }
    Ok(())
}

pub(crate) fn f_boundary(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
    raw_boundary: u64,
) -> Result<Option<SegmentPosition>, SegmentError> {
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            Segment::Note { .. } => {}
            Segment::Raw(raw_span) => {
                if raw_span.start <= raw_boundary && raw_boundary <= raw_span.end {
                    return Ok(Some(SegmentPosition {
                        segment_index: index,
                        offset: raw_boundary - raw_span.start,
                    }));
                }
            }
            Segment::Mem { compact_id } => {
                let mem_span = mem_artifact_span(compact_id, artifacts)?;
                if raw_boundary == mem_span.start {
                    return Ok(Some(SegmentPosition {
                        segment_index: index,
                        offset: 0,
                    }));
                }
                if raw_boundary == mem_span.end {
                    return Ok(Some(SegmentPosition {
                        segment_index: index + 1,
                        offset: 0,
                    }));
                }
                if mem_span.contains_interior(raw_boundary) {
                    return Ok(None);
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn g_boundary(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
    position: SegmentPosition,
) -> Result<Option<u64>, SegmentError> {
    if position.segment_index > segments.len() {
        return Ok(None);
    }
    if position.segment_index == segments.len() {
        if position.offset != 0 {
            return Ok(None);
        }
        return raw_before_index(segments, artifacts, position.segment_index).map(Some);
    }

    match &segments[position.segment_index] {
        Segment::Raw(raw_span) => {
            let Some(raw_boundary) = raw_span.start.checked_add(position.offset) else {
                return Ok(None);
            };
            Ok((raw_boundary <= raw_span.end).then_some(raw_boundary))
        }
        Segment::Mem { compact_id } => {
            let mem_span = mem_artifact_span(compact_id, artifacts)?;
            Ok((position.offset == 0).then_some(mem_span.start))
        }
        Segment::Note { .. } => {
            if position.offset != 0 {
                return Ok(None);
            }
            raw_before_index(segments, artifacts, position.segment_index).map(Some)
        }
    }
}

#[cfg(test)]
pub(crate) fn replace_exact_cover(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
    replacement_span: RawSpan,
    replacement: Segment,
) -> Result<Vec<Segment>, SegmentError> {
    if replacement_span.start >= replacement_span.end {
        return Err(SegmentError::ReplacementEmpty {
            span: replacement_span,
        });
    }
    validate_cover(segments, artifacts)?;

    let mut next_segments = Vec::with_capacity(segments.len());
    let mut in_cover = false;
    let mut inserted = false;

    for segment in segments {
        let Some(segment_span) = span(segment, artifacts)? else {
            if !in_cover {
                next_segments.push(segment.clone());
            }
            continue;
        };
        if segment_span.end <= replacement_span.start || segment_span.start >= replacement_span.end
        {
            if in_cover && !inserted {
                next_segments.push(replacement.clone());
                inserted = true;
                in_cover = false;
            }
            next_segments.push(segment.clone());
            continue;
        }
        if segment_span.start < replacement_span.start || segment_span.end > replacement_span.end {
            return Err(SegmentError::ReplacementCutsSpan {
                replacement: replacement_span,
                existing: segment_span,
            });
        }
        in_cover = true;
    }

    if in_cover && !inserted {
        next_segments.push(replacement);
        inserted = true;
    }
    if !inserted {
        return Err(SegmentError::ReplacementMatchedNoCover {
            span: replacement_span,
        });
    }
    validate_cover(&next_segments, artifacts)?;
    Ok(next_segments)
}

pub(crate) fn canonical_cover<'a>(
    raw_len: u64,
    compact_ids: impl IntoIterator<Item = &'a str>,
    artifacts: &SegmentArtifacts,
) -> Result<Vec<Segment>, SegmentError> {
    let mut candidates = compact_ids
        .into_iter()
        .map(|compact_id| {
            artifacts
                .get(compact_id)
                .copied()
                .map(|span| (compact_id.to_string(), span))
                .ok_or_else(|| SegmentError::MissingMemArtifact {
                    compact_id: compact_id.to_string(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort_by(|left, right| {
        left.1
            .start
            .cmp(&right.1.start)
            .then_with(|| right.1.end.cmp(&left.1.end))
            .then_with(|| left.0.cmp(&right.0))
    });

    let mut selected: Vec<(String, RawSpan)> = Vec::new();
    for (compact_id, candidate_span) in candidates {
        if candidate_span.end > raw_len {
            return Err(SegmentError::CanonicalMemPastRawLen {
                compact_id,
                span: candidate_span,
                raw_len,
            });
        }
        if let Some((_, prior_span)) = selected.last()
            && prior_span.contains_span(candidate_span)
        {
            continue;
        }
        if let Some((_, prior_span)) = selected.last()
            && prior_span.overlaps(candidate_span)
        {
            return Err(SegmentError::CanonicalMemOverlap {
                compact_id,
                span: candidate_span,
                prior: *prior_span,
            });
        }
        selected.push((compact_id, candidate_span));
    }

    let mut segments = Vec::new();
    let mut cursor = 0;
    for (compact_id, mem_span) in selected {
        if cursor < mem_span.start {
            segments.push(Segment::Raw(RawSpan {
                start: cursor,
                end: mem_span.start,
            }));
        }
        segments.push(Segment::Mem { compact_id });
        cursor = mem_span.end;
    }
    if cursor < raw_len {
        segments.push(Segment::Raw(RawSpan {
            start: cursor,
            end: raw_len,
        }));
    }
    validate_cover(&segments, artifacts)?;
    Ok(segments)
}

fn raw_before_index(
    segments: &[Segment],
    artifacts: &SegmentArtifacts,
    index: usize,
) -> Result<u64, SegmentError> {
    let mut cursor = 0;
    for segment in segments.iter().take(index) {
        if let Some(segment_span) = span(segment, artifacts)? {
            cursor = segment_span.end;
        }
    }
    Ok(cursor)
}

fn mem_artifact_span(
    compact_id: &str,
    artifacts: &SegmentArtifacts,
) -> Result<RawSpan, SegmentError> {
    artifacts
        .get(compact_id)
        .copied()
        .ok_or_else(|| SegmentError::MissingMemArtifact {
            compact_id: compact_id.to_string(),
        })
}

#[cfg(test)]
#[path = "segment_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "segment_random_tests.rs"]
mod random_tests;
