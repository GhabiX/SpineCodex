//! Reconstructs model context and preserves source runtime metadata across fork cutoffs.

use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ModelContextScan;
use codex_rollout::ModelContextScanProgress;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::ScanOutcome;

use super::LocalThreadStore;
use super::read_thread;
use super::rollout_lineage::RolloutLineage;
use super::thread_rollout_resolver;
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
/// Paginated JSONL rollouts use a reverse scan. It stops at the newest `CompactedItem` with both
/// replacement history and a window number, and returns that compaction plus its newer suffix. If
/// the newest compaction lacks either field, the scan continues to the beginning of the rollout.
///
/// Compressed segments are decoded before applying their original JSONL offsets. Legacy rollouts
/// keep the existing full-history path.
pub(super) async fn load_latest_model_context(
    store: &LocalThreadStore,
    params: LoadThreadHistoryParams,
) -> ThreadStoreResult<StoredModelContext> {
    let resolved = if params.include_archived {
        thread_rollout_resolver::resolve_current_including_archived(store, params.thread_id).await?
    } else {
        thread_rollout_resolver::resolve_current(store, params.thread_id).await?
    };
    let path =
        resolved
            .map(|resolved| resolved.path)
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: format!("no rollout found for thread id {}", params.thread_id),
            })?;

    load_from_rollout_path(store, params.thread_id, &path).await
}

pub(super) async fn load_from_rollout_path(
    store: &LocalThreadStore,
    thread_id: codex_protocol::ThreadId,
    path: &Path,
) -> ThreadStoreResult<StoredModelContext> {
    let before = super::history_revision::read(path).await;
    let session_meta = codex_rollout::read_session_meta_line(path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read session metadata {}: {err}", path.display()),
        })?;
    if session_meta.meta.id != thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout at {} belongs to thread {}, not {}",
                path.display(),
                session_meta.meta.id,
                thread_id
            ),
        });
    }

    let items = if matches!(session_meta.meta.history_mode, ThreadHistoryMode::Paginated) {
        let lineage = store
            .resolve_rollout_lineage(thread_id, Some(path.to_path_buf()))
            .await?;
        scan_model_context_from_lineage(lineage, session_meta).await?
    } else {
        read_thread::load_history_items(path).await?
    };

    let after = super::history_revision::read(path).await;
    Ok(StoredModelContext {
        revision: before.filter(|revision| Some(revision) == after.as_ref()),
        thread_id,
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
    let before = super::history_revision::read(&path).await;
    let items = if matches!(session_meta.meta.history_mode, ThreadHistoryMode::Paginated) {
        let lineage = store
            .resolve_rollout_lineage(params.thread_id, Some(path.clone()))
            .await?;
        load_complete_history_from_lineage(lineage, session_meta).await?
    } else {
        read_thread::load_history_items(path.as_path()).await?
    };
    let after = super::history_revision::read(&path).await;
    Ok(StoredThreadHistory {
        revision: before.filter(|revision| Some(revision) == after.as_ref()),
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
    let mut session_meta = codex_rollout::read_session_meta_line(source_path)
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to read session metadata {}: {err}",
                source_path.display()
            ),
        })?;
    if session_meta.meta.multi_agent_version.is_none() {
        // Recover only the runtime version before applying the fork cutoff. Stop at the
        // newest version-bearing context instead of retaining the source's full replay.
        let source_lineage = lineage.clone();
        let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
        session_meta.meta.multi_agent_version = tokio::task::spawn_blocking(move || {
            for segment in source_lineage.segments().iter().rev() {
                let file = codex_rollout::open_rollout_seekable_reader(
                    segment.rollout_path.as_path(),
                    || stop.is_closed(),
                )?;
                let mut scanner = match segment.end.map(|end| end.end_byte_offset) {
                    Some(end_byte_offset) => ReverseJsonlScanner::new_at(file, end_byte_offset)?,
                    None => ReverseJsonlScanner::new(file)?,
                };
                while let Some(outcome) = scanner.scan_next_rollout_line()? {
                    let ScanOutcome::Parsed(line) = outcome else {
                        continue;
                    };
                    if let Some(version) = codex_rollout::resume_multi_agent_version(&line.item) {
                        return Ok(Some(version));
                    }
                    // Ancestor metadata does not describe the immediate source's runtime.
                    if matches!(line.item, RolloutItem::SessionMeta(_)) {
                        break;
                    }
                }
            }
            Ok::<_, io::Error>(None)
        })
        .await
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to join fork runtime version scan: {err}"),
        })?
        .map_err(|err| ThreadStoreError::Internal {
            message: format!("failed to read fork runtime version: {err}"),
        })?;
        drop(keep_running);
    }
    let Some(history_base) = history_base else {
        let items = vec![RolloutItem::SessionMeta(Box::new(session_meta))];
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
    let resolved = if params.include_archived {
        thread_rollout_resolver::resolve_current_including_archived(store, params.thread_id).await?
    } else {
        thread_rollout_resolver::resolve_current(store, params.thread_id).await?
    };
    let path =
        resolved
            .map(|resolved| resolved.path)
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

async fn scan_model_context_from_lineage(
    lineage: RolloutLineage,
    session_meta: SessionMetaLine,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
    let scan = tokio::task::spawn_blocking(move || {
        scan_model_context_from_lineage_blocking(&lineage, session_meta, || stop.is_closed())
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to join model context scan: {err}"),
    })?;
    drop(keep_running);
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
    let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
    let result = tokio::task::spawn_blocking(move || {
        load_complete_history_from_lineage_blocking(&lineage, session_meta, || stop.is_closed())
    })
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to join complete lineage scan: {err}"),
    })?;
    drop(keep_running);
    result
}

fn load_complete_history_from_lineage_blocking(
    lineage: &RolloutLineage,
    session_meta: SessionMetaLine,
    is_cancelled: impl Fn() -> bool,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    let mut items = vec![RolloutItem::SessionMeta(Box::new(session_meta))];
    for segment in lineage.segments() {
        let mut file = codex_rollout::open_rollout_seekable_reader(
            segment.rollout_path.as_path(),
            &is_cancelled,
        )
        .map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to open complete lineage {}: {err}",
                segment.rollout_path.display()
            ),
        })?;
        let end_byte_offset = match segment.end {
            Some(end) => end.end_byte_offset,
            None => file
                .seek(SeekFrom::End(0))
                .map_err(|err| ThreadStoreError::Internal {
                    message: format!(
                        "failed to read complete lineage metadata {}: {err}",
                        segment.rollout_path.display()
                    ),
                })?,
        };
        file.rewind().map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to rewind complete lineage {}: {err}",
                segment.rollout_path.display()
            ),
        })?;
        let mut reader = BufReader::new(file);
        let mut byte_offset = 0_u64;
        let mut line = String::new();
        while byte_offset < end_byte_offset {
            let line_start_byte_offset = byte_offset;
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
            let record = match codex_rollout::parse_rollout_line(line.trim_end()) {
                Ok(record) => record,
                Err(err) => {
                    tracing::warn!(
                        rollout_path = %segment.rollout_path.display(),
                        line_start_byte_offset,
                        line_end_byte_offset = byte_offset,
                        error = %err,
                        "skipping rejected complete lineage record"
                    );
                    continue;
                }
            };
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
    is_cancelled: impl Fn() -> bool,
) -> io::Result<Vec<RolloutItem>> {
    let mut scan = ModelContextScan::default();
    'segments: for segment in lineage.segments().iter().rev() {
        let file = codex_rollout::open_rollout_seekable_reader(
            segment.rollout_path.as_path(),
            &is_cancelled,
        )?;
        let mut scanner = match segment.end.map(|end| end.end_byte_offset) {
            Some(end_byte_offset) => ReverseJsonlScanner::new_at(file, end_byte_offset)?,
            None => ReverseJsonlScanner::new(file)?,
        };
        while let Some(outcome) = scanner.scan_next_rollout_line()? {
            let ScanOutcome::Parsed(line) = outcome else {
                continue;
            };
            // Each rollout segment contributes only its local delta. Its session metadata is
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

    let mut items = scan.finish();
    items.insert(0, RolloutItem::SessionMeta(Box::new(session_meta)));
    Ok(items)
}
