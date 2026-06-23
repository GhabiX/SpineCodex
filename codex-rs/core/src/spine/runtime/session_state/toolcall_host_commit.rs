use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::super::SpineError;
use super::super::SpineHostEffects;
use super::SpineCommitAttempt;
use super::SpineCommitAttemptKind;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SpineToolcallCommitPreparation {
    requires_close_like_commit: bool,
}

pub(crate) struct SpineToolcallCommitHostPlan {
    pre_compact_provider_input_tokens: Option<i64>,
    #[cfg(test)]
    output_recording: SpineToolOutputRecording,
    commit_missing_action: SpineToolcallCommitFailureAction,
    retry_limit_action: SpineToolcallCommitFailureAction,
    lock_retry_limit: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct SpineToolcallCommitProviderInputTokens {
    pre_compact: Option<i64>,
    current_turn: Option<i64>,
}

pub(crate) struct SpineToolcallHostCommit {
    plan: SpineToolcallCommitHostPlan,
    lock_retries: usize,
}

pub(crate) enum SpineToolcallCommitHostStep {
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

impl SpineToolcallCommitPreparation {
    pub(super) fn new(requires_close_like_commit: bool) -> Self {
        Self {
            requires_close_like_commit,
        }
    }

    pub(super) fn host_plan(
        self,
        current_turn_provider_input_tokens: Option<i64>,
        tool_resp_already_recorded: bool,
        recorded_inside_hook: bool,
    ) -> SpineToolcallCommitHostPlan {
        #[cfg(not(test))]
        let _ = recorded_inside_hook;
        #[cfg(test)]
        let raw_only_durable_without_emission =
            self.requires_close_like_commit && !tool_resp_already_recorded && !recorded_inside_hook;
        SpineToolcallCommitHostPlan {
            pre_compact_provider_input_tokens: if self.requires_close_like_commit {
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
}

impl SpineToolcallCommitHostPlan {
    pub(crate) fn into_host_commit(self) -> SpineToolcallHostCommit {
        SpineToolcallHostCommit {
            plan: self,
            lock_retries: 0,
        }
    }

    pub(crate) fn host_outcome(
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

    pub(crate) fn provider_input_tokens(
        &self,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> SpineToolcallCommitProviderInputTokens {
        SpineToolcallCommitProviderInputTokens {
            pre_compact: self.pre_compact_provider_input_tokens,
            current_turn: current_turn_provider_input_tokens,
        }
    }

    fn interpret_attempt(
        &self,
        attempt: SpineCommitAttempt,
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
            SpineToolcallCommitFailureAction::FailClosed => Ok(fail_closed_host_step(
                SPINE_TOOLCALL_COMMIT_RUNTIME_MISSING_REASON,
                SpineError::Invariant(format!(
                    "{SPINE_TOOLCALL_COMMIT_RUNTIME_MISSING_REASON} for call_id={call_id}"
                )),
            )),
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
            action @ (SpineToolcallCommitFailureAction::FailClosed
            | SpineToolcallCommitFailureAction::AbortPending) => Ok(host_step_from_failure_action(
                action,
                SPINE_TOOLCALL_COMMIT_RETRY_LIMIT_REASON,
                self.retry_limit_error(call_id),
            )),
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

impl SpineToolcallHostCommit {
    pub(crate) fn host_outcome(
        &self,
        post_commit_effects: SpineHostEffects,
    ) -> SpineCompletedToolCallHostOutcome {
        self.plan.host_outcome(post_commit_effects)
    }

    pub(crate) fn provider_input_tokens(
        &self,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> SpineToolcallCommitProviderInputTokens {
        self.plan
            .provider_input_tokens(current_turn_provider_input_tokens)
    }

    pub(crate) fn interpret_attempt_for_host(
        &mut self,
        attempt: SpineCommitAttempt,
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
        mut attempt_once: AttemptOnce,
        mut yield_retry: YieldRetry,
        mut fail_closed: FailClosed,
        mut abort_pending: AbortPending,
    ) -> Result<Option<SpineHostEffects>, SpineError>
    where
        AttemptOnce: FnMut() -> AttemptOnceFuture,
        AttemptOnceFuture: Future<Output = Result<SpineCommitAttempt, SpineError>>,
        YieldRetry: FnMut() -> YieldRetryFuture,
        YieldRetryFuture: Future<Output = ()>,
        FailClosed: FnMut(&'static str) -> FailClosedFuture,
        FailClosedFuture: Future<Output = ()>,
        AbortPending: FnMut(&'static str) -> AbortPendingFuture,
        AbortPendingFuture: Future<Output = ()>,
    {
        loop {
            let attempt = attempt_once().await?;
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

impl SpineToolcallCommitProviderInputTokens {
    pub(crate) fn pre_compact(&self) -> Option<i64> {
        self.pre_compact
    }

    pub(crate) fn current_turn(&self) -> Option<i64> {
        self.current_turn
    }
}

impl SpineCompletedToolCallHostOutcome {
    pub(crate) fn no_spine_commit() -> Self {
        Self {
            #[cfg(test)]
            recording: SpineToolOutputRecording::Normal,
            post_commit_effects: SpineHostEffects::none(),
            deferred_tree_update: None,
        }
    }

    pub(crate) fn take_post_commit_effects(&mut self) -> SpineHostEffects {
        std::mem::replace(&mut self.post_commit_effects, SpineHostEffects::none())
    }

    pub(crate) fn set_deferred_tree_update(
        &mut self,
        deferred_tree_update: Option<SpineTreeUpdateEvent>,
    ) {
        self.deferred_tree_update = deferred_tree_update;
    }

    pub(crate) fn take_deferred_tree_update(&mut self) -> Option<SpineTreeUpdateEvent> {
        self.deferred_tree_update.take()
    }

    #[cfg(test)]
    pub(crate) fn into_test_parts(
        self,
    ) -> (SpineToolOutputRecording, Option<SpineTreeUpdateEvent>) {
        (self.recording, self.deferred_tree_update)
    }
}

fn host_step_from_failure_action(
    action: SpineToolcallCommitFailureAction,
    reason: &'static str,
    error: SpineError,
) -> SpineToolcallCommitHostStep {
    match action {
        SpineToolcallCommitFailureAction::FailClosed => fail_closed_host_step(reason, error),
        SpineToolcallCommitFailureAction::AbortPending => {
            SpineToolcallCommitHostStep::AbortPending { reason, error }
        }
        SpineToolcallCommitFailureAction::NoSpineCommit => {
            unreachable!("no-commit is not a terminal host action")
        }
    }
}

fn fail_closed_host_step(reason: &'static str, error: SpineError) -> SpineToolcallCommitHostStep {
    SpineToolcallCommitHostStep::FailClosed { reason, error }
}
