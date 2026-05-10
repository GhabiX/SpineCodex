use super::*;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn temp_store() -> (TempDir, SpineSidecarStore) {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T15-38-00-thread.jsonl");
    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("store path");
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

#[test]
fn derives_sidecar_path_from_rollout_path() {
    let rollout_path = Path::new("/tmp/sessions/2026/05/10/rollout-2026-thread.jsonl");

    let sidecar_path =
        SpineSidecarStore::sidecar_dir_for_rollout(rollout_path).expect("sidecar path");

    assert_eq!(
        sidecar_path,
        Path::new("/tmp/sessions/2026/05/10/spine-rollout-2026-thread")
    );

    let no_parent =
        SpineSidecarStore::sidecar_dir_for_rollout(Path::new("rollout-2026-thread.jsonl"))
            .expect_err("relative rollout without a parent should fail");
    assert!(matches!(
        no_parent,
        SpineStoreError::InvalidRolloutPath { reason, .. }
            if reason == "rollout path must include a parent directory"
    ));

    let wrong_extension =
        SpineSidecarStore::sidecar_dir_for_rollout(Path::new("/tmp/rollout-2026-thread.log"))
            .expect_err("non-jsonl rollout should fail");
    assert!(matches!(
        wrong_extension,
        SpineStoreError::InvalidRolloutPath { reason, .. }
            if reason == "rollout path must use the .jsonl extension"
    ));
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
                "status": "live",
                "summary": null,
                "worklog_hash": null,
                "worklog_path": null,
                "plan_path": "nodes/1/plan.json",
            }]
        })
    );
    assert!(store.root().join("nodes").join("1").is_dir());
    assert!(store.trajs_index_path().exists());
}

#[test]
fn records_transition_worklog_and_replays_from_tree() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    let transition = store
        .record_transition(
            &mut state,
            SpineOperation::Open,
            "root scope",
            "Root handoff.",
            8,
        )
        .expect("record transition");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[1, 1]),
        }
    );
    assert_eq!(
        std::fs::read_to_string(store.worklog_path(&id(&[1]))).expect("read worklog"),
        "Root handoff."
    );
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
                "worklog_hash": worklog_hash("Root handoff."),
                "raw_start_ordinal": 8,
            }),
        ]
    );

    let loaded = store.load().expect("load sidecar");

    assert_eq!(loaded, state);
    assert_eq!(
        loaded
            .node(&id(&[1]))
            .and_then(|node| node.worklog.as_deref()),
        Some("Root handoff.")
    );
}

#[test]
fn state_cache_mismatch_fails_fast() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(
            &mut state,
            SpineOperation::Open,
            "root scope",
            "Root handoff.",
            8,
        )
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
fn appends_trajs_index_without_raw_rollout_payload() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_raw_items_recorded(&id(&[1]), "turn-1", 0, 3)
        .expect("append raw items index");
    store
        .append_transition_committed("call-1", &id(&[1]), &id(&[1, 1]), 8)
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
                "from_node": "1",
                "to_node": "1.1",
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
