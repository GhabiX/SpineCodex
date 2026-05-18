#![allow(dead_code)]

use super::compact::SpineCompactBoundary;
use super::compact::effective_index_for_raw_ordinal_with_spans;
use super::compact::raw_ordinal_for_effective_index_with_spans;
use super::compact::validate_spine_replacement_history_admissible;
use super::project_pi::ProjectError;
use super::project_pi::ProjectInput;
use super::project_pi::ProjectResult;
use super::project_pi::project_pi;
use super::projection_epoch::ProjectionEpochMetadata;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::SegmentError;
use super::segment::validate_future_live_boundaries;
use super::state::SpineState;
use super::store::InstalledCompactSpan;
use super::store::SpineSidecarStore;
use super::store::classify_runtime_span_authority;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ResponseItem;
use std::collections::HashSet;
use std::fmt;

pub(crate) const INV_SEGMENT_COVER: &str = "I1 cover(Pi) ordered/gap-free/non-overlap";
pub(crate) const INV_RAW_BOUNDARY: &str = "I2 f/g round-trip at legal boundaries";
pub(crate) const INV_LIVE_BOUNDARY: &str = "I3 no future live raw_start inside Mem interior";
pub(crate) const INV_MEM_EVIDENCE: &str = "I6 every committed Mem has span metadata and body";
pub(crate) const INV_PROJECTION: &str =
    "I8 rollback/fork restrict raw_len, events, Mem artifacts, and cost together";
pub(crate) const INV_RENDER_BRIDGE: &str =
    "I10 render(Pi) output-only host-valid replacement_history";
pub(crate) const INV_STOP_BOUNDARY: &str = "I13 native non-Spine compact is Stop/read-only";
pub(crate) const INV_FEATURE_OFF: &str = "I14 Spine feature-off host behavior unchanged";
pub(crate) const INV_COST_BUDGET: &str = "I15 Cost(Pi) <= Budget before prompt/checkpoint exposure";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeDebugBoundary {
    StartupResume,
    BeforeCompact,
    BeforeCheckpointInstall,
    AfterCompactInstall,
    RollbackProjection,
    ForkSeed,
    FeatureOff,
    BridgeRender,
}

impl RuntimeDebugBoundary {
    fn label(self) -> &'static str {
        match self {
            Self::StartupResume => "startup/resume",
            Self::BeforeCompact => "before compact",
            Self::BeforeCheckpointInstall => "before checkpoint install",
            Self::AfterCompactInstall => "after compact install",
            Self::RollbackProjection => "rollback projection",
            Self::ForkSeed => "fork seed",
            Self::FeatureOff => "feature off",
            Self::BridgeRender => "bridge render",
        }
    }
}

impl fmt::Display for RuntimeDebugBoundary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeDebugAuditError {
    boundary: RuntimeDebugBoundary,
    invariant: &'static str,
    locator: String,
    detail: String,
}

impl RuntimeDebugAuditError {
    pub(crate) fn invariant(&self) -> &'static str {
        self.invariant
    }

    fn failed(
        boundary: RuntimeDebugBoundary,
        invariant: &'static str,
        locator: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            boundary,
            invariant,
            locator: locator.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for RuntimeDebugAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "runtime_debug_checks {} audit failed ({}): {}: {}",
            self.boundary, self.invariant, self.locator, self.detail
        )
    }
}

impl std::error::Error for RuntimeDebugAuditError {}

impl From<RuntimeDebugAuditError> for CodexErr {
    fn from(error: RuntimeDebugAuditError) -> Self {
        CodexErr::Fatal(error.to_string())
    }
}

pub(crate) fn audit_segment_cover(
    boundary: RuntimeDebugBoundary,
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    live_starts: &[u64],
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    let locator = locator.into();
    validate_future_live_boundaries(pi, artifacts, live_starts).map_err(|err| {
        RuntimeDebugAuditError::failed(
            boundary,
            invariant_for_segment_error(&err),
            locator,
            err.to_string(),
        )
    })
}

pub(crate) fn audit_project_pi(
    boundary: RuntimeDebugBoundary,
    input: ProjectInput,
    locator: impl Into<String>,
) -> Result<ProjectResult, RuntimeDebugAuditError> {
    let locator = locator.into();
    project_pi(input).map_err(|err| {
        RuntimeDebugAuditError::failed(
            boundary,
            invariant_for_project_error(&err),
            locator,
            err.to_string(),
        )
    })
}

pub(crate) fn audit_compact_plan_boundaries(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    boundary: &SpineCompactBoundary,
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    let locator = locator.into();
    if boundary.cut_ordinal > boundary.fold_end_ordinal {
        return Err(RuntimeDebugAuditError::failed(
            RuntimeDebugBoundary::BeforeCompact,
            INV_RAW_BOUNDARY,
            locator,
            format!(
                "cut ordinal {} is after fold_end ordinal {} for node {} op {:?}",
                boundary.cut_ordinal, boundary.fold_end_ordinal, boundary.node_id, boundary.op
            ),
        ));
    }
    for raw_boundary in [boundary.cut_ordinal, boundary.fold_end_ordinal] {
        let index = effective_index_for_raw_ordinal_with_spans(
            history,
            raw_boundary,
            runtime_spans,
        )
        .ok_or_else(|| {
            RuntimeDebugAuditError::failed(
                RuntimeDebugBoundary::BeforeCompact,
                INV_RAW_BOUNDARY,
                locator.clone(),
                format!(
                    "raw boundary {raw_boundary} for node {} op {:?} does not map to an effective history index",
                    boundary.node_id, boundary.op
                ),
            )
        })?;
        let round_trip = raw_ordinal_for_effective_index_with_spans(
            history,
            index,
            runtime_spans,
        )
        .ok_or_else(|| {
            RuntimeDebugAuditError::failed(
                RuntimeDebugBoundary::BeforeCompact,
                INV_RAW_BOUNDARY,
                locator.clone(),
                format!(
                    "effective index {index} for raw boundary {raw_boundary} does not map back to a raw ordinal"
                ),
            )
        })?;
        if round_trip != raw_boundary {
            return Err(RuntimeDebugAuditError::failed(
                RuntimeDebugBoundary::BeforeCompact,
                INV_RAW_BOUNDARY,
                locator.clone(),
                format!(
                    "raw boundary {raw_boundary} maps to effective index {index}, which maps back to {round_trip}"
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn audit_compact_checkpoint(
    boundary: RuntimeDebugBoundary,
    replacement_history: &[ResponseItem],
    spans_after_install: &[InstalledCompactSpan],
    required_raw_ordinals: &[u64],
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    let locator = locator.into();
    validate_spine_replacement_history_admissible(
        replacement_history,
        spans_after_install,
        required_raw_ordinals,
    )
    .map_err(|err| {
        RuntimeDebugAuditError::failed(boundary, INV_RENDER_BRIDGE, locator, err.to_string())
    })
}

pub(crate) fn audit_render_pi_equivalence(
    actual: &[ResponseItem],
    expected: &[ResponseItem],
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    if actual == expected {
        return Ok(());
    }
    Err(RuntimeDebugAuditError::failed(
        RuntimeDebugBoundary::BridgeRender,
        INV_RENDER_BRIDGE,
        locator,
        format!(
            "render(Pi) output length {} did not match expected bridge length {}",
            actual.len(),
            expected.len()
        ),
    ))
}

pub(crate) fn audit_meminstall_span_source_equivalence(
    store: &SpineSidecarStore,
    surviving_message_hashes: Option<&HashSet<String>>,
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    let locator = locator.into();
    let installed = store
        .installed_compact_spans_matching_hashes(surviving_message_hashes)
        .map_err(|err| {
            RuntimeDebugAuditError::failed(
                RuntimeDebugBoundary::AfterCompactInstall,
                INV_MEM_EVIDENCE,
                locator.clone(),
                format!("CompactInstalled span source failed: {err}"),
            )
        })?;
    let committed = store
        .committed_mem_install_spans_matching_hashes(surviving_message_hashes)
        .map_err(|err| {
            RuntimeDebugAuditError::failed(
                RuntimeDebugBoundary::AfterCompactInstall,
                INV_MEM_EVIDENCE,
                locator.clone(),
                format!("MemInstallCommitted span source failed: {err}"),
            )
        })?;
    if installed == committed {
        return Ok(());
    }
    // The shadow audit has no independent durable host-checkpoint marker yet.
    // For diagnostics only, CompactInstalled presence is the current proxy for
    // both host checkpoint materialization and bridge-terminal publication.
    let admission = classify_runtime_span_authority(
        !committed.is_empty(),
        !installed.is_empty(),
        !installed.is_empty(),
    );
    Err(RuntimeDebugAuditError::failed(
        RuntimeDebugBoundary::AfterCompactInstall,
        INV_MEM_EVIDENCE,
        locator,
        format!(
            "CompactInstalled span source did not match MemInstallCommitted span source: admission={admission:?}; CompactInstalled={installed:?}; MemInstallCommitted={committed:?}"
        ),
    ))
}

pub(crate) fn audit_projection_epoch(
    boundary: RuntimeDebugBoundary,
    expected_effective_raw_len: u64,
    epoch: &ProjectionEpochMetadata,
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    let locator = locator.into();
    if epoch.effective_raw_len != expected_effective_raw_len {
        return Err(RuntimeDebugAuditError::failed(
            boundary,
            INV_PROJECTION,
            locator,
            format!(
                "projection epoch effective_raw_len {} does not match expected {}",
                epoch.effective_raw_len, expected_effective_raw_len
            ),
        ));
    }
    for (field, value) in [
        (
            "processed_rollout_hash",
            epoch.processed_rollout_hash.as_str(),
        ),
        (
            "surviving_turn_ids_hash",
            epoch.surviving_turn_ids_hash.as_str(),
        ),
        ("state_hash", epoch.state_hash.as_str()),
    ] {
        if !value.starts_with("sha256:") {
            return Err(RuntimeDebugAuditError::failed(
                boundary,
                INV_PROJECTION,
                locator,
                format!("projection epoch {field} is not sha256-addressed: {value}"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn audit_projection_state_matches_runtime(
    boundary: RuntimeDebugBoundary,
    runtime_state: &SpineState,
    projection_state: &SpineState,
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    if runtime_state == projection_state {
        return Ok(());
    }
    Err(RuntimeDebugAuditError::failed(
        boundary,
        INV_PROJECTION,
        locator,
        format!(
            "runtime cursor {} does not match projected cursor {}",
            runtime_state.cursor(),
            projection_state.cursor()
        ),
    ))
}

pub(crate) fn audit_feature_off_boundary(
    spine_runtime_active: bool,
    locator: impl Into<String>,
) -> Result<(), RuntimeDebugAuditError> {
    if !spine_runtime_active {
        return Ok(());
    }
    Err(RuntimeDebugAuditError::failed(
        RuntimeDebugBoundary::FeatureOff,
        INV_FEATURE_OFF,
        locator,
        "Spine runtime is active while the feature is off",
    ))
}

fn invariant_for_segment_error(error: &SegmentError) -> &'static str {
    match error {
        SegmentError::EmptySpan { .. }
        | SegmentError::CoverGap { .. }
        | SegmentError::CoverOverlap { .. }
        | SegmentError::ReplacementEmpty { .. }
        | SegmentError::ReplacementCutsSpan { .. }
        | SegmentError::ReplacementMatchedNoCover { .. }
        | SegmentError::CanonicalMemPastRawLen { .. }
        | SegmentError::CanonicalMemOverlap { .. } => INV_SEGMENT_COVER,
        SegmentError::MissingMemArtifact { .. } => INV_MEM_EVIDENCE,
        SegmentError::LiveStartInsideMem { .. } => INV_LIVE_BOUNDARY,
        SegmentError::BoundaryNotMapped { .. } | SegmentError::BoundaryRoundTrip { .. } => {
            INV_RAW_BOUNDARY
        }
    }
}

fn invariant_for_project_error(error: &ProjectError) -> &'static str {
    match error {
        ProjectError::ProjectCoverGap { .. } | ProjectError::ProjectCoverOverlap { .. } => {
            INV_SEGMENT_COVER
        }
        ProjectError::ProjectLiveStartInsideMem { .. } => INV_LIVE_BOUNDARY,
        ProjectError::ProjectMissingMemInstall { .. }
        | ProjectError::ProjectMissingMemoryBody { .. } => INV_MEM_EVIDENCE,
        ProjectError::ProjectForkEvidenceIncomplete { .. } => INV_PROJECTION,
        ProjectError::ProjectStopBoundary { .. } => INV_STOP_BOUNDARY,
        ProjectError::ProjectBudgetExceeded { .. } => INV_COST_BUDGET,
    }
}

#[cfg(test)]
#[path = "debug_audit_tests.rs"]
mod tests;
