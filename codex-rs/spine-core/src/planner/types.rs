use crate::BoundaryId;
use crate::ContextPlanRecipe;
use crate::SamplingCommit;
use crate::SamplingCommitId;
use crate::SourceLedger;
use crate::SpineCharParser;
use crate::SpineCompiler;
use crate::SpineProjection;
use crate::pressure::InputPressureState;

pub struct PreparedSamplingCommit {
    pub(super) record: SamplingCommit,
    pub(super) plan: ContextPlanRecipe,
    pub(super) projection: SpineProjection,
    pub(super) candidate: CandidatePlannerState,
}

impl PreparedSamplingCommit {
    pub fn durable_record(&self) -> &SamplingCommit {
        &self.record
    }

    pub fn context_plan(&self) -> &ContextPlanRecipe {
        &self.plan
    }

    pub fn projection(&self) -> &SpineProjection {
        &self.projection
    }
}

pub(crate) struct RecoveredPlannerState {
    pub source: SourceLedger,
    pub parser: SpineCharParser,
    pub compiler: SpineCompiler,
    pub committed_source_cells: usize,
    pub previous_pre_boundary: Option<BoundaryId>,
    pub previous_commit_id: Option<SamplingCommitId>,
    pub committed_plan: Option<ContextPlanRecipe>,
    pub input_pressure: InputPressureState,
}

pub(super) struct CandidatePlannerState {
    pub(super) base_commit_id: Option<SamplingCommitId>,
    pub(super) base_source_cells: usize,
    pub(super) parser: SpineCharParser,
    pub(super) compiler: SpineCompiler,
    pub(super) committed_source_cells: usize,
    pub(super) previous_pre_boundary: Option<BoundaryId>,
    pub(super) previous_commit_id: Option<SamplingCommitId>,
    pub(super) next_projection_ordinal: u64,
    pub(super) input_pressure: InputPressureState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamplingCommitOutput {
    pub record: SamplingCommit,
    pub plan: ContextPlanRecipe,
    pub projection: SpineProjection,
}
