use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::super::SpineError;
use super::super::SpineHostEffects;
use super::super::SpineRootCompactTokenMetadata;
use super::super::root_compact::spine_root_compact_body;
use super::SpineCompactEvidence;
use super::SpineRootCompactHostInstall;
use super::SpineSessionState;

impl SpineSessionState {
    pub(crate) fn apply_root_compact_after_history_publish(
        &mut self,
        prepared: SpineRootCompactHostInstall,
        published_variable_context_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        self.ensure_valid()?;
        let publication_variable_context_len = prepared.prepared.variable_context().len();
        if publication_variable_context_len != published_variable_context_len {
            return Err(SpineError::InvalidStore(format!(
                "spine root compact publication variable context length {publication_variable_context_len} does not match published variable context length {published_variable_context_len}"
            )));
        }
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before root compact PS install".to_string(),
            ));
        };
        runtime.install_prepared_root_compact(prepared.prepared);
        runtime.build_tree_snapshot()
    }

    pub(crate) fn take_pending_root_compact_after_history_publish(
        &mut self,
        published_variable_context_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        let prepared = self.pending_root_compact_install.take().ok_or_else(|| {
            SpineError::InvalidStore(
                "spine root compact publish missing prepared install".to_string(),
            )
        })?;
        self.apply_root_compact_after_history_publish(prepared, published_variable_context_len)
    }

    pub(crate) fn prepare_root_compact_commit_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpineRootCompactHostInstall, SpineError> {
        let prepared = {
            let runtime = self.runtime_mut_after_init()?;
            runtime.prepare_root_compact_with_checkpoint(
                rollout_path,
                body,
                raw_items,
                token_metadata,
            )
        };
        match prepared {
            Ok(prepared) => Ok(prepared),
            Err(err) => {
                if !err.should_invalidate_runtime() {
                    tracing::debug!(
                        error_class = ?err.class(),
                        "invalidating Spine runtime after root compact failure to preserve existing fail-closed behavior"
                    );
                }
                self.invalidate(format!(
                    "failed to install Spine root compact [{:?}]: {err}",
                    err.class()
                ));
                Err(err)
            }
        }
        .map(|prepared| SpineRootCompactHostInstall { prepared })
    }

    fn prepare_native_root_compact_apply_with_checkpoint_impl(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Result<SpineRootCompactHostInstall, SpineError> {
        let token_metadata = SpineRootCompactTokenMetadata {
            close_input_tokens: close_provider_input_tokens,
            close_context_tokens: close_provider_input_tokens,
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        };
        self.prepare_root_compact_commit_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_native_root_compact_apply_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Result<SpineRootCompactHostInstall, SpineError> {
        self.prepare_native_root_compact_apply_with_checkpoint_impl(
            rollout_path,
            body,
            raw_items,
            close_provider_input_tokens,
        )
    }

    pub(in crate::spine) fn prepare_native_root_compact_from_history_with_checkpoint(
        &mut self,
        evidence: SpineCompactEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        self.ensure_valid()?;
        if !self.is_ready() {
            return Ok(SpineHostEffects::none());
        }
        let body = spine_root_compact_body(evidence.compacted_history).ok_or_else(|| {
            SpineError::InvalidEvent(
                "native compact replaced host context with no model-visible Spine root memory material"
                    .to_string(),
            )
        })?;
        let install = self.prepare_native_root_compact_apply_with_checkpoint_impl(
            evidence.rollout_path,
            body,
            evidence.raw_items,
            evidence.close_provider_input_tokens,
        )?;
        let variable_context = install.prepared.variable_context().to_vec();
        self.pending_root_compact_install = Some(install);
        Ok(SpineHostEffects::root_compact_variable_context_publication(
            variable_context,
        ))
    }
}
