use crate::ContextEpoch;
use crate::ExecutedSpineFact;
use crate::ExecutionId;
use crate::ExecutionOrigin;
use crate::FactPermit;
use crate::PlannerError;
use crate::PreparedSamplingCommit;
use crate::RecordDigest;
use crate::SamplingArchiveRecord;
use crate::SamplingAttemptId;
use crate::SamplingCommitId;
use crate::SamplingFactSink;
use crate::SamplingHandle;
use crate::SamplingPlanner;
use crate::SourceCellId;
use crate::SourceSnapshot;
use crate::SpineChar;
use crate::SpineCompactBarrierV1;
use crate::SpineConfig;
use crate::SpineOperationFact;
use crate::SpineProjection;
use crate::ThreadNamespace;
use crate::TrimRequest;
use crate::planner::SamplingCommitOutput;
use std::collections::HashMap;
use std::sync::Arc;

/// Owns the complete live canonical sampling transaction.
///
/// Hosts provide source characters and execution outcomes. Identity allocation,
/// fact admission, sealing, reduction, and commit preparation remain inside the
/// SDK so an adaptor cannot construct a second sampling state machine.
pub struct SamplingRuntime {
    planner: SamplingPlanner,
    next_attempt: u64,
    next_commit: u64,
    state: SamplingRuntimeState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplingTerminal {
    Completed,
    Failed,
    Cancelled,
}

pub enum SamplingFinish {
    OrphanedStart,
    Prepared(PreparedSamplingCommit),
}

enum SamplingRuntimeState {
    Idle,
    Active(SamplingExecutionBatch),
    Prepared {
        attempt_id: SamplingAttemptId,
        commit_id: SamplingCommitId,
    },
}

struct SamplingExecutionBatch {
    attempt_id: SamplingAttemptId,
    pre_boundary: crate::BoundaryId,
    started_record_digest: Option<RecordDigest>,
    sink: Arc<SamplingFactSink>,
    next_execution: u64,
    pending: HashMap<String, PendingSamplingExecution>,
}

struct PendingSamplingExecution {
    permit: FactPermit,
    fact: Option<ExecutedSpineFact>,
}

impl SamplingRuntime {
    pub fn new(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
        config: SpineConfig,
    ) -> Result<Self, PlannerError> {
        Ok(Self {
            planner: SamplingPlanner::new(thread, epoch, config)?,
            next_attempt: 0,
            next_commit: 0,
            state: SamplingRuntimeState::Idle,
        })
    }

    pub(crate) fn from_replay(
        planner: SamplingPlanner,
        next_attempt: u64,
        next_commit: u64,
    ) -> Self {
        Self {
            planner,
            next_attempt,
            next_commit,
            state: SamplingRuntimeState::Idle,
        }
    }

    pub fn observe_source<I>(&mut self, characters: I) -> Result<Vec<SourceCellId>, PlannerError>
    where
        I: IntoIterator<Item = SpineChar>,
    {
        if matches!(self.state, SamplingRuntimeState::Prepared { .. }) {
            return Err(PlannerError::SamplingCommitPendingInstall);
        }
        self.planner.observe_source(characters)
    }

    pub fn source_snapshot(&self) -> SourceSnapshot {
        self.planner.source_snapshot()
    }

    pub fn preview_context_plan(&self) -> Result<crate::ContextPlanRecipe, PlannerError> {
        self.planner.preview_context_plan()
    }

    pub fn begin_sampling(&mut self) -> Result<SamplingHandle, PlannerError> {
        match self.state {
            SamplingRuntimeState::Idle => {}
            SamplingRuntimeState::Active(_) => return Err(PlannerError::SamplingAlreadyActive),
            SamplingRuntimeState::Prepared { .. } => {
                return Err(PlannerError::SamplingCommitPendingInstall);
            }
        }
        let thread = self.planner.thread().clone();
        let attempt_id = SamplingAttemptId::parse(
            thread.clone(),
            format!("{}-attempt-{}", thread.as_str(), self.next_attempt),
        )
        .map_err(|_| PlannerError::IdentityScopeMismatch)?;
        self.next_attempt = self.next_attempt.saturating_add(1);
        let handle = self.planner.begin_sampling(attempt_id)?;
        self.state = SamplingRuntimeState::Active(SamplingExecutionBatch {
            attempt_id: handle.attempt().attempt_id().clone(),
            pre_boundary: handle.attempt().pre_boundary().clone(),
            started_record_digest: None,
            sink: handle.fact_sink(),
            next_execution: 0,
            pending: HashMap::new(),
        });
        Ok(handle)
    }

    pub fn sampling_started_record(
        &mut self,
        handle: &SamplingHandle,
        prompt_digest: RecordDigest,
    ) -> Result<SamplingArchiveRecord, PlannerError> {
        if self.require_active(handle)?.started_record_digest.is_some() {
            return Err(PlannerError::SamplingAlreadyStarted);
        }
        let record = self
            .planner
            .sampling_started_record(handle, prompt_digest)?;
        let active = self.require_active_mut(handle)?;
        active.started_record_digest = Some(record.record_digest().clone());
        Ok(record)
    }

    pub fn register_execution(&mut self, key: &str) -> Result<(), PlannerError> {
        let thread = self.planner.thread().clone();
        let active = self.active_mut()?;
        if active.pending.contains_key(key) {
            let _ = active.sink.abort();
            return Err(PlannerError::DuplicateExecutionKey(key.to_string()));
        }
        let execution_id = ExecutionId::parse(
            thread.clone(),
            format!("{}-execution-{}", thread.as_str(), active.next_execution),
        )
        .map_err(|_| PlannerError::IdentityScopeMismatch)?;
        active.next_execution = active.next_execution.saturating_add(1);
        let permit = active
            .sink
            .reserve_execution(execution_id)
            .map_err(PlannerError::Sampling)?;
        active.pending.insert(
            key.to_string(),
            PendingSamplingExecution { permit, fact: None },
        );
        Ok(())
    }

    pub fn stage_execution(
        &mut self,
        key: &str,
        origin: ExecutionOrigin,
        operation: SpineOperationFact,
    ) -> Result<(), PlannerError> {
        let active = self.active_mut()?;
        let execution = active
            .pending
            .get_mut(key)
            .ok_or_else(|| PlannerError::UnknownExecutionKey(key.to_string()))?;
        if execution.fact.is_some() {
            let _ = active.sink.abort();
            return Err(PlannerError::ExecutionAlreadyStaged(key.to_string()));
        }
        execution.fact = Some(ExecutedSpineFact {
            execution_id: execution.permit.execution_id().clone(),
            ordinal: execution.permit.ordinal(),
            origin,
            operation,
        });
        Ok(())
    }

    pub fn finish_execution(&mut self, key: &str, succeeded: bool) -> Result<(), PlannerError> {
        let active = self.active_mut()?;
        let execution = active
            .pending
            .remove(key)
            .ok_or_else(|| PlannerError::UnknownExecutionKey(key.to_string()))?;
        if !succeeded {
            active
                .sink
                .abort_permit(execution.permit)
                .map_err(PlannerError::Sampling)?;
            return Ok(());
        }
        let Some(fact) = execution.fact else {
            let _ = active.sink.abort();
            return Err(PlannerError::SuccessfulExecutionMissingFact(
                key.to_string(),
            ));
        };
        active
            .sink
            .complete(execution.permit, fact)
            .map_err(PlannerError::Sampling)?;
        Ok(())
    }

    fn prepare_sampling(
        &mut self,
        handle: SamplingHandle,
        input_tokens: Option<u64>,
    ) -> Result<PreparedSamplingCommit, PlannerError> {
        let attempt_id = handle.attempt().attempt_id().clone();
        let (pending, started_record_digest) = {
            let active = self.require_active(&handle)?;
            (active.pending.len(), active.started_record_digest.clone())
        };
        if pending != 0 {
            self.abort_sampling(&handle)?;
            return Err(PlannerError::PendingExecutions(pending));
        }
        let Some(started_record_digest) = started_record_digest else {
            self.abort_sampling(&handle)?;
            return Err(PlannerError::SamplingNotStarted);
        };
        let sealed = match handle.seal(self.planner.epoch()) {
            Ok(sealed) => sealed,
            Err(error) => {
                self.abort_sampling(&handle)?;
                return Err(PlannerError::Sampling(error));
            }
        };
        let thread = self.planner.thread().clone();
        let commit_id = SamplingCommitId::parse(
            thread.clone(),
            format!("{}-commit-{}", thread.as_str(), self.next_commit),
        )
        .map_err(|_| PlannerError::IdentityScopeMismatch)?;
        let post_boundary = self
            .planner
            .source_snapshot()
            .last_boundary()
            .cloned()
            .unwrap_or_else(|| handle.attempt().pre_boundary().clone());
        let prepared = match self.planner.prepare_sampling_with_input_tokens(
            sealed,
            post_boundary,
            commit_id.clone(),
            started_record_digest,
            input_tokens,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.planner.discard_sampling(&attempt_id)?;
                self.state = SamplingRuntimeState::Idle;
                return Err(error);
            }
        };
        let active = self.take_active(&attempt_id)?;
        debug_assert!(active.pending.is_empty());
        self.next_commit = self.next_commit.saturating_add(1);
        self.state = SamplingRuntimeState::Prepared {
            attempt_id,
            commit_id,
        };
        Ok(prepared)
    }

    pub fn finish_sampling(
        &mut self,
        handle: SamplingHandle,
        terminal: SamplingTerminal,
    ) -> Result<SamplingFinish, PlannerError> {
        self.finish_sampling_with_input_tokens(handle, terminal, None)
    }

    pub fn finish_sampling_with_input_tokens(
        &mut self,
        handle: SamplingHandle,
        terminal: SamplingTerminal,
        input_tokens: Option<u64>,
    ) -> Result<SamplingFinish, PlannerError> {
        if terminal != SamplingTerminal::Completed && !self.active_sampling_has_delta(&handle) {
            self.abort_sampling(&handle)?;
            return Ok(SamplingFinish::OrphanedStart);
        }
        self.prepare_sampling(handle, input_tokens)
            .map(SamplingFinish::Prepared)
    }

    /// Returns whether aborting the host task could discard canonical sampling work.
    pub fn has_pending_durable_sampling(&self) -> bool {
        match &self.state {
            SamplingRuntimeState::Idle => false,
            SamplingRuntimeState::Prepared { .. } => true,
            SamplingRuntimeState::Active(active) => {
                !active.pending.is_empty()
                    || active.sink.has_completed_facts()
                    || self
                        .planner
                        .source_snapshot()
                        .last_boundary()
                        .is_some_and(|boundary| boundary > &active.pre_boundary)
            }
        }
    }

    pub fn abort_sampling(&mut self, handle: &SamplingHandle) -> Result<(), PlannerError> {
        self.take_active(handle.attempt().attempt_id())?;
        self.planner.abort_sampling(handle)
    }

    fn active_sampling_has_delta(&self, handle: &SamplingHandle) -> bool {
        matches!(
            &self.state,
            SamplingRuntimeState::Active(active)
                if active.attempt_id == *handle.attempt().attempt_id()
                    && (active.sink.has_completed_facts()
                        || self
                            .planner
                            .source_snapshot()
                            .last_boundary()
                            .is_some_and(|boundary| boundary > &active.pre_boundary))
        )
    }

    pub fn install_prepared(
        &mut self,
        prepared: PreparedSamplingCommit,
    ) -> Result<SamplingCommitOutput, PlannerError> {
        let matches_pending = matches!(
            &self.state,
            SamplingRuntimeState::Prepared {
                attempt_id,
                commit_id,
            } if attempt_id == &prepared.durable_record().attempt_id
                && commit_id == &prepared.durable_record().commit_id
        );
        if !matches_pending {
            return Err(PlannerError::PreparedSamplingMismatch);
        }
        let output = self.planner.install_prepared(prepared)?;
        self.state = SamplingRuntimeState::Idle;
        Ok(output)
    }

    pub fn validated_trim_fact(
        &self,
        request: &TrimRequest,
    ) -> Result<SpineOperationFact, PlannerError> {
        self.planner.validated_trim_fact(request)
    }

    pub fn compact(
        &mut self,
        barrier: SpineCompactBarrierV1,
    ) -> Result<SpineProjection, PlannerError> {
        match self.state {
            SamplingRuntimeState::Idle => {}
            SamplingRuntimeState::Active(_) => return Err(PlannerError::SamplingAlreadyActive),
            SamplingRuntimeState::Prepared { .. } => {
                return Err(PlannerError::SamplingCommitPendingInstall);
            }
        }
        self.planner.compact(barrier)
    }

    pub fn projection(&self) -> &SpineProjection {
        self.planner.projection()
    }

    /// Returns the immutable open-time context cost for each live task node.
    pub fn node_context_costs(
        &self,
        context_window_samples: &[crate::ContextWindowSample],
    ) -> std::collections::BTreeMap<crate::NodeId, crate::NodeContextCost> {
        self.planner.node_context_costs(context_window_samples)
    }

    pub fn current_input_tokens(&self) -> Option<u64> {
        self.planner.current_input_tokens()
    }

    pub fn thread(&self) -> &ThreadNamespace {
        self.planner.thread()
    }

    pub const fn epoch(&self) -> ContextEpoch {
        self.planner.epoch()
    }

    pub fn continue_in_namespace(&mut self, thread: ThreadNamespace) -> Result<(), PlannerError> {
        match self.state {
            SamplingRuntimeState::Idle => {}
            SamplingRuntimeState::Active(_) => return Err(PlannerError::SamplingAlreadyActive),
            SamplingRuntimeState::Prepared { .. } => {
                return Err(PlannerError::SamplingCommitPendingInstall);
            }
        }
        let changed = self.planner.thread() != &thread;
        if changed {
            self.planner.continue_in_namespace(thread)?;
            self.next_attempt = 0;
            self.next_commit = 0;
        }
        Ok(())
    }

    fn require_active_mut(
        &mut self,
        handle: &SamplingHandle,
    ) -> Result<&mut SamplingExecutionBatch, PlannerError> {
        let active = self.active_mut()?;
        if active.attempt_id != *handle.attempt().attempt_id() {
            return Err(PlannerError::AttemptMismatch);
        }
        Ok(active)
    }

    fn take_active(
        &mut self,
        attempt_id: &SamplingAttemptId,
    ) -> Result<SamplingExecutionBatch, PlannerError> {
        let state = std::mem::replace(&mut self.state, SamplingRuntimeState::Idle);
        let SamplingRuntimeState::Active(active) = state else {
            self.state = state;
            return Err(match self.state {
                SamplingRuntimeState::Prepared { .. } => PlannerError::SamplingCommitPendingInstall,
                SamplingRuntimeState::Idle => PlannerError::NoActiveSampling,
                SamplingRuntimeState::Active(_) => unreachable!(),
            });
        };
        if &active.attempt_id != attempt_id {
            self.state = SamplingRuntimeState::Active(active);
            return Err(PlannerError::AttemptMismatch);
        }
        Ok(active)
    }

    fn require_active(
        &self,
        handle: &SamplingHandle,
    ) -> Result<&SamplingExecutionBatch, PlannerError> {
        let active = match &self.state {
            SamplingRuntimeState::Active(active) => active,
            SamplingRuntimeState::Idle => return Err(PlannerError::NoActiveSampling),
            SamplingRuntimeState::Prepared { .. } => {
                return Err(PlannerError::SamplingCommitPendingInstall);
            }
        };
        if active.attempt_id != *handle.attempt().attempt_id() {
            return Err(PlannerError::AttemptMismatch);
        }
        Ok(active)
    }

    fn active_mut(&mut self) -> Result<&mut SamplingExecutionBatch, PlannerError> {
        match &mut self.state {
            SamplingRuntimeState::Active(active) => Ok(active),
            SamplingRuntimeState::Idle => Err(PlannerError::NoActiveSampling),
            SamplingRuntimeState::Prepared { .. } => {
                Err(PlannerError::SamplingCommitPendingInstall)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn into_planner(self) -> SamplingPlanner {
        self.planner
    }
}
