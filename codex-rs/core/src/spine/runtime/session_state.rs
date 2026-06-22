use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use super::CompletedToolCall;
use super::CompletedToolCallSegment;
use super::LiveRootCompact;
use super::SpineError;
use super::SpineHistoryUpdate;
use super::SpineHostEffects;
use super::SpineOpenNodeContextProjection;
use super::SpinePreparedRootCompactInstall;
#[cfg(test)]
use super::SpineRootCompactResult;
use super::SpineRootCompactTokenMetadata;
use super::SpineRuntime;
use super::SpineTreeUpdateDelivery;
use super::SpineTrimOutcome;
use super::prepared::SpineCommitPublication;
use super::support::is_real_user_message;
use super::support::tool_request_call_id;
use super::support::tool_response_call_id;
use super::types::SpinePreparedCloseMemory;
use crate::spine::model::NodeId;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::store::SpineCloneBoundary;
use crate::spine::store::SpineStore;

pub(crate) struct PreparedSpineToolcallCommit {
    publication: SpineCommitPublication<SpineHistoryUpdate>,
}

pub(crate) struct PreparedSpineReplayRuntime {
    pub(crate) runtime: Option<SpineRuntime>,
    pub(crate) materialized: Option<Vec<ResponseItem>>,
    pub(crate) live_root_compacts: Vec<LiveRootCompact>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineObservedContextItem<'a> {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpineContextObserveOutcome {
    pub(crate) observed_user_message: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineCompletedToolCallEvidence {
    completed_toolcall: CompletedToolCall,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineToolcallCommitEvidence {
    call_id: String,
    completed_toolcall: SpineCompletedToolCallEvidence,
}

pub(crate) struct SpineToolCallEvidence<'a> {
    kind: SpineToolCallEvidenceKind<'a>,
}

enum SpineToolCallEvidenceKind<'a> {
    Single {
        item: &'a ResponseItem,
    },
    Grouped {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    },
}

pub(crate) struct SpineCompletedToolCallOutputEvidence<'a> {
    call_id: &'a str,
    output_items: &'a [ResponseItem],
    commit_output_item: &'a ResponseItem,
    request_call_ids: SpineCompletedToolCallRequestIds<'a>,
    recording: SpineToolCallOutputHostRecording,
}

enum SpineCompletedToolCallRequestIds<'a> {
    Single(&'a str),
    Grouped(&'a [String]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineToolCallOutputHostRecording {
    MaybePreRecordSingle,
    RecordGroupBeforeCommit,
}

impl<'a> SpineToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            kind: SpineToolCallEvidenceKind::Single { item },
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            kind: SpineToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            },
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<SpineCompletedToolCallOutputEvidence<'a>>, SpineError> {
        match &self.kind {
            SpineToolCallEvidenceKind::Single { item } => {
                let Some(call_id) = tool_response_call_id(item) else {
                    return Ok(None);
                };
                Ok(Some(SpineCompletedToolCallOutputEvidence {
                    call_id,
                    output_items: std::slice::from_ref(*item),
                    commit_output_item: *item,
                    request_call_ids: SpineCompletedToolCallRequestIds::Single(call_id),
                    recording: SpineToolCallOutputHostRecording::MaybePreRecordSingle,
                }))
            }
            SpineToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } => {
                let commit_output_item =
                    validate_grouped_toolcall_outputs(commit_call_id, tool_call_ids, output_items)?;
                Ok(Some(SpineCompletedToolCallOutputEvidence {
                    call_id: *commit_call_id,
                    output_items: *output_items,
                    commit_output_item,
                    request_call_ids: SpineCompletedToolCallRequestIds::Grouped(*tool_call_ids),
                    recording: SpineToolCallOutputHostRecording::RecordGroupBeforeCommit,
                }))
            }
        }
    }
}

impl<'a> SpineCompletedToolCallOutputEvidence<'a> {
    pub(crate) fn call_id(&self) -> &'a str {
        self.call_id
    }

    pub(crate) fn commit_output_item(&self) -> &'a ResponseItem {
        self.commit_output_item
    }

    pub(crate) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        match self.recording {
            SpineToolCallOutputHostRecording::MaybePreRecordSingle => {
                Some((self.call_id, self.commit_output_item))
            }
            SpineToolCallOutputHostRecording::RecordGroupBeforeCommit => None,
        }
    }

    pub(crate) fn output_group_to_record_before_commit(&self) -> Option<&'a [ResponseItem]> {
        match self.recording {
            SpineToolCallOutputHostRecording::MaybePreRecordSingle => None,
            SpineToolCallOutputHostRecording::RecordGroupBeforeCommit => Some(self.output_items),
        }
    }
}

fn validate_grouped_toolcall_outputs<'a>(
    commit_call_id: &str,
    tool_call_ids: &[String],
    output_items: &'a [ResponseItem],
) -> Result<&'a ResponseItem, SpineError> {
    let expected_call_ids = tool_call_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut output_call_ids = BTreeSet::new();
    for item in output_items {
        let Some(call_id) = tool_response_call_id(item) else {
            return Err(SpineError::InvalidEvent(
                "grouped Spine toolcall output item is not a tool response".to_string(),
            ));
        };
        if !expected_call_ids.contains(call_id) {
            return Err(SpineError::InvalidEvent(format!(
                "grouped Spine toolcall unexpected output for call_id={call_id}"
            )));
        }
        output_call_ids.insert(call_id.to_string());
    }
    for call_id in tool_call_ids {
        if !output_call_ids.contains(call_id) {
            return Err(SpineError::InvalidEvent(format!(
                "grouped Spine toolcall missing output for call_id={call_id}"
            )));
        }
    }
    output_items
        .iter()
        .find(|item| tool_response_call_id(item) == Some(commit_call_id))
        .ok_or_else(|| {
            SpineError::InvalidEvent(format!(
                "grouped Spine toolcall missing output for commit call_id={commit_call_id}"
            ))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineToolOutputRecording {
    Skip,
    Normal,
    WithoutSpineObserve,
    RawOnlyDurableWithoutEmission,
}

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
pub(crate) struct SpineToolcallCommitPreparation {
    requires_close_like_commit: bool,
}

impl SpineToolcallCommitPreparation {
    fn new(requires_close_like_commit: bool) -> Self {
        Self {
            requires_close_like_commit,
        }
    }

    pub(crate) fn pre_compact_provider_input_tokens(
        &self,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> Option<i64> {
        if self.requires_close_like_commit {
            current_turn_provider_input_tokens
        } else {
            None
        }
    }

    pub(crate) fn output_recording_after_successful_commit(
        &self,
        tool_resp_already_recorded: bool,
        recorded_inside_hook: bool,
    ) -> SpineToolOutputRecording {
        let raw_only_durable_without_emission =
            self.requires_close_like_commit && !tool_resp_already_recorded && !recorded_inside_hook;
        SpineToolOutputRecording::after_successful_toolcall_commit(
            recorded_inside_hook,
            raw_only_durable_without_emission,
        )
    }
}

pub(crate) struct SpineToolcallCommitInput<'a> {
    pub(crate) call_id: &'a str,
    pub(crate) completed_toolcall: CompletedToolCall,
    pub(crate) tool_resp_item: &'a ResponseItem,
    pub(crate) tool_resp_already_recorded: bool,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) history_items: &'a [ResponseItem],
    pub(crate) expected_history: Vec<ResponseItem>,
    pub(crate) reference_context_item: Option<TurnContextItem>,
    pub(crate) pre_compact_provider_input_tokens: Option<i64>,
    pub(crate) current_turn_provider_input_tokens: Option<i64>,
}

struct CompletedToolCallEvidenceParts {
    call_id: String,
    request_call_ids: Vec<String>,
    request_segments: Vec<CompletedToolCallSegment>,
    response_segments: Vec<CompletedToolCallSegment>,
    missing_request_error: &'static str,
    missing_response_error: &'static str,
}

pub(crate) struct SpinePostApplyEffectPolicy {
    delivery: SpineTreeUpdateDelivery,
}

pub(crate) struct CommittedSpineToolcall {
    installed_commit: bool,
    post_apply_effect_policy: SpinePostApplyEffectPolicy,
}

impl SpineCompletedToolCallEvidence {
    fn new(completed_toolcall: CompletedToolCall) -> Self {
        Self { completed_toolcall }
    }

    fn first_segment_context_index(&self) -> Result<usize, SpineError> {
        self.completed_toolcall
            .segments
            .first()
            .map(|segment| segment.context_index)
            .ok_or_else(|| {
                SpineError::InvalidEvent("completed toolcall missing first segment".to_string())
            })
    }

    fn into_completed_toolcall(self) -> CompletedToolCall {
        self.completed_toolcall
    }
}

impl SpineToolcallCommitEvidence {
    fn new(call_id: impl Into<String>, completed_toolcall: SpineCompletedToolCallEvidence) -> Self {
        Self {
            call_id: call_id.into(),
            completed_toolcall,
        }
    }
}

impl PreparedSpineToolcallCommit {
    fn new(publication: SpineCommitPublication<SpineHistoryUpdate>) -> Self {
        Self { publication }
    }

    pub(crate) fn take_pre_apply_host_effects(&mut self) -> SpineHostEffects {
        SpineHostEffects::from_optional_history_update(self.publication.take_history_update())
    }

    pub(crate) fn post_apply_effect_policy(&self) -> SpinePostApplyEffectPolicy {
        let delivery = if self.publication.defer_tree_update_until_raw_output() {
            SpineTreeUpdateDelivery::AfterRawOutputDurable
        } else {
            SpineTreeUpdateDelivery::Immediate
        };
        SpinePostApplyEffectPolicy { delivery }
    }
}

impl PreparedSpineReplayRuntime {
    fn new(
        runtime: Option<SpineRuntime>,
        materialized: Option<Vec<ResponseItem>>,
        live_root_compacts: Vec<LiveRootCompact>,
    ) -> Self {
        Self {
            runtime,
            materialized,
            live_root_compacts,
        }
    }
}

impl SpinePostApplyEffectPolicy {
    pub(crate) fn host_effects(self, snapshot: Option<SpineTreeUpdateEvent>) -> SpineHostEffects {
        SpineHostEffects::from_optional_tree_update(snapshot, self.delivery)
    }
}

impl CommittedSpineToolcall {
    pub(crate) fn installed_commit(&self) -> bool {
        self.installed_commit
    }

    pub(crate) fn post_apply_host_effects(
        self,
        snapshot: Option<SpineTreeUpdateEvent>,
    ) -> SpineHostEffects {
        self.post_apply_effect_policy.host_effects(snapshot)
    }
}

pub(crate) struct PreparedSpineRootCompactCommit {
    install: SpinePreparedRootCompactInstall,
}

pub(crate) struct PreparedSpineRootCompactApply {
    commit: PreparedSpineRootCompactCommit,
}

impl PreparedSpineRootCompactCommit {
    pub(crate) fn from_install(install: SpinePreparedRootCompactInstall) -> Self {
        Self { install }
    }

    pub(crate) fn materialized(&self) -> &[ResponseItem] {
        &self.install.result().materialized
    }

    #[cfg(test)]
    pub(crate) fn result(&self) -> SpineRootCompactResult {
        self.install.result().clone()
    }
}

impl PreparedSpineRootCompactApply {
    fn new(commit: PreparedSpineRootCompactCommit) -> Self {
        Self { commit }
    }

    pub(crate) fn materialized(&self) -> &[ResponseItem] {
        self.commit.materialized()
    }

    #[cfg(test)]
    pub(crate) fn result(&self) -> SpineRootCompactResult {
        self.commit.result()
    }
}

fn completed_toolcall_request_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Request,
        raw_ordinal,
        context_index,
    }
}

fn completed_toolcall_response_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Response,
        raw_ordinal,
        context_index,
    }
}

fn completed_toolcall_request_segments(
    request_anchors: impl IntoIterator<Item = (u64, usize)>,
) -> Vec<CompletedToolCallSegment> {
    request_anchors
        .into_iter()
        .map(|(raw_ordinal, context_index)| {
            completed_toolcall_request_segment(raw_ordinal, context_index)
        })
        .collect()
}

fn completed_toolcall_response_segments(
    response_raw_ordinals: &[Option<u64>],
    context_start: usize,
) -> Vec<CompletedToolCallSegment> {
    response_raw_ordinals
        .iter()
        .enumerate()
        .filter_map(|(index, raw_ordinal)| {
            raw_ordinal.map(|raw_ordinal| {
                completed_toolcall_response_segment(raw_ordinal, context_start + index)
            })
        })
        .collect()
}

fn completed_toolcall_evidence_from_segments(
    call_id: &str,
    request_call_ids: &[String],
    request_segments: Vec<CompletedToolCallSegment>,
    response_segments: Vec<CompletedToolCallSegment>,
    missing_request_error: &'static str,
    missing_response_error: &'static str,
) -> Result<SpineCompletedToolCallEvidence, SpineError> {
    completed_toolcall_evidence(CompletedToolCallEvidenceParts {
        call_id: call_id.to_string(),
        request_call_ids: request_call_ids.to_vec(),
        request_segments,
        response_segments,
        missing_request_error,
        missing_response_error,
    })
}

fn completed_toolcall_evidence(
    parts: CompletedToolCallEvidenceParts,
) -> Result<SpineCompletedToolCallEvidence, SpineError> {
    let CompletedToolCallEvidenceParts {
        call_id,
        request_call_ids,
        mut request_segments,
        mut response_segments,
        missing_request_error,
        missing_response_error,
    } = parts;
    request_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
    response_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
    if request_segments.is_empty() {
        return Err(SpineError::InvalidEvent(missing_request_error.to_string()));
    }
    if response_segments.is_empty() {
        return Err(SpineError::InvalidEvent(missing_response_error.to_string()));
    }
    let mut segments = Vec::with_capacity(request_segments.len() + response_segments.len());
    segments.extend(request_segments);
    segments.extend(response_segments);
    Ok(SpineCompletedToolCallEvidence::new(CompletedToolCall {
        call_id,
        request_call_ids,
        segments,
    }))
}

#[derive(Debug)]
pub(crate) struct SpineSessionState {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    jit_enabled: bool,
    trim_enabled: bool,
    initial_tree_snapshot_emitted: bool,
    invalid: Option<String>,
}

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self::new_with_features(true, true)
    }

    pub(crate) fn new_with_features(jit_enabled: bool, trim_enabled: bool) -> Self {
        Self {
            raw_len: 0,
            runtime: None,
            jit_enabled,
            trim_enabled,
            initial_tree_snapshot_emitted: false,
            invalid: None,
        }
    }

    pub(crate) fn runtime(&self) -> Option<&SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_mut()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.invalid.is_none() && self.runtime.is_some()
    }

    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn ready_raw_len(&self) -> Result<Option<u64>, SpineError> {
        self.ensure_valid()?;
        if self.runtime().is_none() {
            return Ok(None);
        }
        Ok(Some(self.raw_len))
    }

    pub(crate) fn set_replayed(
        &mut self,
        raw_len: u64,
        mut runtime: Option<SpineRuntime>,
    ) -> Result<(), SpineError> {
        drop(self.runtime.take());
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            runtime.acquire_writer_lock()?;
        }
        self.raw_len = raw_len;
        self.runtime = runtime;
        self.initial_tree_snapshot_emitted = false;
        self.invalid = None;
        Ok(())
    }

    pub(crate) fn invalidate(&mut self, reason: impl Into<String>) {
        self.invalid = Some(reason.into());
    }

    pub(crate) fn release_runtime_for_shutdown(&mut self) {
        self.runtime = None;
    }

    pub(crate) fn release_runtime_for_replay(&mut self) {
        self.runtime = None;
        self.initial_tree_snapshot_emitted = false;
    }

    pub(crate) fn prepare_jit_replay_from_rollout_items(
        &self,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<PreparedSpineReplayRuntime, SpineError> {
        self.ensure_valid()?;
        let mut runtime =
            SpineRuntime::load_for_rollout_items(rollout_path, raw_items, rollback_cuts)?;
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
        }
        let materialized = runtime
            .as_ref()
            .map(|runtime| runtime.materialize_history(raw_items))
            .transpose()?;
        let live_root_compacts = runtime
            .as_ref()
            .map(|runtime| runtime.live_root_compacts())
            .transpose()?
            .unwrap_or_default();
        Ok(PreparedSpineReplayRuntime::new(
            runtime,
            materialized,
            live_root_compacts,
        ))
    }

    pub(crate) fn install_cloned_sidecar_for_fork(
        &mut self,
        boundary: &SpineCloneBoundary,
        target_rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
        SpineStore::clone_for_rollout_with_raw_live(boundary, target_rollout_path, &raw_live)?;
        let raw_ordinal_limit = usize::try_from(boundary.raw_ordinal_limit()).map_err(|_| {
            SpineError::InvalidEvent("clone raw ordinal boundary overflow".to_string())
        })?;
        if raw_ordinal_limit > raw_items.len() {
            return Err(SpineError::InvalidEvent(
                "clone raw ordinal boundary exceeds fork raw length".to_string(),
            ));
        }
        if raw_ordinal_limit == raw_items.len() {
            let runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
                target_rollout_path,
                raw_items,
                &[],
                self.jit_enabled,
            )?;
            return self.set_replayed(
                u64::try_from(raw_items.len())
                    .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
                runtime,
            );
        }

        let prefix_runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
            target_rollout_path,
            &raw_items[..raw_ordinal_limit],
            &[],
            self.jit_enabled,
        )?;
        let mut runtime = prefix_runtime.ok_or_else(|| {
            SpineError::InvalidStore("cloned Spine sidecar is missing after fork clone".to_string())
        })?;
        runtime.set_jit_enabled(self.jit_enabled);
        runtime.set_trim_enabled(self.trim_enabled);

        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for (raw_ordinal, item) in raw_items.iter().enumerate().skip(raw_ordinal_limit) {
            runtime.observe_raw_items(1)?;
            let Some(item) = item.as_ref() else {
                continue;
            };
            let context_index = if runtime.jit_enabled() {
                runtime.materialize_history(raw_items)?.len()
            } else {
                raw_items
                    .iter()
                    .take(raw_ordinal)
                    .filter(|item| item.is_some())
                    .count()
            };
            let raw_ordinal = u64::try_from(raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            runtime.observe_context_item(raw_ordinal, context_index, item)?;
            if let Some(call_id) = tool_response_call_id(item) {
                recorded_tool_outputs.push((call_id.to_string(), raw_ordinal, context_index));
            }
        }
        runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &recorded_tool_outputs,
            raw_items,
        )?;
        self.set_replayed(
            u64::try_from(raw_items.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            Some(runtime),
        )
    }

    pub(crate) fn abort_pending_tool(&mut self, call_id: &str) -> bool {
        let Some(runtime) = self.runtime_mut() else {
            return false;
        };
        runtime.abort_pending(call_id)
    }

    pub(crate) fn abort_any_pending(&mut self) -> Option<String> {
        let runtime = self.runtime_mut()?;
        runtime.abort_any_pending()
    }

    fn runtime_mut_after_init(&mut self) -> Result<&mut SpineRuntime, SpineError> {
        self.ensure_valid()?;
        self.runtime_mut().ok_or_else(|| {
            SpineError::InvalidStore("spine runtime missing after initialization".to_string())
        })
    }

    fn invalid_error(&self) -> Option<SpineError> {
        self.invalid
            .as_ref()
            .map(|reason| SpineError::Invariant(format!("spine runtime is invalid: {reason}")))
    }

    pub(crate) fn ensure_valid(&self) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        let count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        if let Some(runtime) = self.runtime.as_mut() {
            let count = usize::try_from(count)
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
            runtime.observe_raw_items(count)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_runtime(&mut self, rollout_path: &Path) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.runtime.is_none() {
            let mut runtime = SpineRuntime::load_or_create_with_jit(
                rollout_path,
                self.raw_len,
                self.jit_enabled,
            )?;
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            self.runtime = Some(runtime);
        }
        Ok(())
    }

    pub(crate) fn checkpoint_initial_if_jit(
        &self,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(());
        };
        if runtime.jit_enabled() {
            runtime.checkpoint_initial(rollout_path, raw_items)?;
        }
        Ok(())
    }

    pub(crate) fn on_init(&mut self, rollout_path: &Path) -> Result<(), SpineError> {
        self.ensure_runtime(rollout_path)?;
        self.checkpoint_initial_if_jit(rollout_path, &[])
    }

    pub(crate) fn take_initial_tree_snapshot(
        &mut self,
    ) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.initial_tree_snapshot_emitted {
            return Ok(None);
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        if !runtime.jit_enabled() {
            return Ok(None);
        }
        let snapshot = runtime.build_tree_snapshot()?;
        self.initial_tree_snapshot_emitted = true;
        Ok(Some(snapshot))
    }

    pub(crate) fn tree_snapshot_projection(
        &self,
    ) -> Result<Option<(SpineTreeUpdateEvent, Vec<SpineOpenNodeContextProjection>)>, SpineError>
    {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some((
            runtime.build_tree_snapshot()?,
            runtime.open_node_context_projections(),
        )))
    }

    pub(crate) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<Option<String>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        runtime
            .render_tree_with_context_annotations(annotations)
            .map(Some)
    }

    pub(crate) fn apply_root_compact_after_history_publish(
        &mut self,
        prepared: PreparedSpineRootCompactApply,
        published_history_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before root compact PS install".to_string(),
            ));
        };
        runtime.install_prepared_root_compact_install(prepared.commit.install);
        let current_open_index = runtime.current_open_index()?;
        if current_open_index != published_history_len {
            return Err(SpineError::InvalidStore(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {published_history_len}"
            )));
        }
        runtime.build_tree_snapshot()
    }

    pub(crate) fn completed_toolcall_requires_durable_output(
        &self,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(false);
        };
        runtime.has_close_like_control_request(call_id, raw_items)
    }

    pub(crate) fn is_control_output_call_id(&self, call_id: &str) -> bool {
        let Some(runtime) = self.runtime() else {
            return false;
        };
        runtime.is_control_output_call_id(call_id)
    }

    pub(crate) fn prepare_completed_toolcall_for_commit(
        &mut self,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineToolcallCommitPreparation>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        runtime.ensure_pending_from_toolcall_request(call_id, raw_items)?;
        runtime
            .has_close_like_control_request(call_id, raw_items)
            .map(SpineToolcallCommitPreparation::new)
            .map(Some)
    }

    pub(crate) fn observe_toolcall_context_items(
        &mut self,
        items: &[SpineObservedContextItem<'_>],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineContextObserveOutcome, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(SpineContextObserveOutcome::default());
        };
        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for item in items {
            if tool_request_call_id(item.item).is_some() {
                runtime.observe_toolcall_request_anchor(
                    item.raw_ordinal,
                    item.context_index,
                    item.item,
                )?;
            } else if let Some(call_id) = tool_response_call_id(item.item) {
                recorded_tool_outputs.push((
                    call_id.to_string(),
                    item.raw_ordinal,
                    item.context_index,
                ));
                runtime.observe_toolcall_response_anchor(
                    item.raw_ordinal,
                    item.context_index,
                    item.item,
                )?;
            } else if matches!(
                item.item,
                ResponseItem::ToolSearchOutput { call_id: None, .. }
                    | ResponseItem::ToolSearchCall { call_id: None, .. }
            ) {
                continue;
            } else {
                return Err(SpineError::InvalidEvent(
                    "toolcall context observer received non-toolcall item".to_string(),
                ));
            }
        }
        if !recorded_tool_outputs.is_empty() {
            runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
                &recorded_tool_outputs,
                raw_items,
            )?;
        }
        Ok(SpineContextObserveOutcome::default())
    }

    pub(crate) fn observe_non_toolcall_msg(
        &mut self,
        rollout_path: &Path,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(false);
        };
        if runtime.jit_enabled() && is_real_user_message(item) {
            runtime.checkpoint_before_user_msg(rollout_path, raw_ordinal, raw_items)?;
        }
        runtime.on_non_toolcall_msg(raw_ordinal, context_index, item)?;
        Ok(is_real_user_message(item))
    }

    pub(crate) fn trim_tool_response(
        &mut self,
        trim_id: &str,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?.trim_tool_response(trim_id)
    }

    pub(crate) fn slice_tool_response_head(
        &mut self,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_head(trim_id, head, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        &mut self,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_tail(trim_id, tail, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        &mut self,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_anchor(trim_id, anchor, preceding, following, raw_items)
    }

    pub(crate) fn trim_projection_needs_rollout_raw_items(
        &self,
    ) -> Result<Option<bool>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some(runtime.jit_enabled()))
    }

    pub(crate) fn materialize_trim_projection_from_raw_items(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some(runtime.materialize_history(raw_items)?))
    }

    pub(crate) fn materialize_history_if_no_pending_tool_request(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        if runtime.has_pending_tool_request() {
            return Ok(None);
        }
        Ok(Some(runtime.materialize_history(raw_items)?))
    }

    pub(crate) fn project_trim_projection_from_history(
        &self,
        history_items: &[ResponseItem],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some(runtime.project_raw_history_with_trim(history_items)?))
    }

    pub(crate) fn prepare_trim_replay_from_history(
        rollout_path: &Path,
        raw_len: u64,
        history_items: &[ResponseItem],
    ) -> Result<Option<(SpineRuntime, Vec<ResponseItem>)>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        let mut runtime = SpineRuntime::load_or_create_with_jit(rollout_path, raw_len, false)?;
        runtime.set_trim_enabled(true);
        let materialized = runtime.project_raw_history_with_trim(history_items)?;
        Ok(Some((runtime, materialized)))
    }

    pub(crate) fn prepare_root_compact_commit_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<PreparedSpineRootCompactCommit, SpineError> {
        let prepared = {
            let runtime = self.runtime_mut_after_init()?;
            runtime.prepare_root_compact_commit_with_checkpoint(
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
    }

    pub(crate) fn prepare_root_compact_apply_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<PreparedSpineRootCompactApply, SpineError> {
        self.prepare_root_compact_commit_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )
        .map(PreparedSpineRootCompactApply::new)
    }

    pub(crate) fn prepare_native_root_compact_apply_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Result<PreparedSpineRootCompactApply, SpineError> {
        let token_metadata = SpineRootCompactTokenMetadata {
            close_input_tokens: close_provider_input_tokens,
            close_context_tokens: close_provider_input_tokens,
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        };
        self.prepare_root_compact_apply_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )
    }

    pub(crate) fn single_completed_toolcall_evidence(
        &self,
        call_id: &str,
        response_anchor: (u64, usize),
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchor = runtime.pending_tool_request_anchor(call_id)?;
        let completed_toolcall = completed_toolcall_evidence_from_segments(
            call_id,
            &[call_id.to_string()],
            vec![completed_toolcall_request_segment(
                request_anchor.raw_ordinal,
                request_anchor.context_index,
            )],
            vec![completed_toolcall_response_segment(
                response_anchor.0,
                response_anchor.1,
            )],
            "completed toolcall must contain a request",
            "completed toolcall must contain a response",
        )?;
        Ok(Some(SpineToolcallCommitEvidence::new(
            call_id,
            completed_toolcall,
        )))
    }

    pub(crate) fn grouped_completed_toolcall_evidence(
        &self,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_raw_ordinals: &[Option<u64>],
        response_context_start: usize,
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchors = tool_call_ids
            .iter()
            .map(|call_id| runtime.pending_tool_request_anchor(call_id))
            .collect::<Result<Vec<_>, SpineError>>()?;
        let completed_toolcall = completed_toolcall_evidence_from_segments(
            commit_call_id,
            tool_call_ids,
            completed_toolcall_request_segments(
                request_anchors
                    .iter()
                    .map(|anchor| (anchor.raw_ordinal, anchor.context_index)),
            ),
            completed_toolcall_response_segments(response_raw_ordinals, response_context_start),
            "completed grouped toolcall must contain at least one request",
            "completed grouped toolcall must contain at least one response",
        )?;
        Ok(Some(SpineToolcallCommitEvidence::new(
            commit_call_id,
            completed_toolcall,
        )))
    }

    pub(crate) fn completed_toolcall_commit_evidence_from_output(
        &self,
        output: &SpineCompletedToolCallOutputEvidence<'_>,
        output_raw_ordinals: &[Option<u64>],
        output_context_start: usize,
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        match &output.request_call_ids {
            SpineCompletedToolCallRequestIds::Single(call_id) => self
                .single_completed_toolcall_evidence(
                    call_id,
                    (
                        output_raw_ordinals
                            .first()
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                SpineError::InvalidEvent(
                                    "single Spine toolcall output missing raw ordinal".to_string(),
                                )
                            })?,
                        output_context_start,
                    ),
                ),
            SpineCompletedToolCallRequestIds::Grouped(tool_call_ids) => self
                .grouped_completed_toolcall_evidence(
                    output.call_id(),
                    tool_call_ids,
                    output_raw_ordinals,
                    output_context_start,
                ),
        }
    }

    pub(crate) fn prepare_completed_toolcall_commit(
        &mut self,
        evidence: SpineToolcallCommitEvidence,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        pre_compact_provider_input_tokens: Option<i64>,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> Result<Option<PreparedSpineToolcallCommit>, SpineError> {
        let call_id = evidence.call_id;
        let completed_toolcall = evidence.completed_toolcall;
        let toolcall_start = completed_toolcall.first_segment_context_index()?;
        let input = SpineToolcallCommitInput {
            call_id: &call_id,
            completed_toolcall: completed_toolcall.into_completed_toolcall(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            expected_history,
            reference_context_item,
            pre_compact_provider_input_tokens,
            current_turn_provider_input_tokens,
        };
        self.ensure_valid()?;
        if self.runtime().is_none() {
            return Ok(None);
        }
        let memory = {
            let assembly = self
                .runtime_mut()
                .ok_or_else(|| {
                    SpineError::InvalidStore(
                        "spine runtime missing before completed toolcall commit".to_string(),
                    )
                })?
                .prepare_close_memory_assembly_for_completed_toolcall(
                    input.history_items,
                    toolcall_start,
                    input.call_id,
                );
            match assembly {
                Ok(Some(assembly)) => Some(SpinePreparedCloseMemory::new(
                    assembly,
                    input.expected_history,
                )),
                Ok(None) => None,
                Err(err) => {
                    let reason = "spine close memory assembly failed before commit";
                    if input.tool_resp_already_recorded {
                        self.invalidate(format!("{reason} for call_id={}", input.call_id));
                    } else if let Some(runtime) = self.runtime_mut() {
                        runtime.abort_pending(input.call_id);
                    }
                    return Err(err);
                }
            }
        };
        if let Some(prepared_memory) = memory.as_ref()
            && input.history_items != prepared_memory.expected_history()
        {
            let reason = "spine close history changed before commit";
            if input.tool_resp_already_recorded {
                self.invalidate(format!("{reason} for call_id={}", input.call_id));
            } else if let Some(runtime) = self.runtime_mut() {
                runtime.abort_pending(input.call_id);
            }
            return Err(SpineError::Operation(format!(
                "spine.close history changed while compacting suffix for call_id={}",
                input.call_id
            )));
        }
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        runtime.validate_close_expected_history_for_commit(
            input.call_id,
            memory
                .as_ref()
                .map(SpinePreparedCloseMemory::expected_history),
            input.history_items,
        )?;
        let memory_assembly = memory.map(SpinePreparedCloseMemory::into_assembly);
        let commit_application = runtime
            .prepare_or_observe_completed_toolcall_with_pending_baselines(
                input.call_id,
                memory_assembly,
                input.pre_compact_provider_input_tokens,
                input.current_turn_provider_input_tokens,
                input.completed_toolcall,
                input.raw_items,
            )?;
        runtime
            .prepare_commit_publication(
                input.call_id,
                commit_application,
                input.tool_resp_item,
                input.tool_resp_already_recorded,
                input.raw_items,
                input.history_items,
                |call_id, operation, suffix_start, expected_history, replacement| {
                    SpineHistoryUpdate {
                        call_id: call_id.to_string(),
                        operation,
                        suffix_start,
                        expected_history,
                        replacement,
                        reference_context_item: input.reference_context_item,
                    }
                },
            )
            .map(PreparedSpineToolcallCommit::new)
            .map(Some)
    }

    fn persist_toolcall_commit_side_effects(
        &mut self,
        prepared: &PreparedSpineToolcallCommit,
    ) -> Result<(), SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before commit publication side effects".to_string(),
            ));
        };
        runtime.persist_commit_publication_side_effects(&prepared.publication)
    }

    fn apply_toolcall_commit(
        &mut self,
        prepared: PreparedSpineToolcallCommit,
    ) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before commit publication install".to_string(),
            ));
        };
        Ok(runtime.install_commit_publication(prepared.publication))
    }

    pub(crate) fn commit_prepared_toolcall_with_host_effects(
        &mut self,
        call_id: &str,
        mut prepared: PreparedSpineToolcallCommit,
        apply_host_effects: impl FnOnce(SpineHostEffects) -> Result<(), String>,
    ) -> Result<CommittedSpineToolcall, SpineError> {
        let host_effects = prepared.take_pre_apply_host_effects();
        let post_apply_effect_policy = prepared.post_apply_effect_policy();
        if let Err(err) = self.persist_toolcall_commit_side_effects(&prepared) {
            self.invalidate(format!(
                "failed to persist Spine prepared side effects before publishing h(PS) for call_id={call_id}: {err}"
            ));
            return Err(err);
        }
        if let Err(err) = apply_host_effects(host_effects) {
            self.invalidate(format!(
                "failed to publish Spine h(PS) before installing reduced parse stack for call_id={call_id}: {err}"
            ));
            return Err(SpineError::Invariant(err));
        }
        let installed_commit = self.apply_toolcall_commit(prepared)?;
        Ok(CommittedSpineToolcall {
            installed_commit,
            post_apply_effect_policy,
        })
    }
}

#[cfg(test)]
mod completed_toolcall_evidence_tests {
    use super::*;

    fn segment_tuple(segment: &CompletedToolCallSegment) -> (ToolCallSegmentKind, u64, usize) {
        (segment.kind, segment.raw_ordinal, segment.context_index)
    }

    #[test]
    fn single_completed_toolcall_evidence_orders_request_before_response() {
        let toolcall = completed_toolcall_evidence_from_segments(
            "call-a",
            &["call-a".to_string()],
            vec![completed_toolcall_request_segment(10, 5)],
            vec![completed_toolcall_response_segment(11, 6)],
            "completed toolcall must contain a request",
            "completed toolcall must contain a response",
        )
        .expect("single evidence");

        assert_eq!(toolcall.completed_toolcall.call_id, "call-a");
        assert_eq!(
            toolcall.completed_toolcall.request_call_ids,
            vec!["call-a".to_string()]
        );
        assert_eq!(
            toolcall
                .completed_toolcall
                .segments
                .iter()
                .map(segment_tuple)
                .collect::<Vec<_>>(),
            vec![
                (ToolCallSegmentKind::Request, 10, 5),
                (ToolCallSegmentKind::Response, 11, 6),
            ]
        );
    }

    #[test]
    fn grouped_completed_toolcall_evidence_sorts_requests_and_responses_separately() {
        let tool_call_ids = vec!["call-b".to_string(), "call-a".to_string()];
        let toolcall = completed_toolcall_evidence_from_segments(
            "call-a",
            &tool_call_ids,
            completed_toolcall_request_segments([(20, 9), (10, 3)]),
            completed_toolcall_response_segments(&[Some(31), Some(30)], 7),
            "completed grouped toolcall must contain at least one request",
            "completed grouped toolcall must contain at least one response",
        )
        .expect("grouped evidence");

        assert_eq!(toolcall.completed_toolcall.call_id, "call-a");
        assert_eq!(toolcall.completed_toolcall.request_call_ids, tool_call_ids);
        assert_eq!(
            toolcall
                .completed_toolcall
                .segments
                .iter()
                .map(segment_tuple)
                .collect::<Vec<_>>(),
            vec![
                (ToolCallSegmentKind::Request, 10, 3),
                (ToolCallSegmentKind::Request, 20, 9),
                (ToolCallSegmentKind::Response, 31, 7),
                (ToolCallSegmentKind::Response, 30, 8),
            ]
        );
    }
}
