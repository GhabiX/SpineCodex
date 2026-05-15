use super::projection::SpineProjection;
use super::projection::project_spine_state_from_rollout;
use super::runtime::SpineRuntime;
use super::runtime::SpineRuntimeError;
use super::store::SpineSidecarStore;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::SpineUpdatePlanArgs;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ThreadStore;
use std::path::Path;
use std::sync::Arc;

pub(crate) struct InitialSpineScan {
    pub(crate) response_item_count: u64,
    pub(crate) has_spine_history: bool,
    pub(crate) has_non_spine_compaction: bool,
    pub(crate) projection: Option<SpineProjection>,
}

impl InitialSpineScan {
    pub(crate) fn empty() -> Self {
        Self {
            response_item_count: 0,
            has_spine_history: false,
            has_non_spine_compaction: false,
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
                let (items, _, _) =
                    crate::rollout::RolloutRecorder::load_rollout_items(rollout_path).await?;
                Ok(initial_spine_scan_items(
                    &items,
                    SpineProjectionPolicy::Resume,
                )?)
            } else {
                Ok(initial_spine_scan_items(
                    &resumed.history,
                    SpineProjectionPolicy::Resume,
                )?)
            }
        }
        InitialHistory::Forked(items) => Ok(initial_spine_scan_items(
            items,
            SpineProjectionPolicy::Fork,
        )?),
    }
}

enum SpineProjectionPolicy {
    Resume,
    Fork,
}

fn initial_spine_scan_items(
    items: &[RolloutItem],
    projection_policy: SpineProjectionPolicy,
) -> anyhow::Result<InitialSpineScan> {
    let has_spine_history = has_spine_history_items(items);
    let needs_projection = match projection_policy {
        SpineProjectionPolicy::Resume => has_thread_rollback(items) && has_spine_history,
        SpineProjectionPolicy::Fork => has_spine_history,
    };
    Ok(InitialSpineScan {
        response_item_count: response_item_count(items),
        has_spine_history,
        has_non_spine_compaction: latest_compaction_is_non_spine(items),
        projection: needs_projection
            .then(|| project_spine_state_from_rollout(items))
            .transpose()?,
    })
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
        let store = if SpineSidecarStore::has_sidecar_for_rollout(rollout_path)? {
            SpineSidecarStore::for_rollout(rollout_path)?
        } else {
            SpineSidecarStore::create_for_rollout(rollout_path)?
        };
        SpineRuntime::load_or_create(store, next_raw_ordinal)?
    };
    if let Some(projection) = projection
        && runtime.state() != &projection.state
    {
        runtime.record_projection_reset(
            projection.state.clone(),
            projection.response_item_count,
            projection.surviving_turn_ids.clone(),
            projection.surviving_compact_hashes.clone(),
            "resume_projection",
            None,
        )?;
    }
    if let Some(projection) = projection
        && runtime.state() == &projection.state
    {
        runtime.record_projection_survivors(
            projection.surviving_turn_ids.clone(),
            projection.surviving_compact_hashes.clone(),
        );
    }
    if has_non_spine_compaction {
        runtime.mark_non_spine_compacted_history();
    }
    Ok(runtime)
}

pub(crate) fn record_plan_update_snapshot(
    runtime: &mut SpineRuntime,
    turn_id: &str,
    args: SpineUpdatePlanArgs,
) -> Result<Option<SpineTreeUpdateEvent>, SpineRuntimeError> {
    if !runtime.is_mutable() {
        return Ok(None);
    }
    runtime.record_plan_update(turn_id, args)?;
    Ok(Some(runtime.build_tree_snapshot()?))
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

    let projection = project_spine_state_from_rollout(rollout_items)?;
    let projected_response_count = projection.response_item_count;
    let expected_response_count = response_item_count(rollout_items);
    if projected_response_count > expected_response_count {
        anyhow::bail!(
            "forked spine projection counted {projected_response_count} response items, expected {expected_response_count}"
        );
    }

    child_store.create()?;
    child_store.record_projection_reset(projection.state.clone(), "fork_seed", None)?;
    child_store
        .copy_projected_compact_index_from(&parent_store, &projection.surviving_compact_hashes)?;
    child_store.copy_projected_node_artifacts_from(
        &parent_store,
        projection.node_ids(),
        &projection.surviving_turn_ids,
    )?;
    Ok(())
}

fn response_item_count(items: &[RolloutItem]) -> u64 {
    u64::try_from(
        items
            .iter()
            .filter(|item| matches!(item, RolloutItem::ResponseItem(_)))
            .count(),
    )
    .unwrap_or(u64::MAX)
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
        RolloutItem::Compacted(compacted) => is_spine_compact_message(&compacted.message),
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            name, namespace, ..
        }) => super::is_spine_history_tool(name, namespace.as_deref()),
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
            RolloutItem::Compacted(compacted) => {
                Some(!is_spine_compact_message(&compacted.message))
            }
            _ => None,
        })
        .unwrap_or(false)
}

fn is_spine_compact_message(message: &str) -> bool {
    message.starts_with("Spine compacted ")
}
