#![allow(dead_code)]

use super::ids::NodeId;
use super::projection_epoch::ProjectionEpochMetadata;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::SegmentError;
use super::segment::canonical_cover;
use super::segment::span;
use super::segment::validate_cover;
use super::segment::validate_future_live_boundaries;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::CommittedMemInstall;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use thiserror::Error;

#[derive(Clone, Debug)]
pub(crate) struct ProjectInput {
    pub(crate) raw_len: u64,
    pub(crate) state: SpineState,
    pub(crate) mem_installs: Vec<ProjectMemInstall>,
    pub(crate) required_mem_ids: BTreeSet<String>,
    pub(crate) notes: Vec<ProjectNote>,
    pub(crate) projection_epoch: Option<ProjectionEpochMetadata>,
    pub(crate) resource_profile: Option<ProjectResourceProfile>,
    pub(crate) stop_reason: Option<String>,
}

impl ProjectInput {
    pub(crate) fn new(raw_len: u64, state: SpineState) -> Self {
        Self {
            raw_len,
            state,
            mem_installs: Vec::new(),
            required_mem_ids: BTreeSet::new(),
            notes: Vec::new(),
            projection_epoch: None,
            resource_profile: None,
            stop_reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectMemInstall {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) span: RawSpan,
    pub(crate) body_verified: bool,
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
            body_verified: true,
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
            body_verified: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectNote {
    pub(crate) raw_ordinal: u64,
    pub(crate) kind: String,
}

impl ProjectNote {
    pub(crate) fn new(raw_ordinal: u64, kind: impl Into<String>) -> Self {
        Self {
            raw_ordinal,
            kind: kind.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProjectResourceProfile {
    pub(crate) raw_tokens_per_item: u64,
    pub(crate) mem_tokens: BTreeMap<String, u64>,
    pub(crate) note_tokens: BTreeMap<String, u64>,
    pub(crate) budget_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectResult {
    pub(crate) pi: Vec<Segment>,
    pub(crate) visible_nodes: Vec<NodeId>,
    pub(crate) live_boundaries: Vec<u64>,
    pub(crate) admitted_mem_ids: Vec<String>,
    pub(crate) rejected_mem_ids: Vec<RejectedProjectMem>,
    pub(crate) cost: Option<ProjectCost>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectCost {
    pub(crate) total_tokens: u64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum ProjectError {
    #[error("ProjectCoverGap: {message}")]
    ProjectCoverGap { message: String },
    #[error("ProjectCoverOverlap: {message}")]
    ProjectCoverOverlap { message: String },
    #[error("ProjectLiveStartInsideMem: {message}")]
    ProjectLiveStartInsideMem { message: String },
    #[error("ProjectMissingMemInstall: {compact_id}")]
    ProjectMissingMemInstall { compact_id: String },
    #[error("ProjectMissingMemoryBody: {compact_id}")]
    ProjectMissingMemoryBody { compact_id: String },
    #[error("ProjectForkEvidenceIncomplete: {message}")]
    ProjectForkEvidenceIncomplete { message: String },
    #[error("ProjectStopBoundary: {reason}")]
    ProjectStopBoundary { reason: String },
    #[error("ProjectBudgetExceeded: cost {cost} exceeds budget {budget}")]
    ProjectBudgetExceeded { cost: u64, budget: u64 },
}

pub(crate) fn project_pi(input: ProjectInput) -> Result<ProjectResult, ProjectError> {
    if let Some(reason) = input.stop_reason {
        return Err(ProjectError::ProjectStopBoundary { reason });
    }
    if let Some(epoch) = &input.projection_epoch
        && epoch.effective_raw_len != input.raw_len
    {
        return Err(ProjectError::ProjectForkEvidenceIncomplete {
            message: format!(
                "projection epoch effective_raw_len {} does not match ProjectInput raw_len {}",
                epoch.effective_raw_len, input.raw_len
            ),
        });
    }

    let visible_nodes = input.state.visible_spine();
    let visible_set = visible_nodes.iter().cloned().collect::<HashSet<_>>();
    let live_boundaries = live_boundaries(&input.state, &visible_nodes)?;
    let mut rejected = Vec::new();
    let mut visible_installs = Vec::new();

    let install_ids = input
        .mem_installs
        .iter()
        .map(|install| install.compact_id.clone())
        .collect::<BTreeSet<_>>();
    for required in &input.required_mem_ids {
        if !install_ids.contains(required) {
            return Err(ProjectError::ProjectMissingMemInstall {
                compact_id: required.clone(),
            });
        }
    }

    let all_installs = input.mem_installs;
    let all_artifacts = segment_artifacts(&all_installs);
    for install in all_installs {
        if !install.body_verified {
            return Err(ProjectError::ProjectMissingMemoryBody {
                compact_id: install.compact_id,
            });
        }
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
        if !matches!(node.status, NodeStatus::Finished | NodeStatus::Closed) {
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
    let base_pi = canonical_cover(input.raw_len, compact_ids, &artifacts)?;
    let pi = insert_notes(base_pi, &artifacts, input.raw_len, input.notes)?;
    validate_cover(&pi, &artifacts)?;
    validate_future_live_boundaries(&pi, &artifacts, &live_boundaries)?;

    let admitted_mem_ids = pi
        .iter()
        .filter_map(|segment| match segment {
            Segment::Mem { compact_id } => Some(compact_id.clone()),
            Segment::Raw(_) | Segment::Note { .. } => None,
        })
        .collect::<Vec<_>>();
    mark_covered_rejections(&mut rejected, &all_artifacts, &pi)?;
    let cost = input
        .resource_profile
        .as_ref()
        .map(|profile| project_cost(&pi, &artifacts, profile))
        .transpose()?;

    Ok(ProjectResult {
        pi,
        visible_nodes,
        live_boundaries,
        admitted_mem_ids,
        rejected_mem_ids: rejected,
        cost,
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
        if matches!(node.status, NodeStatus::Live | NodeStatus::Opened) {
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

fn insert_notes(
    segments: Vec<Segment>,
    artifacts: &SegmentArtifacts,
    raw_len: u64,
    notes: Vec<ProjectNote>,
) -> Result<Vec<Segment>, ProjectError> {
    let mut notes = notes;
    notes.sort_by(|left, right| {
        left.raw_ordinal
            .cmp(&right.raw_ordinal)
            .then_with(|| left.kind.cmp(&right.kind))
    });
    if let Some(note) = notes.iter().find(|note| note.raw_ordinal > raw_len) {
        return Err(ProjectError::ProjectForkEvidenceIncomplete {
            message: format!(
                "note {} at raw ordinal {} exceeds raw_len {}",
                note.kind, note.raw_ordinal, raw_len
            ),
        });
    }

    let mut result = Vec::new();
    let mut note_index = 0;
    for segment in segments {
        let Some(segment_span) = span(&segment, artifacts)? else {
            result.push(segment);
            continue;
        };
        while note_index < notes.len() && notes[note_index].raw_ordinal < segment_span.start {
            result.push(Segment::note(notes[note_index].kind.clone()));
            note_index += 1;
        }
        match segment {
            Segment::Raw(raw_span) => {
                let mut cursor = raw_span.start;
                while note_index < notes.len() && notes[note_index].raw_ordinal <= raw_span.end {
                    let note = &notes[note_index];
                    if note.raw_ordinal > cursor {
                        result.push(Segment::Raw(RawSpan {
                            start: cursor,
                            end: note.raw_ordinal,
                        }));
                    }
                    result.push(Segment::note(note.kind.clone()));
                    cursor = note.raw_ordinal;
                    note_index += 1;
                }
                if cursor < raw_span.end {
                    result.push(Segment::Raw(RawSpan {
                        start: cursor,
                        end: raw_span.end,
                    }));
                }
            }
            Segment::Mem { compact_id } => {
                while note_index < notes.len()
                    && notes[note_index].raw_ordinal == segment_span.start
                {
                    result.push(Segment::note(notes[note_index].kind.clone()));
                    note_index += 1;
                }
                if note_index < notes.len()
                    && segment_span.start < notes[note_index].raw_ordinal
                    && notes[note_index].raw_ordinal < segment_span.end
                {
                    return Err(ProjectError::ProjectLiveStartInsideMem {
                        message: format!(
                            "note {} at raw ordinal {} lies inside Mem {compact_id} {}",
                            notes[note_index].kind, notes[note_index].raw_ordinal, segment_span
                        ),
                    });
                }
                result.push(Segment::Mem { compact_id });
                while note_index < notes.len() && notes[note_index].raw_ordinal == segment_span.end
                {
                    result.push(Segment::note(notes[note_index].kind.clone()));
                    note_index += 1;
                }
            }
            Segment::Note { .. } => unreachable!("base cover does not contain notes"),
        }
    }
    while note_index < notes.len() {
        result.push(Segment::note(notes[note_index].kind.clone()));
        note_index += 1;
    }
    Ok(result)
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

fn project_cost(
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    profile: &ProjectResourceProfile,
) -> Result<ProjectCost, ProjectError> {
    let mut total = 0u64;
    for segment in pi {
        match segment {
            Segment::Raw(raw_span) => {
                total = total.saturating_add(
                    raw_span
                        .end
                        .saturating_sub(raw_span.start)
                        .saturating_mul(profile.raw_tokens_per_item),
                );
            }
            Segment::Mem { compact_id } => {
                let Some(tokens) = profile.mem_tokens.get(compact_id) else {
                    return Err(ProjectError::ProjectMissingMemoryBody {
                        compact_id: compact_id.clone(),
                    });
                };
                total = total.saturating_add(*tokens);
            }
            Segment::Note { kind } => {
                total = total.saturating_add(*profile.note_tokens.get(kind).unwrap_or(&0));
            }
        }
    }
    if let Some(budget) = profile.budget_tokens
        && total > budget
    {
        return Err(ProjectError::ProjectBudgetExceeded {
            cost: total,
            budget,
        });
    }
    validate_cover(pi, artifacts)?;
    Ok(ProjectCost {
        total_tokens: total,
    })
}

impl From<SegmentError> for ProjectError {
    fn from(error: SegmentError) -> Self {
        match error {
            SegmentError::CoverGap { .. } | SegmentError::ReplacementMatchedNoCover { .. } => {
                ProjectError::ProjectCoverGap {
                    message: error.to_string(),
                }
            }
            SegmentError::CoverOverlap { .. }
            | SegmentError::CanonicalMemOverlap { .. }
            | SegmentError::ReplacementCutsSpan { .. } => ProjectError::ProjectCoverOverlap {
                message: error.to_string(),
            },
            SegmentError::LiveStartInsideMem { .. }
            | SegmentError::BoundaryNotMapped { .. }
            | SegmentError::BoundaryRoundTrip { .. } => ProjectError::ProjectLiveStartInsideMem {
                message: error.to_string(),
            },
            SegmentError::MissingMemArtifact { compact_id } => {
                ProjectError::ProjectMissingMemInstall { compact_id }
            }
            SegmentError::CanonicalMemPastRawLen { .. }
            | SegmentError::EmptySpan { .. }
            | SegmentError::ReplacementEmpty { .. } => {
                ProjectError::ProjectForkEvidenceIncomplete {
                    message: error.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "project_pi_tests.rs"]
mod tests;
