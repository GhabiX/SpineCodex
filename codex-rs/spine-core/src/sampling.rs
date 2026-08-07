use crate::AdmissionOrdinal;
use crate::BoundaryId;
use crate::ContextEpoch;
use crate::ExecutedFactError;
use crate::ExecutedSpineFact;
use crate::ExecutionId;
use crate::SamplingAttemptId;
use crate::SpineOperationFact;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SamplingAttempt {
    attempt_id: SamplingAttemptId,
    epoch: ContextEpoch,
    pre_boundary: BoundaryId,
}

impl SamplingAttempt {
    pub(crate) fn new(
        attempt_id: SamplingAttemptId,
        epoch: ContextEpoch,
        pre_boundary: BoundaryId,
    ) -> Result<Self, SamplingError> {
        if attempt_id.thread() != pre_boundary.thread() || pre_boundary.epoch() != epoch {
            return Err(SamplingError::InvalidAttemptScope);
        }
        Ok(Self {
            attempt_id,
            epoch,
            pre_boundary,
        })
    }

    pub(crate) fn begin(self) -> SamplingHandle {
        SamplingHandle {
            sink: Arc::new(SamplingFactSink::new(self.clone())),
            attempt: self,
        }
    }

    pub(crate) fn attempt_id(&self) -> &SamplingAttemptId {
        &self.attempt_id
    }

    pub(crate) const fn epoch(&self) -> ContextEpoch {
        self.epoch
    }

    pub(crate) fn pre_boundary(&self) -> &BoundaryId {
        &self.pre_boundary
    }
}

#[derive(Debug)]
pub struct SamplingHandle {
    attempt: SamplingAttempt,
    sink: Arc<SamplingFactSink>,
}

impl SamplingHandle {
    pub(crate) fn attempt(&self) -> &SamplingAttempt {
        &self.attempt
    }

    pub(crate) fn fact_sink(&self) -> Arc<SamplingFactSink> {
        Arc::clone(&self.sink)
    }

    pub(crate) fn seal(
        &self,
        current_epoch: ContextEpoch,
    ) -> Result<SealedSampling, SamplingError> {
        let facts = self.sink.seal(current_epoch)?;
        Ok(SealedSampling {
            attempt: self.attempt.clone(),
            facts,
        })
    }

    pub(crate) fn abort(&self) -> Result<(), SamplingError> {
        self.sink.abort_transaction()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineFactKind {
    Deferred,
    Open,
    Close,
    Next,
    Spawn,
    Trim,
}

impl SpineFactKind {
    const fn is_exclusive(self) -> bool {
        matches!(self, Self::Open | Self::Close | Self::Next | Self::Spawn)
    }

    const fn of(fact: &SpineOperationFact) -> Self {
        match fact {
            SpineOperationFact::Open { .. } => Self::Open,
            SpineOperationFact::Close { .. } => Self::Close,
            SpineOperationFact::Next { .. } => Self::Next,
            SpineOperationFact::Spawn { .. } => Self::Spawn,
            SpineOperationFact::Trim { .. } => Self::Trim,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineFactReservation {
    attempt_id: SamplingAttemptId,
    epoch: ContextEpoch,
    execution_id: ExecutionId,
    ordinal: AdmissionOrdinal,
    kind: SpineFactKind,
}

impl SpineFactReservation {
    #[cfg(test)]
    pub(crate) fn new(
        attempt: &SamplingAttempt,
        execution_id: ExecutionId,
        ordinal: AdmissionOrdinal,
        kind: SpineFactKind,
    ) -> Result<Self, SamplingError> {
        if execution_id.thread() != attempt.attempt_id.thread() {
            return Err(SamplingError::ExecutionScopeMismatch);
        }
        Ok(Self {
            attempt_id: attempt.attempt_id.clone(),
            epoch: attempt.epoch,
            execution_id,
            ordinal,
            kind,
        })
    }

    #[cfg(test)]
    pub(crate) fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    #[cfg(test)]
    pub(crate) const fn ordinal(&self) -> AdmissionOrdinal {
        self.ordinal
    }

    #[cfg(test)]
    pub(crate) const fn kind(&self) -> SpineFactKind {
        self.kind
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FactPermit {
    attempt_id: SamplingAttemptId,
    reservation: SpineFactReservation,
}

impl FactPermit {
    pub(crate) fn execution_id(&self) -> &ExecutionId {
        &self.reservation.execution_id
    }

    pub(crate) const fn ordinal(&self) -> AdmissionOrdinal {
        self.reservation.ordinal
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptedFact {
    pub execution_id: ExecutionId,
    pub ordinal: AdmissionOrdinal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SealedSampling {
    attempt: SamplingAttempt,
    facts: Vec<ExecutedSpineFact>,
}

impl SealedSampling {
    pub(crate) fn attempt(&self) -> &SamplingAttempt {
        &self.attempt
    }

    pub(crate) fn facts(&self) -> &[ExecutedSpineFact] {
        &self.facts
    }
}

#[derive(Debug)]
pub(crate) struct SamplingFactSink {
    attempt: SamplingAttempt,
    state: Mutex<SinkState>,
}

#[derive(Debug)]
struct SinkState {
    lifecycle: SinkLifecycle,
    active: BTreeMap<AdmissionOrdinal, SpineFactReservation>,
    completed: BTreeMap<AdmissionOrdinal, ExecutedSpineFact>,
    seen_ordinals: BTreeSet<AdmissionOrdinal>,
    seen_executions: BTreeSet<ExecutionId>,
    exclusive: Option<AdmissionOrdinal>,
    next_ordinal: u64,
}

impl Default for SinkState {
    fn default() -> Self {
        Self {
            lifecycle: SinkLifecycle::Open,
            active: BTreeMap::new(),
            completed: BTreeMap::new(),
            seen_ordinals: BTreeSet::new(),
            seen_executions: BTreeSet::new(),
            exclusive: None,
            next_ordinal: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SinkLifecycle {
    Open,
    Sealed,
    Aborted,
    Failed,
}

impl SamplingFactSink {
    fn new(attempt: SamplingAttempt) -> Self {
        Self {
            attempt,
            state: Mutex::new(SinkState::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn reserve(
        &self,
        reservation: SpineFactReservation,
    ) -> Result<FactPermit, SamplingError> {
        if reservation.attempt_id != self.attempt.attempt_id
            || reservation.epoch != self.attempt.epoch
        {
            return Err(SamplingError::ReservationAttemptMismatch);
        }
        let mut state = self.lock_state()?;
        state.next_ordinal = state
            .next_ordinal
            .max(reservation.ordinal.value().saturating_add(1));
        reserve(&self.attempt, &mut state, reservation)
    }

    pub(crate) fn reserve_execution(
        &self,
        execution_id: ExecutionId,
    ) -> Result<FactPermit, SamplingError> {
        if execution_id.thread() != self.attempt.attempt_id.thread() {
            return Err(SamplingError::ExecutionScopeMismatch);
        }
        let mut state = self.lock_state()?;
        let reservation = SpineFactReservation {
            attempt_id: self.attempt.attempt_id.clone(),
            epoch: self.attempt.epoch,
            execution_id,
            ordinal: AdmissionOrdinal::new(state.next_ordinal),
            kind: SpineFactKind::Deferred,
        };
        state.next_ordinal = state.next_ordinal.saturating_add(1);
        reserve(&self.attempt, &mut state, reservation)
    }

    pub(crate) fn complete(
        &self,
        permit: FactPermit,
        fact: ExecutedSpineFact,
    ) -> Result<AcceptedFact, SamplingError> {
        let mut state = self.lock_state()?;
        require_open(state.lifecycle)?;
        let result = self.validate_completion(&state, &permit, &fact);
        if let Err(error) = result {
            fail_transaction(&mut state);
            return Err(error);
        }

        state.active.remove(&permit.reservation.ordinal);
        state
            .completed
            .insert(permit.reservation.ordinal, fact.clone());
        Ok(AcceptedFact {
            execution_id: fact.execution_id,
            ordinal: fact.ordinal,
        })
    }

    pub(crate) fn abort_permit(&self, permit: FactPermit) -> Result<(), SamplingError> {
        let mut state = self.lock_state()?;
        require_open(state.lifecycle)?;
        self.require_matching_permit(&state, &permit)?;
        state.active.remove(&permit.reservation.ordinal);
        if state.exclusive == Some(permit.reservation.ordinal) {
            state.exclusive = None;
        }
        Ok(())
    }

    pub(crate) fn abort(&self) -> Result<(), SamplingError> {
        self.abort_transaction()
    }

    pub(crate) fn has_completed_facts(&self) -> bool {
        self.lock_state()
            .is_ok_and(|state| !state.completed.is_empty())
    }

    fn validate_completion(
        &self,
        state: &SinkState,
        permit: &FactPermit,
        fact: &ExecutedSpineFact,
    ) -> Result<(), SamplingError> {
        self.require_matching_permit(state, permit)?;
        fact.validate().map_err(SamplingError::InvalidFact)?;
        if fact.execution_id != permit.reservation.execution_id {
            return Err(SamplingError::FactReservationMismatch("execution_id"));
        }
        if fact.ordinal != permit.reservation.ordinal {
            return Err(SamplingError::FactReservationMismatch("ordinal"));
        }
        if permit.reservation.kind != SpineFactKind::Deferred
            && SpineFactKind::of(&fact.operation) != permit.reservation.kind
        {
            return Err(SamplingError::FactReservationMismatch("kind"));
        }
        if let SpineOperationFact::Trim { target, .. } = &fact.operation
            && state.completed.values().any(|completed| {
                matches!(
                    &completed.operation,
                    SpineOperationFact::Trim {
                        target: completed_target,
                        ..
                    } if completed_target == target
                )
            })
        {
            return Err(SamplingError::ConflictingTrimTarget);
        }
        Ok(())
    }

    fn require_matching_permit(
        &self,
        state: &SinkState,
        permit: &FactPermit,
    ) -> Result<(), SamplingError> {
        if permit.attempt_id != self.attempt.attempt_id {
            return Err(SamplingError::PermitAttemptMismatch);
        }
        match state.active.get(&permit.reservation.ordinal) {
            Some(reservation) if reservation == &permit.reservation => Ok(()),
            Some(_) => Err(SamplingError::PermitReservationMismatch),
            None => Err(SamplingError::PermitNotActive),
        }
    }

    fn seal(&self, current_epoch: ContextEpoch) -> Result<Vec<ExecutedSpineFact>, SamplingError> {
        let mut state = self.lock_state()?;
        require_open(state.lifecycle)?;
        if current_epoch != self.attempt.epoch {
            state.lifecycle = SinkLifecycle::Aborted;
            state.active.clear();
            state.completed.clear();
            state.exclusive = None;
            return Err(SamplingError::StaleEpoch {
                expected: self.attempt.epoch,
                actual: current_epoch,
            });
        }
        if !state.active.is_empty() {
            let ordinals = state.active.keys().copied().collect();
            state.lifecycle = SinkLifecycle::Aborted;
            state.active.clear();
            state.completed.clear();
            state.exclusive = None;
            return Err(SamplingError::PendingPermits(ordinals));
        }
        let exclusive = state
            .completed
            .values()
            .filter(|fact| SpineFactKind::of(&fact.operation).is_exclusive())
            .map(|fact| fact.ordinal)
            .collect::<Vec<_>>();
        if let [existing, rejected, ..] = exclusive.as_slice() {
            let error = SamplingError::ExclusiveConflict {
                existing: *existing,
                rejected: *rejected,
            };
            fail_transaction(&mut state);
            return Err(error);
        }

        let facts = state.completed.values().cloned().collect();
        state.lifecycle = SinkLifecycle::Sealed;
        Ok(facts)
    }

    fn abort_transaction(&self) -> Result<(), SamplingError> {
        let mut state = self.lock_state()?;
        match state.lifecycle {
            SinkLifecycle::Open | SinkLifecycle::Failed => {
                state.lifecycle = SinkLifecycle::Aborted;
                state.active.clear();
                state.completed.clear();
                state.exclusive = None;
                Ok(())
            }
            SinkLifecycle::Aborted => Ok(()),
            SinkLifecycle::Sealed => Err(SamplingError::TransactionSealed),
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, SinkState>, SamplingError> {
        self.state
            .lock()
            .map_err(|_| SamplingError::SynchronizationPoisoned)
    }
}

fn reserve(
    attempt: &SamplingAttempt,
    state: &mut SinkState,
    reservation: SpineFactReservation,
) -> Result<FactPermit, SamplingError> {
    require_open(state.lifecycle)?;
    if state.seen_ordinals.contains(&reservation.ordinal) {
        let error = SamplingError::DuplicateOrdinal(reservation.ordinal);
        fail_transaction(state);
        return Err(error);
    }
    if state.seen_executions.contains(&reservation.execution_id) {
        let error = SamplingError::DuplicateExecution(reservation.execution_id);
        fail_transaction(state);
        return Err(error);
    }
    if reservation.kind.is_exclusive()
        && let Some(existing) = state.exclusive
    {
        let error = SamplingError::ExclusiveConflict {
            existing,
            rejected: reservation.ordinal,
        };
        fail_transaction(state);
        return Err(error);
    }
    state.seen_ordinals.insert(reservation.ordinal);
    state
        .seen_executions
        .insert(reservation.execution_id.clone());
    if reservation.kind.is_exclusive() {
        state.exclusive = Some(reservation.ordinal);
    }
    state
        .active
        .insert(reservation.ordinal, reservation.clone());
    Ok(FactPermit {
        attempt_id: attempt.attempt_id.clone(),
        reservation,
    })
}

fn fail_transaction(state: &mut SinkState) {
    state.lifecycle = SinkLifecycle::Failed;
    state.active.clear();
    state.completed.clear();
    state.exclusive = None;
}

fn require_open(lifecycle: SinkLifecycle) -> Result<(), SamplingError> {
    match lifecycle {
        SinkLifecycle::Open => Ok(()),
        SinkLifecycle::Sealed => Err(SamplingError::TransactionSealed),
        SinkLifecycle::Aborted => Err(SamplingError::TransactionAborted),
        SinkLifecycle::Failed => Err(SamplingError::TransactionFailed),
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SamplingError {
    #[error("sampling attempt boundary has a mismatched scope")]
    InvalidAttemptScope,
    #[error("fact execution belongs to a different thread")]
    ExecutionScopeMismatch,
    #[error("fact reservation belongs to a different sampling attempt")]
    ReservationAttemptMismatch,
    #[error("sampling ordinal {} was already reserved", .0.value())]
    DuplicateOrdinal(AdmissionOrdinal),
    #[error("execution {} was already reserved", .0.as_str())]
    DuplicateExecution(ExecutionId),
    #[error(
        "exclusive fact at ordinal {} conflicts with ordinal {}",
        existing.value(),
        rejected.value()
    )]
    ExclusiveConflict {
        existing: AdmissionOrdinal,
        rejected: AdmissionOrdinal,
    },
    #[error("fact permit belongs to a different sampling attempt")]
    PermitAttemptMismatch,
    #[error("fact permit does not match the active reservation")]
    PermitReservationMismatch,
    #[error("fact permit is no longer active")]
    PermitNotActive,
    #[error("completed fact does not match reserved {0}")]
    FactReservationMismatch(&'static str),
    #[error("multiple trim facts target the same source output")]
    ConflictingTrimTarget,
    #[error("invalid completed fact: {0}")]
    InvalidFact(ExecutedFactError),
    #[error("sampling has {} non-terminal fact permits", .0.len())]
    PendingPermits(Vec<AdmissionOrdinal>),
    #[error(
        "sampling epoch {} is stale; current epoch is {}",
        expected.value(),
        actual.value()
    )]
    StaleEpoch {
        expected: ContextEpoch,
        actual: ContextEpoch,
    },
    #[error("sampling transaction is sealed")]
    TransactionSealed,
    #[error("sampling transaction is aborted")]
    TransactionAborted,
    #[error("sampling transaction has failed")]
    TransactionFailed,
    #[error("sampling fact sink synchronization was poisoned")]
    SynchronizationPoisoned,
}
