use codex_protocol::models::ResponseItem;
use std::path::Path;

use crate::spine::SpineError;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::compact_checkpoint::build_compact_checkpoint;
use crate::spine::model::TrimProjection;
use crate::spine::parse_stack::ParseStack;

use super::publication::ParserRootCompactPublication;
use super::publication::full_variable_context_publication_update_from_state;
use super::publication::root_compact_checkpoint_publication_proof;

pub(in crate::spine) struct ParserRootCompactPreparedTxn {
    publication: ParserRootCompactPublication,
    prepared_install: ParserRootCompactPreparedInstall,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserRootCompactPreparedCommitInstall {
    pending_install: ParserRootCompactPendingInstall,
    final_install: ParserRootCompactInstall,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserObserveInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserOpenInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserCommitInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserCommitPreparedInstall {
    pending_install: ParserCommitPendingInstall,
    final_install: ParserCommitInstall,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserCommitPendingInstall {
    pending_state: ParserPreparedState,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserRootCompactInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserRootCompactPreparedInstall {
    pending_install: ParserRootCompactPendingInstall,
    final_install: ParserRootCompactInstall,
}

#[derive(Debug)]
pub(in crate::spine) struct ParserRootCompactPendingInstall {
    pending_state: ParserPreparedState,
}

impl ParserRootCompactPreparedTxn {
    pub(super) fn new(
        publication: ParserRootCompactPublication,
        prepared_install: ParserRootCompactPreparedInstall,
    ) -> Self {
        Self {
            publication,
            prepared_install,
        }
    }

    pub(in crate::spine) fn validate_current_open_matches_variable_context_len(
        &self,
    ) -> Result<(), SpineError> {
        self.publication
            .validate_current_open_matches_variable_context_len()
    }

    pub(in crate::spine) fn build_compact_checkpoint(
        &self,
        rollout_path: &Path,
        raw_boundary: u64,
        token_seq: u64,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineCompactCheckpoint, SpineError> {
        let proof = self.checkpoint_publication_proof();
        build_compact_checkpoint(
            rollout_path,
            raw_boundary,
            token_seq,
            raw_live,
            raw_items,
            proof.parse_stack(),
            proof.variable_context(),
            proof.variable_context(),
        )
    }

    fn checkpoint_publication_proof(
        &self,
    ) -> super::publication::ParserRootCompactCheckpointProof<'_> {
        root_compact_checkpoint_publication_proof(
            self.prepared_install.final_state().parse_stack(),
            &self.publication,
        )
    }

    pub(in crate::spine) fn consume_variable_context_and_install<T>(
        self,
        consume: impl FnOnce(Vec<ResponseItem>, ParserRootCompactPreparedCommitInstall) -> T,
    ) -> T {
        consume(
            self.publication.into_variable_context(),
            self.prepared_install.into_prepared_commit_install(),
        )
    }
}

impl ParserRootCompactPreparedCommitInstall {
    fn new(
        pending_install: ParserRootCompactPendingInstall,
        final_install: ParserRootCompactInstall,
    ) -> Self {
        Self {
            pending_install,
            final_install,
        }
    }

    pub(super) fn pending_state(&self) -> &ParserPreparedState {
        self.pending_install.pending_state()
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_install.into_final_state()
    }
}

impl ParserObserveInstall {
    pub(super) fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }
}

impl ParserCommitInstall {
    pub(super) fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }

    pub(in crate::spine) fn full_variable_context_publication_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        self.final_state.full_variable_context_publication_update(
            call_id,
            operation,
            raw_items,
            trim_projection,
            history_items,
            build_update,
        )
    }
}

impl ParserCommitPendingInstall {
    pub(super) fn new(pending_state: ParserPreparedState) -> Self {
        Self { pending_state }
    }

    pub(super) fn pending_state(&self) -> &ParserPreparedState {
        &self.pending_state
    }
}

impl ParserCommitPreparedInstall {
    pub(super) fn new(
        pending_install: ParserCommitPendingInstall,
        final_install: ParserCommitInstall,
    ) -> Self {
        Self {
            pending_install,
            final_install,
        }
    }

    pub(super) fn pending_state(&self) -> &ParserPreparedState {
        self.pending_install.pending_state()
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_install.into_final_state()
    }

    pub(in crate::spine) fn full_variable_context_publication_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        self.final_install.full_variable_context_publication_update(
            call_id,
            operation,
            raw_items,
            trim_projection,
            history_items,
            build_update,
        )
    }
}

impl ParserOpenInstall {
    pub(super) fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }

    pub(in crate::spine) fn into_commit_install(self) -> ParserCommitInstall {
        ParserCommitInstall::new(self.final_state)
    }
}

impl ParserRootCompactInstall {
    pub(super) fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    pub(super) fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }
}

impl ParserRootCompactPreparedInstall {
    pub(super) fn new(
        pending_install: ParserRootCompactPendingInstall,
        final_install: ParserRootCompactInstall,
    ) -> Self {
        Self {
            pending_install,
            final_install,
        }
    }

    fn final_state(&self) -> &ParserPreparedState {
        &self.final_install.final_state
    }

    fn into_prepared_commit_install(self) -> ParserRootCompactPreparedCommitInstall {
        ParserRootCompactPreparedCommitInstall::new(self.pending_install, self.final_install)
    }
}

impl ParserRootCompactPendingInstall {
    pub(super) fn new(pending_state: ParserPreparedState) -> Self {
        Self { pending_state }
    }

    pub(super) fn pending_state(&self) -> &ParserPreparedState {
        &self.pending_state
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParserPreparedState {
    parse_stack: ParseStack,
}

impl ParserPreparedState {
    pub(super) fn new(parse_stack: ParseStack) -> Self {
        Self { parse_stack }
    }

    pub(super) fn parse_stack(&self) -> &ParseStack {
        &self.parse_stack
    }

    fn full_variable_context_publication_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        full_variable_context_publication_update_from_state(
            self.parse_stack(),
            call_id,
            operation,
            raw_items,
            trim_projection,
            history_items,
            build_update,
        )
    }

    pub(super) fn into_parse_stack_for_install(self) -> ParseStack {
        self.parse_stack
    }
}
