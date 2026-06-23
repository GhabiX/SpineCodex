use super::SpineRuntime;

mod completed_toolcall_evidence;
mod completed_toolcall_session;
mod lifecycle_session;
mod root_compact_session;
mod state_types;
mod toolcall_host_commit;
mod tree_session;
mod trim_session;

pub(crate) use completed_toolcall_evidence::SpineCompletedToolCallOutputEvidence;
pub(crate) use completed_toolcall_evidence::SpineToolCallEvidence;
pub(crate) use completed_toolcall_evidence::SpineToolcallCommitEvidence;
pub(crate) use completed_toolcall_evidence::SpineToolcallHookEvidence;
pub(crate) use completed_toolcall_session::SpineCommitAttempt;
use completed_toolcall_session::SpineCommitAttemptKind;
use state_types::CommittedSpineToolcall;
pub(crate) use state_types::PreparedSpineRootCompactCommit;
pub(crate) use state_types::SpineCompactEvidence;
pub(crate) use state_types::SpineGroupedToolcallOutputRecordingPlan;
pub(crate) use state_types::SpineInitEvidence;
pub(crate) use state_types::SpineMessageEvidence;
pub(crate) use state_types::SpineNativeCompactEvidence;
pub(crate) use state_types::SpineObservedContextItem;
pub(crate) use state_types::SpineRootCompactHostInstall;
pub(crate) use state_types::SpineSingleToolcallOutputRecordingPlan;
pub(crate) use toolcall_host_commit::SpineCompletedToolCallHostOutcome;
#[cfg(test)]
pub(crate) use toolcall_host_commit::SpineToolOutputRecording;
pub(crate) use toolcall_host_commit::SpineToolcallCommitHostPlan;
pub(crate) use toolcall_host_commit::SpineToolcallCommitProviderInputTokens;
pub(crate) use toolcall_host_commit::SpineToolcallHostCommit;

#[derive(Debug)]
pub(crate) struct SpineSessionState {
    pub(super) raw_len: u64,
    pub(super) runtime: Option<SpineRuntime>,
    pub(super) pending_root_compact_install: Option<SpineRootCompactHostInstall>,
    pub(super) jit_enabled: bool,
    pub(super) trim_enabled: bool,
    pub(super) initial_tree_snapshot_emitted: bool,
    pub(super) invalid: Option<String>,
}
