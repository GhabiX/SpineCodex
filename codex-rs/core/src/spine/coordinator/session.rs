use super::*;
use crate::spine::observer::CodexSpineObserverHandler;
pub(crate) struct SpineSamplingAttemptGuard {
    session: Arc<Session>,
    pub(crate) attempt: Option<SpineSamplingAttempt>,
}

pub(crate) struct SpineExecutionGuard {
    session: Arc<Session>,
    key: Option<String>,
}

pub(crate) struct SpineSessionAdapter {
    pub(crate) coordinator: SharedSpineCoordinator,
    pub(crate) sampling_active: tokio::sync::watch::Sender<bool>,
}

impl SpineSamplingAttemptGuard {
    fn take(&mut self) -> SpineSamplingAttempt {
        self.attempt
            .take()
            .expect("Spine sampling attempt must be present")
    }
}

impl Drop for SpineSamplingAttemptGuard {
    fn drop(&mut self) {
        if let Some(attempt) = self.attempt.as_ref() {
            self.session.try_spine("abort sampling", |coordinator| {
                coordinator.abort_sampling(attempt)
            });
        }
        self.session.spine.sampling_active.send_replace(false);
    }
}

impl SpineExecutionGuard {
    pub(crate) fn finish(mut self, succeeded: bool) {
        if let Some(key) = self.key.take() {
            self.session.finish_spine_execution(&key, succeeded);
        }
    }
}

impl Drop for SpineExecutionGuard {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.session.finish_spine_execution(&key, false);
        }
    }
}

impl SpineSessionAdapter {
    pub(crate) fn from_configuration(
        enabled: bool,
        session_id: String,
        config: SpineConfig,
    ) -> Result<Self, CoordinatorError> {
        Self::from_configuration_with_observer(
            enabled,
            session_id,
            config,
            CodexSpineObserverHandler::default(),
        )
    }

    pub(crate) fn from_configuration_with_observer(
        enabled: bool,
        session_id: String,
        config: SpineConfig,
        observer: CodexSpineObserverHandler,
    ) -> Result<Self, CoordinatorError> {
        let coordinator = enabled
            .then(|| CodexSpineCoordinator::new_with_observer(session_id, config, observer))
            .transpose()?;
        Ok(Self {
            coordinator: Arc::new(std::sync::Mutex::new(coordinator)),
            sampling_active: tokio::sync::watch::channel(false).0,
        })
    }
}

impl Session {
    pub(crate) fn lock_spine_coordinator(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<CodexSpineCoordinator>> {
        self.spine
            .coordinator
            .lock()
            .unwrap_or_else(|_| panic!("Spine coordinator mutex must not be poisoned"))
    }

    pub(crate) async fn begin_spine_sampling(
        self: &Arc<Self>,
        prompt: &[ResponseItem],
    ) -> anyhow::Result<Option<SpineSamplingAttemptGuard>> {
        if self.lock_spine_coordinator().is_none() {
            return Ok(None);
        }
        let attempt = {
            let mut coordinator = self.lock_spine_coordinator();
            let coordinator = coordinator
                .as_mut()
                .expect("Spine coordinator was checked before sampling");
            coordinator.begin_sampling()?
        };
        let guard = SpineSamplingAttemptGuard {
            session: Arc::clone(self),
            attempt: Some(attempt),
        };
        self.spine.sampling_active.send_replace(true);
        let item = {
            let mut coordinator = self.lock_spine_coordinator();
            coordinator
                .as_mut()
                .expect("Spine coordinator was checked before sampling")
                .sampling_started_rollout_item(
                    guard
                        .attempt
                        .as_ref()
                        .expect("Spine sampling attempt must be present"),
                    prompt,
                )?
        };
        self.persist_spine_rollout_items(&[item])
            .await
            .map_err(|error| self.latch_spine_error(error))?;
        Ok(Some(guard))
    }

    pub(crate) async fn wait_for_pending_spine_sampling(&self) {
        let should_wait = self
            .lock_spine_coordinator()
            .as_ref()
            .is_some_and(CodexSpineCoordinator::has_pending_durable_sampling);
        if !should_wait {
            return;
        }
        let mut sampling_active = self.spine.sampling_active.subscribe();
        while *sampling_active.borrow_and_update() {
            if sampling_active.changed().await.is_err() {
                break;
            }
        }
    }

    pub(crate) fn has_pending_spine_sampling(&self) -> bool {
        self.lock_spine_coordinator()
            .as_ref()
            .is_some_and(CodexSpineCoordinator::has_pending_durable_sampling)
    }

    #[cfg(test)]
    pub(crate) async fn finish_spine_sampling(
        &self,
        attempt: SpineSamplingAttemptGuard,
        terminal: spine_core::host::SamplingTerminal,
    ) -> anyhow::Result<()> {
        self.finish_spine_sampling_with_input_tokens(attempt, terminal, None)
            .await
    }

    pub(crate) async fn finish_spine_sampling_with_input_tokens(
        &self,
        mut attempt: SpineSamplingAttemptGuard,
        terminal: spine_core::host::SamplingTerminal,
        input_tokens: Option<i64>,
    ) -> anyhow::Result<()> {
        let model_context_window = self
            .token_usage_info()
            .await
            .and_then(|info| info.model_context_window);
        let commit = {
            let mut coordinator = self.lock_spine_coordinator();
            let coordinator = coordinator
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Spine coordinator is unavailable"))?;
            if let Some(model_context_window) = model_context_window {
                coordinator.record_context_window(model_context_window);
            }
            coordinator.finish_canonical_sampling_with_input_tokens(
                attempt.take(),
                terminal,
                input_tokens,
            )?
        };
        match commit {
            Some(commit) => self.persist_install_spine_canonical_commit(commit).await,
            None => Ok(()),
        }
    }

    pub(crate) async fn persist_install_spine_canonical_commit(
        &self,
        commit: CanonicalSamplingCommit,
    ) -> anyhow::Result<()> {
        let rollout_item = commit.rollout_item();
        self.persist_spine_rollout_items(std::slice::from_ref(&rollout_item))
            .await
            .map_err(|error| self.latch_spine_error(error))?;

        let installed = {
            let mut coordinator = self.lock_spine_coordinator();
            let coordinator = coordinator.as_mut().ok_or_else(|| {
                anyhow::anyhow!("Spine coordinator disappeared after persistence")
            })?;
            coordinator
                .install_canonical_sampling(commit)
                .map_err(|error| {
                    let reason = error.to_string();
                    coordinator.latch_durability_fault(reason.clone());
                    anyhow::anyhow!(
                        "Spine canonical install failed after durable acknowledgement: {reason}"
                    )
                })?
        };
        self.install_spine_model_context(installed.context.items.clone())
            .await;
        let mut coordinator = self.lock_spine_coordinator();
        let coordinator = coordinator
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Spine coordinator disappeared before publication"))?;
        coordinator.publish_canonical_sampling(&installed);
        Ok(())
    }

    async fn persist_spine_rollout_items(&self, items: &[RolloutItem]) -> anyhow::Result<()> {
        let Some(live_thread) = self.live_thread() else {
            return Ok(());
        };
        live_thread.append_items(items).await?;
        live_thread.flush().await?;
        Ok(())
    }

    pub(crate) fn begin_spine_execution(
        self: &Arc<Self>,
        name: &codex_tools::ToolName,
        key: &str,
    ) -> Option<SpineExecutionGuard> {
        let is_spine_tool = name.namespace.as_deref() == Some(spine_core::host::SPINE_NAMESPACE)
            && spine_core::host::SpineTool::all()
                .iter()
                .any(|tool| tool.name() == name.name);
        if !is_spine_tool
            || self
                .try_spine("register execution", |coordinator| {
                    coordinator.register_execution(key)
                })
                .is_none()
        {
            return None;
        }
        Some(SpineExecutionGuard {
            session: Arc::clone(self),
            key: Some(key.to_string()),
        })
    }

    pub(crate) fn stage_spine_fact(
        &self,
        key: &str,
        origin: spine_core::host::ExecutionOrigin,
        operation: SpineOperationFact,
    ) {
        self.try_spine("stage fact", |coordinator| {
            coordinator.stage_execution(key, origin, operation)
        });
    }

    fn finish_spine_execution(&self, key: &str, succeeded: bool) {
        self.try_spine("finish execution", |coordinator| {
            coordinator.finish_execution(key, succeeded)
        });
    }

    pub(crate) fn latch_spine_durability_fault(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.with_spine_coordinator(|coordinator| coordinator.latch_durability_fault(reason));
    }

    fn latch_spine_error(&self, error: anyhow::Error) -> anyhow::Error {
        self.latch_spine_durability_fault(error.to_string());
        error
    }

    fn with_spine_coordinator<R>(
        &self,
        f: impl FnOnce(&mut CodexSpineCoordinator) -> R,
    ) -> Option<R> {
        let mut coordinator = self.lock_spine_coordinator();
        coordinator.as_mut().map(f)
    }

    fn try_spine<R>(
        &self,
        action: &'static str,
        f: impl FnOnce(&mut CodexSpineCoordinator) -> Result<R, CoordinatorError>,
    ) -> Option<R> {
        self.with_spine_coordinator(f).and_then(|result| {
            result
                .inspect_err(|error| {
                    tracing::warn!(%error, action, "Spine coordinator operation failed");
                })
                .ok()
        })
    }
}
