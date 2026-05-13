use super::*;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use tempfile::TempDir;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn temp_store() -> (TempDir, SpineSidecarStore) {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T15-38-00-thread.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("store path");
    (temp, store)
}

fn read_json_lines(path: impl AsRef<Path>) -> Vec<Value> {
    let contents = std::fs::read_to_string(path).expect("read jsonl");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse json line"))
        .collect()
}

fn read_json(path: impl AsRef<Path>) -> Value {
    let contents = std::fs::read_to_string(path).expect("read json");
    serde_json::from_str(&contents).expect("parse json")
}

fn assistant_rollout_item(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(codex_protocol::models::ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![codex_protocol::models::ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

fn event_rollout_item() -> RolloutItem {
    RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::Warning(
        codex_protocol::protocol::WarningEvent {
            message: "not a response item".to_string(),
        },
    ))
}

#[test]
fn creates_and_reads_base_locator_for_rollout_path() {
    let rollout_path = Path::new("/tmp/sessions/2026/05/10/rollout-2026-thread.jsonl");

    let sidecar_path =
        SpineSidecarStore::default_sidecar_dir_for_rollout(rollout_path).expect("sidecar path");

    assert_eq!(
        sidecar_path,
        Path::new("/tmp/sessions/2026/05/10/spine-rollout-2026-thread")
    );
    assert_eq!(
        SpineSidecarStore::locator_path_for_rollout(rollout_path).expect("locator path"),
        Path::new("/tmp/sessions/2026/05/10/rollout-2026-thread.spine.json")
    );

    let no_parent =
        SpineSidecarStore::default_sidecar_dir_for_rollout(Path::new("rollout-2026-thread.jsonl"))
            .expect_err("relative rollout without a parent should fail");
    assert!(matches!(
        no_parent,
        SpineStoreError::InvalidRolloutPath { reason, .. }
            if reason == "rollout path must include a parent directory"
    ));

    let wrong_extension = SpineSidecarStore::default_sidecar_dir_for_rollout(Path::new(
        "/tmp/rollout-2026-thread.log",
    ))
    .expect_err("non-jsonl rollout should fail");
    assert!(matches!(
        wrong_extension,
        SpineStoreError::InvalidRolloutPath { reason, .. }
            if reason == "rollout path must use the .jsonl extension"
    ));
}

#[test]
fn for_rollout_requires_base_locator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");

    let error = SpineSidecarStore::for_rollout(&rollout_path)
        .expect_err("locator is required when loading a spine store");

    assert!(
        matches!(error, SpineStoreError::Io { path, .. } if path.ends_with("rollout-test.spine.json"))
    );
}

#[test]
fn for_rollout_migrates_default_sidecar_without_locator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");
    let root = SpineSidecarStore::default_sidecar_dir_for_rollout(&rollout_path)
        .expect("default sidecar dir");
    std::fs::create_dir_all(&root).expect("create legacy sidecar root");
    std::fs::write(
        root.join("tree.jsonl"),
        serde_json::to_string(&json!({
            "type": "node_created",
            "seq": 1,
            "node_id": "1",
            "parent_id": null,
            "raw_start_ordinal": 0,
        }))
        .expect("serialize root event")
            + "\n",
    )
    .expect("write legacy tree");
    std::fs::write(root.join("compact.index.jsonl"), "").expect("write compact index");

    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("load legacy sidecar");

    assert_eq!(store.root(), root.as_path());
    assert_eq!(
        store.load().expect("load migrated sidecar").cursor(),
        &id(&[1])
    );
    assert_eq!(
        read_json(SpineSidecarStore::locator_path_for_rollout(&rollout_path).expect("locator")),
        json!({
            "version": 1,
            "base": "spine-rollout-test",
        })
    );
}

#[test]
fn create_for_rollout_writes_base_locator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp
        .path()
        .join("sessions")
        .join("2026")
        .join("05")
        .join("12")
        .join("rollout-test.jsonl");

    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("create store");
    let loaded = SpineSidecarStore::for_rollout(&rollout_path).expect("load store");

    assert_eq!(loaded, store);
    assert_eq!(
        read_json(SpineSidecarStore::locator_path_for_rollout(&rollout_path).expect("locator")),
        json!({
            "version": 1,
            "base": "spine-rollout-test",
        })
    );
}

#[test]
fn create_writes_root_ledger_and_state_cache() {
    let (_temp, store) = temp_store();

    let state = store.create().expect("create sidecar");

    assert_eq!(state.cursor(), &id(&[1]));
    assert_eq!(
        read_json_lines(store.tree_path()),
        vec![json!({
            "type": "node_created",
            "seq": 1,
            "node_id": "1",
            "parent_id": null,
            "raw_start_ordinal": 0,
        })]
    );
    assert_eq!(
        read_json(store.state_path()),
        json!({
            "cursor": "1",
            "nodes": [{
                "node_id": "1",
                "parent_id": null,
                "raw_start_ordinal": 0,
                "status": "live",
                "summary": null,
                "worklog_path": "nodes/1/worklog.md",
                "plan_path": "nodes/1/plan.json",
            }]
        })
    );
    assert!(store.root().join("nodes").join("1").is_dir());
    assert!(store.trajs_index_path().exists());
    assert!(store.compact_index_path().exists());
    assert!(store.raw_rollout_path().exists());
}

#[test]
fn records_transition_summary_and_replays_from_tree() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    let transition = store
        .record_transition(&mut state, SpineOperation::Open, "root scope", 8, "turn-1")
        .expect("record transition");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[1, 1]),
        }
    );
    assert!(!store.worklog_path(&id(&[1])).exists());
    assert!(store.root().join("nodes").join("1").join("1").is_dir());
    assert!(!store.root().join("nodes").join("1.1").exists());
    assert_eq!(
        read_json_lines(store.tree_path()),
        vec![
            json!({
                "type": "node_created",
                "seq": 1,
                "node_id": "1",
                "parent_id": null,
                "raw_start_ordinal": 0,
            }),
            json!({
                "type": "transition_applied",
                "seq": 2,
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
                "to_parent_id": "1",
                "summary": "root scope",
                "raw_start_ordinal": 8,
                "source_turn_id": "turn-1",
            }),
        ]
    );

    let loaded = store.load().expect("load sidecar");

    assert_eq!(loaded, state);
    assert_eq!(
        loaded
            .node(&id(&[1, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(8)
    );
}

#[test]
fn records_root_epoch_archive_and_replays_from_tree() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, "root scope", 8, "turn-1")
        .expect("record root open");
    store
        .record_transition(
            &mut state,
            SpineOperation::Open,
            "child scope",
            13,
            "turn-2",
        )
        .expect("record nested open");

    let transition = store
        .record_root_epoch_archive(
            &mut state,
            "context compacted",
            21,
            "compact-1",
            "turn-compact",
        )
        .expect("record root archive");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 2]),
        }
    );
    assert!(store.root().join("nodes").join("1").join("2").is_dir());
    assert_eq!(
        read_json_lines(store.tree_path())[3],
        json!({
            "type": "root_epoch_archived",
            "seq": 4,
            "archived_root_id": "1.1",
            "next_root_id": "1.2",
            "next_parent_id": "1",
            "summary": "context compacted",
            "raw_start_ordinal": 21,
            "compact_id": "compact-1",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");

    assert_eq!(loaded, state);
    assert_eq!(loaded.cursor(), &id(&[1, 2]));
    assert_eq!(
        loaded.node(&id(&[1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 2]))
            .and_then(|node| node.raw_start_ordinal),
        Some(21)
    );
}

#[test]
fn root_cursor_archive_creates_epoch_under_hidden_root() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    let transition = store
        .record_root_epoch_archive(
            &mut state,
            "context compacted",
            7,
            "compact-root",
            "turn-compact",
        )
        .expect("record root cursor archive");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 2]),
        }
    );
    assert_eq!(
        read_json_lines(store.tree_path())[1],
        json!({
            "type": "root_epoch_archived",
            "seq": 2,
            "archived_root_id": "1.1",
            "next_root_id": "1.2",
            "next_parent_id": "1",
            "summary": "context compacted",
            "raw_start_ordinal": 7,
            "compact_id": "compact-root",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");
    assert_eq!(loaded.cursor(), &id(&[1, 2]));
    assert_eq!(
        loaded
            .node(&id(&[1]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 2]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
}

#[test]
fn generated_worklog_sections_do_not_break_transition_replay_hash() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, "root scope", 8, "turn-1")
        .expect("record transition");

    store
        .append_worklog_section(&id(&[1]), "\n\n## Auto Compact\n\ngenerated summary\n")
        .expect("append generated section");

    let worklog = std::fs::read_to_string(store.worklog_path(&id(&[1]))).expect("read worklog");
    assert!(worklog.contains("spine:auto-compact-generated"));
    assert!(worklog.contains("generated summary"));

    let loaded = store.load().expect("load sidecar");

    assert_eq!(loaded, state);
}

#[test]
fn appends_node_trajs_next_to_worklog() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, "root scope", 8, "turn-1")
        .expect("record transition");

    store
        .append_worklog_section(&id(&[1, 1]), "\n\n## Auto Compact\n\ngenerated summary\n")
        .expect("append generated worklog");
    store
        .append_node_trajs_items(&id(&[1, 1]), &[assistant_rollout_item("folded suffix")])
        .expect("append node trajs");

    let worklog_path = store.worklog_path(&id(&[1, 1]));
    let trajs_path = store.node_trajs_path(&id(&[1, 1]));
    assert_eq!(worklog_path.parent(), trajs_path.parent());
    assert_eq!(trajs_path, store.root().join("nodes/1/1/trajs.jsonl"));
    assert!(worklog_path.exists());
    assert_eq!(
        read_json_lines(trajs_path),
        vec![json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "folded suffix",
                }],
            },
        })]
    );
}

#[test]
fn state_cache_mismatch_fails_fast() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, "root scope", 8, "turn-1")
        .expect("record transition");
    let mut cache = read_json(store.state_path());
    cache["cursor"] = json!("9");
    std::fs::write(
        store.state_path(),
        serde_json::to_string_pretty(&cache).expect("serialize cache"),
    )
    .expect("write mutated cache");

    let error = store.load().expect_err("mismatched cache should fail");

    assert!(matches!(error, SpineStoreError::StateCacheMismatch { .. }));
}

#[test]
fn writes_plan_snapshot_without_planbridge_integration() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    let plan = json!({
        "items": [{
            "text": "implement sidecar store",
            "status": "in_progress",
        }]
    });

    let path = store
        .write_plan(&id(&[1]), &plan)
        .expect("write plan snapshot");

    assert_eq!(path, store.root().join("nodes/1/plan.json"));
    assert_eq!(read_json(path), plan);
}

#[test]
fn writes_plan_snapshot_with_plantree_and_replays_without_mutating_state() {
    let (_temp, store) = temp_store();
    let state = store.create().expect("create sidecar");
    let snapshot = PlanSnapshot {
        node_id: "1".to_string(),
        revision: 1,
        explanation: Some("group upcoming checkpoints".to_string()),
        items: vec![PlanSnapshotItem {
            stable_task_id: "step-1".to_string(),
            step: "plan scope tree".to_string(),
            status: "in_progress".to_string(),
        }],
        spine_plantree: Some(PlanTreeSnapshot {
            anchor_node_id: "1".to_string(),
            root: crate::spine::plan_bridge::PlanTreeScope {
                existing_node_id: Some("1".to_string()),
                summary: "Fix task".to_string(),
                status: Some("in_progress".to_string()),
                checkpoints: Vec::new(),
                children: vec![
                    crate::spine::plan_bridge::PlanTreeScope {
                        existing_node_id: None,
                        summary: "Reproduce".to_string(),
                        status: Some("pending".to_string()),
                        checkpoints: vec![crate::spine::plan_bridge::PlanTreeCheckpoint {
                            task: "run repro".to_string(),
                            status: "pending".to_string(),
                        }],
                        children: Vec::new(),
                    },
                    crate::spine::plan_bridge::PlanTreeScope {
                        existing_node_id: Some("1".to_string()),
                        summary: "Continue root".to_string(),
                        status: Some("pending".to_string()),
                        checkpoints: vec![crate::spine::plan_bridge::PlanTreeCheckpoint {
                            task: "keep root task focused".to_string(),
                            status: "pending".to_string(),
                        }],
                        children: Vec::new(),
                    },
                ],
            },
        }),
        source_turn_id: "turn-alloc".to_string(),
        event_seq: 2,
    };

    let path = store
        .write_plan_snapshot(&id(&[1]), &snapshot)
        .expect("write plan snapshot");

    assert_eq!(path, store.root().join("nodes/1/plan.json"));
    assert_eq!(
        store.read_plan_revision(&id(&[1])).expect("revision"),
        Some(1)
    );
    assert_eq!(
        store
            .read_plan_snapshot(&id(&[1]))
            .expect("read plan snapshot"),
        Some(snapshot)
    );
    assert_eq!(
        read_json_lines(store.tree_path()),
        vec![
            json!({
                "type": "node_created",
                "seq": 1,
                "node_id": "1",
                "parent_id": null,
                "raw_start_ordinal": 0,
            }),
            json!({
                "type": "task_plan_updated",
                "seq": 2,
                "node_id": "1",
                "revision": 1,
                "explanation": "group upcoming checkpoints",
                "items": [
                    {
                        "stable_task_id": "step-1",
                        "step": "plan scope tree",
                        "status": "in_progress",
                    }
                ],
                "spine_plantree": {
                    "anchor_node_id": "1",
                    "root": {
                        "existing_node_id": "1",
                        "summary": "Fix task",
                        "status": "in_progress",
                        "children": [
                            {
                                "existing_node_id": null,
                                "summary": "Reproduce",
                                "status": "pending",
                                "checkpoints": [{"task": "run repro", "status": "pending"}],
                            },
                            {
                                "existing_node_id": "1",
                                "summary": "Continue root",
                                "status": "pending",
                                "checkpoints": [{"task": "keep root task focused", "status": "pending"}],
                            },
                        ],
                    },
                },
                "source_turn_id": "turn-alloc",
            }),
        ]
    );
    assert_eq!(store.load().expect("reload sidecar"), state);
}

#[test]
fn appends_trajs_index_without_raw_rollout_payload() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_raw_items_recorded(&id(&[1]), "turn-1", 0, 3)
        .expect("append raw items index");
    store
        .append_transition_committed(
            "call-1",
            SpineOperation::Open,
            &id(&[1]),
            &id(&[1, 1]),
            0,
            8,
        )
        .expect("append transition index");

    let events = read_json_lines(store.trajs_index_path());

    assert_eq!(
        events,
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 3,
            }),
            json!({
                "type": "transition_committed",
                "seq": 2,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
                "call_start_ordinal": 0,
                "boundary_end": 8,
            }),
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| event.get("raw_payload").is_none())
    );
}

#[test]
fn estimates_raw_response_tokens_from_raw_mirror_response_items_only() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_raw_mirror_items(&[
            assistant_rollout_item("alpha"),
            event_rollout_item(),
            assistant_rollout_item("beta beta"),
            assistant_rollout_item("gamma"),
        ])
        .expect("append raw mirror items");

    let all = store
        .estimate_raw_response_tokens(0, 3)
        .expect("estimate all response items");
    let suffix = store
        .estimate_raw_response_tokens(1, 3)
        .expect("estimate suffix response items");

    assert!(all > suffix);
    assert!(suffix > 0);
}

#[test]
fn records_size_hint_emission_without_changing_replayed_state() {
    let (_temp, store) = temp_store();
    let state = store.create().expect("create sidecar");

    assert!(
        !store
            .has_size_hint_emitted(&id(&[1]), 30_000)
            .expect("query missing hint")
    );
    store
        .append_size_hint_emitted(&id(&[1]), 30_000, 31_200, "runtime_observation")
        .expect("append hint event");

    assert!(
        store
            .has_size_hint_emitted(&id(&[1]), 30_000)
            .expect("query emitted hint")
    );
    assert!(
        !store
            .has_size_hint_emitted(&id(&[1]), 50_000)
            .expect("query other threshold")
    );
    assert_eq!(store.load().expect("load sidecar"), state);
    assert_eq!(
        read_json_lines(store.tree_path())[1],
        json!({
            "type": "spine_hint_emitted",
            "seq": 2,
            "node_id": "1",
            "threshold_tokens": 30000,
            "estimated_tokens": 31200,
            "source": "runtime_observation",
        })
    );
}

#[test]
fn validates_matching_open_for_close_scope() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    let missing = store
        .validate_matching_open_for_scope(&id(&[1]), 4)
        .expect_err("missing matching open should fail");
    assert!(
        matches!(missing, SpineStoreError::InvalidLedger(message) if message.contains("matching open"))
    );

    store
        .append_transition_committed(
            "call-1",
            SpineOperation::Open,
            &id(&[1]),
            &id(&[1, 1]),
            0,
            2,
        )
        .expect("append open transition");

    store
        .validate_matching_open_for_scope(&id(&[1]), 4)
        .expect("matching open validates scope");
}

#[test]
fn appends_compact_index_and_raw_mirror_events() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_raw_mirror_items(&[RolloutItem::ResponseItem(
            codex_protocol::models::ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![codex_protocol::models::ContentItem::OutputText {
                    text: "raw item".to_string(),
                }],
                phase: None,
            },
        )])
        .expect("append raw mirror item");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");
    store
        .append_compact_installed(
            "compact-1",
            &id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
            7,
            "nodes/1/2/worklog.md",
            "sha1:abc",
        )
        .expect("append compact installed");
    store
        .append_raw_mirror_compact_checkpoint("compact-1", "sha1:abc", 7)
        .expect("append raw mirror checkpoint");

    assert_eq!(
        read_json_lines(store.compact_index_path()),
        vec![
            json!({
                "type": "compact_started",
                "seq": 1,
                "compact_id": "compact-1",
                "node_id": "1.2",
                "op": "next",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "strategy": "codex_builtin_text",
                "raw_trajs": "raw/rollout.raw.jsonl",
                "rollout": "../rollout.jsonl",
            }),
            json!({
                "type": "compact_installed",
                "seq": 2,
                "compact_id": "compact-1",
                "node_id": "1.2",
                "op": "next",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "replacement_history_len": 7,
                "worklog_path": "nodes/1/2/worklog.md",
                "message_hash": "sha1:abc",
            }),
        ]
    );

    let raw_mirror = read_json_lines(store.raw_rollout_path());
    assert_eq!(raw_mirror[0]["type"], "response_item");
    assert_eq!(
        raw_mirror[1],
        json!({
            "type": "raw_mirror_event",
            "compact_id": "compact-1",
            "message_hash": "sha1:abc",
            "replacement_history_len": 7,
        })
    );
}

#[test]
fn compact_index_started_without_terminal_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");

    let error = store.load().expect_err("dangling compact should fail");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("dangling compact_started for compact-1")
    ));
}

#[test]
fn projection_reset_replays_projected_state_and_copies_artifacts() {
    let (temp, source_store) = temp_store();
    let mut source_state = source_store.create().expect("create source");
    source_store
        .record_transition(
            &mut source_state,
            SpineOperation::Open,
            "source scope",
            2,
            "turn-1",
        )
        .expect("record source transition");
    source_store
        .append_worklog_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nsource worklog\n")
        .expect("write source worklog");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(source_state.clone(), "fork_seed", None)
        .expect("record projection reset");
    child_store
        .copy_node_artifacts_from(&source_store, source_state.nodes().keys())
        .expect("copy artifacts");

    let replayed = child_store.load().expect("load child projection");
    assert_eq!(replayed.cursor(), &id(&[1, 1]));
    assert_eq!(
        replayed.node(&id(&[1])).expect("root").summary.as_deref(),
        Some("source scope")
    );
    assert!(
        child_store
            .read_worklog(&id(&[1, 1]))
            .expect("read copied worklog")
            .contains("source worklog")
    );
    assert_eq!(read_json_lines(child_store.tree_path()).len(), 2);
}

#[test]
fn root_cursor_archive_creates_epoch_under_hidden_root() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    let transition = store
        .record_root_epoch_archive(
            &mut state,
            "context compacted",
            7,
            "compact-root",
            "turn-compact",
        )
        .expect("record root cursor archive");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 2]),
        }
    );
    assert_eq!(
        read_json_lines(store.tree_path())[1],
        json!({
            "type": "root_epoch_archived",
            "seq": 2,
            "archived_root_id": "1.1",
            "next_root_id": "1.2",
            "next_parent_id": "1",
            "summary": "context compacted",
            "raw_start_ordinal": 7,
            "compact_id": "compact-root",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");
    assert_eq!(loaded.cursor(), &id(&[1, 2]));
    assert_eq!(
        loaded
            .node(&id(&[1]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 2]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
}

#[test]
fn projected_artifact_copy_filters_non_surviving_turn_files() {
    let (temp, source_store) = temp_store();
    let mut source_state = source_store.create().expect("create source");
    source_store
        .record_transition(
            &mut source_state,
            SpineOperation::Open,
            "source scope",
            2,
            "surviving-turn",
        )
        .expect("record source transition");
    source_store
        .append_worklog_section(&id(&[1]), "\n\n## Auto Compact\n\nsurviving worklog\n")
        .expect("write surviving worklog");
    source_store
        .append_worklog_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nrolled back worklog\n")
        .expect("write rolled back worklog");
    source_store
        .write_plan_snapshot(
            &id(&[1, 1]),
            &PlanSnapshot {
                node_id: "1.1".to_string(),
                revision: 1,
                explanation: None,
                items: vec![PlanSnapshotItem {
                    stable_task_id: "step-1".to_string(),
                    step: "rolled back plan".to_string(),
                    status: "in_progress".to_string(),
                }],
                scope_allocation: None,
                source_turn_id: "rolled-back-turn".to_string(),
                event_seq: source_store
                    .next_tree_event_seq()
                    .expect("next tree seq for plan"),
            },
        )
        .expect("write rolled back plan");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(source_state.clone(), "fork_seed", None)
        .expect("record projection reset");
    child_store
        .copy_projected_node_artifacts_from(
            &source_store,
            source_state.nodes().keys(),
            &HashSet::from(["surviving-turn".to_string()]),
        )
        .expect("copy projected artifacts");

    assert!(
        child_store
            .read_worklog(&id(&[1]))
            .expect("read copied surviving worklog")
            .contains("surviving worklog")
    );
    assert!(matches!(
        child_store.read_worklog(&id(&[1, 1])),
        Err(SpineStoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!child_store.plan_path(&id(&[1, 1])).exists());
}

#[test]
fn compact_index_started_then_installed_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");
    store
        .append_compact_installed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "nodes/1/worklog.md",
            "sha1:abc",
        )
        .expect("append compact installed");

    store.load().expect("resolved compact should load");
}

#[test]
fn compact_index_started_then_failed_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");
    store
        .append_compact_failed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "strategy failed",
        )
        .expect("append compact failed");

    store.load().expect("failed compact should be terminal");
}

#[test]
fn compact_index_terminal_without_started_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_installed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "nodes/1/worklog.md",
            "sha1:abc",
        )
        .expect("append compact installed");

    let error = store
        .load()
        .expect_err("terminal without started should fail");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("compact_installed without matching compact_started")
    ));
}

#[test]
fn compact_index_terminal_mismatch_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");
    store
        .append_compact_installed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Close,
            4,
            9,
            7,
            "nodes/1/worklog.md",
            "sha1:abc",
        )
        .expect("append mismatched compact installed");

    let error = store.load().expect_err("mismatched terminal should fail");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("compact_installed does not match compact_started")
    ));
}

#[test]
fn compact_index_duplicate_terminal_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "../rollout.jsonl",
        )
        .expect("append compact started");
    store
        .append_compact_installed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "nodes/1/worklog.md",
            "sha1:abc",
        )
        .expect("append compact installed");
    store
        .append_compact_failed(
            "compact-1",
            &id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "codex_builtin_text",
            "late failure",
        )
        .expect("append duplicate terminal");

    let error = store.load().expect_err("duplicate terminal should fail");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("duplicate terminal event for compact-1")
    ));
}

#[test]
fn compact_index_seq_gap_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    std::fs::write(
        store.compact_index_path(),
        serde_json::to_string(&json!({
            "type": "compact_started",
            "seq": 2,
            "compact_id": "compact-1",
            "node_id": "1",
            "op": "next",
            "cut_ordinal": 4,
            "fold_end_ordinal": 9,
            "strategy": "codex_builtin_text",
            "raw_trajs": "raw/rollout.raw.jsonl",
            "rollout": "../rollout.jsonl",
        }))
        .expect("serialize compact event")
            + "\n",
    )
    .expect("write compact index");

    let error = store.load().expect_err("seq gap should fail");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("compact.index.jsonl line 1 has seq 2, expected 1")
    ));
}
