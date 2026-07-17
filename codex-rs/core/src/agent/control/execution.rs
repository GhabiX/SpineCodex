use super::AgentControl;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

#[derive(Default)]
pub(super) struct AgentExecutionLimiter {
    state: Mutex<AgentExecutionState>,
    max_threads: OnceLock<usize>,
}

#[derive(Default)]
struct AgentExecutionState {
    active: usize,
    pending: usize,
    reserved_threads: HashSet<ThreadId>,
}

pub(crate) struct AgentExecutionGuard {
    limiter: Arc<AgentExecutionLimiter>,
}

pub(crate) struct AgentExecutionReservation {
    limiter: Arc<AgentExecutionLimiter>,
    active: bool,
}

impl Drop for AgentExecutionGuard {
    fn drop(&mut self) {
        let mut state = self
            .limiter
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
    }
}

impl AgentExecutionReservation {
    pub(crate) fn commit(mut self, thread_id: ThreadId) {
        let mut state = self
            .limiter
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending = state.pending.saturating_sub(1);
        state.active += 1;
        state.reserved_threads.insert(thread_id);
        self.active = false;
    }
}

impl Drop for AgentExecutionReservation {
    fn drop(&mut self) {
        if self.active {
            let mut state = self
                .limiter
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending = state.pending.saturating_sub(1);
        }
    }
}

impl AgentControl {
    pub(crate) async fn ensure_execution_capacity_for_op(
        &self,
        thread_id: ThreadId,
        op: &Op,
    ) -> CodexResult<()> {
        self.ensure_execution_capacity_for_turn_start(thread_id, op_starts_turn(op))
            .await
    }

    pub(super) async fn ensure_execution_capacity_for_turn_start(
        &self,
        thread_id: ThreadId,
        starts_turn: bool,
    ) -> CodexResult<()> {
        if !starts_turn {
            return Ok(());
        }
        let state = self.upgrade()?;
        let thread = state.get_thread(thread_id).await?;
        if thread.codex.session.active_turn.lock().await.is_some() {
            return Ok(());
        }
        let config = thread.codex.session.get_config().await;
        let multi_agent_version = thread
            .multi_agent_version()
            .unwrap_or_else(|| config.multi_agent_version_from_features());
        self.ensure_execution_capacity(multi_agent_version, &thread.session_source)
    }

    pub(crate) fn ensure_execution_capacity(
        &self,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> CodexResult<()> {
        if !is_execution_limited(multi_agent_version, session_source) {
            return Ok(());
        }
        let max_threads = self.agent_execution_limiter.max_threads();
        if self.agent_execution_limiter.has_capacity() {
            Ok(())
        } else {
            Err(CodexErr::AgentLimitReached { max_threads })
        }
    }

    pub(crate) fn reserve_execution_slots(
        &self,
        count: usize,
    ) -> CodexResult<Vec<AgentExecutionReservation>> {
        Arc::clone(&self.agent_execution_limiter).reserve(count)
    }

    pub(crate) fn execution_guard(
        &self,
        thread_id: ThreadId,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> Option<AgentExecutionGuard> {
        is_execution_limited(multi_agent_version, session_source).then(|| {
            let limiter = Arc::clone(&self.agent_execution_limiter);
            if limiter.claim(thread_id) {
                AgentExecutionGuard { limiter }
            } else {
                limiter.guard()
            }
        })
    }

    pub(crate) fn release_execution_reservation(&self, thread_id: ThreadId) {
        self.agent_execution_limiter.release_reserved(thread_id);
    }
}

impl AgentExecutionLimiter {
    pub(super) fn initialize(&self, max_threads: usize) {
        self.max_threads.get_or_init(|| max_threads);
    }

    fn max_threads(&self) -> usize {
        self.max_threads.get().copied().unwrap_or(usize::MAX)
    }

    fn has_capacity(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.saturating_add(state.pending) < self.max_threads()
    }

    fn reserve(self: Arc<Self>, count: usize) -> CodexResult<Vec<AgentExecutionReservation>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let max_threads = self.max_threads();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .active
            .saturating_add(state.pending)
            .saturating_add(count)
            > max_threads
        {
            return Err(CodexErr::AgentLimitReached { max_threads });
        }
        state.pending += count;
        drop(state);
        Ok((0..count)
            .map(|_| AgentExecutionReservation {
                limiter: Arc::clone(&self),
                active: true,
            })
            .collect())
    }

    fn claim(&self, thread_id: ThreadId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reserved_threads
            .remove(&thread_id)
    }

    fn release_reserved(&self, thread_id: ThreadId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reserved_threads.remove(&thread_id) {
            state.active = state.active.saturating_sub(1);
        }
    }

    fn guard(self: Arc<Self>) -> AgentExecutionGuard {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active += 1;
        AgentExecutionGuard { limiter: self }
    }
}

fn op_starts_turn(op: &Op) -> bool {
    matches!(op, Op::UserInput { .. })
        || matches!(op, Op::InterAgentCommunication { communication } if communication.trigger_turn)
}

fn is_execution_limited(
    multi_agent_version: MultiAgentVersion,
    session_source: &SessionSource,
) -> bool {
    multi_agent_version == MultiAgentVersion::V2
        && matches!(session_source, SessionSource::SubAgent(_))
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
