use std::fs::File;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ModelContextScan;
use codex_rollout::ModelContextScanProgress;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::ScanOutcome;

use super::LocalThreadStore;
use super::read_thread;
use super::rollout_lineage::RolloutLineage;
use crate::LoadThreadHistoryParams;
use crate::StoredModelContext;
use crate::StoredThreadHistory;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

#[cfg(test)]
#[path = "model_context_tests.rs"]
mod tests;

/// Loads rollout items needed to reconstruct the latest model-visible context.
///
/// Plain paginated JSONL rollouts use a reverse scan. When it finds both a usable replacement-
/// history checkpoint and the completed user-turn context needed for resume metadata, the returned
/// replay starts with the canonical head `SessionMeta` followed by that newest suffix. When no
/// bounded cutoff is available, the scan continues to the beginning and returns the complete
/// replay it already accumulated.
///
/// Legacy and compressed rollout shapes keep the existing full-history path.
pub(super) async fn load_latest_model_context(
    store: &LocalThreadStore,
    params: LoadThreadHistoryParams,
) -> ThreadStoreResult<StoredModelContext> {
    let (path, session_meta) = resolve_rollout_source(store, &params).await?;

    let items = if uses_paginated_lineage(path.as_path(), &session_meta) {
        let lineage = store.resolve_rollout_lineage(params.thread_id).await?;
        scan_model_context_from_lineage(lineage, session_meta).await?
    } else {
        read_thread::load_history_items(path.as_path()).await?
    };

    Ok(StoredModelContext {
        thread_id: params.thread_id,
        items,
    })
}

/// Loads the complete logical lineage used by canonical replay.
///
/// This intentionally stays separate from the bounded model-context reader: compaction is a valid
/// model-context checkpoint, but it is not a complete checkpoint for state machines whose
/// canonical records before and after the compact share absolute coordinates.
pub(super) async fn load_complete_history(
    store: &LocalThreadStore,
    params: LoadThreadHistoryParams,
) -> ThreadStoreResult<StoredThreadHistory> {
    let (path, session_meta) = resolve_rollout_source(store, &params).await?;
    let items = if uses_paginated_lineage(path.as_path(), &session_meta) {
        let lineage = store.resolve_rollout_lineage(params.thread_id).await?;
        load_complete_history_from_lineage(lineage, session_meta).await?
    } else {
        read_thread::load_history_items(path.as_path()).await?
    };
    Ok(StoredThreadHistory {
        thread_id: params.thread_id,
        items,
    })
}

pub(super) struct ForkStartupHistory {
    pub(super) model_context: Vec<RolloutItem>,
    pub(super) complete_history: Vec<RolloutItem>,
}

/// Loads startup context from a fork's frozen inherited prefix.
pub(super) async fn load_for_fork(
    lineage: RolloutLineage,
    history_base: Option<HistoryPosition>,
) -> ThreadStoreResult<ForkStartupHistory> {
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
                "failed to read session metadata {}: {err}",
                source_path.display()
            ),
        })?;
    let Some(history_base) = history_base else {
        let items = vec![RolloutItem::SessionMeta(session_meta)];
        return Ok(ForkStartupHistory {
            model_context: items.clone(),
            complete_history: items,
        });
    };
    let lineage = lineage.truncate_at(history_base).await?;
    let (model_context, complete_history) = tokio::try_join!(
        scan_model_context_from_lineage(lineage.clone(), session_meta.clone()),
        load_complete_history_from_lineage(lineage, session_meta),
    )?;
    Ok(ForkStartupHistory {
        model_context,
        complete_history,
    })
}

async fn resolve_rollout_source(
    store: &LocalThreadStore,
    params: &LoadThreadHistoryParams,
) -> ThreadStoreResult<(PathBuf, SessionMetaLine)> {
    let path = read_thread::resolve_rollout_path(store, params.thread_id, params.include_archived)
        .await?
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!("no rollout found for thread id {}", params.thread_id),
        })?;
    let session_meta = codex_rollout::read_session_meta_line(path.as_path())
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read session metadata {}: {err}", path.display()),
        })?;
    if session_meta.meta.id != params.thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout at {} belongs to thread {}, not {}",
                path.display(),
                session_meta.meta.id,
                params.thread_id
            ),
        });
    }
    Ok((path, session_meta))
}

fn uses_paginated_lineage(path: &Path, session_meta: &SessionMetaLine) -> bool {
    matches!(session_meta.meta.history_mode, ThreadHistoryMode::Paginated)
        && !path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .is_some_and(|file_name| file_name.ends_with(".jsonl.zst"))
}

async fn scan_model_context_from_lineage(
    lineage: RolloutLineage,
    session_meta: SessionMetaLine,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    let scan = tokio::task::spawn_blocking(move || {
        scan_model_context_from_lineage_blocking(&lineage, session_meta)
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to join model context scan: {err}"),
    })?;
    match scan {
        Ok(items) => Ok(items),
        Err(err) => Err(ThreadStoreError::Internal {
            message: format!("failed to scan paginated model context lineage: {err}"),
        }),
    }
}

async fn load_complete_history_from_lineage(
    lineage: RolloutLineage,
    session_meta: SessionMetaLine,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    tokio::task::spawn_blocking(move || {
        load_complete_history_from_lineage_blocking(&lineage, session_meta)
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to join complete lineage scan: {err}"),
    })?
}

fn load_complete_history_from_lineage_blocking(
    lineage: &RolloutLineage,
    session_meta: SessionMetaLine,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    let mut items = vec![RolloutItem::SessionMeta(session_meta)];
    for segment in lineage.segments() {
        let file = File::open(segment.rollout_path.as_path()).map_err(|err| {
            ThreadStoreError::Internal {
                message: format!(
                    "failed to open complete lineage {}: {err}",
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
                        "failed to read complete lineage metadata {}: {err}",
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
                            "failed to read complete lineage {}: {err}",
                            segment.rollout_path.display()
                        ),
                    })?;
            if bytes_read == 0 {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "complete lineage cutoff exceeds rollout {}",
                        segment.rollout_path.display()
                    ),
                });
            }
            byte_offset = byte_offset
                .checked_add(
                    u64::try_from(bytes_read).map_err(|_| ThreadStoreError::Internal {
                        message: "complete lineage record length overflow".to_string(),
                    })?,
                )
                .ok_or_else(|| ThreadStoreError::Internal {
                    message: "complete lineage byte offset overflow".to_string(),
                })?;
            if byte_offset > end_byte_offset {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!(
                        "complete lineage cutoff is not a record boundary in {}",
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
                        "invalid complete lineage record in {}: {err}",
                        segment.rollout_path.display()
                    ),
                }
            })?;
            if matches!(&record.item, RolloutItem::SessionMeta(_)) {
                continue;
            }
            match record.ordinal {
                Some(ordinal) => {
                    if ordinal < segment.start_ordinal() {
                        continue;
                    }
                    if segment
                        .end
                        .is_some_and(|end| ordinal >= end.end_ordinal_exclusive)
                    {
                        return Err(ThreadStoreError::InvalidRequest {
                            message: format!(
                                "complete lineage ordinal exceeds inherited history in {}",
                                segment.rollout_path.display()
                            ),
                        });
                    }
                }
                None if segment.end.is_some() || segment.start_ordinal() > 1 => {
                    return Err(ThreadStoreError::InvalidRequest {
                        message: format!(
                            "paginated complete lineage record in {} is missing an ordinal",
                            segment.rollout_path.display()
                        ),
                    });
                }
                None => {}
            }
            items.push(record.item);
        }
    }
    Ok(items)
}

fn scan_model_context_from_lineage_blocking(
    lineage: &RolloutLineage,
    session_meta: SessionMetaLine,
) -> io::Result<Vec<RolloutItem>> {
    let mut scan = ModelContextScan::default();
    'segments: for segment in lineage.segments().iter().rev() {
        let file = File::open(segment.rollout_path.as_path())?;
        let mut scanner = match segment.end.map(|end| end.end_byte_offset) {
            Some(end_byte_offset) => ReverseJsonlScanner::new_at(file, end_byte_offset)?,
            None => ReverseJsonlScanner::new(file)?,
        };
        while let Some(outcome) = scanner.scan_next::<RolloutLine>()? {
            let ScanOutcome::Parsed(line) = outcome else {
                continue;
            };
            // Each physical segment contributes only its local delta. Its head metadata is
            // replaced with the requested thread's canonical SessionMeta after replay.
            if matches!(&line.item, RolloutItem::SessionMeta(_)) {
                break;
            }
            match scan.push(line.item) {
                ModelContextScanProgress::Continue => {}
                ModelContextScanProgress::Complete => break 'segments,
            }
        }
    }

    let canonical_meta = session_meta.clone();
    let mut items = scan.finish(session_meta);
    if !matches!(items.first(), Some(RolloutItem::SessionMeta(_))) {
        items.insert(0, RolloutItem::SessionMeta(canonical_meta));
    }
    Ok(items)
}
