use sha1::Digest;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;

use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::spine::candidate_mem_plan::CandidateMem;
use crate::spine::candidate_mem_plan::CandidateMemPlan;
use crate::spine::candidate_mem_plan::CandidateMemPlanMode;
use crate::spine::candidate_mem_plan::plan_candidate_mem;
use crate::spine::compact::CODEX_BUILTIN_TEXT_STRATEGY;
use crate::spine::compact::SpineCompactBoundary;
use crate::spine::compact::build_root_archive_replacement_history_from_candidate_plan;
use crate::spine::compact::build_suffix_replacement_history_from_candidate_plan;
use crate::spine::compact::compact_suffix_with_codex_builtin_text;
use crate::spine::compact::prepare_spine_compact_plan;
use crate::spine::compact::raw_ordinal_for_effective_index_with_spans;
use crate::spine::compact::render_auto_compact_memory;
use crate::spine::compact::render_spine_handoff_item;
use crate::spine::compact::render_spine_initial_context_item;
use crate::spine::compact::render_spine_memory_item;
use crate::spine::compact::resolve_root_archive_cut;
use crate::spine::mem_install::MemoryBodyRef;
use crate::spine::segment::RawSpan;
use crate::spine::store::CompactAttemptRecord;
use crate::spine::store::CompactStartedRecord;
use crate::spine::store::CompactTerminalRecord;
use crate::spine::store::InstalledCompactSpan;
use crate::spine::store::MemInstallCommittedRecord;
use crate::spine::store::NoteEvidenceCommittedRecord;
use crate::spine::store::NotePlacement as SpineNotePlacement;
use crate::spine::store::SpineOperation;
use crate::spine::store::SpineSidecarStore;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineCompactedCheckpoint;
use codex_protocol::protocol::SpineCompactedCheckpointKind;
use codex_protocol::protocol::TurnContextItem;

impl Session {
    pub(crate) async fn install_spine_root_epoch_compaction(
        &self,
        turn_context: &TurnContext,
        history: Vec<ResponseItem>,
        compact_summary: String,
        reference_context_item: Option<TurnContextItem>,
    ) -> CodexResult<()> {
        let spine = self
            .spine
            .as_ref()
            .ok_or_else(|| CodexErr::Fatal("spine runtime is not initialized".to_string()))?;
        let (store, surviving_compact_ids) = {
            let runtime = spine.lock().await;
            (
                runtime.store().clone(),
                runtime.surviving_compact_ids().cloned(),
            )
        };
        let boundary = spine
            .lock()
            .await
            .plan_root_epoch_archive()
            .map_err(|err| CodexErr::Fatal(format!("Spine root archive planning failed: {err}")))?;
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| CodexErr::Fatal(format!("failed to resolve rollout path: {err:#}")))?
            .ok_or_else(|| {
                CodexErr::Fatal("spine root archive requires a local rollout path".to_string())
            })?;
        let spine_tree = spine
            .lock()
            .await
            .render_tree_for_prompt()
            .map_err(|err| CodexErr::Fatal(format!("failed to render spine tree: {err}")))?;
        let prep = prepare_spine_compact_plan(
            &store,
            &boundary,
            &history,
            rollout_path,
            spine_tree,
            surviving_compact_ids.as_ref(),
        )?;
        let acknowledged_raw_ordinal = spine.lock().await.current_ordinal();
        if prep.effective_boundary.fold_end_ordinal > acknowledged_raw_ordinal {
            return Err(CodexErr::Fatal(format!(
                "spine root archive fold end {} for node {} op {:?} exceeds acknowledged raw ordinal {}",
                prep.effective_boundary.fold_end_ordinal,
                prep.effective_boundary.node_id,
                prep.effective_boundary.op,
                acknowledged_raw_ordinal
            )));
        }
        let compacted_body = compact_summary
            .strip_prefix(crate::compact::SUMMARY_PREFIX)
            .map(str::trim)
            .filter(|body| !body.is_empty())
            .unwrap_or(compact_summary.as_str());
        let rendered_memory_item = render_spine_memory_item(
            &boundary.node_id,
            boundary.op,
            &boundary.transition_summary,
            compacted_body,
        );
        let initial_context_items = if reference_context_item.is_some() {
            vec![render_spine_initial_context_item(
                self.build_initial_context(turn_context).await,
            )?]
        } else {
            Vec::new()
        };
        let (archive_cut_ordinal, archive_cut_index) = resolve_root_archive_cut(
            &history,
            prep.plan.cut_index,
            prep.effective_boundary.fold_end_ordinal,
            &prep.runtime_spans,
        )?;
        let root_boundary = SpineCompactBoundary {
            cut_ordinal: archive_cut_ordinal,
            ..prep.effective_boundary.clone()
        };
        let mut memory_input = prep.plan.input.clone();
        memory_input.cut_ordinal = root_boundary.cut_ordinal;
        memory_input.suffix_items = history[archive_cut_index..prep.plan.fold_end_index].to_vec();
        let memory_markdown = render_auto_compact_memory(&memory_input, compacted_body);
        let compact_message = format!(
            "Spine compacted root epoch {} [{}, {})",
            boundary.node_id, root_boundary.cut_ordinal, root_boundary.fold_end_ordinal
        );
        let (compact_id, compact_attempt, compact_started) =
            spine_compact_attempt_records(&root_boundary, prep.compact_index_rollout_path.clone());
        store
            .append_compact_started(compact_started)
            .map_err(|err| {
                CodexErr::Fatal(format!("failed to record spine root archive start: {err}"))
            })?;
        let candidate_plan = match validate_root_mem_install_segment_plan(
            &history,
            &prep.runtime_spans,
            &root_boundary,
            &compact_id,
        ) {
            Ok(candidate_plan) => candidate_plan,
            Err(err) => {
                record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
                return Err(err);
            }
        };
        let replacement_history = match build_root_archive_replacement_history_from_candidate_plan(
            &history,
            &prep.runtime_spans,
            &compact_id,
            initial_context_items.clone(),
            rendered_memory_item,
            &candidate_plan,
        ) {
            Ok(replacement_history) => replacement_history,
            Err(err) => {
                record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
                return Err(err);
            }
        };
        let body_ref = stage_spine_memory_before_meminstall(
            &store,
            &boundary,
            &compact_attempt,
            &memory_markdown,
            &memory_input.suffix_items,
            "root archive",
            "root epoch",
        )?;
        #[cfg(test)]
        if compacted_body.contains("__spine_fail_root_archive_before_meminstall__") {
            let err = "injected spine root archive failure before MemInstall commit";
            record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
            return Err(CodexErr::Fatal(err.to_string()));
        }
        if let Err(err) = append_initial_context_note_evidence(
            &store,
            &compact_id,
            &root_boundary,
            &initial_context_items,
            &prep.compact_index_rollout_path,
        ) {
            record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
            return Err(CodexErr::Fatal(format!(
                "failed to commit spine root archive Note evidence before MemInstall commit: {err}"
            )));
        }
        if let Err(err) = store.append_mem_install_committed(MemInstallCommittedRecord {
            attempt: compact_attempt.clone(),
            body_ref,
            projection_ref: root_mem_install_projection_ref(&compact_id, &root_boundary),
            source_rollout_ref: prep.compact_index_rollout_path.clone(),
        }) {
            record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
            return Err(CodexErr::Fatal(format!(
                "failed to commit spine root archive MemInstall before host checkpoint: {err}"
            )));
        }
        ensure_meminstall_committed(&store, &compact_id, "root epoch reset")?;
        {
            let mut runtime = spine.lock().await;
            if let Err(err) = runtime.record_root_epoch_archive(
                boundary.transition_summary.clone(),
                root_boundary.fold_end_ordinal,
                &compact_id,
                &turn_context.sub_id,
            ) {
                return Err(self
                    .poison_spine_compact(format!(
                        "failed to record spine root epoch archive after MemInstall commit: {err}"
                    ))
                    .await);
            }
        }
        ensure_meminstall_committed(&store, &compact_id, "root checkpoint render")?;
        let compacted_item = CompactedItem {
            message: compact_message.clone(),
            replacement_history: None,
            spine: Some(SpineCompactedCheckpoint {
                compact_id: compact_id.clone(),
                kind: SpineCompactedCheckpointKind::RootEpoch,
            }),
        };
        if let Err(err) = self
            .try_replace_compacted_history(
                replacement_history.clone(),
                reference_context_item,
                compacted_item,
            )
            .await
        {
            return Err(self
                .poison_spine_compact(format!(
                    "failed to install spine root archive host checkpoint after MemInstall commit: {err}"
                ))
                .await);
        }
        #[cfg(test)]
        if compacted_body.contains("__spine_fail_root_archive_after_rollout_checkpoint__") {
            return Err(self
                .poison_spine_compact(
                    "injected spine root archive failure after rollout checkpoint",
                )
                .await);
        }
        if let Err(err) = store.validate_mem_install_survivors(&HashSet::from([compact_id.clone()]))
        {
            return Err(self
                .poison_spine_compact(format!(
                    "failed to validate spine root archive MemInstall after host checkpoint: {err}"
                ))
                .await);
        }
        let snapshot = {
            let mut runtime = spine.lock().await;
            runtime.record_surviving_compact_id(compact_id);
            match runtime.build_tree_snapshot() {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    return Err(self
                        .poison_spine_compact(format!(
                            "failed to build spine tree snapshot after bridge checkpoint root archive: {err}"
                        ))
                        .await);
                }
            }
        };
        self.send_event(turn_context, EventMsg::SpineTreeUpdate(snapshot))
            .await;
        Ok(())
    }

    pub(super) async fn compact_spine_suffix_after_transition(
        &self,
        turn_context: &TurnContext,
        client_session: &mut ModelClientSession,
        prompt_envelope: &Prompt,
        boundary: SpineCompactBoundary,
        cancellation_token: &CancellationToken,
    ) -> CodexResult<()> {
        let spine = self
            .spine
            .as_ref()
            .ok_or_else(|| CodexErr::Fatal("spine runtime is not initialized".to_string()))?;
        let (store, surviving_compact_ids) = {
            let runtime = spine.lock().await;
            (
                runtime.store().clone(),
                runtime.surviving_compact_ids().cloned(),
            )
        };
        let close_outlines = if boundary.op == SpineOperation::Close {
            let runtime = spine.lock().await;
            let audit_outline = runtime
                .render_context_compacted_outline(&boundary.node_id)
                .map_err(|err| {
                    CodexErr::Fatal(format!(
                        "failed to render spine compact scope outline: {err}"
                    ))
                })?;
            let model_outline = runtime
                .render_model_context_compacted_outline(&boundary.node_id)
                .map_err(|err| {
                    CodexErr::Fatal(format!(
                        "failed to render spine compact model scope outline: {err}"
                    ))
                })?;
            Some((audit_outline, model_outline))
        } else {
            None
        };
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| CodexErr::Fatal(format!("failed to resolve rollout path: {err:#}")))?
            .ok_or_else(|| {
                CodexErr::Fatal("spine compact requires a local rollout path".to_string())
            })?;
        let history = self.clone_history().await.raw_items().to_vec();
        let spine_tree = spine
            .lock()
            .await
            .render_tree_for_prompt()
            .map_err(|err| CodexErr::Fatal(format!("failed to render spine tree: {err}")))?;
        let prep = prepare_spine_compact_plan(
            &store,
            &boundary,
            &history,
            rollout_path,
            spine_tree,
            surviving_compact_ids.as_ref(),
        )?;
        let (compact_id, compact_attempt, compact_started) = spine_compact_attempt_records(
            &prep.effective_boundary,
            prep.compact_index_rollout_path.clone(),
        );
        store
            .append_compact_started(compact_started)
            .map_err(|err| {
                CodexErr::Fatal(format!("failed to record spine compact start: {err}"))
            })?;

        let compact_output = match Box::pin(compact_suffix_with_codex_builtin_text(
            self,
            turn_context,
            client_session,
            prompt_envelope,
            prep.plan.input.clone(),
            cancellation_token,
        ))
        .await
        {
            Ok(output) => output,
            Err(err) => {
                let terminal = if matches!(err, CodexErr::TurnAborted | CodexErr::Interrupted) {
                    store.append_compact_interrupted(CompactTerminalRecord {
                        attempt: compact_attempt.clone(),
                        strategy: CODEX_BUILTIN_TEXT_STRATEGY.to_string(),
                        error: err.to_string(),
                    })
                } else {
                    store.append_compact_failed(CompactTerminalRecord {
                        attempt: compact_attempt.clone(),
                        strategy: CODEX_BUILTIN_TEXT_STRATEGY.to_string(),
                        error: err.to_string(),
                    })
                };
                terminal.map_err(|store_err| {
                    CodexErr::Fatal(format!(
                        "failed to record spine compact terminal state after strategy error {err}: {store_err}"
                    ))
                })?;
                return Err(err);
            }
        };
        let mut memory_markdown = compact_output.memory_markdown.clone();
        let mut model_memory_body = compact_output.compacted_body.clone();
        if let Some((audit_outline, model_outline)) = close_outlines {
            memory_markdown.push_str("\n\n");
            memory_markdown.push_str(&audit_outline);
            model_memory_body.push_str("\n\n");
            model_memory_body.push_str(&model_outline);
        }
        let to_node = spine.lock().await.cursor().clone();
        let rendered_memory_item = render_spine_memory_item(
            &boundary.node_id,
            boundary.op,
            &boundary.transition_summary,
            &model_memory_body,
        );
        let handoff_item = render_spine_handoff_item(&boundary.node_id, &to_node);
        let projected_state = {
            let runtime = spine.lock().await;
            runtime.state().clone()
        };
        let candidate_plan = validate_suffix_mem_install_segment_plan(
            &history,
            &prep.runtime_spans,
            &prep.effective_boundary,
            &compact_id,
            &projected_state,
        )?;
        let replacement_history = build_suffix_replacement_history_from_candidate_plan(
            &history,
            &prep.runtime_spans,
            &compact_id,
            &candidate_plan,
            rendered_memory_item,
            vec![handoff_item],
        )?;
        let body_ref = stage_spine_memory_before_meminstall(
            &store,
            &boundary,
            &compact_attempt,
            &memory_markdown,
            &prep.plan.input.suffix_items,
            "compact",
            "compact node",
        )?;
        #[cfg(test)]
        if compact_output
            .compacted_body
            .contains("__spine_fail_suffix_before_meminstall__")
        {
            let err = "injected spine suffix compact failure before MemInstall commit";
            record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
            return Err(CodexErr::Fatal(err.to_string()));
        }

        if let Err(err) = store.append_mem_install_committed(MemInstallCommittedRecord {
            attempt: compact_attempt.clone(),
            body_ref,
            projection_ref: suffix_mem_install_projection_ref(
                &compact_id,
                &prep.effective_boundary,
            ),
            source_rollout_ref: prep.compact_index_rollout_path.clone(),
        }) {
            record_pre_meminstall_compact_failed(&store, &compact_attempt, err.to_string())?;
            return Err(CodexErr::Fatal(format!(
                "failed to commit spine compact MemInstall before host checkpoint: {err}"
            )));
        }
        ensure_meminstall_committed(&store, &compact_id, "checkpoint render")?;
        let compacted_item = CompactedItem {
            message: compact_output.compact_message.clone(),
            replacement_history: None,
            spine: Some(SpineCompactedCheckpoint {
                compact_id: compact_id.clone(),
                kind: SpineCompactedCheckpointKind::Suffix,
            }),
        };
        if let Err(err) = self
            .try_replace_compacted_history(replacement_history.clone(), None, compacted_item)
            .await
        {
            return Err(self
                .poison_spine_compact(format!(
                    "failed to install spine compact host checkpoint after MemInstall commit: {err}"
                ))
                .await);
        }
        #[cfg(test)]
        if compact_output
            .compacted_body
            .contains("__spine_fail_suffix_after_rollout_checkpoint__")
        {
            return Err(self
                .poison_spine_compact(
                    "injected spine suffix compact failure after rollout checkpoint",
                )
                .await);
        }
        if let Err(err) = store.validate_mem_install_survivors(&HashSet::from([compact_id.clone()]))
        {
            return Err(self
                .poison_spine_compact(format!(
                    "failed to validate spine MemInstall after host checkpoint: {err}"
                ))
                .await);
        }
        spine.lock().await.record_surviving_compact_id(compact_id);
        Ok(())
    }
}
fn sha1_digest(value: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(value.as_bytes());
    format!("sha1:{:x}", hasher.finalize())
}

pub(crate) fn deterministic_spine_compact_id(boundary: &SpineCompactBoundary) -> String {
    sha1_digest(&format!(
        "spine-compact-v3\nop={:?}\nnode={}\ncut={}\nfold_end={}\nsummary={}\ninstruction={}",
        boundary.op,
        boundary.node_id,
        boundary.cut_ordinal,
        boundary.fold_end_ordinal,
        boundary.transition_summary,
        boundary.compact_instruction.as_deref().unwrap_or_default()
    ))
}

pub(super) fn spine_compact_attempt_records(
    boundary: &SpineCompactBoundary,
    rollout: String,
) -> (String, CompactAttemptRecord, CompactStartedRecord) {
    let compact_id = deterministic_spine_compact_id(boundary);
    let attempt = CompactAttemptRecord {
        compact_id: compact_id.clone(),
        node_id: boundary.node_id.clone(),
        op: boundary.op,
        cut_ordinal: boundary.cut_ordinal,
        fold_end_ordinal: boundary.fold_end_ordinal,
    };
    let started = CompactStartedRecord {
        attempt: attempt.clone(),
        strategy: CODEX_BUILTIN_TEXT_STRATEGY.to_string(),
        rollout,
    };
    (compact_id, attempt, started)
}

fn validate_suffix_mem_install_segment_plan(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    boundary: &SpineCompactBoundary,
    compact_id: &str,
    state: &crate::spine::state::SpineState,
) -> CodexResult<CandidateMemPlan> {
    let raw_len = raw_ordinal_for_effective_index_with_spans(history, history.len(), runtime_spans)
        .ok_or_else(|| {
            CodexErr::Fatal("suffix MemInstall segment plan could not map history end".to_string())
        })?;
    let candidate = CandidateMem::new(
        compact_id.to_string(),
        boundary.node_id.clone(),
        boundary.op,
        RawSpan {
            start: boundary.cut_ordinal,
            end: boundary.fold_end_ordinal,
        },
    );
    plan_candidate_mem(
        raw_len,
        runtime_spans,
        &candidate,
        CandidateMemPlanMode::ProjectionBacked { state },
    )
}

fn validate_root_mem_install_segment_plan(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    boundary: &SpineCompactBoundary,
    compact_id: &str,
) -> CodexResult<CandidateMemPlan> {
    let raw_len = raw_ordinal_for_effective_index_with_spans(history, history.len(), runtime_spans)
        .ok_or_else(|| {
            CodexErr::Fatal("root MemInstall segment plan could not map history end".to_string())
        })?;
    let candidate = CandidateMem::new(
        compact_id.to_string(),
        boundary.node_id.clone(),
        boundary.op,
        RawSpan {
            start: boundary.cut_ordinal,
            end: boundary.fold_end_ordinal,
        },
    );
    plan_candidate_mem(
        raw_len,
        runtime_spans,
        &candidate,
        CandidateMemPlanMode::CoverOnly {
            live_boundaries: &[boundary.fold_end_ordinal],
        },
    )
}

pub(super) fn suffix_mem_install_projection_ref(
    compact_id: &str,
    boundary: &SpineCompactBoundary,
) -> String {
    format!(
        "projection:suffix:{compact_id}:node={}:span={}-{}",
        boundary.node_id, boundary.cut_ordinal, boundary.fold_end_ordinal
    )
}

pub(super) fn root_mem_install_projection_ref(
    compact_id: &str,
    boundary: &SpineCompactBoundary,
) -> String {
    format!(
        "projection:root-archive:{compact_id}:node={}:span={}-{}",
        boundary.node_id, boundary.cut_ordinal, boundary.fold_end_ordinal
    )
}

pub(super) fn append_initial_context_note_evidence(
    store: &SpineSidecarStore,
    compact_id: &str,
    boundary: &SpineCompactBoundary,
    initial_context_items: &[ResponseItem],
    source_rollout_ref: &str,
) -> CodexResult<()> {
    let projection_ref = root_mem_install_projection_ref(compact_id, boundary);
    for (index, item) in initial_context_items.iter().enumerate() {
        store
            .append_note_evidence_committed(NoteEvidenceCommittedRecord {
                compact_id: compact_id.to_string(),
                placement: SpineNotePlacement::BeforeMem,
                kind: format!("initial_context_{index}"),
                items: vec![item.clone()],
                projection_ref: projection_ref.clone(),
                source_rollout_ref: source_rollout_ref.to_string(),
            })
            .map_err(|err| CodexErr::Fatal(err.to_string()))?;
    }
    if initial_context_items.is_empty() {
        store
            .append_note_evidence_committed(NoteEvidenceCommittedRecord {
                compact_id: compact_id.to_string(),
                placement: SpineNotePlacement::BeforeMem,
                kind: "initial_context_empty".to_string(),
                items: vec![ResponseItem::Message {
                    id: None,
                    role: "developer".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "<spine_initial_context_empty runtime_generated=\"true\" />"
                            .to_string(),
                    }],
                    phase: None,
                }],
                projection_ref,
                source_rollout_ref: source_rollout_ref.to_string(),
            })
            .map_err(|err| CodexErr::Fatal(err.to_string()))?;
    }
    Ok(())
}

pub(super) fn stage_spine_memory_before_meminstall(
    store: &SpineSidecarStore,
    boundary: &SpineCompactBoundary,
    attempt: &CompactAttemptRecord,
    memory_markdown: &str,
    folded_items: &[ResponseItem],
    memory_context: &str,
    trajs_context: &str,
) -> CodexResult<MemoryBodyRef> {
    if let Err(err) = store.append_memory_section(&boundary.node_id, memory_markdown) {
        record_pre_meminstall_compact_failed(store, attempt, err.to_string())?;
        return Err(CodexErr::Fatal(format!(
            "failed to stage spine {memory_context} memory before MemInstall commit: {err}"
        )));
    }
    let body_ref = store
        .generated_memory_sections(&boundary.node_id)
        .map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to read staged spine {memory_context} memory: {err}"
            ))
        })?
        .last()
        .map(|section| section.body_ref())
        .ok_or_else(|| {
            CodexErr::Fatal(format!(
                "staged spine {memory_context} memory has no generated section"
            ))
        })?;
    let node_trajs_items = folded_items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect::<Vec<_>>();
    if let Err(err) = store.append_node_trajs_items(&boundary.node_id, &node_trajs_items) {
        record_pre_meminstall_compact_failed(store, attempt, err.to_string())?;
        return Err(CodexErr::Fatal(format!(
            "failed to archive spine {trajs_context} trajs before MemInstall commit: {err}"
        )));
    }
    Ok(body_ref)
}

pub(super) fn ensure_meminstall_committed(
    store: &SpineSidecarStore,
    compact_id: &str,
    boundary: &str,
) -> CodexResult<()> {
    let committed = store.committed_mem_installs().map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to reload committed spine MemInstall before {boundary}: {err}"
        ))
    })?;
    if committed
        .iter()
        .any(|install| install.compact_id == compact_id)
    {
        return Ok(());
    }
    Err(CodexErr::Fatal(format!(
        "{boundary} could not find committed MemInstall {compact_id}"
    )))
}

pub(super) fn record_pre_meminstall_compact_failed(
    store: &SpineSidecarStore,
    attempt: &CompactAttemptRecord,
    error: String,
) -> CodexResult<()> {
    store
        .append_compact_failed(CompactTerminalRecord {
            attempt: attempt.clone(),
            strategy: CODEX_BUILTIN_TEXT_STRATEGY.to_string(),
            error: error.clone(),
        })
        .map_err(|store_err| {
            CodexErr::Fatal(format!(
                "failed to record spine compact failure before MemInstall commit after error {error}: {store_err}"
            ))
        })
}
