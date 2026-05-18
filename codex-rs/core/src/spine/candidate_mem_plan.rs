use super::ids::NodeId;
use super::project_pi::ProjectInput;
use super::project_pi::ProjectMemInstall;
use super::project_pi::ProjectMemRejectionReason;
use super::project_pi::project_pi;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::SegmentError;
use super::segment::canonical_cover;
use super::segment::validate_future_live_boundaries;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateAdmissionPolicy {
    MustAdmit,
    AdmitOrRejectNodeNotVisible,
}

pub(crate) enum CandidateMemPlanMode<'a> {
    ProjectionBacked {
        state: &'a SpineState,
        admission: CandidateAdmissionPolicy,
    },
    CoverOnly {
        live_boundaries: &'a [u64],
        admission: CandidateAdmissionPolicy,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateMemPlan {
    pub(crate) pi: Vec<Segment>,
    pub(crate) artifacts: SegmentArtifacts,
    pub(crate) admitted_candidate: bool,
    pub(crate) rejected_candidate_reason: Option<ProjectMemRejectionReason>,
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

pub(crate) fn plan_candidate_mem(
    raw_len: u64,
    runtime_spans: &[InstalledCompactSpan],
    candidate: &CandidateMem,
    mode: CandidateMemPlanMode<'_>,
) -> CodexResult<CandidateMemPlan> {
    let CandidateMemCover { pi, artifacts } =
        plan_candidate_mem_cover(raw_len, runtime_spans, candidate).map_err(|err| {
            CodexErr::Fatal(format!(
                "candidate Mem segment cover rejected {}: {err}",
                candidate.compact_id
            ))
        })?;

    match mode {
        CandidateMemPlanMode::ProjectionBacked { state, admission } => {
            let live_starts = visible_live_starts_for_segment_plan(state)?;
            validate_future_live_boundaries(&pi, &artifacts, &live_starts).map_err(|err| {
                CodexErr::Fatal(format!(
                    "candidate Mem segment live-boundary validation failed for {}: {err}",
                    candidate.compact_id
                ))
            })?;
            let result = project_candidate_mem(raw_len, runtime_spans, candidate, state)?;
            let rejected_candidate_reason = result.rejected_mem_ids.iter().find_map(|rejection| {
                (rejection.compact_id == candidate.compact_id).then_some(rejection.reason.clone())
            });
            let admitted_candidate = result
                .admitted_mem_ids
                .iter()
                .any(|admitted_id| admitted_id == &candidate.compact_id);
            validate_candidate_admission(
                candidate,
                admitted_candidate,
                &rejected_candidate_reason,
                admission,
            )?;
            Ok(CandidateMemPlan {
                pi,
                artifacts,
                admitted_candidate,
                rejected_candidate_reason,
            })
        }
        CandidateMemPlanMode::CoverOnly {
            live_boundaries,
            admission,
        } => {
            validate_future_live_boundaries(&pi, &artifacts, live_boundaries).map_err(|err| {
                CodexErr::Fatal(format!(
                    "candidate Mem segment live-boundary validation failed for {}: {err}",
                    candidate.compact_id
                ))
            })?;
            validate_candidate_admission(candidate, true, &None, admission)?;
            Ok(CandidateMemPlan {
                pi,
                artifacts,
                admitted_candidate: true,
                rejected_candidate_reason: None,
            })
        }
    }
}

fn project_candidate_mem(
    raw_len: u64,
    runtime_spans: &[InstalledCompactSpan],
    candidate: &CandidateMem,
    state: &SpineState,
) -> CodexResult<super::project_pi::ProjectResult> {
    let mut input = ProjectInput::new(raw_len, state.clone());
    for span in runtime_spans {
        input.mem_installs.push(
            ProjectMemInstall::new(
                span.compact_id.clone(),
                span.node_id.clone(),
                span.cut_ordinal,
                span.fold_end_ordinal,
            )
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "candidate Mem segment plan rejected installed span {}: {err}",
                    span.compact_id
                ))
            })?,
        );
    }
    let node_id = candidate.node_id.clone().ok_or_else(|| {
        CodexErr::Fatal(format!(
            "candidate Mem segment plan requires node id for {}",
            candidate.compact_id
        ))
    })?;
    input.mem_installs.push(
        ProjectMemInstall::new(
            candidate.compact_id.clone(),
            node_id,
            candidate.span.start,
            candidate.span.end,
        )
        .map_err(|err| {
            CodexErr::Fatal(format!(
                "candidate Mem segment plan rejected candidate span {}: {err}",
                candidate.compact_id
            ))
        })?,
    );
    project_pi(input).map_err(|err| {
        CodexErr::Fatal(format!(
            "candidate Mem segment plan failed Project(Pi) validation for {}: {err}",
            candidate.compact_id
        ))
    })
}

fn validate_candidate_admission(
    candidate: &CandidateMem,
    admitted_candidate: bool,
    rejected_candidate_reason: &Option<ProjectMemRejectionReason>,
    admission: CandidateAdmissionPolicy,
) -> CodexResult<()> {
    if admitted_candidate {
        return Ok(());
    }
    if admission == CandidateAdmissionPolicy::AdmitOrRejectNodeNotVisible
        && *rejected_candidate_reason == Some(ProjectMemRejectionReason::NodeNotVisible)
    {
        return Ok(());
    }
    Err(CodexErr::Fatal(format!(
        "candidate Mem segment plan did not admit candidate Mem {}",
        candidate.compact_id
    )))
}

fn visible_live_starts_for_segment_plan(state: &SpineState) -> CodexResult<Vec<u64>> {
    let mut live_starts = Vec::new();
    for node_id in state.visible_spine() {
        let Some(node) = state.node(&node_id) else {
            return Err(CodexErr::Fatal(format!(
                "candidate Mem segment plan visible node {node_id} is missing"
            )));
        };
        if matches!(node.status, NodeStatus::Live | NodeStatus::Opened) {
            let Some(raw_start) = node.raw_start_ordinal else {
                return Err(CodexErr::Fatal(format!(
                    "candidate Mem segment plan mutable node {node_id} is missing raw_start_ordinal"
                )));
            };
            live_starts.push(raw_start);
        }
    }
    Ok(live_starts)
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
