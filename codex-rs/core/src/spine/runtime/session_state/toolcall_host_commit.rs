use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::super::SpineError;
use super::super::SpineHostEffects;
use super::SpineCommitAttempt;
use super::SpineCommitAttemptKind;
use super::completed_toolcall_evidence::SpineToolcallCommitEvidence;

pub(crate) struct SpineCompletedToolCallHostOutcome {
    #[cfg(test)]
    recording: SpineToolOutputRecording,
    post_commit_effects: SpineHostEffects,
    deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineToolOutputRecording {
    Skip,
    Normal,
    WithoutSpineObserve,
    RawOnlyDurableWithoutEmission,
}

#[cfg(test)]
impl SpineToolOutputRecording {
    pub(crate) fn after_successful_toolcall_commit(
        recorded_inside_hook: bool,
        raw_only_durable_without_emission: bool,
    ) -> Self {
        if recorded_inside_hook {
            Self::Skip
        } else if raw_only_durable_without_emission {
            Self::RawOnlyDurableWithoutEmission
        } else {
            Self::WithoutSpineObserve
        }
    }
}

pub(super) struct SpineToolcallCommitHostPlan {
    pre_compact_provider_input_tokens: Option<i64>,
    #[cfg(test)]
    output_recording: SpineToolOutputRecording,
    commit_missing_action: SpineToolcallCommitFailureAction,
    retry_limit_action: SpineToolcallCommitFailureAction,
    lock_retry_limit: usize,
}

pub(crate) struct SpineToolcallHostCommit {
    plan: SpineToolcallCommitHostPlan,
    evidence: SpineToolcallCommitEvidence,
    lock_retries: usize,
}

pub(crate) struct SpineToolcallHostCommitAttempt {
    evidence: SpineToolcallCommitEvidence,
    pre_compact_provider_input_tokens: Option<i64>,
    current_turn_provider_input_tokens: Option<i64>,
}

pub(crate) struct SpineToolcallHostAttempt {
    kind: SpineCommitAttemptKind,
}

enum SpineToolcallCommitHostStep {
    Done(SpineHostEffects),
    Retry,
    NoSpineCommit,
    FailClosed {
        reason: &'static str,
        error: SpineError,
    },
    AbortPending {
        reason: &'static str,
        error: SpineError,
    },
}

const SPINE_TOOLCALL_COMMIT_LOCK_RETRY_LIMIT: usize = 4096;
const SPINE_TOOLCALL_COMMIT_RUNTIME_MISSING_REASON: &str =
    "spine runtime missing during completed toolcall commit";
const SPINE_TOOLCALL_COMMIT_RETRY_LIMIT_REASON: &str =
    "spine tool output commit lock retry limit exceeded before commit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineToolcallCommitFailureAction {
    FailClosed,
    AbortPending,
    NoSpineCommit,
}

impl SpineToolcallCommitHostPlan {
    pub(super) fn new(
        requires_close_like_commit: bool,
        current_turn_provider_input_tokens: Option<i64>,
        tool_resp_already_recorded: bool,
        recorded_inside_hook: bool,
    ) -> Self {
        #[cfg(not(test))]
        let _ = recorded_inside_hook;
        #[cfg(test)]
        let raw_only_durable_without_emission =
            requires_close_like_commit && !tool_resp_already_recorded && !recorded_inside_hook;
        SpineToolcallCommitHostPlan {
            pre_compact_provider_input_tokens: if requires_close_like_commit {
                current_turn_provider_input_tokens
            } else {
                None
            },
            #[cfg(test)]
            output_recording: SpineToolOutputRecording::after_successful_toolcall_commit(
                recorded_inside_hook,
                raw_only_durable_without_emission,
            ),
            commit_missing_action: if tool_resp_already_recorded {
                SpineToolcallCommitFailureAction::FailClosed
            } else {
                SpineToolcallCommitFailureAction::NoSpineCommit
            },
            retry_limit_action: if tool_resp_already_recorded {
                SpineToolcallCommitFailureAction::FailClosed
            } else {
                SpineToolcallCommitFailureAction::AbortPending
            },
            lock_retry_limit: SPINE_TOOLCALL_COMMIT_LOCK_RETRY_LIMIT,
        }
    }

    pub(super) fn into_host_commit(
        self,
        evidence: SpineToolcallCommitEvidence,
    ) -> SpineToolcallHostCommit {
        SpineToolcallHostCommit {
            plan: self,
            evidence,
            lock_retries: 0,
        }
    }

    fn host_outcome(
        &self,
        post_commit_effects: SpineHostEffects,
    ) -> SpineCompletedToolCallHostOutcome {
        SpineCompletedToolCallHostOutcome {
            #[cfg(test)]
            recording: self.output_recording,
            post_commit_effects,
            deferred_tree_update: None,
        }
    }

    fn interpret_attempt(
        &self,
        attempt: SpineToolcallHostAttempt,
        lock_retries: usize,
        call_id: &str,
    ) -> Result<SpineToolcallCommitHostStep, SpineError> {
        match attempt.kind {
            SpineCommitAttemptKind::Done(output) => Ok(SpineToolcallCommitHostStep::Done(output)),
            SpineCommitAttemptKind::RuntimeMissing => self.commit_missing_decision(call_id),
            SpineCommitAttemptKind::Retry => self.retry_decision(lock_retries, call_id),
        }
    }

    fn commit_missing_decision(
        &self,
        call_id: &str,
    ) -> Result<SpineToolcallCommitHostStep, SpineError> {
        match self.commit_missing_action {
            SpineToolcallCommitFailureAction::FailClosed => {
                Ok(SpineToolcallCommitHostStep::FailClosed {
                    reason: SPINE_TOOLCALL_COMMIT_RUNTIME_MISSING_REASON,
                    error: SpineError::Invariant(format!(
                        "{SPINE_TOOLCALL_COMMIT_RUNTIME_MISSING_REASON} for call_id={call_id}"
                    )),
                })
            }
            SpineToolcallCommitFailureAction::NoSpineCommit => {
                Ok(SpineToolcallCommitHostStep::NoSpineCommit)
            }
            SpineToolcallCommitFailureAction::AbortPending => Err(SpineError::Invariant(format!(
                "unsupported Spine runtime-missing action for call_id={call_id}"
            ))),
        }
    }

    fn retry_decision(
        &self,
        lock_retries: usize,
        call_id: &str,
    ) -> Result<SpineToolcallCommitHostStep, SpineError> {
        if lock_retries < self.lock_retry_limit {
            return Ok(SpineToolcallCommitHostStep::Retry);
        }
        match self.retry_limit_action {
            SpineToolcallCommitFailureAction::FailClosed => {
                Ok(SpineToolcallCommitHostStep::FailClosed {
                    reason: SPINE_TOOLCALL_COMMIT_RETRY_LIMIT_REASON,
                    error: self.retry_limit_error(call_id),
                })
            }
            SpineToolcallCommitFailureAction::AbortPending => {
                Ok(SpineToolcallCommitHostStep::AbortPending {
                    reason: SPINE_TOOLCALL_COMMIT_RETRY_LIMIT_REASON,
                    error: self.retry_limit_error(call_id),
                })
            }
            SpineToolcallCommitFailureAction::NoSpineCommit => Err(SpineError::Invariant(format!(
                "unsupported Spine retry-limit action for call_id={call_id}"
            ))),
        }
    }

    fn retry_limit_error(&self, call_id: &str) -> SpineError {
        SpineError::Operation(format!(
            "spine tool output commit could not acquire session locks after {} retries for call_id={call_id}",
            self.lock_retry_limit
        ))
    }
}

impl SpineToolcallHostAttempt {
    pub(in crate::spine) fn host_lock_busy() -> Self {
        Self {
            kind: SpineCommitAttemptKind::Retry,
        }
    }

    pub(super) fn from_commit_attempt(attempt: SpineCommitAttempt) -> Self {
        Self { kind: attempt.kind }
    }
}

impl SpineToolcallHostCommit {
    pub(in crate::spine) fn host_outcome(
        &self,
        post_commit_effects: SpineHostEffects,
    ) -> SpineCompletedToolCallHostOutcome {
        self.plan.host_outcome(post_commit_effects)
    }

    fn attempt_input(
        &self,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> SpineToolcallHostCommitAttempt {
        SpineToolcallHostCommitAttempt {
            evidence: self.evidence.clone(),
            pre_compact_provider_input_tokens: self.plan.pre_compact_provider_input_tokens,
            current_turn_provider_input_tokens,
        }
    }

    fn interpret_attempt_for_host(
        &mut self,
        attempt: SpineToolcallHostAttempt,
        call_id: &str,
    ) -> Result<SpineToolcallCommitHostStep, SpineError> {
        let step = self
            .plan
            .interpret_attempt(attempt, self.lock_retries, call_id)?;
        if matches!(step, SpineToolcallCommitHostStep::Retry) {
            self.lock_retries += 1;
        }
        Ok(step)
    }

    pub(crate) async fn run_host_commit_loop<
        AttemptOnce,
        AttemptOnceFuture,
        YieldRetry,
        YieldRetryFuture,
        FailClosed,
        FailClosedFuture,
        AbortPending,
        AbortPendingFuture,
    >(
        &mut self,
        call_id: &str,
        current_turn_provider_input_tokens: Option<i64>,
        mut attempt_once: AttemptOnce,
        mut yield_retry: YieldRetry,
        mut fail_closed: FailClosed,
        mut abort_pending: AbortPending,
    ) -> Result<Option<SpineHostEffects>, SpineError>
    where
        AttemptOnce: FnMut(SpineToolcallHostCommitAttempt) -> AttemptOnceFuture,
        AttemptOnceFuture: Future<Output = Result<SpineToolcallHostAttempt, SpineError>>,
        YieldRetry: FnMut() -> YieldRetryFuture,
        YieldRetryFuture: Future<Output = ()>,
        FailClosed: FnMut(&'static str) -> FailClosedFuture,
        FailClosedFuture: Future<Output = ()>,
        AbortPending: FnMut(&'static str) -> AbortPendingFuture,
        AbortPendingFuture: Future<Output = ()>,
    {
        loop {
            let attempt_input = self.attempt_input(current_turn_provider_input_tokens);
            let attempt = attempt_once(attempt_input).await?;
            match self.interpret_attempt_for_host(attempt, call_id)? {
                SpineToolcallCommitHostStep::Done(effects) => return Ok(Some(effects)),
                SpineToolcallCommitHostStep::Retry => {
                    yield_retry().await;
                }
                SpineToolcallCommitHostStep::NoSpineCommit => return Ok(None),
                SpineToolcallCommitHostStep::FailClosed { reason, error } => {
                    fail_closed(reason).await;
                    return Err(error);
                }
                SpineToolcallCommitHostStep::AbortPending { reason, error } => {
                    abort_pending(reason).await;
                    return Err(error);
                }
            }
        }
    }
}

impl SpineToolcallHostCommitAttempt {
    pub(in crate::spine) fn into_commit_evidence(self) -> SpineToolcallCommitEvidence {
        self.evidence
    }

    pub(in crate::spine) fn pre_compact_provider_input_tokens(&self) -> Option<i64> {
        self.pre_compact_provider_input_tokens
    }

    pub(in crate::spine) fn current_turn_provider_input_tokens(&self) -> Option<i64> {
        self.current_turn_provider_input_tokens
    }
}

impl SpineCompletedToolCallHostOutcome {
    pub(in crate::spine) fn no_spine_commit() -> Self {
        Self {
            #[cfg(test)]
            recording: SpineToolOutputRecording::Normal,
            post_commit_effects: SpineHostEffects::none(),
            deferred_tree_update: None,
        }
    }

    pub(in crate::spine) fn take_post_commit_effects(&mut self) -> SpineHostEffects {
        std::mem::replace(&mut self.post_commit_effects, SpineHostEffects::none())
    }

    pub(in crate::spine) fn set_deferred_tree_update(
        &mut self,
        deferred_tree_update: Option<SpineTreeUpdateEvent>,
    ) {
        self.deferred_tree_update = deferred_tree_update;
    }

    pub(in crate::spine) fn take_deferred_tree_update(&mut self) -> Option<SpineTreeUpdateEvent> {
        self.deferred_tree_update.take()
    }

    #[cfg(test)]
    pub(in crate::spine) fn into_test_parts(
        self,
    ) -> (SpineToolOutputRecording, Option<SpineTreeUpdateEvent>) {
        (self.recording, self.deferred_tree_update)
    }
}
