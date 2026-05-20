use super::ids::NodeId;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::SegmentError;
use super::segment::canonical_cover;
use super::segment::span;
use super::segment::validate_future_live_boundaries;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::CommittedMemInstall;
use std::collections::HashSet;
use thiserror::Error;

#[derive(Clone, Debug)]
pub(crate) struct ProjectInput {
    pub(crate) raw_len: u64,
    pub(crate) state: SpineState,
    pub(crate) mem_installs: Vec<ProjectMemInstall>,
}

impl ProjectInput {
    pub(crate) fn new(raw_len: u64, state: SpineState) -> Self {
        Self {
            raw_len,
            state,
            mem_installs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectMemInstall {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) span: RawSpan,
}

impl ProjectMemInstall {
    pub(crate) fn new(
        compact_id: impl Into<String>,
        node_id: NodeId,
        start: u64,
        end: u64,
    ) -> Result<Self, ProjectError> {
        Ok(Self {
            compact_id: compact_id.into(),
            node_id,
            span: RawSpan::new(start, end).map_err(ProjectError::from)?,
        })
    }
}

impl From<&CommittedMemInstall> for ProjectMemInstall {
    fn from(install: &CommittedMemInstall) -> Self {
        Self {
            compact_id: install.compact_id.clone(),
            node_id: install.node_id.clone(),
            span: RawSpan {
                start: install.cut_ordinal,
                end: install.fold_end_ordinal,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectResult {
    pub(crate) pi: Vec<Segment>,
    pub(crate) visible_nodes: Vec<NodeId>,
    pub(crate) live_boundaries: Vec<u64>,
    pub(crate) admitted_mem_ids: Vec<String>,
    pub(crate) rejected_mem_ids: Vec<RejectedProjectMem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RejectedProjectMem {
    pub(crate) compact_id: String,
    pub(crate) reason: ProjectMemRejectionReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectMemRejectionReason {
    NodeNotVisible,
    NodeNotSealed,
    CoveredByAncestor,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum ProjectError {
    #[error("ProjectCoverGap: {message}")]
    ProjectCoverGap { message: String },
    #[error("ProjectCoverOverlap: {message}")]
    ProjectCoverOverlap { message: String },
    #[error("ProjectLiveStartInsideMem: {message}")]
    ProjectLiveStartInsideMem { message: String },
    #[error("ProjectForkEvidenceIncomplete: {message}")]
    ProjectForkEvidenceIncomplete { message: String },
}

pub(crate) fn project_pi(input: ProjectInput) -> Result<ProjectResult, ProjectError> {
    let visible_nodes = input.state.visible_spine();
    let visible_set = visible_nodes.iter().cloned().collect::<HashSet<_>>();
    let live_boundaries = live_boundaries(&input.state, &visible_nodes)?;
    let mut rejected = Vec::new();
    let mut visible_installs = Vec::new();

    let all_installs = input.mem_installs;
    let all_artifacts = segment_artifacts(&all_installs);
    for install in all_installs {
        if !visible_set.contains(&install.node_id) {
            rejected.push(RejectedProjectMem {
                compact_id: install.compact_id,
                reason: ProjectMemRejectionReason::NodeNotVisible,
            });
            continue;
        }
        let node = input.state.node(&install.node_id).ok_or_else(|| {
            ProjectError::ProjectForkEvidenceIncomplete {
                message: format!("visible node {} is missing from state", install.node_id),
            }
        })?;
        if !matches!(node.status, NodeStatus::Closed) {
            rejected.push(RejectedProjectMem {
                compact_id: install.compact_id,
                reason: ProjectMemRejectionReason::NodeNotSealed,
            });
            continue;
        }
        visible_installs.push(install);
    }

    let artifacts = segment_artifacts(&visible_installs);
    let compact_ids = visible_installs
        .iter()
        .map(|install| install.compact_id.as_str())
        .collect::<Vec<_>>();
    let pi = canonical_cover(input.raw_len, compact_ids, &artifacts)?;
    validate_future_live_boundaries(&pi, &artifacts, &live_boundaries)?;

    let admitted_mem_ids = pi
        .iter()
        .filter_map(|segment| match segment {
            Segment::Mem { compact_id } => Some(compact_id.clone()),
            Segment::Raw(_) | Segment::Note { .. } => None,
        })
        .collect::<Vec<_>>();
    mark_covered_rejections(&mut rejected, &all_artifacts, &pi)?;

    Ok(ProjectResult {
        pi,
        visible_nodes,
        live_boundaries,
        admitted_mem_ids,
        rejected_mem_ids: rejected,
    })
}

fn live_boundaries(state: &SpineState, visible_nodes: &[NodeId]) -> Result<Vec<u64>, ProjectError> {
    let mut boundaries = Vec::new();
    for node_id in visible_nodes {
        let node =
            state
                .node(node_id)
                .ok_or_else(|| ProjectError::ProjectForkEvidenceIncomplete {
                    message: format!("visible node {node_id} is missing from state"),
                })?;
        if matches!(node.status, NodeStatus::Live | NodeStatus::Suspended) {
            let raw_start = node.raw_start_ordinal.ok_or_else(|| {
                ProjectError::ProjectForkEvidenceIncomplete {
                    message: format!("mutable visible node {node_id} is missing raw_start_ordinal"),
                }
            })?;
            if boundaries.last().copied() != Some(raw_start) {
                boundaries.push(raw_start);
            }
        }
    }
    Ok(boundaries)
}

fn segment_artifacts(installs: &[ProjectMemInstall]) -> SegmentArtifacts {
    installs
        .iter()
        .map(|install| (install.compact_id.clone(), install.span))
        .collect()
}

fn mark_covered_rejections(
    rejected: &mut [RejectedProjectMem],
    artifacts: &SegmentArtifacts,
    pi: &[Segment],
) -> Result<(), ProjectError> {
    let admitted_spans = pi
        .iter()
        .filter_map(|segment| match segment {
            Segment::Mem { compact_id } => Some(compact_id),
            Segment::Raw(_) | Segment::Note { .. } => None,
        })
        .map(|compact_id| span(&Segment::mem(compact_id.clone()), artifacts))
        .collect::<Result<Vec<_>, _>>()?;
    for rejection in rejected {
        let Some(rejected_span) = artifacts.get(&rejection.compact_id).copied() else {
            continue;
        };
        if admitted_spans
            .iter()
            .flatten()
            .any(|span| span.start <= rejected_span.start && rejected_span.end <= span.end)
        {
            rejection.reason = ProjectMemRejectionReason::CoveredByAncestor;
        }
    }
    Ok(())
}

impl From<SegmentError> for ProjectError {
    fn from(error: SegmentError) -> Self {
        match error {
            SegmentError::CoverGap { .. } => ProjectError::ProjectCoverGap {
                message: error.to_string(),
            },
            #[cfg(test)]
            SegmentError::ReplacementMatchedNoCover { .. } => ProjectError::ProjectCoverGap {
                message: error.to_string(),
            },
            SegmentError::CoverOverlap { .. } | SegmentError::CanonicalMemOverlap { .. } => {
                ProjectError::ProjectCoverOverlap {
                    message: error.to_string(),
                }
            }
            #[cfg(test)]
            SegmentError::ReplacementCutsSpan { .. } => ProjectError::ProjectCoverOverlap {
                message: error.to_string(),
            },
            SegmentError::LiveStartInsideMem { .. }
            | SegmentError::BoundaryNotMapped { .. }
            | SegmentError::BoundaryRoundTrip { .. } => ProjectError::ProjectLiveStartInsideMem {
                message: error.to_string(),
            },
            SegmentError::MissingMemArtifact { .. } => {
                ProjectError::ProjectForkEvidenceIncomplete {
                    message: error.to_string(),
                }
            }
            SegmentError::CanonicalMemPastRawLen { .. } | SegmentError::EmptySpan { .. } => {
                ProjectError::ProjectForkEvidenceIncomplete {
                    message: error.to_string(),
                }
            }
            #[cfg(test)]
            SegmentError::ReplacementEmpty { .. } => ProjectError::ProjectForkEvidenceIncomplete {
                message: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "project_pi_tests.rs"]
mod tests;
