use super::ids::NodeId;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::SegmentError;
use super::segment::canonical_cover;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateMem {
    pub(crate) compact_id: String,
    pub(crate) node_id: Option<NodeId>,
    pub(crate) op: Option<SpineOperation>,
    pub(crate) span: RawSpan,
}

impl CandidateMem {
    pub(crate) fn new(
        compact_id: impl Into<String>,
        node_id: NodeId,
        op: SpineOperation,
        span: RawSpan,
    ) -> Self {
        Self {
            compact_id: compact_id.into(),
            node_id: Some(node_id),
            op: Some(op),
            span,
        }
    }

    pub(crate) fn anonymous(compact_id: impl Into<String>, span: RawSpan) -> Self {
        Self {
            compact_id: compact_id.into(),
            node_id: None,
            op: None,
            span,
        }
    }

    fn label(&self) -> String {
        match (&self.node_id, self.op) {
            (Some(node_id), Some(op)) => {
                format!(
                    "{} node {} op {:?} span {}",
                    self.compact_id, node_id, op, self.span
                )
            }
            _ => format!("{} span {}", self.compact_id, self.span),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateMemCover {
    pub(crate) pi: Vec<Segment>,
    pub(crate) artifacts: SegmentArtifacts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateMemCoverError {
    candidate_label: String,
    source: SegmentError,
}

impl fmt::Display for CandidateMemCoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "candidate Mem {}: {}", self.candidate_label, self.source)
    }
}

pub(crate) fn plan_candidate_mem_cover(
    raw_len: u64,
    runtime_spans: &[InstalledCompactSpan],
    candidate: &CandidateMem,
) -> Result<CandidateMemCover, CandidateMemCoverError> {
    let mut artifacts = artifacts_from_runtime_spans(runtime_spans);
    artifacts.insert(candidate.compact_id.clone(), candidate.span);
    let mut compact_ids = runtime_spans
        .iter()
        .map(|span| span.compact_id.as_str())
        .collect::<Vec<_>>();
    compact_ids.push(candidate.compact_id.as_str());
    let pi = canonical_cover(raw_len, compact_ids, &artifacts).map_err(|source| {
        CandidateMemCoverError {
            candidate_label: candidate.label(),
            source,
        }
    })?;
    Ok(CandidateMemCover { pi, artifacts })
}

fn artifacts_from_runtime_spans(runtime_spans: &[InstalledCompactSpan]) -> SegmentArtifacts {
    runtime_spans
        .iter()
        .map(|span| {
            (
                span.compact_id.clone(),
                RawSpan {
                    start: span.cut_ordinal,
                    end: span.fold_end_ordinal,
                },
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "candidate_mem_plan_tests.rs"]
mod tests;
