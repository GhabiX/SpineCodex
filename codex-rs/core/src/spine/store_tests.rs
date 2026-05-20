use super::*;
use crate::spine::fast_fail::RuntimeFastFailError;
use crate::spine::mem_install::MemoryBodyError;
use crate::spine::mem_install::MemoryBodyRef;
use crate::spine::mem_install::MemorySectionId;
use crate::spine::mem_install::memory_body_hash;
use crate::spine::state::NodeStatus;
use crate::spine::state::StateCheckpoint;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
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

fn test_projection_epoch(
    checkpoint: &StateCheckpoint,
) -> crate::spine::projection_epoch::ProjectionEpochMetadata {
    crate::spine::projection_epoch::projection_epoch_metadata(
        "test_rollout",
        &[],
        checkpoint,
        0,
        &HashSet::new(),
        &HashSet::new(),
    )
    .expect("projection epoch metadata")
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

fn compact_attempt(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> CompactAttemptRecord {
    CompactAttemptRecord {
        compact_id: compact_id.to_string(),
        node_id,
        op,
        cut_ordinal,
        fold_end_ordinal,
    }
}

fn compact_started(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> CompactStartedRecord {
    CompactStartedRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        strategy: "codex_builtin_text".to_string(),
        rollout: "../rollout.jsonl".to_string(),
    }
}

fn append_legacy_compact_installed_event(
    store: &SpineSidecarStore,
    compact_id: &str,
    node_id: NodeId,
    op: &str,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    message_hash: &str,
) {
    let seq = store
        .next_jsonl_seq(&store.compact_index_path())
        .expect("seq");
    let memory_node_path = node_id
        .segments()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("/");
    let memory_path = format!("nodes/{memory_node_path}/memory.md");
    store
        .append_json_line(
            &store.compact_index_path(),
            &json!({
                "type": "compact_installed",
                "seq": seq,
                "compact_id": compact_id,
                "node_id": node_id.to_string(),
                "op": op,
                "cut_ordinal": cut_ordinal,
                "fold_end_ordinal": fold_end_ordinal,
                "memory_path": memory_path,
                "message_hash": message_hash,
            }),
        )
        .expect("append legacy compact_installed event");
}

fn mem_install_committed(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    body_ref: MemoryBodyRef,
) -> MemInstallCommittedRecord {
    MemInstallCommittedRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        body_ref,
        projection_ref: "projection:seq-1".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    }
}

fn note_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn note_evidence_committed(
    compact_id: &str,
    placement: NotePlacement,
    kind: &str,
    items: Vec<ResponseItem>,
) -> NoteEvidenceCommittedRecord {
    NoteEvidenceCommittedRecord {
        compact_id: compact_id.to_string(),
        placement,
        kind: kind.to_string(),
        items,
        projection_ref: "projection:note".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    }
}

fn append_root_compact_started(store: &SpineSidecarStore, compact_id: &str) {
    store
        .append_compact_started(compact_started(
            compact_id,
            id(&[1]),
            SpineOperation::Archive,
            0,
            7,
        ))
        .expect("append root compact started");
}

fn append_root_memory_install_after_started(store: &SpineSidecarStore, compact_id: &str) {
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nroot body\n")
        .expect("append root memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    store
        .append_mem_install_committed(mem_install_committed(
            compact_id,
            id(&[1]),
            SpineOperation::Archive,
            0,
            7,
            body_ref,
        ))
        .expect("append root mem install");
}

fn append_root_meminstall_evidence(store: &SpineSidecarStore, compact_id: &str) {
    store
        .append_compact_started(compact_started(
            compact_id,
            id(&[1]),
            SpineOperation::Archive,
            0,
            7,
        ))
        .expect("append root compact started");
    store
        .append_note_evidence_committed(note_evidence_committed(
            compact_id,
            NotePlacement::BeforeMem,
            "initial_context_empty",
            vec![note_item("root initial context sentinel")],
        ))
        .expect("append root note evidence");
    store
        .append_memory_section(
            &id(&[1]),
            "\n\n## Auto Compact\n\nBase: /base\nFold: response ordinals [0, 7)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: ../rollout.jsonl\n\nroot body\n\n## Node Summary\n\nsummary\n",
        )
        .expect("append root memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    store
        .append_mem_install_committed(mem_install_committed(
            compact_id,
            id(&[1]),
            SpineOperation::Archive,
            0,
            7,
            body_ref,
        ))
        .expect("append root mem install");
}

fn append_meminstall_evidence(
    store: &SpineSidecarStore,
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> MemoryBodyRef {
    store
        .append_compact_started(compact_started(
            compact_id,
            node_id.clone(),
            op,
            cut_ordinal,
            fold_end_ordinal,
        ))
        .expect("append compact started");
    let body = format!("{compact_id} body");
    store
        .append_memory_section(
            &node_id,
            &format!(
                "\n\n## Auto Compact\n\nBase: /base\nFold: response ordinals [{cut_ordinal}, {fold_end_ordinal})\nNode trajs: nodes/{}/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: ../rollout.jsonl\n\n{body}\n\n## Node Summary\n\nsummary\n",
                node_id
                    .segments()
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("/")
            ),
        )
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&node_id)
        .expect("generated sections")
        .last()
        .expect("generated section")
        .body_ref();
    store
        .append_mem_install_committed(mem_install_committed(
            compact_id,
            node_id,
            op,
            cut_ordinal,
            fold_end_ordinal,
            body_ref.clone(),
        ))
        .expect("append mem install");
    body_ref
}

fn append_meminstall_checkpoint_evidence(
    store: &SpineSidecarStore,
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) {
    append_meminstall_evidence(
        store,
        compact_id,
        node_id.clone(),
        op,
        cut_ordinal,
        fold_end_ordinal,
    );
}

fn compact_terminal(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    error: &str,
) -> CompactTerminalRecord {
    CompactTerminalRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        strategy: "codex_builtin_text".to_string(),
        error: error.to_string(),
    }
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
fn committed_mem_install_spans_admit_verified_survivor() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Close,
        4,
        9,
    );

    let spans = store
        .committed_mem_install_spans_matching_ids(None)
        .expect("read committed MemInstall spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-current");
    assert_eq!(spans[0].node_id, id(&[1, 2]));
    assert_eq!(spans[0].op, SpineOperation::Close);
    assert_eq!(spans[0].cut_ordinal, 4);
    assert_eq!(spans[0].fold_end_ordinal, 9);
}

#[test]
fn committed_mem_install_spans_filter_runtime_spans_by_surviving_ids() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    for (compact_id, node_id, start, end) in [
        ("compact-old", id(&[1, 1]), 1, 4),
        ("compact-current", id(&[1, 2]), 4, 9),
    ] {
        append_meminstall_evidence(
            &store,
            compact_id,
            node_id.clone(),
            SpineOperation::Close,
            start,
            end,
        );
    }

    let surviving_ids = HashSet::from(["compact-current".to_string()]);
    let spans = store
        .committed_mem_install_spans_matching_ids(Some(&surviving_ids))
        .expect("read filtered MemInstall spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-current");
}

#[test]
fn compact_index_rejects_legacy_compact_installed_json() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_legacy_compact_installed_event(
        &store,
        "compact-legacy",
        id(&[1, 2]),
        "close",
        4,
        9,
        "sha1:legacy",
    );

    let error = store
        .load()
        .expect_err("legacy compact_installed JSON should fail closed");

    assert!(matches!(
        error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("unknown variant `compact_installed`")
    ));
}

#[test]
fn root_meminstall_survivor_validation_checks_store_evidence() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    append_root_meminstall_evidence(&store, "compact-root");
    store
        .record_root_epoch_archive(
            &mut state,
            "context compacted",
            7,
            "compact-root",
            "turn-root",
        )
        .expect("record root reset");

    let surviving_ids = HashSet::from(["compact-root".to_string()]);
    store
        .validate_root_meminstall_survivors(&surviving_ids)
        .expect("complete root survivor evidence should validate");

    let (_temp_missing_reset, missing_reset_store) = temp_store();
    missing_reset_store.create().expect("create sidecar");
    append_root_meminstall_evidence(&missing_reset_store, "compact-root");
    let err = missing_reset_store
        .validate_root_meminstall_survivors(&surviving_ids)
        .expect_err("missing root reset must fail closed");
    assert!(matches!(
        err,
        SpineStoreError::InvalidLedger(message)
            if message.contains("partial root MemInstall survivor set")
    ));

    let (_temp_missing_meminstall, missing_meminstall_store) = temp_store();
    let mut missing_meminstall_state = missing_meminstall_store.create().expect("create sidecar");
    missing_meminstall_store
        .record_root_epoch_archive(
            &mut missing_meminstall_state,
            "context compacted",
            7,
            "compact-root",
            "turn-root",
        )
        .expect("record root reset");
    let err = missing_meminstall_store
        .validate_root_meminstall_survivors(&surviving_ids)
        .expect_err("missing MemInstall must fail closed");
    assert!(matches!(
        err,
        SpineStoreError::InvalidLedger(message)
            if message.contains("partial root MemInstall survivor set")
    ));
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
fn default_sidecar_without_locator_is_not_auto_migrated() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");
    let root = SpineSidecarStore::default_sidecar_dir_for_rollout(&rollout_path)
        .expect("default sidecar dir");
    std::fs::create_dir_all(&root).expect("create unsupported sidecar root");

    assert!(!SpineSidecarStore::has_sidecar_for_rollout(&rollout_path).expect("check sidecar"));
    assert!(matches!(
        SpineSidecarStore::for_rollout(&rollout_path),
        Err(SpineStoreError::Io { path, .. })
            if path.ends_with("rollout-test.spine.json")
    ));
    assert!(matches!(
        SpineSidecarStore::create_for_rollout(&rollout_path),
        Err(SpineStoreError::AlreadyInitialized { path })
            if path == root
    ));
    assert!(
        !SpineSidecarStore::locator_path_for_rollout(&rollout_path)
            .expect("locator")
            .exists()
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
fn create_writes_root_ledger() {
    let (_temp, store) = temp_store();

    let state = store.create().expect("create sidecar");

    assert_eq!(state.cursor(), &id(&[1, 1]));
    assert_eq!(
        read_json_lines(store.tree_path()),
        vec![json!({
            "type": "spine_initialized",
            "seq": 1,
            "initial_raw_start_ordinal": 0,
        })]
    );
    assert_eq!(store.load().expect("load sidecar"), state);
    assert!(store.root().join("nodes").join("1").is_dir());
    assert!(store.root().join("nodes").join("1").join("1").is_dir());
    assert!(store.trajs_index_path().exists());
    assert!(store.compact_index_path().exists());
    assert!(!store.hints_path().exists());
    assert!(store.raw_rollout_path().exists());
}

#[test]
fn tree_metadata_cache_matches_replayed_next_seq() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("create store");
    let mut state = store.create().expect("create sidecar");

    assert_eq!(
        store.next_tree_event_seq().expect("next seq after create"),
        2
    );
    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record transition");
    assert_eq!(
        store
            .next_tree_event_seq()
            .expect("cached next seq after transition"),
        3
    );

    let reloaded = SpineSidecarStore::for_rollout(&rollout_path).expect("reload store");
    assert_eq!(
        reloaded
            .next_tree_event_seq()
            .expect("replayed next seq before load"),
        3
    );
    assert_eq!(reloaded.load().expect("load sidecar"), state);
    assert_eq!(
        reloaded
            .next_tree_event_seq()
            .expect("replayed next seq after load"),
        3
    );
}

#[test]
fn records_transition_summary_and_replays_from_tree() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    let transition = store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record transition");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 1, 1]),
        }
    );
    assert!(!store.memory_path(&id(&[1, 1])).exists());
    assert!(
        store
            .root()
            .join("nodes")
            .join("1")
            .join("1")
            .join("1")
            .is_dir()
    );
    assert!(!store.root().join("nodes").join("1.1").exists());
    let tree = read_json_lines(store.tree_path());
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[0]["type"], "spine_initialized");
    assert_eq!(
        tree[1],
        json!({
            "type": "transition_applied",
            "seq": 2,
            "op": "open",
            "from_node": "1.1",
            "to_node": "1.1.1",
            "summary": null,
            "raw_start_ordinal": 8,
            "source_turn_id": "turn-1",
        })
    );

    let loaded = store.load().expect("load sidecar");

    assert_eq!(loaded, state);
    assert_eq!(
        loaded
            .node(&id(&[1, 1, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(8)
    );
}

#[test]
fn legacy_child_summary_tree_event_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "transition_applied",
                "seq": 2,
                "op": "close",
                "from_node": "1.1",
                "to_node": "1.2",
                "summary": "old parent summary",
                "child_summary": "old child summary",
                "raw_start_ordinal": 8,
                "source_turn_id": "turn-legacy",
            }),
        )
        .expect("append legacy child_summary event");
    store
        .set_cached_next_tree_seq(3)
        .expect("advance test metadata cache");

    let error = store
        .load()
        .expect_err("legacy close shape should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("unknown field `child_summary`")
    ));
}

#[test]
fn transition_without_source_turn_id_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "transition_applied",
                "seq": 2,
                "op": "open",
                "from_node": "1.1",
                "to_node": "1.1.1",
                "summary": null,
                "raw_start_ordinal": 8
            }),
        )
        .expect("append transition without source turn");
    store
        .set_cached_next_tree_seq(3)
        .expect("advance test metadata cache");

    let error = store
        .load()
        .expect_err("transition without source turn must fail closed");
    assert!(matches!(
        error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("missing field `source_turn_id`")
    ));
}

#[test]
fn records_root_epoch_archive_and_replays_from_tree() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record root open");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 13, "turn-2")
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
            from: id(&[1]),
            to: id(&[2, 1]),
        }
    );
    assert!(store.root().join("nodes").join("2").join("1").is_dir());
    assert_eq!(
        read_json_lines(store.tree_path())[3],
        json!({
            "type": "root_epoch_reset",
            "seq": 4,
            "root_id": "1",
            "next_leaf_id": "2.1",
            "summary": "context compacted",
            "raw_start_ordinal": 21,
            "compact_id": "compact-1",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");

    assert_eq!(loaded, state);
    assert_eq!(loaded.cursor(), &id(&[2, 1]));
    assert_eq!(
        loaded.node(&id(&[1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        loaded.node(&id(&[1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        loaded.node(&id(&[1, 1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 1, 1, 1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        loaded
            .nodes()
            .values()
            .filter(|node| node.status == NodeStatus::Live)
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>(),
        vec![id(&[2, 1])]
    );
    assert_eq!(
        loaded
            .node(&id(&[2, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(21)
    );
}

#[test]
fn root_cursor_archive_parent_links_survive_reload() {
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
            from: id(&[1]),
            to: id(&[2, 1]),
        }
    );
    assert_eq!(
        read_json_lines(store.tree_path())[1],
        json!({
            "type": "root_epoch_reset",
            "seq": 2,
            "root_id": "1",
            "next_leaf_id": "2.1",
            "summary": "context compacted",
            "raw_start_ordinal": 7,
            "compact_id": "compact-root",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");
    assert_eq!(loaded.cursor(), &id(&[2, 1]));
    assert_eq!(
        loaded
            .node(&id(&[1]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[2]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[2, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[2]))
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
    assert_eq!(loaded.nodes().len(), 4);
}

#[test]
fn generated_memory_sections_do_not_break_transition_replay_hash() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record transition");

    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\ngenerated summary\n")
        .expect("append generated section");

    let memory = std::fs::read_to_string(store.memory_path(&id(&[1]))).expect("read memory");
    assert!(memory.contains("spine:auto-compact-generated"));
    assert!(memory.contains("generated summary"));

    let loaded = store.load().expect("load sidecar");

    assert_eq!(loaded, state);
}

#[test]
fn mem_install_generated_sections_are_independently_referenced() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_memory_section(
            &id(&[1]),
            "\n\n## Auto Compact\n\nBase: /parent/base\nFold: response ordinals [2, 10)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: /parent/rollout.jsonl\n\nfirst body\n\n## Node Summary\n\nfirst\n",
        )
        .expect("append first generated section");
    store
        .append_memory_section(
            &id(&[1]),
            "\n\n## Auto Compact\n\nBase: /parent/base\nFold: response ordinals [10, 20)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: /parent/rollout.jsonl\n\nsecond body\n\n## Node Summary\n\nsecond\n",
        )
        .expect("append second generated section");

    let sections = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections");

    assert_eq!(sections.len(), 2);
    assert_eq!(
        sections[0].section_id.to_string(),
        "nodes/1/memory.md#section-0"
    );
    assert_eq!(sections[0].body, "first body");
    assert_eq!(
        sections[1].section_id.to_string(),
        "nodes/1/memory.md#section-1"
    );
    assert_eq!(sections[1].body, "second body");
    assert_eq!(
        store
            .verify_memory_body_ref(&id(&[1]), &sections[1].body_ref())
            .expect("verify second section"),
        sections[1]
    );

    let wrong_storage_ref = MemoryBodyRef {
        section_id: MemorySectionId::new("nodes/2/memory.md", 1),
        body_hash: sections[1].body_hash.clone(),
    };
    assert!(matches!(
        store.verify_memory_body_ref(&id(&[1]), &wrong_storage_ref),
        Err(SpineStoreError::MemoryBody(
            MemoryBodyError::StorageMismatch { .. }
        ))
    ));
}

#[test]
fn mem_install_body_hash_ignores_imported_audit_path_drift() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_memory_section(
            &id(&[1]),
            "\n\n## Auto Compact\n\nBase: /parent/base\nFold: response ordinals [2, 10)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: /parent/rollout.jsonl\n\nportable body\n\n## Node Summary\n\nsummary\n",
        )
        .expect("append generated section");
    let original = store
        .generated_memory_sections(&id(&[1]))
        .expect("read original");
    let memory_path = store.memory_path(&id(&[1]));
    let relocated = std::fs::read_to_string(&memory_path)
        .expect("read memory")
        .replace("/parent/base", "/child/base")
        .replace("/parent/rollout.jsonl", "/child/rollout.jsonl");
    std::fs::write(&memory_path, relocated).expect("rewrite memory");

    let relocated = store
        .generated_memory_sections(&id(&[1]))
        .expect("read relocated");

    assert_eq!(original[0].body, "portable body");
    assert_eq!(original[0].body_hash, relocated[0].body_hash);
}

#[test]
fn appends_node_trajs_next_to_memory() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record transition");

    store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\ngenerated summary\n")
        .expect("append generated memory");
    store
        .append_node_trajs_items(&id(&[1, 1]), &[assistant_rollout_item("folded suffix")])
        .expect("append node trajs");

    let memory_path = store.memory_path(&id(&[1, 1]));
    let trajs_path = store.node_trajs_path(&id(&[1, 1]));
    assert_eq!(memory_path.parent(), trajs_path.parent());
    assert_eq!(trajs_path, store.root().join("nodes/1/1/trajs.jsonl"));
    assert!(memory_path.exists());
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
        read_json_lines(store.tree_path()),
        vec![json!({
            "type": "spine_initialized",
            "seq": 1,
            "initial_raw_start_ordinal": 0,
        })]
    );
    assert_eq!(
        read_json_lines(store.hints_path()),
        vec![json!({
            "type": "size_hint_emitted",
            "seq": 1,
            "node_id": "1",
            "threshold_tokens": 30000,
            "estimated_tokens": 31200,
            "source": "runtime_observation",
        })]
    );
}

#[test]
fn legacy_tree_size_hint_emission_is_read_for_dedup_but_not_replayed() {
    let (_temp, store) = temp_store();
    let state = store.create().expect("create sidecar");
    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "spine_hint_emitted",
                "seq": 2,
                "node_id": "1",
                "threshold_tokens": 30000,
                "estimated_tokens": 31200,
                "source": "runtime_observation",
            }),
        )
        .expect("append legacy hint event");

    assert!(
        store
            .has_size_hint_emitted(&id(&[1]), 30_000)
            .expect("query legacy emitted hint")
    );
    assert_eq!(store.load().expect("load sidecar"), state);
}

#[test]
fn size_hint_cache_seq_gap_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_size_hint_emitted(&id(&[1]), 20_000, 21_000, "setup")
        .expect("create hint cache");
    store
        .append_json_line(
            &store.hints_path(),
            &json!({
                "type": "size_hint_emitted",
                "seq": 3,
                "node_id": "1",
                "threshold_tokens": 30000,
                "estimated_tokens": 31200,
                "source": "runtime_observation",
            }),
        )
        .expect("append invalid hint event");

    let error = store
        .has_size_hint_emitted(&id(&[1]), 30_000)
        .expect_err("hint cache seq gap should fail closed");

    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("cache/hints.jsonl line 2 has seq 3, expected 2")
    ));
}

#[test]
fn size_hint_cache_rejects_legacy_tree_event_shape() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_size_hint_emitted(&id(&[1]), 20_000, 21_000, "setup")
        .expect("create hint cache");
    store
        .append_json_line(
            &store.hints_path(),
            &json!({
                "type": "spine_hint_emitted",
                "seq": 2,
                "node_id": "1",
                "threshold_tokens": 30000,
                "estimated_tokens": 31200,
                "source": "runtime_observation",
            }),
        )
        .expect("append invalid hint event");

    let error = store
        .has_size_hint_emitted(&id(&[1]), 30_000)
        .expect_err("hint cache event shape should fail closed");

    assert!(matches!(
        error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("unknown variant `spine_hint_emitted`")
    ));
}

#[test]
fn legacy_tree_size_hint_no_longer_blocks_tree_seq() {
    let (temp, store) = temp_store();
    let rollout_path = temp.path().join("rollout-2026-05-10T15-38-00-thread.jsonl");
    let mut state = store.create().expect("create sidecar");
    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "spine_hint_emitted",
                "seq": 2,
                "node_id": "1",
                "threshold_tokens": 30000,
                "estimated_tokens": 31200,
                "source": "runtime_observation",
            }),
        )
        .expect("append legacy hint event");

    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("reload store");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 10, "turn-after")
        .expect("append tree event after legacy hint");

    let tree = read_json_lines(store.tree_path());
    assert_eq!(tree[2]["type"], "transition_applied");
    assert_eq!(tree[2]["seq"], 3);
    assert_eq!(store.load().expect("load sidecar"), state);
}

#[test]
fn size_hint_cache_emission_does_not_advance_tree_seq() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .append_size_hint_emitted(&id(&[1]), 30_000, 31_200, "runtime_observation")
        .expect("append hint event");

    store
        .record_transition(&mut state, SpineOperation::Open, None, 10, "turn-after")
        .expect("append tree event after hint");

    let tree = read_json_lines(store.tree_path());
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[1]["type"], "transition_applied");
    assert_eq!(tree[1]["seq"], 2);
}

#[test]
fn compact_index_records_mem_install_without_polluting_raw_mirror() {
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
    append_meminstall_checkpoint_evidence(
        &store,
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Close,
        4,
        9,
    );
    assert_eq!(
        read_json_lines(store.compact_index_path()),
        vec![
            json!({
                "type": "compact_started",
                "seq": 1,
                "compact_id": "compact-1",
                "node_id": "1.2",
                "op": "close",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "strategy": "codex_builtin_text",
                "rollout": "../rollout.jsonl",
            }),
            json!({
                "type": "mem_install_committed",
                "seq": 2,
                "schema_version": 3,
                "compact_id": "compact-1",
                "node_id": "1.2",
                "op": "close",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "memory_section_id": "nodes/1/2/memory.md#section-0",
                "body_hash": memory_body_hash("compact-1 body"),
                "storage_ref": "nodes/1/2/memory.md",
                "projection_ref": "projection:seq-1",
                "source_rollout_ref": "../rollout.jsonl",
            }),
        ]
    );

    let raw_mirror = read_json_lines(store.raw_rollout_path());
    assert_eq!(raw_mirror.len(), 1);
    assert_eq!(raw_mirror[0]["type"], "response_item");
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_committed_writes_semantic_marker() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(
            &id(&[1]),
            "\n\n## Auto Compact\n\nBase: /base\nFold: response ordinals [4, 9)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: ../rollout.jsonl\n\nportable body\n\n## Node Summary\n\nsummary\n",
        )
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();

    store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            body_ref.clone(),
        ))
        .expect("append mem install commit");
    assert_eq!(
        read_json_lines(store.compact_index_path()),
        vec![
            json!({
                "type": "compact_started",
                "seq": 1,
                "compact_id": "compact-1",
                "node_id": "1",
                "op": "close",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "strategy": "codex_builtin_text",
                "rollout": "../rollout.jsonl",
            }),
            json!({
                "type": "mem_install_committed",
                "seq": 2,
                "schema_version": 3,
                "compact_id": "compact-1",
                "node_id": "1",
                "op": "close",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "memory_section_id": "nodes/1/memory.md#section-0",
                "body_hash": memory_body_hash("portable body"),
                "storage_ref": "nodes/1/memory.md",
                "projection_ref": "projection:seq-1",
                "source_rollout_ref": "../rollout.jsonl",
            }),
        ]
    );

    let installs = store
        .committed_mem_installs()
        .expect("committed mem installs");
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].compact_id, "compact-1");
    assert_eq!(installs[0].body_ref, body_ref);
    store.load().expect("mem install commit is terminal");
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_missing_started_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nbody\n")
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();

    let error = store
        .append_mem_install_committed(mem_install_committed(
            "compact-missing-start",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            body_ref,
        ))
        .expect_err("missing started should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallMissingStarted { .. })
    ));
}

#[test]
fn compact_index_note_evidence_commits_structured_items() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_root_compact_started(&store, "compact-root");
    let item = note_item("structured initial context");
    store
        .append_note_evidence_committed(note_evidence_committed(
            "compact-root",
            NotePlacement::BeforeMem,
            "initial_context_0",
            vec![item.clone()],
        ))
        .expect("append note evidence");
    append_root_memory_install_after_started(&store, "compact-root");

    let evidence = store
        .committed_note_evidence()
        .expect("committed note evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].compact_id, "compact-root");
    assert_eq!(evidence[0].placement, NotePlacement::BeforeMem);
    assert_eq!(evidence[0].kind, "initial_context_0");
    assert_eq!(evidence[0].items, vec![item]);
    assert!(evidence[0].items_hash.starts_with("sha256:"));
}

#[test]
fn compact_index_note_evidence_duplicate_kind_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_root_compact_started(&store, "compact-root");
    store
        .append_note_evidence_committed(note_evidence_committed(
            "compact-root",
            NotePlacement::BeforeMem,
            "initial_context_0",
            vec![note_item("first")],
        ))
        .expect("append first note evidence");

    let error = store
        .append_note_evidence_committed(note_evidence_committed(
            "compact-root",
            NotePlacement::BeforeMem,
            "initial_context_0",
            vec![note_item("second")],
        ))
        .expect_err("duplicate note evidence kind should fail closed");
    assert!(
        error
            .to_string()
            .contains("duplicate NoteEvidenceCommitted")
    );
}

#[test]
fn compact_index_note_evidence_before_mem_for_non_root_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-child",
            id(&[1, 1]),
            SpineOperation::Close,
            3,
            7,
        ))
        .expect("append child compact started");

    let error = store
        .append_note_evidence_committed(note_evidence_committed(
            "compact-child",
            NotePlacement::BeforeMem,
            "initial_context_0",
            vec![note_item("not allowed")],
        ))
        .expect_err("before_mem note evidence is root-only");
    assert!(
        error
            .to_string()
            .contains("before_mem placement for non-root")
    );
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_missing_body_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nbody\n")
        .expect("append memory section");
    let missing_body_ref = MemoryBodyRef {
        section_id: MemorySectionId::new("nodes/1/memory.md", 1),
        body_hash: memory_body_hash("body"),
    };

    let error = store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            missing_body_ref,
        ))
        .expect_err("missing body should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallMissingBody { .. })
    ));
    assert_eq!(read_json_lines(store.compact_index_path()).len(), 1);
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_missing_projection_ref_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nbody\n")
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    let mut record =
        mem_install_committed("compact-1", id(&[1]), SpineOperation::Close, 4, 9, body_ref);
    record.projection_ref.clear();

    let error = store
        .append_mem_install_committed(record)
        .expect_err("missing projection ref should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(
            RuntimeFastFailError::MemInstallMissingProjectionRef { .. }
        )
    ));
    assert_eq!(read_json_lines(store.compact_index_path()).len(), 1);
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_duplicate_compact_id_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nbody\n")
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            body_ref.clone(),
        ))
        .expect("append first mem install commit");

    let error = store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            body_ref,
        ))
        .expect_err("duplicate mem install should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallDuplicateCompactId { .. })
    ));
    assert_eq!(read_json_lines(store.compact_index_path()).len(), 2);
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_body_dependency_drift_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nbody\n")
        .expect("append memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            body_ref,
        ))
        .expect("append mem install commit");
    std::fs::write(store.memory_path(&id(&[1])), "changed body").expect("rewrite memory");

    let error = store
        .load()
        .expect_err("missing committed body should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallMissingBody { .. })
    ));
}

#[test]
fn compact_index_started_without_terminal_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
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
    let mut source_checkpoint = StateCheckpoint::new(0);
    source_store
        .record_transition(&mut source_state, SpineOperation::Open, None, 2, "turn-1")
        .map(|transition| source_checkpoint.record_open(&transition.from, &transition.to, 2))
        .expect("record source transition");
    source_store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nsource memory\n")
        .expect("write source memory");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(
            &source_state,
            source_checkpoint.clone(),
            "fork_seed",
            None,
            test_projection_epoch(&source_checkpoint),
        )
        .expect("record projection reset");
    let projection_event = read_json_lines(child_store.tree_path())
        .into_iter()
        .find(|event| event["type"] == "projection_reset")
        .expect("projection reset event");
    assert_eq!(projection_event["source_rollout_ref"], "test_rollout");
    assert_eq!(projection_event["processed_rollout_len"], 0);
    assert_eq!(projection_event["effective_raw_len"], 0);
    assert!(
        projection_event["processed_rollout_hash"]
            .as_str()
            .expect("processed rollout hash")
            .starts_with("sha256:")
    );
    let latest_epoch = child_store
        .latest_projection_epoch()
        .expect("latest projection epoch")
        .expect("projection epoch metadata");
    assert_eq!(latest_epoch.source_rollout_ref, "test_rollout");
    child_store
        .copy_node_artifacts_from(&source_store, source_state.nodes().keys())
        .expect("copy artifacts");

    let replayed = child_store.load().expect("load child projection");
    assert_eq!(replayed.cursor(), &id(&[1, 1, 1]));
    assert_eq!(replayed.node(&id(&[1])).expect("root").summary, None);
    assert!(
        child_store
            .read_memory(&id(&[1, 1]))
            .expect("read copied memory")
            .contains("source memory")
    );
    assert_eq!(read_json_lines(child_store.tree_path()).len(), 2);
}

#[test]
fn projection_reset_checkpoint_mismatch_fails_closed() {
    let (temp, source_store) = temp_store();
    let mut source_state = source_store.create().expect("create source");
    let mut source_checkpoint = StateCheckpoint::new(0);
    source_store
        .record_transition(&mut source_state, SpineOperation::Open, None, 2, "turn-1")
        .map(|transition| source_checkpoint.record_open(&transition.from, &transition.to, 2))
        .expect("record source transition");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");

    let error = child_store
        .record_projection_reset(
            &source_state,
            StateCheckpoint::new(0),
            "fork_seed",
            None,
            test_projection_epoch(&source_checkpoint),
        )
        .expect_err("projection reset checkpoint must match projected state");

    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("projection reset checkpoint does not replay")
    ));
}

#[test]
fn projection_reset_without_epoch_metadata_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");

    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "projection_reset",
                "seq": 2,
                "reason": "legacy_projection",
                "source_turn_id": null,
                "checkpoint": {
                    "initial_raw_start_ordinal": 0,
                    "events": []
                }
            }),
        )
        .expect("append legacy projection reset");

    let error = store
        .latest_projection_epoch()
        .expect_err("legacy projection epoch must fail closed");
    assert!(matches!(
        error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("missing field `source_rollout_ref`")
    ));
    let load_error = store
        .load()
        .expect_err("legacy projection epoch must also fail replay");
    assert!(matches!(
        load_error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("missing field `source_rollout_ref`")
    ));
}

#[test]
fn projection_reset_with_derived_memory_path_fails_closed() {
    let (_temp, store) = temp_store();
    let checkpoint = StateCheckpoint::new(0);
    store.create().expect("create sidecar");
    let epoch = test_projection_epoch(&checkpoint);

    store
        .append_json_line(
            &store.tree_path(),
            &json!({
                "type": "projection_reset",
                "seq": 2,
                "reason": "old_snapshot_shape",
                "source_turn_id": null,
                "source_rollout_ref": epoch.source_rollout_ref,
                "processed_rollout_len": epoch.processed_rollout_len,
                "processed_rollout_hash": epoch.processed_rollout_hash,
                "effective_raw_len": epoch.effective_raw_len,
                "surviving_turn_ids_hash": epoch.surviving_turn_ids_hash,
                "surviving_compact_ids": epoch.surviving_compact_ids,
                "checkpoint_hash": epoch.checkpoint_hash,
                "checkpoint": {
                    "initial_raw_start_ordinal": 0,
                    "events": [],
                    "nodes": []
                }
            }),
        )
        .expect("append old projection reset shape");

    let load_error = store
        .load()
        .expect_err("derived checkpoint node cache must fail closed");
    assert!(matches!(
        load_error,
        SpineStoreError::Json { source, .. }
            if source.to_string().contains("unknown field `nodes`")
    ));
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
            from: id(&[1]),
            to: id(&[2, 1]),
        }
    );
    assert_eq!(
        read_json_lines(store.tree_path())[1],
        json!({
            "type": "root_epoch_reset",
            "seq": 2,
            "root_id": "1",
            "next_leaf_id": "2.1",
            "summary": "context compacted",
            "raw_start_ordinal": 7,
            "compact_id": "compact-root",
            "source_turn_id": "turn-compact",
        })
    );

    let loaded = store.load().expect("load archived sidecar");
    assert_eq!(loaded.cursor(), &id(&[2, 1]));
    assert_eq!(
        loaded
            .node(&id(&[1]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[2]))
            .and_then(|node| node.parent_id.clone()),
        None
    );
    assert_eq!(
        loaded
            .node(&id(&[2, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[2]))
    );
    assert_eq!(
        loaded
            .node(&id(&[1, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[1]))
    );
    assert_eq!(loaded.nodes().len(), 4);
}

#[test]
fn projected_artifact_copy_filters_non_surviving_turn_files() {
    let (temp, source_store) = temp_store();
    let mut source_state = source_store.create().expect("create source");
    let mut source_checkpoint = StateCheckpoint::new(0);
    source_store
        .record_transition(
            &mut source_state,
            SpineOperation::Open,
            None,
            2,
            "surviving-turn",
        )
        .map(|transition| source_checkpoint.record_open(&transition.from, &transition.to, 2))
        .expect("record source transition");
    source_store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nsurviving memory\n")
        .expect("write surviving memory");
    source_store
        .append_memory_section(
            &id(&[1, 1, 1]),
            "\n\n## Auto Compact\n\nrolled back memory\n",
        )
        .expect("write rolled back memory");
    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(
            &source_state,
            source_checkpoint.clone(),
            "fork_seed",
            None,
            test_projection_epoch(&source_checkpoint),
        )
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
            .read_memory(&id(&[1, 1]))
            .expect("read copied surviving memory")
            .contains("surviving memory")
    );
    assert!(matches!(
        child_store.read_memory(&id(&[1, 1, 1])),
        Err(SpineStoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn projected_artifact_copy_fails_on_existing_destination_file() {
    let (temp, source_store) = temp_store();
    let mut source_state = source_store.create().expect("create source");
    let mut source_checkpoint = StateCheckpoint::new(0);
    source_store
        .record_transition(
            &mut source_state,
            SpineOperation::Open,
            None,
            2,
            "surviving-turn",
        )
        .map(|transition| source_checkpoint.record_open(&transition.from, &transition.to, 2))
        .expect("record source transition");
    source_store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nsource memory\n")
        .expect("write source memory");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(
            &source_state,
            source_checkpoint.clone(),
            "fork_seed",
            None,
            test_projection_epoch(&source_checkpoint),
        )
        .expect("record projection reset");
    child_store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nstale child memory\n")
        .expect("write stale child memory");

    let error = child_store
        .copy_projected_node_artifacts_from(
            &source_store,
            source_state.nodes().keys(),
            &HashSet::from(["surviving-turn".to_string()]),
        )
        .expect_err("existing destination artifact must fail closed");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("refusing to overwrite existing spine node artifact")
    ));
}

#[test]
fn compact_index_started_then_installed_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_checkpoint_evidence(
        &store,
        "compact-1",
        id(&[1]),
        SpineOperation::Close,
        4,
        9,
    );

    store.load().expect("resolved compact should load");
}

#[test]
fn committed_mem_install_spans_return_full_runtime_ledger() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_checkpoint_evidence(
        &store,
        "compact-child",
        id(&[1, 1]),
        SpineOperation::Close,
        1,
        4,
    );
    append_meminstall_checkpoint_evidence(
        &store,
        "compact-scope",
        id(&[1]),
        SpineOperation::Close,
        1,
        6,
    );
    append_meminstall_checkpoint_evidence(
        &store,
        "compact-sibling",
        id(&[2]),
        SpineOperation::Close,
        7,
        9,
    );

    let spans = store
        .committed_mem_install_spans_matching_ids(None)
        .expect("read committed MemInstall spans");

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].compact_id, "compact-child");
    assert_eq!(spans[0].node_id, id(&[1, 1]));
    assert_eq!(spans[0].op, SpineOperation::Close);
    assert_eq!(spans[0].cut_ordinal, 1);
    assert_eq!(spans[0].fold_end_ordinal, 4);
    assert_eq!(spans[1].compact_id, "compact-scope");
    assert_eq!(spans[1].node_id, id(&[1]));
    assert_eq!(spans[1].op, SpineOperation::Close);
    assert_eq!(spans[1].cut_ordinal, 1);
    assert_eq!(spans[1].fold_end_ordinal, 6);
    assert_eq!(spans[2].compact_id, "compact-sibling");
    assert_eq!(spans[2].node_id, id(&[2]));
    assert_eq!(spans[2].cut_ordinal, 7);
    assert_eq!(spans[2].fold_end_ordinal, 9);
}

#[test]
fn projected_compact_spans_filter_stale_duplicate_boundaries() {
    let (temp, source_store) = temp_store();
    source_store.create().expect("create source sidecar");
    append_meminstall_checkpoint_evidence(
        &source_store,
        "compact-old",
        id(&[1, 1]),
        SpineOperation::Close,
        1,
        4,
    );
    append_meminstall_checkpoint_evidence(
        &source_store,
        "compact-new",
        id(&[1, 1]),
        SpineOperation::Close,
        1,
        6,
    );

    let surviving_ids = HashSet::from(["compact-new".to_string()]);
    let filtered_spans = source_store
        .committed_mem_install_spans_matching_ids(Some(&surviving_ids))
        .expect("read filtered MemInstall spans");
    assert_eq!(filtered_spans.len(), 1);
    assert_eq!(filtered_spans[0].compact_id, "compact-new");
    assert_eq!(filtered_spans[0].fold_end_ordinal, 6);

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child sidecar");
    child_store
        .copy_projected_compact_index_from(&source_store, &surviving_ids)
        .expect("copy filtered compact index");
    child_store
        .copy_node_artifacts_from(&source_store, [id(&[1, 1])].iter())
        .expect("copy surviving memory artifact");
    let copied_spans = child_store
        .committed_mem_install_spans_matching_ids(None)
        .expect("read copied MemInstall spans");
    assert_eq!(copied_spans.len(), 1);
    assert_eq!(copied_spans[0].compact_id, "compact-new");
    assert_eq!(copied_spans[0].fold_end_ordinal, 6);

    let copied_events = read_json_lines(child_store.compact_index_path());
    assert_eq!(copied_events.len(), 2);
    assert_eq!(copied_events[0]["seq"], 1);
    assert_eq!(copied_events[1]["seq"], 2);
    assert_eq!(copied_events[0]["compact_id"], "compact-new");
    assert_eq!(copied_events[1]["compact_id"], "compact-new");
}

#[test]
fn validate_mem_install_survivors_accepts_verified_meminstall() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-mem-only",
        id(&[1]),
        SpineOperation::Close,
        1,
        4,
    );

    let err =
        store.validate_mem_install_survivors(&HashSet::from(["compact-mem-only".to_string()]));

    assert!(err.is_ok());
}

#[test]
fn copy_projected_compact_index_from_keeps_meminstall_survivor() {
    let (temp, source_store) = temp_store();
    source_store.create().expect("create source sidecar");
    append_meminstall_evidence(
        &source_store,
        "compact-mem-only",
        id(&[1]),
        SpineOperation::Close,
        1,
        4,
    );

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child sidecar");
    child_store
        .copy_projected_compact_index_from(
            &source_store,
            &HashSet::from(["compact-mem-only".to_string()]),
        )
        .expect("copy compact index");
    child_store
        .copy_node_artifacts_from(&source_store, [id(&[1])].iter())
        .expect("copy surviving memory artifact");

    let spans = child_store
        .committed_mem_install_spans_matching_ids(None)
        .expect("read copied MemInstall spans");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-mem-only");
    assert_eq!(read_json_lines(child_store.compact_index_path()).len(), 2);
}

#[test]
fn committed_mem_install_spans_filter_by_surviving_ids() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-old",
        id(&[1, 1]),
        SpineOperation::Close,
        1,
        4,
    );
    append_meminstall_evidence(
        &store,
        "compact-new",
        id(&[1, 2]),
        SpineOperation::Close,
        4,
        8,
    );

    let surviving_ids = HashSet::from(["compact-new".to_string()]);
    let spans = store
        .committed_mem_install_spans_matching_ids(Some(&surviving_ids))
        .expect("read filtered committed spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-new");
}

#[test]
fn committed_mem_install_spans_reject_missing_body() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(&store, "compact-1", id(&[1]), SpineOperation::Close, 4, 9);
    std::fs::write(store.memory_path(&id(&[1])), "changed body").expect("rewrite memory");

    let error = store
        .committed_mem_install_spans()
        .expect_err("missing committed body should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallMissingBody { .. })
    ));
}

#[test]
fn committed_mem_install_spans_reject_duplicate_compact_id() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(&store, "compact-1", id(&[1]), SpineOperation::Close, 4, 9);
    let mut events = store
        .read_compact_index_events()
        .expect("read compact index events");
    let duplicate = events[1].clone();
    events.push(duplicate);
    store
        .write_compact_index_events(events)
        .expect("write duplicate compact index");

    let error = store
        .committed_mem_install_spans()
        .expect_err("duplicate committed span should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallDuplicateCompactId { .. })
    ));
}

#[test]
fn committed_mem_install_spans_skip_failed_or_interrupted_attempts() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-failed",
            id(&[1]),
            SpineOperation::Close,
            1,
            3,
        ))
        .expect("append failed compact started");
    store
        .append_compact_failed(compact_terminal(
            "compact-failed",
            id(&[1]),
            SpineOperation::Close,
            1,
            3,
            "failed",
        ))
        .expect("append compact failed");
    store
        .append_compact_started(compact_started(
            "compact-interrupted",
            id(&[1, 1]),
            SpineOperation::Close,
            3,
            5,
        ))
        .expect("append interrupted compact started");
    store
        .append_compact_interrupted(compact_terminal(
            "compact-interrupted",
            id(&[1, 1]),
            SpineOperation::Close,
            3,
            5,
            "interrupted",
        ))
        .expect("append compact interrupted");
    append_meminstall_evidence(
        &store,
        "compact-ok",
        id(&[1, 2]),
        SpineOperation::Close,
        5,
        8,
    );

    let spans = store
        .committed_mem_install_spans()
        .expect("read committed spans");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-ok");
}

#[test]
fn compact_index_started_then_failed_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_failed(compact_terminal(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            "strategy failed",
        ))
        .expect("append compact failed");

    store.load().expect("failed compact should be terminal");
}

#[test]
fn compact_index_started_then_interrupted_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_interrupted(compact_terminal(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            "turn aborted",
        ))
        .expect("append compact interrupted");

    assert_eq!(
        read_json_lines(store.compact_index_path())[1],
        json!({
            "type": "compact_interrupted",
            "seq": 2,
            "compact_id": "compact-1",
            "node_id": "1",
            "op": "close",
            "cut_ordinal": 4,
            "fold_end_ordinal": 9,
            "strategy": "codex_builtin_text",
            "error": "turn aborted",
        })
    );
    store
        .load()
        .expect("interrupted compact should be terminal");
}

#[test]
fn compact_index_terminal_mismatch_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    let body_ref =
        append_meminstall_evidence(&store, "compact-1", id(&[1]), SpineOperation::Close, 4, 9);
    store
        .write_compact_index_events(vec![
            CompactIndexEvent::CompactStarted {
                seq: 1,
                compact_id: "compact-1".to_string(),
                node_id: "1".to_string(),
                op: SpineOperation::Close,
                cut_ordinal: 4,
                fold_end_ordinal: 10,
                strategy: "codex_builtin_text".to_string(),
                rollout: "../rollout.jsonl".to_string(),
            },
            CompactIndexEvent::MemInstallCommitted {
                seq: 2,
                schema_version: MEM_INSTALL_COMMITTED_SCHEMA_VERSION,
                compact_id: "compact-1".to_string(),
                node_id: "1".to_string(),
                op: SpineOperation::Close,
                cut_ordinal: 4,
                fold_end_ordinal: 9,
                memory_section_id: body_ref.section_id.to_string(),
                body_hash: body_ref.body_hash,
                storage_ref: body_ref.section_id.storage_ref,
                projection_ref: "projection:seq-1".to_string(),
                source_rollout_ref: "../rollout.jsonl".to_string(),
            },
        ])
        .expect("write mismatched compact index");

    let error = store.load().expect_err("mismatched terminal should fail");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallSpanMismatch {
            compact_id,
        }) if compact_id == "compact-1"
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
            "op": "close",
            "cut_ordinal": 4,
            "fold_end_ordinal": 9,
            "strategy": "codex_builtin_text",
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
