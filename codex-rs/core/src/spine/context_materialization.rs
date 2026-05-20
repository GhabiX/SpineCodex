use super::checkpoint_render::render_spine_handoff_item;
use super::checkpoint_render::render_spine_memory_item;
use super::project_pi::ProjectInput;
use super::project_pi::ProjectMemInstall;
use super::project_pi::project_pi;
use super::projection::SpineProjectionInputs;
use super::projection::effective_rollout_items;
use super::projection::project_spine_state_from_inputs;
use super::segment::Segment;
use super::state::SpineState;
use super::store::CommittedMemInstall;
use super::store::CommittedNoteEvidence;
use super::store::NotePlacement;
use super::store::SpineOperation;
use super::store::SpineSidecarStore;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineCompactedCheckpointKind;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;

pub(crate) struct SpineMaterializationInput<'a> {
    pub(crate) replay_items: &'a [RolloutItem],
    pub(crate) branch_ref: String,
    pub(crate) persisted_prefix_items: &'a [RolloutItem],
    pub(crate) store: &'a SpineSidecarStore,
}

#[derive(Debug)]
pub(crate) struct SpineMaterialization {
    pub(crate) history: Vec<ResponseItem>,
}

pub(crate) fn materialize_spine_checkpoint_history(
    compacted: &CompactedItem,
    replay_items: &[RolloutItem],
    rollout_path: &Path,
    index: usize,
) -> CodexResult<Vec<ResponseItem>> {
    let Some(spine_checkpoint) = compacted.spine.as_ref() else {
        return Err(CodexErr::Fatal(format!(
            "unsupported compacted rollout item at index {index}: missing Spine checkpoint"
        )));
    };
    if !matches!(
        spine_checkpoint.kind,
        SpineCompactedCheckpointKind::Suffix | SpineCompactedCheckpointKind::RootEpoch
    ) {
        return Err(CodexErr::Fatal(format!(
            "unsupported Spine {:?} compacted rollout item at index {index}",
            spine_checkpoint.kind
        )));
    }
    let store = SpineSidecarStore::for_rollout(rollout_path).map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to load Spine sidecar for compacted rollout item at index {index}: {err}"
        ))
    })?;
    materialize_spine_context(SpineMaterializationInput {
        replay_items,
        branch_ref: rollout_path.to_string_lossy().into_owned(),
        persisted_prefix_items: replay_items,
        store: &store,
    })
    .map(|materialized| materialized.history)
}

pub(crate) fn materialize_spine_context(
    input: SpineMaterializationInput<'_>,
) -> CodexResult<SpineMaterialization> {
    let projection = project_spine_state_from_inputs(SpineProjectionInputs {
        replay_items: input.replay_items,
        branch_ref: input.branch_ref,
        persisted_prefix_items: input.persisted_prefix_items,
    })
    .map_err(|err| CodexErr::Fatal(format!("failed to project spine context: {err}")))?;
    input
        .store
        .validate_mem_install_survivors(&projection.surviving_compact_ids)
        .map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to validate Spine materialization MemInstall survivors: {err}"
            ))
        })?;
    let effective = effective_rollout_items(input.replay_items).map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to compute effective Spine rollout for materialization: {err}"
        ))
    })?;
    let raw_items = effective_response_items(&effective);
    let installs = input.store.committed_mem_installs().map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to load Spine materialization MemInstall ledger: {err}"
        ))
    })?;
    let surviving_installs = surviving_installs(&installs, &projection.surviving_compact_ids);
    let note_evidence = input.store.committed_note_evidence().map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to load Spine materialization Note evidence ledger: {err}"
        ))
    })?;
    let note_evidence = note_evidence_by_mem(
        &note_evidence,
        &projection.surviving_compact_ids,
        &projection.root_epoch_compact_ids,
    )?;
    let projected_state = projection.state.clone();
    let mut project_input = ProjectInput::new(projection.response_item_count, projection.state);
    for install in &surviving_installs {
        project_input.mem_installs.push(
            ProjectMemInstall::new(
                install.compact_id.clone(),
                install.node_id.clone(),
                install.cut_ordinal,
                install.fold_end_ordinal,
            )
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to admit Spine materialization MemInstall {}: {err}",
                    install.compact_id
                ))
            })?,
        );
    }
    let projected = project_pi(project_input).map_err(|err| {
        CodexErr::Fatal(format!("failed to project Spine materialization Pi: {err}"))
    })?;
    let memory_items = render_memory_items(input.store, &projected_state, &surviving_installs)?;
    let install_by_id = surviving_installs
        .iter()
        .map(|install| (install.compact_id.as_str(), install))
        .collect::<BTreeMap<_, _>>();
    let mut history = Vec::new();
    for segment in projected.pi {
        match segment {
            Segment::Raw(raw_span) => {
                let start = usize::try_from(raw_span.start).map_err(|_| {
                    CodexErr::Fatal(format!(
                        "Spine materialization raw start {} cannot fit usize",
                        raw_span.start
                    ))
                })?;
                let end = usize::try_from(raw_span.end).map_err(|_| {
                    CodexErr::Fatal(format!(
                        "Spine materialization raw end {} cannot fit usize",
                        raw_span.end
                    ))
                })?;
                if end > raw_items.len() || start > end {
                    return Err(CodexErr::Fatal(format!(
                        "Spine materialization Raw {} is outside effective raw item length {}",
                        raw_span,
                        raw_items.len()
                    )));
                }
                history.extend(raw_items[start..end].iter().cloned());
            }
            Segment::Mem { compact_id } => {
                if let Some(notes) =
                    note_items_for_mem(&note_evidence, &compact_id, NotePlacement::BeforeMem)
                {
                    history.extend(notes);
                }
                let item = memory_items.get(&compact_id).ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "Spine materialization missing rendered Mem item for {compact_id}"
                    ))
                })?;
                history.push(item.clone());
                if let Some(handoff) = handoff_note_for_mem(
                    &projected_state,
                    install_by_id.get(compact_id.as_str()).copied(),
                ) {
                    history.push(handoff);
                }
                if let Some(notes) =
                    note_items_for_mem(&note_evidence, &compact_id, NotePlacement::AfterMem)
                {
                    history.extend(notes);
                }
            }
            Segment::Note { kind } => {
                return Err(CodexErr::Fatal(format!(
                    "Spine materialization cannot render Note({kind}) without structured note evidence"
                )));
            }
        }
    }
    Ok(SpineMaterialization { history })
}

fn effective_response_items(items: &[&RolloutItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(response_item) => Some(response_item.clone()),
            _ => None,
        })
        .collect()
}

fn surviving_installs(
    installs: &[CommittedMemInstall],
    surviving_ids: &HashSet<String>,
) -> Vec<CommittedMemInstall> {
    installs
        .iter()
        .filter(|install| surviving_ids.contains(&install.compact_id))
        .cloned()
        .collect()
}

fn note_evidence_by_mem(
    evidence: &[CommittedNoteEvidence],
    surviving_ids: &HashSet<String>,
    root_epoch_ids: &HashSet<String>,
) -> CodexResult<BTreeMap<(String, NotePlacement), Vec<ResponseItem>>> {
    let mut notes = BTreeMap::<(String, NotePlacement), Vec<ResponseItem>>::new();
    for item in evidence {
        if !surviving_ids.contains(&item.compact_id) {
            continue;
        }
        if item.items.is_empty() {
            return Err(CodexErr::Fatal(format!(
                "Spine materialization invalid Note evidence {}/{}",
                item.compact_id, item.kind
            )));
        }
        notes
            .entry((item.compact_id.clone(), item.placement))
            .or_default()
            .extend(item.items.iter().cloned());
    }
    for compact_id in root_epoch_ids {
        if surviving_ids.contains(compact_id)
            && !notes.contains_key(&(compact_id.clone(), NotePlacement::BeforeMem))
        {
            return Err(CodexErr::Fatal(format!(
                "Spine materialization missing Note evidence for root compact {compact_id}"
            )));
        }
    }
    Ok(notes)
}

fn note_items_for_mem(
    notes: &BTreeMap<(String, NotePlacement), Vec<ResponseItem>>,
    compact_id: &str,
    placement: NotePlacement,
) -> Option<Vec<ResponseItem>> {
    notes.get(&(compact_id.to_string(), placement)).cloned()
}

fn render_memory_items(
    store: &SpineSidecarStore,
    state: &SpineState,
    installs: &[CommittedMemInstall],
) -> CodexResult<BTreeMap<String, ResponseItem>> {
    let mut items = BTreeMap::new();
    for install in installs {
        let section = store
            .verify_memory_body_ref(&install.node_id, &install.body_ref)
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to verify Spine materialization body for {}: {err}",
                    install.compact_id
                ))
            })?;
        let summary = state
            .node(&install.node_id)
            .and_then(|node| node.summary.clone())
            .unwrap_or_else(|| install.compact_id.clone());
        items.insert(
            install.compact_id.clone(),
            render_spine_memory_item(&install.node_id, install.op, &summary, &section.body),
        );
    }
    Ok(items)
}

fn handoff_note_for_mem(
    state: &SpineState,
    install: Option<&CommittedMemInstall>,
) -> Option<ResponseItem> {
    let install = install?;
    if install.op != SpineOperation::Close {
        return None;
    }
    let to_node = state.node(&install.node_id)?.parent_id.as_ref()?;
    Some(render_spine_handoff_item(&install.node_id, to_node))
}

#[cfg(test)]
#[path = "context_materialization_tests.rs"]
mod tests;
