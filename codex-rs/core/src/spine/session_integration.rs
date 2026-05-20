use super::projection::SpineProjection;
use super::projection::project_spine_state_from_rollout_with_source;
use super::projection::surviving_spine_compact_ids_from_rollout;
use super::projection_epoch::ProjectionEpochClassification;
use super::projection_epoch::classify_projection_epoch;
use super::projection_epoch::projection_rollout_position;
use super::runtime::SpineRuntime;
use super::runtime::SpineRuntimeError;
use super::state::SpineState;
use super::store::SpineSidecarStore;
use super::store::SpineStoreError;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::RolloutItem;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ThreadStore;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

pub(crate) struct InitialSpineScan {
    pub(crate) response_item_count: u64,
    pub(crate) has_spine_history: bool,
    pub(crate) has_non_spine_compaction: bool,
    pub(crate) surviving_compact_ids: HashSet<String>,
    pub(crate) projection: Option<SpineProjection>,
}

impl InitialSpineScan {
    pub(crate) fn empty() -> Self {
        Self {
            response_item_count: 0,
            has_spine_history: false,
            has_non_spine_compaction: false,
            surviving_compact_ids: HashSet::new(),
            projection: None,
        }
    }
}

pub(crate) async fn initial_spine_scan(
    initial_history: &InitialHistory,
) -> anyhow::Result<InitialSpineScan> {
    match initial_history {
        InitialHistory::New | InitialHistory::Cleared => Ok(InitialSpineScan::empty()),
        InitialHistory::Resumed(resumed) => {
            if let Some(rollout_path) = resumed.rollout_path.as_ref() {
                let (rollout_items, _, _) =
                    crate::rollout::RolloutRecorder::load_rollout_items(rollout_path).await?;
                let mut scan = initial_spine_scan_items(
                    &resumed.history,
                    SpineProjectionPolicy::Resume,
                    rollout_path.to_string_lossy(),
                )?;
                if scan.has_spine_history {
                    scan.projection = resume_projection_from_sidecar_epoch(
                        rollout_path,
                        &rollout_items,
                        &resumed.history,
                    )?
                    .or(scan.projection);
                }
                Ok(scan)
            } else {
                Ok(initial_spine_scan_items(
                    &resumed.history,
                    SpineProjectionPolicy::Resume,
                    "resumed_initial_history".into(),
                )?)
            }
        }
        InitialHistory::Forked(items) => Ok(initial_spine_scan_items(
            items,
            SpineProjectionPolicy::Fork,
            "forked_initial_history".into(),
        )?),
    }
}

pub(crate) fn initial_history_has_spine_history(initial_history: &InitialHistory) -> bool {
    match initial_history {
        InitialHistory::New | InitialHistory::Cleared => false,
        InitialHistory::Resumed(resumed) => has_spine_history_items(&resumed.history),
        InitialHistory::Forked(items) => has_spine_history_items(items),
    }
}

enum SpineProjectionPolicy {
    Resume,
    Fork,
}

fn initial_spine_scan_items(
    items: &[RolloutItem],
    projection_policy: SpineProjectionPolicy,
    source_rollout_ref: std::borrow::Cow<'_, str>,
) -> anyhow::Result<InitialSpineScan> {
    let has_spine_history = has_spine_history_items(items);
    let needs_projection = match projection_policy {
        SpineProjectionPolicy::Resume => has_thread_rollback(items) && has_spine_history,
        SpineProjectionPolicy::Fork => has_spine_history,
    };
    Ok(InitialSpineScan {
        response_item_count: response_item_count(items)?,
        has_spine_history,
        has_non_spine_compaction: latest_compaction_is_non_spine(items),
        surviving_compact_ids: surviving_spine_compact_ids_from_rollout(items)?,
        projection: needs_projection
            .then(|| project_spine_state_from_rollout_with_source(source_rollout_ref, items))
            .transpose()?,
    })
}

fn resume_projection_from_sidecar_epoch(
    rollout_path: &Path,
    rollout_items: &[RolloutItem],
    replay_items: &[RolloutItem],
) -> anyhow::Result<Option<SpineProjection>> {
    if !SpineSidecarStore::has_sidecar_for_rollout(rollout_path)? {
        return Ok(None);
    }
    let store = SpineSidecarStore::for_rollout(rollout_path)?;
    if !store.tree_path().exists() {
        return Ok(None);
    }
    let Some(epoch) = store.latest_projection_epoch()? else {
        return Ok(None);
    };
    let epoch_len = usize::try_from(epoch.processed_rollout_len).map_err(|_| {
        anyhow::anyhow!(
            "spine sidecar projection epoch length {} cannot be represented on this platform",
            epoch.processed_rollout_len
        )
    })?;
    let prefix_len = epoch_len.min(rollout_items.len());
    let source_rollout_ref = rollout_path.to_string_lossy();
    let current_prefix =
        projection_rollout_position(source_rollout_ref.as_ref(), &rollout_items[..prefix_len])?;
    let current_processed_rollout_len = u64::try_from(rollout_items.len())
        .map_err(|_| anyhow::anyhow!("resumed rollout has too many items"))?;
    match classify_projection_epoch(&epoch, &current_prefix, current_processed_rollout_len) {
        ProjectionEpochClassification::Current | ProjectionEpochClassification::Behind => {
            let projection =
                project_spine_state_from_rollout_with_source(source_rollout_ref, replay_items)?;
            store.validate_root_meminstall_survivors(&projection.root_epoch_compact_ids)?;
            Ok(Some(projection))
        }
        ProjectionEpochClassification::Ahead => {
            anyhow::bail!(
                "spine sidecar projection epoch is ahead of resumed rollout at {}: epoch processed_rollout_len {}, current rollout len {}",
                store.root().display(),
                epoch.processed_rollout_len,
                current_processed_rollout_len
            )
        }
        ProjectionEpochClassification::Divergent => {
            anyhow::bail!(
                "spine sidecar projection epoch diverges from resumed rollout at {}",
                store.root().display()
            )
        }
    }
}

#[cfg(test)]
pub(crate) async fn initial_spine_response_item_count(
    initial_history: &InitialHistory,
) -> anyhow::Result<u64> {
    Ok(initial_spine_scan(initial_history)
        .await?
        .response_item_count)
}

#[cfg(test)]
pub(crate) async fn initial_spine_has_non_spine_compacted_history(
    initial_history: &InitialHistory,
) -> anyhow::Result<bool> {
    Ok(initial_spine_scan(initial_history)
        .await?
        .has_non_spine_compaction)
}

#[cfg(test)]
pub(crate) async fn initial_spine_has_spine_history(
    initial_history: &InitialHistory,
) -> anyhow::Result<bool> {
    Ok(initial_spine_scan(initial_history).await?.has_spine_history)
}

#[cfg(test)]
pub(crate) async fn initial_spine_projection(
    initial_history: &InitialHistory,
) -> anyhow::Result<Option<SpineProjection>> {
    Ok(initial_spine_scan(initial_history).await?.projection)
}

pub(crate) fn load_initial_spine_runtime(
    rollout_path: &Path,
    next_raw_ordinal: u64,
    has_spine_history: bool,
    has_non_spine_compaction: bool,
    surviving_compact_ids: &HashSet<String>,
    projection: Option<&SpineProjection>,
) -> Result<SpineRuntime, SpineRuntimeError> {
    let mut runtime = if has_spine_history {
        let store = SpineSidecarStore::for_rollout(rollout_path)?;
        if !store.tree_path().exists() {
            return Err(super::store::SpineStoreError::InvalidLedger(format!(
                "spine sidecar is missing for existing Spine rollout history at {}",
                store.root().display()
            ))
            .into());
        }
        SpineRuntime::load(store, next_raw_ordinal)?
    } else {
        let has_existing_sidecar = SpineSidecarStore::has_sidecar_for_rollout(rollout_path)?;
        let store = if has_existing_sidecar {
            SpineSidecarStore::for_rollout(rollout_path)?
        } else {
            SpineSidecarStore::create_for_rollout(rollout_path)?
        };
        let runtime = SpineRuntime::load_or_create(store, next_raw_ordinal)?;
        if has_existing_sidecar && runtime.state() != &SpineState::new() {
            return Err(SpineStoreError::InvalidLedger(
                "existing Spine sidecar state is not admissible for history without Spine evidence"
                    .to_string(),
            )
            .into());
        }
        runtime
    };
    if has_spine_history {
        runtime
            .store()
            .validate_mem_install_survivors(surviving_compact_ids)?;
    }
    if let Some(projection) = projection
        && runtime.state() != &projection.state
    {
        runtime.record_projection_reset(
            projection.state.clone(),
            projection.checkpoint.clone(),
            projection.response_item_count,
            projection.surviving_turn_ids.clone(),
            projection.surviving_compact_ids.clone(),
            projection.epoch.clone(),
            "resume_projection",
            None,
        )?;
    }
    if let Some(projection) = projection
        && runtime.state() == &projection.state
    {
        runtime.record_projection_survivors(
            projection.surviving_turn_ids.clone(),
            projection.surviving_compact_ids.clone(),
        );
    }
    if has_non_spine_compaction {
        runtime.mark_non_spine_compacted_history();
    }
    Ok(runtime)
}

pub(crate) fn after_response_items_recorded(
    runtime: &mut SpineRuntime,
    turn_id: &str,
    items: &[ResponseItem],
    start_ordinal: u64,
) -> Result<(), SpineRuntimeError> {
    let end_ordinal = end_ordinal_for_items(start_ordinal, items)?;
    runtime
        .after_response_items_recorded(turn_id, items, start_ordinal, end_ordinal)
        .map(|_| ())
}

pub(crate) fn after_prelude_items_recorded(
    runtime: &mut SpineRuntime,
    turn_id: &str,
    items: &[ResponseItem],
    start_ordinal: u64,
) -> Result<(), SpineRuntimeError> {
    let end_ordinal = end_ordinal_for_items(start_ordinal, items)?;
    runtime.after_prelude_items_recorded(turn_id, items, start_ordinal, end_ordinal)
}

pub(crate) async fn seed_forked_spine_sidecar(
    thread_store: &Arc<dyn ThreadStore>,
    initial_history: &InitialHistory,
    child_rollout_path: &Path,
) -> anyhow::Result<()> {
    let InitialHistory::Forked(rollout_items) = initial_history else {
        return Ok(());
    };
    if !has_spine_history_items(rollout_items) {
        return Ok(());
    }

    let parent_thread_id = initial_history
        .forked_from_id()
        .ok_or_else(|| anyhow::anyhow!("forked spine history is missing a source thread id"))?;
    let child_store = if SpineSidecarStore::locator_path_for_rollout(child_rollout_path)?.exists() {
        SpineSidecarStore::for_rollout(child_rollout_path)?
    } else {
        SpineSidecarStore::create_for_rollout(child_rollout_path)?
    };
    if child_store.tree_path().exists() {
        return Ok(());
    }
    let parent = thread_store
        .read_thread(ReadThreadParams {
            thread_id: parent_thread_id,
            include_archived: true,
            include_history: false,
        })
        .await?;
    let parent_rollout_path = parent.rollout_path.ok_or_else(|| {
        anyhow::anyhow!("source thread {parent_thread_id} has no local rollout path")
    })?;
    let parent_store = SpineSidecarStore::for_rollout(&parent_rollout_path)?;
    if !parent_store.tree_path().exists() {
        anyhow::bail!(
            "source thread {parent_thread_id} has no spine sidecar at {}",
            parent_store.root().display()
        );
    }

    let projection = project_spine_state_from_rollout_with_source(
        parent_rollout_path.to_string_lossy(),
        rollout_items,
    )?;
    parent_store.validate_mem_install_survivors(&projection.surviving_compact_ids)?;
    parent_store.validate_root_meminstall_survivors(&projection.root_epoch_compact_ids)?;
    let projected_response_count = projection.response_item_count;
    let expected_response_count = response_item_count(rollout_items)?;
    if projected_response_count > expected_response_count {
        anyhow::bail!(
            "forked spine projection counted {projected_response_count} response items, expected {expected_response_count}"
        );
    }

    child_store.create()?;
    child_store.record_projection_reset(
        &projection.state,
        projection.checkpoint.clone(),
        "fork_seed",
        None,
        projection.epoch.clone(),
    )?;
    child_store
        .copy_projected_compact_index_from(&parent_store, &projection.surviving_compact_ids)?;
    child_store.copy_projected_node_artifacts_from(
        &parent_store,
        projection.node_ids(),
        &projection.surviving_turn_ids,
    )?;
    Ok(())
}

fn response_item_count(items: &[RolloutItem]) -> anyhow::Result<u64> {
    u64::try_from(
        items
            .iter()
            .filter(|item| matches!(item, RolloutItem::ResponseItem(_)))
            .count(),
    )
    .map_err(|_| anyhow::anyhow!("spine response item count cannot fit in u64"))
}

fn end_ordinal_for_items(
    start_ordinal: u64,
    items: &[ResponseItem],
) -> Result<u64, SpineRuntimeError> {
    let item_count =
        u64::try_from(items.len()).map_err(|_| SpineRuntimeError::RawOrdinalOverflow)?;
    start_ordinal
        .checked_add(item_count)
        .ok_or(SpineRuntimeError::RawOrdinalOverflow)
}

fn has_spine_history_items(items: &[RolloutItem]) -> bool {
    items.iter().any(|item| match item {
        RolloutItem::Compacted(compacted) => compacted.spine.is_some(),
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            name, namespace, ..
        }) => super::is_spine_shaped_history_tool(name, namespace.as_deref()),
        _ => false,
    })
}

fn has_thread_rollback(items: &[RolloutItem]) -> bool {
    items
        .iter()
        .any(|item| matches!(item, RolloutItem::EventMsg(EventMsg::ThreadRolledBack(_))))
}

fn latest_compaction_is_non_spine(items: &[RolloutItem]) -> bool {
    items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => Some(compacted.spine.is_none()),
            _ => None,
        })
        .unwrap_or(false)
}
