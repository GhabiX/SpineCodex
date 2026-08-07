use std::fs;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SpineSamplingStartedItem;
use codex_protocol::protocol::SpineTransitionItem;
use codex_protocol::protocol::ThreadHistoryMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::find_spine_sampling_boundary;
use crate::local::rollout_lineage::RolloutLineage;
use crate::local::rollout_lineage::RolloutLineageSegment;

#[tokio::test]
async fn selects_latest_uncommitted_sampling_boundary_across_lineage() {
    let home = TempDir::new().expect("temp dir");
    let root_id = ThreadId::new();
    let child_id = ThreadId::new();
    let root_path = write_rollout(
        home.path(),
        root_id,
        /*history_base*/ None,
        0,
        vec![
            event_item(),
            sampling_started("committed"),
            event_item(),
            transition("committed"),
            event_item(),
        ],
    );
    let root_end = history_position(root_path.as_path(), root_id, 6);
    let child_path = write_rollout(
        home.path(),
        child_id,
        Some(root_end),
        6,
        vec![
            event_item(),
            sampling_started("open"),
            event_item(),
            event_item(),
        ],
    );
    let expected_position = history_position(child_path.as_path(), child_id, 9);
    let lineage = RolloutLineage {
        segments: vec![
            RolloutLineageSegment {
                thread_id: root_id,
                rollout_path: root_path,
                start_ordinal: 1,
                end: Some(root_end),
            },
            RolloutLineageSegment {
                thread_id: child_id,
                rollout_path: child_path,
                start_ordinal: 7,
                end: None,
            },
        ],
    };

    let selected = find_spine_sampling_boundary(&lineage)
        .await
        .expect("select open sampling");

    assert_eq!(selected.position, expected_position);
    let serialized = serde_json::to_value(selected.complete_history.as_ref())
        .expect("serialize selected history");
    let selected_items = serialized.as_array().expect("history array");
    assert_eq!(selected_items.len(), 8);
    assert_eq!(
        selected_items.last().expect("sampling marker"),
        &serde_json::to_value(sampling_started("open")).expect("serialize marker")
    );
}

#[tokio::test]
async fn selects_open_sampling_boundary_from_ancestor_segment() {
    let home = TempDir::new().expect("temp dir");
    let root_id = ThreadId::new();
    let child_id = ThreadId::new();
    let root_path = write_rollout(
        home.path(),
        root_id,
        /*history_base*/ None,
        0,
        vec![
            event_item(),
            sampling_started("ancestor-open"),
            event_item(),
        ],
    );
    let expected_position = history_position(root_path.as_path(), root_id, 3);
    let root_end = history_position(root_path.as_path(), root_id, 4);
    let child_path = write_rollout(
        home.path(),
        child_id,
        Some(root_end),
        4,
        vec![event_item(), event_item()],
    );
    let lineage = RolloutLineage {
        segments: vec![
            RolloutLineageSegment {
                thread_id: root_id,
                rollout_path: root_path,
                start_ordinal: 1,
                end: Some(root_end),
            },
            RolloutLineageSegment {
                thread_id: child_id,
                rollout_path: child_path,
                start_ordinal: 5,
                end: None,
            },
        ],
    };

    let selected = find_spine_sampling_boundary(&lineage)
        .await
        .expect("select inherited open sampling");

    assert_eq!(selected.position, expected_position);
    assert_eq!(selected.complete_history.len(), 3);
}

#[tokio::test]
async fn rejects_missing_or_committed_sampling_boundary() {
    let home = TempDir::new().expect("temp dir");
    for (case, items) in [
        ("missing", vec![event_item()]),
        (
            "committed",
            vec![sampling_started("started"), transition("finished")],
        ),
    ] {
        let thread_id = ThreadId::new();
        let path = write_rollout(home.path(), thread_id, /*history_base*/ None, 0, items);
        let lineage = RolloutLineage {
            segments: vec![RolloutLineageSegment {
                thread_id,
                rollout_path: path,
                start_ordinal: 1,
                end: None,
            }],
        };

        let err = find_spine_sampling_boundary(&lineage)
            .await
            .expect_err(case);

        assert!(
            err.to_string()
                .contains("no uncommitted Spine sampling boundary"),
            "{case}: {err}"
        );
    }
}

fn write_rollout(
    home: &Path,
    thread_id: ThreadId,
    history_base: Option<HistoryPosition>,
    session_meta_ordinal: u64,
    items: Vec<RolloutItem>,
) -> PathBuf {
    let path = home.join(format!("rollout-{thread_id}.jsonl"));
    let session_meta = RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            session_id: thread_id.into(),
            id: thread_id,
            history_mode: ThreadHistoryMode::Paginated,
            history_base,
            ..SessionMeta::default()
        },
        git: None,
    });
    let lines = std::iter::once(session_meta)
        .chain(items)
        .enumerate()
        .map(|(offset, item)| {
            serde_json::to_string(&RolloutLine {
                timestamp: "2026-08-07T00:00:00Z".to_string(),
                ordinal: Some(
                    session_meta_ordinal
                        .checked_add(u64::try_from(offset).expect("fixture ordinal"))
                        .expect("fixture ordinal overflow"),
                ),
                item,
            })
            .expect("serialize rollout line")
        })
        .collect::<Vec<_>>();
    fs::write(path.as_path(), format!("{}\n", lines.join("\n"))).expect("write rollout");
    path
}

fn history_position(
    path: &Path,
    thread_id: ThreadId,
    end_ordinal_exclusive: u64,
) -> HistoryPosition {
    let contents = fs::read(path).expect("read rollout");
    let mut byte_offset = 0_u64;
    for line in contents.split_inclusive(|byte| *byte == b'\n') {
        let record: RolloutLine = serde_json::from_slice(line).expect("parse rollout line");
        if record.ordinal == Some(end_ordinal_exclusive) {
            break;
        }
        byte_offset = byte_offset
            .checked_add(u64::try_from(line.len()).expect("fixture line length"))
            .expect("fixture offset overflow");
    }
    HistoryPosition {
        thread_id,
        end_ordinal_exclusive,
        end_byte_offset: byte_offset,
    }
}

fn event_item() -> RolloutItem {
    RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::ShutdownComplete)
}

fn sampling_started(label: &str) -> RolloutItem {
    RolloutItem::SpineSamplingStarted(SpineSamplingStartedItem {
        version: 1,
        payload: json!({ "label": label }),
    })
}

fn transition(label: &str) -> RolloutItem {
    RolloutItem::SpineTransition(SpineTransitionItem {
        version: 1,
        payload: json!({ "label": label }),
    })
}
