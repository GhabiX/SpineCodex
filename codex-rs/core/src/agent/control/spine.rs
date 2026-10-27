use std::time::Duration;

use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;

use super::AgentControl;

impl AgentControl {
    pub(crate) async fn wait_for_spine_spawn_turn_idle(
        &self,
        thread_id: ThreadId,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        let thread = state.get_thread(thread_id).await?;
        loop {
            if thread.session.active_turn.lock().await.is_none() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// Sends a continuation after the caller has atomically reserved a Spine execution slot.
    pub(crate) async fn send_spine_spawn_continuation(
        &self,
        thread_id: ThreadId,
        agent_path: &AgentPath,
        input: Vec<UserInput>,
        start_options: TurnStartOptions,
    ) -> CodexResult<String> {
        let _state = match self.upgrade() {
            Ok(state) => state,
            Err(error) => {
                self.release_execution_reservation(agent_path);
                return Err(error);
            }
        };
        let result = self.send_input(thread_id, input, start_options).await;
        if result.is_err() {
            self.release_execution_reservation(agent_path);
        }
        result
    }
}
