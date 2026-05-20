use super::candidate_mem_plan::CandidateMemPlan;
use super::host_bridge::HostBridgeProjection;
use super::host_materialization::SpineHostMaterializationInput;
use super::host_materialization::SpineHostRawSource;
use super::host_materialization::insert_spine_host_note_segments;
use super::host_materialization::materialize_spine_host_history;
use super::store::InstalledCompactSpan;
use super::store::NotePlacement;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;

pub(crate) fn materialize_live_suffix_checkpoint(
    old_history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    compact_id: &str,
    candidate_plan: &CandidateMemPlan,
    memory_item: ResponseItem,
    note_items: Vec<ResponseItem>,
) -> CodexResult<Vec<ResponseItem>> {
    let mut pi = candidate_plan.pi.clone();
    let artifacts = &candidate_plan.artifacts;
    let note_items = note_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("suffix_note_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    pi = insert_spine_host_note_segments(
        pi,
        compact_id,
        note_items.keys().cloned().collect(),
        NotePlacement::AfterMem,
    )?;
    let mut mem_items = memory_items_from_runtime_spans(old_history, runtime_spans)?;
    mem_items.insert(compact_id.to_string(), memory_item);
    materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts,
        raw_source: SpineHostRawSource::HostBridge {
            history: old_history,
            runtime_spans,
        },
        mem_items: &mem_items,
        note_items: &note_items,
    })
}

pub(crate) fn resolve_live_root_archive_cut(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<(u64, usize)> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let (archive_cut_ordinal, archive_cut_index) =
        projection
            .first_span_in_prefix(planned_cut_index)
            .map_or_else(
                || {
                    projection
                        .raw_for_effective_index(planned_cut_index)
                        .map(|raw| (raw, planned_cut_index))
                        .ok_or_else(|| {
                            CodexErr::Fatal(format!(
                                "spine root archive render(Pi) planned cut index {planned_cut_index} does not map to a raw ordinal"
                            ))
                        })
                },
                Ok,
            )?;
    if fold_end_ordinal < archive_cut_ordinal {
        return Err(CodexErr::Fatal(format!(
            "spine root archive render(Pi) fold_end ordinal {fold_end_ordinal} is before archive cut ordinal {archive_cut_ordinal}"
        )));
    }
    Ok((archive_cut_ordinal, archive_cut_index))
}

pub(crate) fn materialize_live_root_epoch_checkpoint(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    root_compact_id: &str,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    candidate_plan: &CandidateMemPlan,
) -> CodexResult<Vec<ResponseItem>> {
    let mut pi = candidate_plan.pi.clone();
    let artifacts = &candidate_plan.artifacts;
    let note_items = initial_context_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("initial_context_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    pi = insert_spine_host_note_segments(
        pi,
        root_compact_id,
        note_items.keys().cloned().collect(),
        NotePlacement::BeforeMem,
    )?;
    let mut mem_items = memory_items_from_runtime_spans(history, runtime_spans)?;
    mem_items.insert(root_compact_id.to_string(), root_memory_item);
    materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts,
        raw_source: SpineHostRawSource::HostBridge {
            history,
            runtime_spans,
        },
        mem_items: &mem_items,
        note_items: &note_items,
    })
}

fn memory_items_from_runtime_spans(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<BTreeMap<String, ResponseItem>> {
    let mut items = BTreeMap::new();
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    for span in runtime_spans {
        let item = projection.memory_item_for_span(&span.compact_id)?;
        items.insert(span.compact_id.clone(), item);
    }
    Ok(items)
}
