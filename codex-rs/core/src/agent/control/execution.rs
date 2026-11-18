//! Tracks local running capacity and releases reservations through the shared guard.
//! The local permit owns the running count; root and MAv1 turns remain unrestricted.

use super::LocalAgentControl;
use crate::agent::types::AgentExecutionGuard;
use codex_protocol::AgentPath;
use codex_protocol::error::AgentErrorContext;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::MultiAgentVersion;
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
    reserved_agent_paths: HashSet<String>,
}

struct LocalExecutionPermit {
    limiter: Arc<AgentExecutionLimiter>,
}

pub(crate) struct AgentExecutionReservation {
    limiter: Arc<AgentExecutionLimiter>,
    active: bool,
}

impl Drop for LocalExecutionPermit {
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
    pub(crate) fn commit(mut self, agent_path: &AgentPath) {
        let mut state = self
            .limiter
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending = state.pending.saturating_sub(1);
        state.active += 1;
        state
            .reserved_agent_paths
            .insert(agent_path.as_str().to_string());
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

impl LocalAgentControl {
    pub(crate) fn ensure_execution_capacity(
        &self,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> CodexResult<()> {
        if !is_execution_limited(multi_agent_version, session_source) {
            return Ok(());
        }
        let max_threads = self.runtime.agent_execution_limiter.max_threads();
        if self.runtime.agent_execution_limiter.has_capacity() {
            Ok(())
        } else {
            Err(
                CodexErr::new(CodexErrorDetails::AgentLimitReached { max_threads })
                    .with_agent_context(AgentErrorContext::ExecutionCapacity),
            )
        }
    }

    pub(crate) fn execution_guard(
        &self,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> Option<AgentExecutionGuard> {
        if let Some(agent_path) = session_source.get_agent_path() {
            let limiter = Arc::clone(&self.runtime.spine_spawn_limiter);
            if limiter.claim(&agent_path) {
                return Some(AgentExecutionGuard::new(LocalExecutionPermit { limiter }));
            }
        }
        is_execution_limited(multi_agent_version, session_source)
            .then(|| Arc::clone(&self.runtime.agent_execution_limiter).guard())
    }

    pub(crate) fn reserve_spine_spawn_slots(
        &self,
        count: usize,
    ) -> CodexResult<Vec<AgentExecutionReservation>> {
        Arc::clone(&self.runtime.spine_spawn_limiter).reserve(count)
    }

    pub(crate) fn release_execution_reservation(&self, agent_path: &AgentPath) {
        self.runtime
            .spine_spawn_limiter
            .release_reserved(agent_path);
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
            return Err(CodexErr::new(CodexErrorDetails::AgentLimitReached {
                max_threads,
            }));
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

    fn claim(&self, agent_path: &AgentPath) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reserved_agent_paths
            .remove(agent_path.as_str())
    }

    fn release_reserved(&self, agent_path: &AgentPath) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reserved_agent_paths.remove(agent_path.as_str()) {
            state.active = state.active.saturating_sub(1);
        }
    }

    fn guard(self: Arc<Self>) -> AgentExecutionGuard {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active += 1;
        AgentExecutionGuard::new(LocalExecutionPermit { limiter: self })
    }
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
