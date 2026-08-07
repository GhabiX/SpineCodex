use crate::BoundaryId;
use crate::CommittedSpineExecution;
use crate::ContextEpoch;
use crate::ContextPlanRecipe;
use crate::ContextPlanSource;
use crate::Feature;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::SamplingArchiveRecord;
use crate::SamplingAttempt;
use crate::SamplingAttemptId;
use crate::SamplingCommit;
use crate::SamplingCommitId;
use crate::SamplingHandle;
use crate::SamplingStarted;
use crate::SealedSampling;
use crate::SourceCellId;
use crate::SourceLedger;
use crate::SourceSnapshot;
use crate::SpineChar;
use crate::SpineCharParser;
use crate::SpineCompactBarrierV1;
use crate::SpineCompiler;
use crate::SpineConfig;
use crate::SpineOperationFact;
use crate::SpineProjection;
use crate::ThreadNamespace;
use crate::TrimRequest;
use crate::TrimTicket;
use crate::archive::SAMPLING_STARTED_SCHEMA;
use crate::pressure::InputPressureState;
use crate::sampling_delta::FactBindingMode;
use crate::sampling_delta::SamplingDelta;
use crate::sampling_delta::SamplingDeltaError;
use crate::sampling_delta::preview_source_delta;
use crate::sampling_delta::reduce_compact_delta;
use crate::sampling_delta::reduce_sampling_delta;

mod context_builder;
mod error;
mod helpers;
mod types;

pub(crate) use context_builder::build_context_plan;
pub use error::PlannerError;
pub use error::PlannerTransitionError;
use types::CandidatePlannerState;
pub use types::PreparedSamplingCommit;
pub(crate) use types::RecoveredPlannerState;
pub use types::SamplingCommitOutput;

#[derive(Clone, Debug)]
pub(crate) struct SamplingPlanner {
    source: SourceLedger,
    parser: SpineCharParser,
    compiler: SpineCompiler,
    committed_source_cells: usize,
    previous_pre_boundary: Option<BoundaryId>,
    previous_commit_id: Option<SamplingCommitId>,
    committed_plan: Option<ContextPlanRecipe>,
    input_pressure: InputPressureState,
    next_projection_ordinal: u64,
    active_attempt: Option<SamplingAttemptId>,
    jit_enabled: bool,
    trim_enabled: bool,
    spawn_enabled: bool,
}

impl SamplingPlanner {
    pub fn new(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
        config: SpineConfig,
    ) -> Result<Self, PlannerError> {
        let jit_enabled = config.is_enabled(Feature::Jit);
        let trim_enabled = config.is_enabled(Feature::Trim);
        let spawn_enabled = config.is_enabled(Feature::Spawn);
        Ok(Self {
            source: SourceLedger::new(thread, epoch).map_err(PlannerError::Source)?,
            compiler: SpineCompiler::new(config).map_err(PlannerError::Initialize)?,
            parser: SpineCharParser::default(),
            committed_source_cells: 0,
            previous_pre_boundary: None,
            previous_commit_id: None,
            committed_plan: None,
            input_pressure: InputPressureState::default(),
            next_projection_ordinal: 0,
            active_attempt: None,
            jit_enabled,
            trim_enabled,
            spawn_enabled,
        })
    }

    pub fn observe_source<I>(&mut self, characters: I) -> Result<Vec<SourceCellId>, PlannerError>
    where
        I: IntoIterator<Item = SpineChar>,
    {
        self.source.append(characters).map_err(PlannerError::Source)
    }

    pub fn source_snapshot(&self) -> SourceSnapshot {
        self.source.snapshot()
    }

    /// Builds the current source-only context view without advancing durable sampling state.
    ///
    /// JIT hosts need the live projection of ordinary source items before the next real stream
    /// starts. This preview uses the same parser/compiler state as sampling reduction, but keeps
    /// the candidate private until the corresponding sampling commit is installed.
    pub fn preview_context_plan(&self) -> Result<ContextPlanRecipe, PlannerError> {
        let snapshot = self.source.snapshot();
        let mut parser = self.parser.clone();
        let mut compiler = self.compiler.clone();
        if self.committed_source_cells < snapshot.cells().len() {
            preview_source_delta(
                &snapshot,
                self.committed_source_cells,
                &mut parser,
                &mut compiler,
            )
            .map_err(map_sampling_delta_error)?;
        }
        let mut projection_ordinal = self.next_projection_ordinal;
        build_context_plan(
            &snapshot,
            compiler.projection(),
            compiler.trim_projection(),
            &parser.pending_boundaries(),
            self.committed_plan.as_ref(),
            &mut projection_ordinal,
            self.spawn_enabled,
        )
    }

    pub fn continue_in_namespace(&mut self, thread: ThreadNamespace) -> Result<(), PlannerError> {
        if self.active_attempt.is_some() {
            return Err(PlannerError::SamplingAlreadyActive);
        }
        self.source
            .continue_in_namespace(thread, self.committed_source_cells)
            .map_err(PlannerError::Source)?;
        self.previous_pre_boundary = None;
        self.next_projection_ordinal = 0;
        Ok(())
    }

    pub fn validated_trim_fact(
        &self,
        request: &TrimRequest,
    ) -> Result<SpineOperationFact, PlannerError> {
        if !self.trim_enabled {
            return Err(PlannerError::FeatureDisabled(Feature::Trim));
        }
        let projection = self
            .compiler
            .trim_projection()
            .ok_or(PlannerError::TrimRuntimeUnavailable)?;
        let (response_boundary, call_id, validated_edit) = projection
            .validated_edit(request)
            .map_err(PlannerError::InvalidTrim)?;
        let snapshot = self.source.snapshot();
        let target = snapshot
            .stable_tool_output(response_boundary, &call_id)
            .ok_or(PlannerError::MissingTrimBoundary(response_boundary))?;
        let ticket = TrimTicket::parse(
            self.source.thread().clone(),
            self.source.epoch(),
            format!("trim-{}", target.response.ordinal()),
        )
        .map_err(|error| PlannerError::InvalidTrim(error.to_string()))?;
        Ok(SpineOperationFact::Trim {
            ticket,
            target,
            validated_edit,
            source_digest: snapshot.digest().as_str().to_string(),
        })
    }

    pub fn begin_sampling(
        &mut self,
        attempt_id: SamplingAttemptId,
    ) -> Result<SamplingHandle, PlannerError> {
        if self.active_attempt.is_some() {
            return Err(PlannerError::SamplingAlreadyActive);
        }
        if !self.jit_enabled {
            return Err(PlannerError::JitDisabled);
        }
        if attempt_id.thread() != self.source.thread() {
            return Err(PlannerError::IdentityScopeMismatch);
        }
        let pre_boundary = self.current_boundary();
        let attempt = SamplingAttempt::new(attempt_id.clone(), self.source.epoch(), pre_boundary)
            .map_err(PlannerError::Sampling)?;
        self.active_attempt = Some(attempt_id);
        Ok(attempt.begin())
    }

    pub fn sampling_started_record(
        &self,
        handle: &SamplingHandle,
        prompt_digest: RecordDigest,
    ) -> Result<SamplingArchiveRecord, PlannerError> {
        self.require_active(handle.attempt().attempt_id())?;
        SamplingArchiveRecord::SamplingStarted(SamplingStarted {
            schema: SAMPLING_STARTED_SCHEMA.to_string(),
            attempt_id: handle.attempt().attempt_id().clone(),
            epoch: self.source.epoch(),
            pre_boundary: handle.attempt().pre_boundary().clone(),
            previous_commit_id: self.previous_commit_id.clone(),
            prompt_digest,
            source_digest: self.source.digest().clone(),
            record_digest: RecordDigest::parse("0".repeat(64)).map_err(PlannerError::Archive)?,
        })
        .finalize_digest()
        .map_err(PlannerError::Archive)
    }

    pub fn abort_sampling(&mut self, handle: &SamplingHandle) -> Result<(), PlannerError> {
        self.require_active(handle.attempt().attempt_id())?;
        handle.abort().map_err(PlannerError::Sampling)?;
        self.active_attempt = None;
        Ok(())
    }

    #[cfg(test)]
    pub fn prepare_sampling(
        &mut self,
        sealed: SealedSampling,
        post_boundary: BoundaryId,
        commit_id: SamplingCommitId,
        started_record_digest: RecordDigest,
    ) -> Result<PreparedSamplingCommit, PlannerError> {
        self.prepare_sampling_with_input_tokens(
            sealed,
            post_boundary,
            commit_id,
            started_record_digest,
            None,
        )
    }

    pub(crate) fn prepare_sampling_with_input_tokens(
        &mut self,
        sealed: SealedSampling,
        post_boundary: BoundaryId,
        commit_id: SamplingCommitId,
        started_record_digest: RecordDigest,
        input_tokens: Option<u64>,
    ) -> Result<PreparedSamplingCommit, PlannerError> {
        self.require_active(sealed.attempt().attempt_id())?;
        self.validate_commit_scope(&sealed, &post_boundary, &commit_id)?;

        let snapshot = self.source.snapshot();
        if snapshot.last_boundary() != Some(&post_boundary) {
            return Err(PlannerError::PostBoundaryIsNotSourceTail);
        }
        if self.committed_source_cells > snapshot.cells().len() {
            return Err(PlannerError::CommittedSourcePrefixMissing);
        }

        let mut parser = self.parser.clone();
        let mut compiler = self.compiler.clone();
        let ordered_facts = sealed.facts().to_vec();
        let mut input_pressure = self.input_pressure.clone();
        input_pressure.apply_sampling(
            input_tokens,
            ordered_facts.iter().map(|fact| &fact.operation),
        );
        let opens_node = ordered_facts.iter().any(|fact| {
            matches!(
                fact.operation,
                SpineOperationFact::Open { .. } | SpineOperationFact::Next { .. }
            )
        });
        let open_input_tokens = if opens_node {
            input_pressure.current_input_tokens()
        } else {
            None
        };
        let fact_sources = reduce_sampling_delta(
            SamplingDelta {
                snapshot: &snapshot,
                committed_source_cells: self.committed_source_cells,
                pre_boundary: RawBoundary(sealed.attempt().pre_boundary().ordinal()),
                post_boundary: RawBoundary(post_boundary.ordinal()),
                facts: &ordered_facts,
                open_input_tokens,
                binding_mode: FactBindingMode::Derive,
            },
            &mut parser,
            &mut compiler,
        )
        .map_err(map_sampling_delta_error)?;

        let mut projection_ordinal = self.next_projection_ordinal;
        let plan = build_context_plan(
            &snapshot,
            compiler.projection(),
            compiler.trim_projection(),
            &parser.pending_boundaries(),
            self.committed_plan.as_ref(),
            &mut projection_ordinal,
            self.spawn_enabled,
        )?;
        let pre_boundary = sealed.attempt().pre_boundary().clone();
        let executions = ordered_facts
            .into_iter()
            .zip(fact_sources)
            .map(|(fact, source_span)| CommittedSpineExecution {
                execution_id: fact.execution_id,
                ordinal: fact.ordinal,
                origin: fact.origin,
                source_span: crate::archive::SourceSpan {
                    start: source_span.start,
                    end: source_span.end,
                },
                operation: fact.operation,
            })
            .collect();
        let record = SamplingCommit {
            schema: crate::archive::SAMPLING_COMMIT_SCHEMA.to_string(),
            attempt_id: sealed.attempt().attempt_id().clone(),
            started_record_digest,
            commit_id: commit_id.clone(),
            epoch: self.source.epoch(),
            previous_pre_boundary: self.previous_pre_boundary.clone(),
            pre_boundary: pre_boundary.clone(),
            post_boundary,
            previous_commit_id: self.previous_commit_id.clone(),
            input_tokens,
            executions,
            source_digest: snapshot.digest().clone(),
            record_digest: RecordDigest::parse("0".repeat(64)).map_err(PlannerError::Archive)?,
        };
        let record = match SamplingArchiveRecord::SamplingCommit(record)
            .finalize_digest()
            .map_err(PlannerError::Archive)?
        {
            SamplingArchiveRecord::SamplingCommit(record) => record,
            SamplingArchiveRecord::SamplingStarted(_) => {
                unreachable!("sampling commit finalization preserves its variant")
            }
        };
        let prepared = PreparedSamplingCommit {
            record,
            plan,
            projection: compiler.projection().clone(),
            candidate: CandidatePlannerState {
                base_commit_id: self.previous_commit_id.clone(),
                base_source_cells: self.committed_source_cells,
                parser,
                compiler,
                committed_source_cells: snapshot.cells().len(),
                previous_pre_boundary: Some(pre_boundary),
                previous_commit_id: Some(commit_id),
                next_projection_ordinal: projection_ordinal,
                input_pressure,
            },
        };
        self.active_attempt = None;
        Ok(prepared)
    }

    pub fn install_prepared(
        &mut self,
        prepared: PreparedSamplingCommit,
    ) -> Result<SamplingCommitOutput, PlannerError> {
        if prepared.record.attempt_id.thread() != self.source.thread()
            || prepared.record.epoch != self.source.epoch()
        {
            return Err(PlannerError::IdentityScopeMismatch);
        }
        if prepared.record.source_digest != *self.source.digest()
            || self.previous_commit_id != prepared.candidate.base_commit_id
            || self.committed_source_cells != prepared.candidate.base_source_cells
        {
            return Err(PlannerError::PreparedSamplingStale);
        }
        self.parser = prepared.candidate.parser;
        self.compiler = prepared.candidate.compiler;
        self.committed_source_cells = prepared.candidate.committed_source_cells;
        self.previous_pre_boundary = prepared.candidate.previous_pre_boundary;
        self.previous_commit_id = prepared.candidate.previous_commit_id;
        self.next_projection_ordinal = prepared.candidate.next_projection_ordinal;
        self.input_pressure = prepared.candidate.input_pressure;
        self.committed_plan = Some(prepared.plan.clone());
        Ok(SamplingCommitOutput {
            record: prepared.record,
            plan: prepared.plan,
            projection: prepared.projection,
        })
    }

    pub(crate) fn discard_sampling(
        &mut self,
        attempt_id: &SamplingAttemptId,
    ) -> Result<(), PlannerError> {
        self.require_active(attempt_id)?;
        self.active_attempt = None;
        Ok(())
    }

    pub fn projection(&self) -> &SpineProjection {
        self.compiler.projection()
    }

    pub(crate) fn node_context_costs(
        &self,
        context_window_samples: &[crate::ContextWindowSample],
    ) -> std::collections::BTreeMap<crate::NodeId, crate::NodeContextCost> {
        self.compiler.node_context_costs(context_window_samples)
    }

    pub(crate) fn thread(&self) -> &ThreadNamespace {
        self.source.thread()
    }

    pub(crate) const fn epoch(&self) -> ContextEpoch {
        self.source.epoch()
    }

    pub(crate) fn current_input_tokens(&self) -> Option<u64> {
        self.input_pressure.current_input_tokens()
    }

    pub(crate) fn compact(
        &mut self,
        barrier: SpineCompactBarrierV1,
    ) -> Result<SpineProjection, PlannerError> {
        if self.active_attempt.is_some() {
            return Err(PlannerError::SamplingAlreadyActive);
        }
        barrier
            .validate()
            .map_err(|error| PlannerError::InvalidCompactBarrier(error.to_string()))?;
        if barrier.thread != *self.source.thread()
            || barrier.previous_epoch != self.source.epoch()
            || barrier.next_epoch
                != self
                    .source
                    .epoch()
                    .checked_next()
                    .unwrap_or(self.source.epoch())
        {
            return Err(PlannerError::IdentityScopeMismatch);
        }
        let mut candidate = self.clone();
        let snapshot = candidate.source.snapshot();
        reduce_compact_delta(
            &snapshot,
            candidate.committed_source_cells,
            &barrier,
            &mut candidate.parser,
            &mut candidate.compiler,
        )
        .map_err(map_sampling_delta_error)?;
        candidate
            .source
            .advance_epoch(barrier.next_epoch)
            .map_err(PlannerError::Source)?;
        candidate
            .source
            .append(
                barrier
                    .replacement_boundaries
                    .iter()
                    .copied()
                    .map(|boundary| SpineChar::Opaque { boundary }),
            )
            .map_err(PlannerError::Source)?;
        candidate.committed_source_cells = candidate.source.snapshot().cells().len();
        candidate.previous_pre_boundary = None;
        candidate.next_projection_ordinal = 0;
        candidate.input_pressure.compact();
        let snapshot = candidate.source.snapshot();
        candidate.committed_plan = Some(build_context_plan(
            &snapshot,
            candidate.compiler.projection(),
            candidate.compiler.trim_projection(),
            &candidate.parser.pending_boundaries(),
            None,
            &mut candidate.next_projection_ordinal,
            candidate.spawn_enabled,
        )?);
        *self = candidate;
        Ok(self.compiler.projection().clone())
    }

    pub(crate) fn from_replay_state(
        state: RecoveredPlannerState,
        runtime_config: SpineConfig,
    ) -> Result<Self, PlannerError> {
        let jit_enabled = runtime_config.is_enabled(Feature::Jit);
        let trim_enabled = runtime_config.is_enabled(Feature::Trim);
        let spawn_enabled = runtime_config.is_enabled(Feature::Spawn);
        let mut compiler = state.compiler;
        compiler
            .set_runtime_config(runtime_config)
            .map_err(PlannerError::Initialize)?;
        let next_projection_ordinal = state
            .committed_plan
            .as_ref()
            .into_iter()
            .flat_map(|plan| &plan.cells)
            .filter_map(|cell| match cell {
                crate::ContextPlanCell::Projection { projection_id, .. } => {
                    Some(projection_id.ordinal().saturating_add(1))
                }
                crate::ContextPlanCell::Source { .. } => None,
            })
            .max()
            .unwrap_or(0);
        Ok(Self {
            source: state.source,
            parser: state.parser,
            compiler,
            committed_source_cells: state.committed_source_cells,
            previous_pre_boundary: state.previous_pre_boundary,
            previous_commit_id: state.previous_commit_id,
            committed_plan: state.committed_plan,
            input_pressure: state.input_pressure,
            next_projection_ordinal,
            active_attempt: None,
            jit_enabled,
            trim_enabled,
            spawn_enabled,
        })
    }

    fn current_boundary(&self) -> BoundaryId {
        self.source.current_boundary()
    }

    fn require_active(&self, attempt_id: &SamplingAttemptId) -> Result<(), PlannerError> {
        match &self.active_attempt {
            Some(active) if active == attempt_id => Ok(()),
            Some(_) => Err(PlannerError::AttemptMismatch),
            None => Err(PlannerError::NoActiveSampling),
        }
    }

    fn validate_commit_scope(
        &self,
        sealed: &SealedSampling,
        post_boundary: &BoundaryId,
        commit_id: &SamplingCommitId,
    ) -> Result<(), PlannerError> {
        if sealed.attempt().epoch() != self.source.epoch()
            || sealed.attempt().attempt_id().thread() != self.source.thread()
            || sealed.attempt().pre_boundary().thread() != self.source.thread()
            || post_boundary.thread() != self.source.thread()
            || post_boundary.epoch() != self.source.epoch()
            || commit_id.thread() != self.source.thread()
        {
            return Err(PlannerError::IdentityScopeMismatch);
        }
        if sealed.attempt().pre_boundary().ordinal() > post_boundary.ordinal() {
            return Err(PlannerError::InvalidBoundaryOrder);
        }
        for fact in sealed.facts() {
            if fact.execution_id.thread() != self.source.thread() {
                return Err(PlannerError::IdentityScopeMismatch);
            }
            fact.validate().map_err(PlannerError::InvalidFact)?;
            match fact.operation {
                SpineOperationFact::Trim { .. } if !self.trim_enabled => {
                    return Err(PlannerError::FeatureDisabled(Feature::Trim));
                }
                SpineOperationFact::Spawn { .. } if !self.spawn_enabled => {
                    return Err(PlannerError::FeatureDisabled(Feature::Spawn));
                }
                SpineOperationFact::Open { .. }
                | SpineOperationFact::Close { .. }
                | SpineOperationFact::Next { .. }
                | SpineOperationFact::Spawn { .. }
                | SpineOperationFact::Trim { .. } => {}
            }
        }
        Ok(())
    }
}

fn map_sampling_delta_error(error: SamplingDeltaError) -> PlannerError {
    match error {
        SamplingDeltaError::Parse(error) => PlannerError::Parse(error),
        SamplingDeltaError::Compile(error) => helpers::map_compile_error(error),
        SamplingDeltaError::MissingSourceBoundary(boundary) => {
            PlannerError::MissingSourceBoundary(boundary)
        }
        SamplingDeltaError::MissingTrimSource(source) => PlannerError::MissingTrimSource(source),
        SamplingDeltaError::FactHasNoSourceGroup(execution) => {
            PlannerError::FactHasNoSourceGroup(execution)
        }
        SamplingDeltaError::FactSourceAppliedMoreThanOnce
        | SamplingDeltaError::FactSourceExecutionMismatch => PlannerError::InvalidBoundaryOrder,
    }
}
