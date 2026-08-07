use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::sync::Arc;

use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMetaLine;

use super::LocalThreadStore;
use super::live_writer;
use super::model_context;
use super::rollout_lineage::RolloutLineage;
use super::thread_history::find_source_turn;
use super::thread_history::find_visible_turn;
use crate::ForkBoundary;
use crate::PrepareForkParams;
use crate::PreparedFork;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn prepare(
    store: &LocalThreadStore,
    params: PrepareForkParams,
) -> ThreadStoreResult<PreparedFork> {
    let PrepareForkParams {
        thread_id,
        boundary,
    } = params;
    let source_reservation = store.live_writer_locks.reserve_lifecycle(thread_id).await;
    // Keep the source reserved until persistence and lineage materialization finish, even if the
    // caller cancels fork preparation.
    let lineage_store = store.clone();
    let (lineage, source_reservation) = tokio::spawn(async move {
        match live_writer::persist_thread(&lineage_store, thread_id).await {
            Ok(()) | Err(ThreadStoreError::ThreadNotFound { .. }) => {}
            Err(err) => return Err(err),
        }
        let lineage = lineage_store
            .resolve_rollout_lineage_for_reference(thread_id)
            .await?;
        Ok::<_, ThreadStoreError>((lineage, source_reservation))
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to resolve fork lineage: {err}"),
    })??;
    let source_segment = lineage
        .segments()
        .last()
        .ok_or_else(|| ThreadStoreError::Internal {
            message: "fork lineage has no source segment".to_string(),
        })?;
    if store.state_db.is_none() {
        return Err(ThreadStoreError::Unsupported {
            operation: "prepare_fork",
        });
    }
    if !matches!(boundary, ForkBoundary::Latest) {
        for segment in lineage
            .segments()
            .iter()
            .take(lineage.segments().len().saturating_sub(1))
        {
            let _ancestor_writer_guard = store.live_writer_locks.lock(segment.thread_id()).await;
            super::thread_history_materialization::materialize_to_sqlite(
                store,
                segment.thread_id(),
                segment.rollout_path.as_path(),
            )
            .await?;
        }
    }
    let source_writer_guard = store.live_writer_locks.lock(thread_id).await;
    super::thread_history_materialization::materialize_to_sqlite(
        store,
        thread_id,
        source_segment.rollout_path.as_path(),
    )
    .await?;

    let latest_projection_state = super::thread_history::projection_state(store, thread_id)
        .await?
        .ok_or_else(|| ThreadStoreError::Internal {
            message: format!("missing projection state for paginated thread {thread_id}"),
        })?;
    let latest_position = HistoryPosition {
        thread_id,
        end_ordinal_exclusive: latest_projection_state.next_ordinal,
        end_byte_offset: latest_projection_state.next_byte_offset,
    };
    let pool = store.thread_history_db().await?;
    let (position, complete_history) = match boundary {
        ForkBoundary::Latest => (latest_position, None),
        ForkBoundary::ThroughTurn(turn_id) => {
            let row = find_visible_turn(pool, &lineage, turn_id.as_str()).await?;
            if row.status == "inProgress" {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!("lastTurnId '{turn_id}' identifies an in-progress turn"),
                });
            }
            let rollout_end_ordinal = row
                .rollout_end_ordinal
                .ok_or_else(|| missing_turn_position(turn_id.as_str()))?;
            let rollout_end_byte_offset = row
                .rollout_end_byte_offset
                .ok_or_else(|| missing_turn_position(turn_id.as_str()))?;
            (
                HistoryPosition {
                    thread_id: row.physical_thread_id,
                    end_ordinal_exclusive: u64::try_from(rollout_end_ordinal)
                        .map_err(|_| invalid_turn_position(turn_id.as_str()))?
                        .checked_add(1)
                        .ok_or_else(|| invalid_turn_position(turn_id.as_str()))?,
                    end_byte_offset: u64::try_from(rollout_end_byte_offset)
                        .map_err(|_| invalid_turn_position(turn_id.as_str()))?,
                },
                None,
            )
        }
        ForkBoundary::BeforeTurn(turn_id) => {
            let row = find_source_turn(pool, &lineage, turn_id.as_str()).await?;
            if row.rollout_end_ordinal == Some(row.rollout_ordinal) {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!("turn {turn_id} does not have a persisted start boundary"),
                });
            }
            let rollout_byte_offset = row
                .rollout_byte_offset
                .ok_or_else(|| missing_turn_position(turn_id.as_str()))?;
            (
                HistoryPosition {
                    thread_id: row.physical_thread_id,
                    end_ordinal_exclusive: u64::try_from(row.rollout_ordinal)
                        .map_err(|_| invalid_turn_position(turn_id.as_str()))?,
                    end_byte_offset: u64::try_from(rollout_byte_offset)
                        .map_err(|_| invalid_turn_position(turn_id.as_str()))?,
                },
                None,
            )
        }
        ForkBoundary::ThroughLatestSpineSamplingStarted => {
            let boundary = find_spine_sampling_boundary(&lineage).await?;
            (boundary.position, Some(boundary.complete_history))
        }
    };
    let segment_index = lineage
        .segments()
        .iter()
        .position(|segment| segment.thread_id() == position.thread_id)
        .ok_or_else(|| ThreadStoreError::Internal {
            message: "fork position is outside the source lineage".to_string(),
        })?;
    if lineage.segments()[segment_index].end.is_some_and(|end| {
        position.end_ordinal_exclusive > end.end_ordinal_exclusive
            || position.end_byte_offset > end.end_byte_offset
    }) {
        return Err(ThreadStoreError::InvalidRequest {
            message: "fork boundary exceeds inherited source history".to_string(),
        });
    }
    let history_base =
        if position.end_ordinal_exclusive == lineage.segments()[segment_index].start_ordinal() {
            segment_index
                .checked_sub(1)
                .and_then(|index| lineage.segments()[index].end)
        } else {
            Some(position)
        };
    drop(source_writer_guard);
    let model_context = Arc::new(model_context::load_for_fork(lineage, history_base).await?);

    Ok(PreparedFork::new(
        thread_id,
        history_base,
        model_context,
        complete_history,
        source_reservation,
    ))
}

#[derive(Debug)]
struct SpineSamplingBoundary {
    position: HistoryPosition,
    complete_history: Arc<Vec<RolloutItem>>,
}

async fn find_spine_sampling_boundary(
    lineage: &RolloutLineage,
) -> ThreadStoreResult<SpineSamplingBoundary> {
    let source_path = lineage
        .segments()
        .last()
        .map(|segment| segment.rollout_path.as_path())
        .ok_or_else(|| ThreadStoreError::Internal {
            message: "fork lineage has no source segment".to_string(),
        })?;
    let session_meta = codex_rollout::read_session_meta_line(source_path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to read sampling-boundary metadata {}: {err}",
                source_path.display()
            ),
        })?;
    let lineage = lineage.clone();
    tokio::task::spawn_blocking(move || {
        find_spine_sampling_boundary_blocking(&lineage, session_meta)
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to join sampling-boundary scan: {err}"),
    })?
}

fn find_spine_sampling_boundary_blocking(
    lineage: &RolloutLineage,
    session_meta: SessionMetaLine,
) -> ThreadStoreResult<SpineSamplingBoundary> {
    let mut complete_history = vec![RolloutItem::SessionMeta(session_meta)];
    let mut open_sampling = None;
    for segment in lineage.segments() {
        let file = File::open(segment.rollout_path.as_path()).map_err(|err| {
            ThreadStoreError::Internal {
                message: format!(
                    "failed to open sampling-boundary rollout {}: {err}",
                    segment.rollout_path.display()
                ),
            }
        })?;
        let end_byte_offset = match segment.end {
            Some(end) => end.end_byte_offset,
            None => file
                .metadata()
                .map_err(|err| ThreadStoreError::Internal {
                    message: format!(
                        "failed to read sampling-boundary rollout metadata {}: {err}",
                        segment.rollout_path.display()
                    ),
                })?
                .len(),
        };
        let mut reader = BufReader::new(file);
        let mut byte_offset = 0_u64;
        let mut line = String::new();
        while byte_offset < end_byte_offset {
            line.clear();
            let bytes_read =
                reader
                    .read_line(&mut line)
                    .map_err(|err| ThreadStoreError::Internal {
                        message: format!(
                            "failed to read sampling-boundary rollout {}: {err}",
                            segment.rollout_path.display()
                        ),
                    })?;
            if bytes_read == 0 {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "sampling-boundary cutoff exceeds rollout {}",
                        segment.rollout_path.display()
                    ),
                });
            }
            byte_offset = byte_offset
                .checked_add(
                    u64::try_from(bytes_read).map_err(|_| ThreadStoreError::Internal {
                        message: "sampling-boundary line length overflow".to_string(),
                    })?,
                )
                .ok_or_else(|| ThreadStoreError::Internal {
                    message: "sampling-boundary byte offset overflow".to_string(),
                })?;
            if byte_offset > end_byte_offset {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "sampling-boundary cutoff is not a record boundary in {}",
                        segment.rollout_path.display()
                    ),
                });
            }
            if line.trim().is_empty() {
                continue;
            }
            let record: RolloutLine = serde_json::from_str(line.trim_end()).map_err(|err| {
                ThreadStoreError::InvalidRequest {
                    message: format!(
                        "invalid sampling-boundary record in {}: {err}",
                        segment.rollout_path.display()
                    ),
                }
            })?;
            let ordinal = record
                .ordinal
                .ok_or_else(|| ThreadStoreError::InvalidRequest {
                    message: format!(
                        "paginated sampling-boundary record in {} is missing an ordinal",
                        segment.rollout_path.display()
                    ),
                })?;
            if ordinal < segment.start_ordinal() {
                continue;
            }
            if segment
                .end
                .is_some_and(|end| ordinal >= end.end_ordinal_exclusive)
            {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "sampling-boundary ordinal exceeds inherited source history in {}",
                        segment.rollout_path.display()
                    ),
                });
            }
            let spine_record = match &record.item {
                RolloutItem::SpineSamplingStarted(_) => Some(true),
                RolloutItem::SpineTransition(_) => Some(false),
                _ => None,
            };
            complete_history.push(record.item);
            match spine_record {
                Some(true) => {
                    open_sampling = Some((
                        HistoryPosition {
                            thread_id: segment.thread_id(),
                            end_ordinal_exclusive: ordinal.checked_add(1).ok_or_else(|| {
                                ThreadStoreError::Internal {
                                    message: "sampling-boundary ordinal overflow".to_string(),
                                }
                            })?,
                            end_byte_offset: byte_offset,
                        },
                        complete_history.len(),
                    ));
                }
                Some(false) => open_sampling = None,
                None => {}
            }
        }
    }

    let Some((position, item_count)) = open_sampling else {
        return Err(ThreadStoreError::InvalidRequest {
            message: "source history has no uncommitted Spine sampling boundary".to_string(),
        });
    };
    complete_history.truncate(item_count);
    Ok(SpineSamplingBoundary {
        position,
        complete_history: Arc::new(complete_history),
    })
}

fn missing_turn_position(turn_id: &str) -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: format!("turn {turn_id} does not have persisted rollout positions"),
    }
}

fn invalid_turn_position(turn_id: &str) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("invalid rollout position for turn {turn_id}"),
    }
}

#[cfg(test)]
#[path = "paginated_fork_tests.rs"]
mod tests;
