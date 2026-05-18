use super::*;
use crate::spine::debug_audit::INV_MEM_EVIDENCE;
use crate::spine::debug_audit::audit_bridge_checkpoint_span_source;
use crate::spine::fast_fail::RuntimeFastFailError;
use crate::spine::mem_install::MemoryBodyError;
use crate::spine::mem_install::MemoryBodyRef;
use crate::spine::mem_install::MemorySectionId;
use crate::spine::mem_install::memory_body_hash;
use crate::spine::projection_epoch::projection_epoch_metadata;
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
    state: &SpineState,
) -> crate::spine::projection_epoch::ProjectionEpochMetadata {
    projection_epoch_metadata(
        "test_rollout",
        &[],
        state,
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

fn compact_installed(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    replacement_history_len: usize,
    message_hash: &str,
) -> CompactInstalledRecord {
    let memory_node_path = node_id
        .segments()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("/");
    let memory_path = format!("nodes/{memory_node_path}/memory.md");
    CompactInstalledRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        replacement_history_len,
        memory_path,
        message_hash: message_hash.to_string(),
    }
}

fn bridge_checkpoint_committed(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    replacement_history_len: usize,
    message_hash: &str,
) -> BridgeCheckpointCommittedRecord {
    BridgeCheckpointCommittedRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        replacement_history_len,
        message_hash: message_hash.to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    }
}

fn mem_install_committed(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    replacement_history_len: usize,
    message_hash: &str,
    body_ref: MemoryBodyRef,
) -> MemInstallCommittedRecord {
    MemInstallCommittedRecord {
        attempt: compact_attempt(compact_id, node_id, op, cut_ordinal, fold_end_ordinal),
        body_ref,
        replacement_history_len,
        message_hash: message_hash.to_string(),
        projection_ref: "projection:seq-1".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    }
}

fn append_root_meminstall_evidence(
    store: &SpineSidecarStore,
    compact_id: &str,
    message_hash: &str,
) {
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
            3,
            message_hash,
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
    replacement_history_len: usize,
    message_hash: &str,
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
            replacement_history_len,
            message_hash,
            body_ref.clone(),
        ))
        .expect("append mem install");
    body_ref
}

fn append_bridge_checkpoint(
    store: &SpineSidecarStore,
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    replacement_history_len: usize,
    message_hash: &str,
) {
    store
        .append_bridge_checkpoint_committed(bridge_checkpoint_committed(
            compact_id,
            node_id,
            op,
            cut_ordinal,
            fold_end_ordinal,
            replacement_history_len,
            message_hash,
        ))
        .expect("append bridge checkpoint");
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
fn root_meminstall_survivor_matrix() {
    for bits in 0_u8..64 {
        let root_epoch_reset = bits & 0b000001 != 0;
        let mem_install_committed = bits & 0b000010 != 0;
        let compact_span = bits & 0b000100 != 0;
        let body_artifact = bits & 0b001000 != 0;
        let projection_source_ref = bits & 0b010000 != 0;
        let bridge_checkpoint_ref = bits & 0b100000 != 0;

        let admission = classify_root_meminstall_survivor(
            root_epoch_reset,
            mem_install_committed,
            compact_span,
            body_artifact,
            projection_source_ref,
            bridge_checkpoint_ref,
        );

        match bits {
            0 => assert_eq!(
                admission,
                RootMemInstallSurvivorAdmission::OldEpochProjected
            ),
            63 => assert_eq!(
                admission,
                RootMemInstallSurvivorAdmission::RootMemInstallAdmitted
            ),
            _ => assert_eq!(
                admission,
                RootMemInstallSurvivorAdmission::PartialRootMemInstallFailClosed
            ),
        }
    }
}

#[test]
fn runtime_span_authority_policy_table() {
    use RuntimeSpanAuthorityAdmission::*;

    let cases = [
        (false, false, false, NoSpan),
        (false, true, false, InvalidHostCheckpointWithoutMemInstall),
        (false, true, true, UseLegacyCompactInstalledSpan),
        (
            false,
            false,
            true,
            InvalidCompactInstalledWithoutHostCheckpoint,
        ),
        (true, false, false, DeferCommittedSpanUntilHostCheckpoint),
        (true, true, false, PoisonCommittedSpanWithoutBridgeTerminal),
        (true, true, true, UseCommittedInstalledSpan),
        (
            true,
            false,
            true,
            InvalidCompactInstalledWithoutHostCheckpoint,
        ),
    ];

    for (mem_install_committed, host_checkpoint_materialized, compact_installed, expected) in cases
    {
        assert_eq!(
            classify_runtime_span_authority(
                mem_install_committed,
                host_checkpoint_materialized,
                compact_installed,
            ),
            expected,
            "mem_install_committed={mem_install_committed} host_checkpoint_materialized={host_checkpoint_materialized} compact_installed={compact_installed}"
        );
    }
}

#[test]
fn runtime_span_authority_policy_blocks_direct_p5_switch_for_committed_only() {
    assert_eq!(
        classify_runtime_span_authority(true, false, false,),
        RuntimeSpanAuthorityAdmission::DeferCommittedSpanUntilHostCheckpoint
    );
    assert_eq!(
        classify_runtime_span_authority(true, true, false,),
        RuntimeSpanAuthorityAdmission::PoisonCommittedSpanWithoutBridgeTerminal
    );
}

#[test]
fn runtime_span_authority_admissions_classify_actual_compact_ids() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-legacy",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
        ))
        .expect("append legacy compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-legacy",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
            3,
            "sha1:legacy",
        ))
        .expect("append legacy compact installed");

    append_meminstall_evidence(
        &store,
        "compact-defer",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        8,
        5,
        "sha1:defer",
    );

    append_meminstall_evidence(
        &store,
        "compact-poison",
        id(&[1, 3]),
        SpineOperation::Next,
        8,
        12,
        6,
        "sha1:poison",
    );
    append_bridge_checkpoint(
        &store,
        "compact-poison",
        id(&[1, 3]),
        SpineOperation::Next,
        8,
        12,
        6,
        "sha1:poison",
    );

    append_meminstall_evidence(
        &store,
        "compact-current",
        id(&[1, 4]),
        SpineOperation::Next,
        12,
        16,
        7,
        "sha1:current",
    );
    append_bridge_checkpoint(
        &store,
        "compact-current",
        id(&[1, 4]),
        SpineOperation::Next,
        12,
        16,
        7,
        "sha1:current",
    );
    store
        .append_compact_installed(compact_installed(
            "compact-current",
            id(&[1, 4]),
            SpineOperation::Next,
            12,
            16,
            7,
            "sha1:current",
        ))
        .expect("append current compact installed");

    let admissions = store
        .runtime_span_authority_admissions_matching_hashes(None)
        .expect("classify runtime span authority");

    assert_eq!(
        admissions
            .iter()
            .map(|record| (record.span.compact_id.as_str(), record.admission))
            .collect::<Vec<_>>(),
        vec![
            (
                "compact-legacy",
                RuntimeSpanAuthorityAdmission::UseLegacyCompactInstalledSpan,
            ),
            (
                "compact-defer",
                RuntimeSpanAuthorityAdmission::DeferCommittedSpanUntilHostCheckpoint,
            ),
            (
                "compact-poison",
                RuntimeSpanAuthorityAdmission::PoisonCommittedSpanWithoutBridgeTerminal,
            ),
            (
                "compact-current",
                RuntimeSpanAuthorityAdmission::UseCommittedInstalledSpan,
            ),
        ]
    );
    assert_eq!(admissions[3].span.cut_ordinal, 12);
    assert_eq!(admissions[3].span.fold_end_ordinal, 16);
}

#[test]
fn runtime_span_authority_admissions_keep_transitional_rows_legacy() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-transitional",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:transitional",
    );

    let mut events = store
        .read_compact_index_events()
        .expect("read compact index events");
    events.push(CompactIndexEvent::CompactInstalled {
        seq: 0,
        compact_id: "compact-transitional".to_string(),
        node_id: "1.2".to_string(),
        op: SpineOperation::Next,
        cut_ordinal: 4,
        fold_end_ordinal: 9,
        replacement_history_len: 7,
        memory_path: "nodes/1/2/memory.md".to_string(),
        message_hash: "sha1:transitional".to_string(),
    });
    store
        .write_compact_index_events(events)
        .expect("write transitional compact index");

    let admissions = store
        .runtime_span_authority_admissions_matching_hashes(None)
        .expect("classify transitional row");

    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].span.compact_id, "compact-transitional");
    assert_eq!(
        admissions[0].admission,
        RuntimeSpanAuthorityAdmission::UseLegacyCompactInstalledSpan
    );
}

#[test]
fn runtime_span_authority_admissions_filter_by_surviving_hashes() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-old",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
        ))
        .expect("append old compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-old",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
            3,
            "sha1:old",
        ))
        .expect("append old compact installed");
    append_meminstall_evidence(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );
    append_bridge_checkpoint(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );
    store
        .append_compact_installed(compact_installed(
            "compact-current",
            id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:current",
        ))
        .expect("append current compact installed");

    let surviving_hashes = HashSet::from(["sha1:current".to_string()]);
    let admissions = store
        .runtime_span_authority_admissions_matching_hashes(Some(&surviving_hashes))
        .expect("classify filtered runtime span authority");

    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].span.compact_id, "compact-current");
    assert_eq!(
        admissions[0].admission,
        RuntimeSpanAuthorityAdmission::UseCommittedInstalledSpan
    );
}

#[test]
fn bridge_checkpoint_committed_spans_admit_meminstall_bridge_terminal() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );
    append_bridge_checkpoint(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );

    let spans = store
        .bridge_checkpoint_committed_spans_matching_hashes(None)
        .expect("read bridge checkpoint spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-current");
    assert_eq!(spans[0].node_id, id(&[1, 2]));
    assert_eq!(spans[0].op, SpineOperation::Next);
    assert_eq!(spans[0].cut_ordinal, 4);
    assert_eq!(spans[0].fold_end_ordinal, 9);
    assert_eq!(spans[0].replacement_history_len, 7);
    assert_eq!(spans[0].message_hash, "sha1:current");
}

#[test]
fn bridge_checkpoint_committed_spans_filter_by_surviving_hashes() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    for (compact_id, node_id, start, end, len, hash) in [
        ("compact-old", id(&[1, 1]), 1, 4, 3, "sha1:old"),
        ("compact-current", id(&[1, 2]), 4, 9, 7, "sha1:current"),
    ] {
        append_meminstall_evidence(
            &store,
            compact_id,
            node_id.clone(),
            SpineOperation::Next,
            start,
            end,
            len,
            hash,
        );
        append_bridge_checkpoint(
            &store,
            compact_id,
            node_id.clone(),
            SpineOperation::Next,
            start,
            end,
            len,
            hash,
        );
    }

    let surviving_hashes = HashSet::from(["sha1:current".to_string()]);
    let spans = store
        .bridge_checkpoint_committed_spans_matching_hashes(Some(&surviving_hashes))
        .expect("read filtered bridge checkpoint spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-current");
    assert_eq!(spans[0].message_hash, "sha1:current");
}

#[test]
fn bridge_checkpoint_committed_spans_skip_meminstall_only() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-deferred",
        id(&[1]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:deferred",
    );

    let spans = store
        .bridge_checkpoint_committed_spans_matching_hashes(None)
        .expect("read bridge checkpoint spans");

    assert!(
        spans.is_empty(),
        "MemInstall-only rows are not terminal runtime spans"
    );
}

#[test]
fn bridge_checkpoint_committed_spans_reject_bridge_without_meminstall() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-invalid",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    let mut events = store
        .read_compact_index_events()
        .expect("read compact index events");
    events.push(CompactIndexEvent::BridgeCheckpointCommitted {
        seq: 0,
        compact_id: "compact-invalid".to_string(),
        node_id: "1".to_string(),
        op: SpineOperation::Next,
        cut_ordinal: 4,
        fold_end_ordinal: 9,
        replacement_history_len: 7,
        message_hash: "sha1:invalid".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    });
    store
        .write_compact_index_events(events)
        .expect("write invalid compact index");

    let err = store
        .bridge_checkpoint_committed_spans_matching_hashes(None)
        .expect_err("bridge checkpoint without MemInstall should fail closed");

    assert!(matches!(
        err,
        SpineStoreError::InvalidLedger(message)
            if message.contains("bridge_checkpoint_committed")
                && message.contains("precedes mem_install_committed")
    ));
}

#[test]
fn bridge_checkpoint_committed_spans_ignore_transitional_compact_installed_presence() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-transitional",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:transitional",
    );
    let mut events = store
        .read_compact_index_events()
        .expect("read compact index events");
    events.push(CompactIndexEvent::CompactInstalled {
        seq: 0,
        compact_id: "compact-transitional".to_string(),
        node_id: "1.2".to_string(),
        op: SpineOperation::Next,
        cut_ordinal: 4,
        fold_end_ordinal: 9,
        replacement_history_len: 7,
        memory_path: "nodes/1/2/memory.md".to_string(),
        message_hash: "sha1:transitional".to_string(),
    });
    store
        .write_compact_index_events(events)
        .expect("write transitional compact index");

    let spans = store
        .bridge_checkpoint_committed_spans_matching_hashes(None)
        .expect("read bridge checkpoint spans");

    assert!(
        spans.is_empty(),
        "CompactInstalled without BridgeCheckpointCommitted is not terminal source"
    );
}

#[test]
fn root_meminstall_survivor_matrix_validates_store_evidence() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    append_root_meminstall_evidence(&store, "compact-root", "sha1:root");
    store
        .record_root_epoch_archive(
            &mut state,
            "context compacted",
            7,
            "compact-root",
            "turn-root",
        )
        .expect("record root reset");

    let surviving_hashes = HashSet::from(["sha1:root".to_string()]);
    store
        .validate_root_meminstall_survivors(&surviving_hashes)
        .expect("complete root survivor evidence should validate");

    let (_temp_missing_reset, missing_reset_store) = temp_store();
    missing_reset_store.create().expect("create sidecar");
    append_root_meminstall_evidence(&missing_reset_store, "compact-root", "sha1:root");
    let err = missing_reset_store
        .validate_root_meminstall_survivors(&surviving_hashes)
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
        .validate_root_meminstall_survivors(&surviving_hashes)
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
fn create_writes_root_ledger_and_state_cache() {
    let (_temp, store) = temp_store();

    let state = store.create().expect("create sidecar");

    assert_eq!(state.cursor(), &id(&[1, 1]));
    assert_eq!(
        read_json_lines(store.tree_path()),
        vec![json!({
            "type": "spine_initialized",
            "seq": 1,
            "state": {
                "cursor": "1.1",
                "nodes": [
                    {
                        "node_id": "1",
                        "parent_id": null,
                        "raw_start_ordinal": 0,
                        "status": "opened",
                        "summary": null,
                        "memory_path": "nodes/1/memory.md",
                        "plan_path": "nodes/1/plan.json",
                    },
                    {
                        "node_id": "1.1",
                        "parent_id": "1",
                        "raw_start_ordinal": 0,
                        "status": "live",
                        "summary": null,
                        "memory_path": "nodes/1/1/memory.md",
                        "plan_path": "nodes/1/1/plan.json",
                    },
                ],
            },
        })]
    );
    assert_eq!(
        read_json(store.state_path()),
        json!({
            "cursor": "1.1",
            "nodes": [
                {
                    "node_id": "1",
                    "parent_id": null,
                    "raw_start_ordinal": 0,
                    "status": "opened",
                    "summary": null,
                    "memory_path": "nodes/1/memory.md",
                    "plan_path": "nodes/1/plan.json",
                },
                {
                    "node_id": "1.1",
                    "parent_id": "1",
                    "raw_start_ordinal": 0,
                    "status": "live",
                    "summary": null,
                    "memory_path": "nodes/1/1/memory.md",
                    "plan_path": "nodes/1/1/plan.json",
                },
            ],
        })
    );
    assert!(store.root().join("nodes").join("1").is_dir());
    assert!(store.root().join("nodes").join("1").join("1").is_dir());
    assert!(store.trajs_index_path().exists());
    assert!(store.compact_index_path().exists());
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
fn failed_tree_append_does_not_advance_metadata_cache() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");

    assert_eq!(
        store.next_tree_event_seq().expect("next seq after create"),
        2
    );
    let bad_snapshot = PlanSnapshot {
        node_id: "1".to_string(),
        revision: 1,
        explanation: Some("wrong seq".to_string()),
        items: Vec::new(),
        spine_plantree: None,
        source_turn_id: "turn-bad".to_string(),
        event_seq: 3,
    };
    let error = store
        .write_plan_snapshot(&id(&[1]), &bad_snapshot)
        .expect_err("wrong tree seq should fail before append");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("tree event seq 3 does not match next tree seq 2")
    ));
    assert_eq!(
        store
            .next_tree_event_seq()
            .expect("next seq after failed append"),
        2
    );

    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("next valid append should still use seq 2");
    assert_eq!(read_json_lines(store.tree_path())[1]["seq"], json!(2));
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
            "to_parent_id": "1.1",
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
            "next_parent_id": "2",
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
        Some(NodeStatus::Finished)
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
            "next_parent_id": "2",
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
fn state_cache_mismatch_replays_and_repairs_cache() {
    let (_temp, store) = temp_store();
    let mut state = store.create().expect("create sidecar");
    store
        .record_transition(&mut state, SpineOperation::Open, None, 8, "turn-1")
        .expect("record transition");
    let mut cache = read_json(store.state_path());
    cache["cursor"] = json!("9");
    std::fs::write(
        store.state_path(),
        serde_json::to_string_pretty(&cache).expect("serialize cache"),
    )
    .expect("write mutated cache");

    let loaded = store.load().expect("mismatched cache should replay");

    assert_eq!(loaded.cursor(), &id(&[1, 1, 1]));
    let repaired = read_json(store.state_path());
    assert_eq!(repaired["cursor"], json!("1.1.1"));
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
                        existing_node_id: None,
                        summary: "Continue future".to_string(),
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
    let tree = read_json_lines(store.tree_path());
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[0]["type"], "spine_initialized");
    assert_eq!(
        tree[1],
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
                            "existing_node_id": null,
                            "summary": "Continue future",
                            "status": "pending",
                            "checkpoints": [{"task": "keep root task focused", "status": "pending"}],
                        },
                    ],
                },
            },
            "source_turn_id": "turn-alloc",
        })
    );
    assert_eq!(store.load().expect("reload sidecar"), state);
}

#[test]
fn replay_rejects_duplicate_plantree_existing_scope_nodes() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    let snapshot = PlanSnapshot {
        node_id: "1".to_string(),
        revision: 1,
        explanation: Some("ambiguous duplicate scope".to_string()),
        items: Vec::new(),
        spine_plantree: Some(PlanTreeSnapshot {
            anchor_node_id: "1".to_string(),
            root: crate::spine::plan_bridge::PlanTreeScope {
                existing_node_id: Some("1".to_string()),
                summary: "Root scope".to_string(),
                status: None,
                checkpoints: Vec::new(),
                children: vec![crate::spine::plan_bridge::PlanTreeScope {
                    existing_node_id: Some("1".to_string()),
                    summary: "Duplicate root scope".to_string(),
                    status: None,
                    checkpoints: Vec::new(),
                    children: Vec::new(),
                }],
            },
        }),
        source_turn_id: "turn-dup".to_string(),
        event_seq: 2,
    };

    store
        .write_plan_snapshot(&id(&[1]), &snapshot)
        .expect("write duplicate PlanTree snapshot");

    let error = store
        .load()
        .expect_err("duplicate PlanTree scope nodes should fail replay");
    assert!(
        error
            .to_string()
            .contains("spine_plantree duplicates scope node [1]"),
        "unexpected error: {error}"
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
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:abc",
        ))
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
                "memory_path": "nodes/1/2/memory.md",
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
fn bridge_checkpoint_committed_writes_host_checkpoint_phase_marker() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref,
        ))
        .expect("append mem install commit");
    store
        .append_bridge_checkpoint_committed(bridge_checkpoint_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
        ))
        .expect("append bridge checkpoint");

    assert_eq!(
        read_json_lines(store.compact_index_path())[2],
        json!({
            "type": "bridge_checkpoint_committed",
            "seq": 3,
            "compact_id": "compact-1",
            "node_id": "1",
            "op": "next",
            "cut_ordinal": 4,
            "fold_end_ordinal": 9,
            "replacement_history_len": 7,
            "message_hash": "sha1:message",
            "source_rollout_ref": "../rollout.jsonl",
        })
    );
    store
        .load()
        .expect("bridge checkpoint without terminal is a replayable poison boundary");
}

#[test]
fn bridge_checkpoint_committed_requires_meminstall() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");

    let error = store
        .append_bridge_checkpoint_committed(bridge_checkpoint_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
        ))
        .expect_err("bridge checkpoint before MemInstall should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("precedes mem_install_committed")
    ));
}

#[test]
fn bridge_checkpoint_committed_rejects_meminstall_mismatch() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref,
        ))
        .expect("append mem install commit");

    let error = store
        .append_bridge_checkpoint_committed(bridge_checkpoint_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            8,
            "sha1:message",
        ))
        .expect_err("replacement_history_len mismatch should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("replacement_history_len does not match mem_install_committed")
    ));
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_committed_writes_semantic_marker() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref.clone(),
        ))
        .expect("append mem install commit");
    store
        .append_bridge_checkpoint_committed(bridge_checkpoint_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
        ))
        .expect("append bridge checkpoint");

    assert_eq!(
        read_json_lines(store.compact_index_path()),
        vec![
            json!({
                "type": "compact_started",
                "seq": 1,
                "compact_id": "compact-1",
                "node_id": "1",
                "op": "next",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "strategy": "codex_builtin_text",
                "raw_trajs": "raw/rollout.raw.jsonl",
                "rollout": "../rollout.jsonl",
            }),
            json!({
                "type": "mem_install_committed",
                "seq": 2,
                "schema_version": 1,
                "compact_id": "compact-1",
                "node_id": "1",
                "op": "next",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "memory_section_id": "nodes/1/memory.md#section-0",
                "body_hash": memory_body_hash("portable body"),
                "storage_ref": "nodes/1/memory.md",
                "message_hash": "sha1:message",
                "replacement_history_len": 7,
                "projection_ref": "projection:seq-1",
                "source_rollout_ref": "../rollout.jsonl",
                "committed_at_seq": 2,
            }),
            json!({
                "type": "bridge_checkpoint_committed",
                "seq": 3,
                "compact_id": "compact-1",
                "node_id": "1",
                "op": "next",
                "cut_ordinal": 4,
                "fold_end_ordinal": 9,
                "replacement_history_len": 7,
                "message_hash": "sha1:message",
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
fn compact_installed_after_meminstall_requires_bridge_checkpoint() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref,
        ))
        .expect("append mem install commit");

    let error = store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
        ))
        .expect_err("new compact installed must have bridge checkpoint marker");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("without bridge_checkpoint_committed")
    ));
}

#[test]
fn runtime_fast_fail_compact_index_legacy_compact_installed_is_not_mem_install_commit() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-legacy",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_memory_section(&id(&[1]), "\n\n## Auto Compact\n\nlegacy body\n")
        .expect("append legacy memory section");
    let body_ref = store
        .generated_memory_sections(&id(&[1]))
        .expect("generated sections")[0]
        .body_ref();
    store
        .append_compact_installed(compact_installed(
            "compact-legacy",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:legacy",
        ))
        .expect("append compact installed");

    store
        .load()
        .expect("legacy compact installed remains valid");
    assert!(
        store
            .committed_mem_installs()
            .expect("committed mem installs")
            .is_empty()
    );
    let error = store
        .append_mem_install_committed(mem_install_committed(
            "compact-legacy",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:legacy",
            body_ref,
        ))
        .expect_err("mem install after checkpoint should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallCheckpointBeforeCommit {
            terminal: "compact_installed",
            ..
        })
    ));
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref,
        ))
        .expect_err("missing started should fail closed");
    assert!(matches!(
        error,
        SpineStoreError::RuntimeFastFail(RuntimeFastFailError::MemInstallMissingStarted { .. })
    ));
}

#[test]
fn runtime_fast_fail_compact_index_mem_install_missing_body_fails_closed() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
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
            SpineOperation::Next,
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
    let mut record = mem_install_committed(
        "compact-1",
        id(&[1]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
        body_ref,
    );
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
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
            body_ref.clone(),
        ))
        .expect("append first mem install commit");

    let error = store
        .append_mem_install_committed(mem_install_committed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
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
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
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
            SpineOperation::Next,
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
    source_store
        .record_transition(&mut source_state, SpineOperation::Open, None, 2, "turn-1")
        .expect("record source transition");
    source_store
        .append_memory_section(&id(&[1, 1]), "\n\n## Auto Compact\n\nsource memory\n")
        .expect("write source memory");

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child");
    child_store
        .record_projection_reset(
            source_state.clone(),
            "fork_seed",
            None,
            test_projection_epoch(&source_state),
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
fn latest_projection_epoch_ignores_legacy_and_rejects_partial_metadata() {
    let (_temp, store) = temp_store();
    let state = store.create().expect("create sidecar");

    store
        .append_tree_event(&TreeEvent::ProjectionReset {
            seq: store.next_tree_seq().expect("legacy projection seq"),
            reason: "legacy_projection".to_string(),
            source_turn_id: None,
            source_rollout_ref: None,
            processed_rollout_len: None,
            processed_rollout_hash: None,
            effective_raw_len: None,
            surviving_turn_ids_hash: None,
            surviving_compact_ids: None,
            state_hash: None,
            state: StateSnapshot::from_state(&state),
        })
        .expect("append legacy projection reset");
    assert!(
        store
            .latest_projection_epoch()
            .expect("legacy projection epoch")
            .is_none()
    );

    store
        .append_tree_event(&TreeEvent::ProjectionReset {
            seq: store.next_tree_seq().expect("partial projection seq"),
            reason: "partial_projection".to_string(),
            source_turn_id: None,
            source_rollout_ref: Some("rollout.jsonl".to_string()),
            processed_rollout_len: None,
            processed_rollout_hash: None,
            effective_raw_len: None,
            surviving_turn_ids_hash: None,
            surviving_compact_ids: None,
            state_hash: None,
            state: StateSnapshot::from_state(&state),
        })
        .expect("append partial projection reset");

    let error = store
        .latest_projection_epoch()
        .expect_err("partial projection epoch must fail closed");
    assert!(matches!(
        error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("partial projection epoch metadata")
    ));
    let load_error = store
        .load()
        .expect_err("partial projection epoch must also fail replay");
    assert!(matches!(
        load_error,
        SpineStoreError::InvalidLedger(message)
            if message.contains("partial projection epoch metadata")
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
            "next_parent_id": "2",
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
    source_store
        .record_transition(
            &mut source_state,
            SpineOperation::Open,
            None,
            2,
            "surviving-turn",
        )
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
                spine_plantree: None,
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
        .record_projection_reset(
            source_state.clone(),
            "fork_seed",
            None,
            test_projection_epoch(&source_state),
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
    assert!(!child_store.plan_path(&id(&[1, 1])).exists());
}

#[test]
fn compact_index_started_then_installed_loads() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:abc",
        ))
        .expect("append compact installed");

    store.load().expect("resolved compact should load");
}

#[test]
fn installed_compact_spans_return_full_runtime_ledger() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-child",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
        ))
        .expect("append child compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-child",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
            3,
            "sha1:child",
        ))
        .expect("append child compact installed");
    store
        .append_compact_started(compact_started(
            "compact-scope",
            id(&[1]),
            SpineOperation::Close,
            1,
            6,
        ))
        .expect("append scope compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-scope",
            id(&[1]),
            SpineOperation::Close,
            1,
            6,
            2,
            "sha1:scope",
        ))
        .expect("append scope compact installed");
    store
        .append_compact_started(compact_started(
            "compact-sibling",
            id(&[2]),
            SpineOperation::Next,
            7,
            9,
        ))
        .expect("append sibling compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-sibling",
            id(&[2]),
            SpineOperation::Next,
            7,
            9,
            4,
            "sha1:sibling",
        ))
        .expect("append sibling compact installed");

    let spans = store
        .installed_compact_spans()
        .expect("read installed spans");

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].compact_id, "compact-child");
    assert_eq!(spans[0].node_id, id(&[1, 1]));
    assert_eq!(spans[0].op, SpineOperation::Next);
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
    source_store
        .append_compact_started(compact_started(
            "compact-old",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
        ))
        .expect("append old compact started");
    source_store
        .append_compact_installed(compact_installed(
            "compact-old",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
            3,
            "sha1:old",
        ))
        .expect("append old compact installed");
    source_store
        .append_compact_started(compact_started(
            "compact-new",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            6,
        ))
        .expect("append new compact started");
    source_store
        .append_compact_installed(compact_installed(
            "compact-new",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            6,
            3,
            "sha1:new",
        ))
        .expect("append new compact installed");

    let surviving_hashes = HashSet::from(["sha1:new".to_string()]);
    let filtered_spans = source_store
        .installed_compact_spans_matching_hashes(Some(&surviving_hashes))
        .expect("read filtered spans");
    assert_eq!(filtered_spans.len(), 1);
    assert_eq!(filtered_spans[0].compact_id, "compact-new");
    assert_eq!(filtered_spans[0].fold_end_ordinal, 6);

    let child_rollout = temp.path().join("rollout-child.jsonl");
    let child_store = SpineSidecarStore::create_for_rollout(child_rollout).expect("child store");
    child_store.create().expect("create child sidecar");
    child_store
        .copy_projected_compact_index_from(&source_store, &surviving_hashes)
        .expect("copy filtered compact index");
    let copied_spans = child_store
        .installed_compact_spans()
        .expect("read copied spans");
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
fn committed_mem_install_spans_match_compact_installed_for_suffix() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );
    append_bridge_checkpoint(
        &store,
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1, 2]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:message",
        ))
        .expect("append compact installed");

    assert_eq!(
        store
            .committed_mem_install_spans()
            .expect("read committed spans"),
        store
            .installed_compact_spans()
            .expect("read installed spans")
    );
}

#[test]
fn committed_mem_install_spans_match_compact_installed_for_root_archive() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        0,
        7,
        3,
        "sha1:root",
    );
    append_bridge_checkpoint(
        &store,
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        0,
        7,
        3,
        "sha1:root",
    );
    store
        .append_compact_installed(compact_installed(
            "compact-root",
            id(&[1]),
            SpineOperation::Archive,
            0,
            7,
            3,
            "sha1:root",
        ))
        .expect("append compact installed");

    assert_eq!(
        store
            .committed_mem_install_spans()
            .expect("read committed spans"),
        store
            .installed_compact_spans()
            .expect("read installed spans")
    );
}

#[test]
fn bridge_checkpoint_span_source_audit_accepts_matching_sources() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );
    append_bridge_checkpoint(
        &store,
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );

    audit_bridge_checkpoint_span_source(&store, None, "compact index")
        .expect("matching span sources should pass shadow audit");
}

#[test]
fn bridge_checkpoint_span_source_audit_reports_committed_only_span() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-committed-only",
        id(&[1]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );

    let error = audit_bridge_checkpoint_span_source(&store, None, "compact index")
        .expect_err("committed-only source mismatch should be reported");

    assert_eq!(error.invariant(), INV_MEM_EVIDENCE);
    assert!(error.to_string().contains("compact-committed-only"));
    assert!(
        error
            .to_string()
            .contains("BridgeCheckpointCommitted span source did not match")
    );
}

#[test]
fn bridge_checkpoint_span_source_audit_ignores_legacy_installed_outside_current_hash() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_started(compact_started(
            "compact-legacy",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
        ))
        .expect("append legacy compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-legacy",
            id(&[1, 1]),
            SpineOperation::Next,
            1,
            4,
            3,
            "sha1:legacy",
        ))
        .expect("append legacy compact installed");
    append_meminstall_evidence(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );
    append_bridge_checkpoint(
        &store,
        "compact-current",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:current",
    );

    let current_hashes = HashSet::from(["sha1:current".to_string()]);
    audit_bridge_checkpoint_span_source(&store, Some(&current_hashes), "compact index")
        .expect("current-hash shadow audit should ignore legacy installed-only spans");
}

#[test]
fn committed_mem_install_spans_filter_by_surviving_hashes() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-old",
        id(&[1, 1]),
        SpineOperation::Next,
        1,
        4,
        3,
        "sha1:old",
    );
    append_meminstall_evidence(
        &store,
        "compact-new",
        id(&[1, 2]),
        SpineOperation::Next,
        4,
        8,
        5,
        "sha1:new",
    );

    let surviving_hashes = HashSet::from(["sha1:new".to_string()]);
    let spans = store
        .committed_mem_install_spans_matching_hashes(Some(&surviving_hashes))
        .expect("read filtered committed spans");

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].compact_id, "compact-new");
    assert_eq!(spans[0].message_hash, "sha1:new");
}

#[test]
fn committed_mem_install_spans_reject_missing_body() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    append_meminstall_evidence(
        &store,
        "compact-1",
        id(&[1]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );
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
    append_meminstall_evidence(
        &store,
        "compact-1",
        id(&[1]),
        SpineOperation::Next,
        4,
        9,
        7,
        "sha1:message",
    );
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
            SpineOperation::Next,
            1,
            3,
        ))
        .expect("append failed compact started");
    store
        .append_compact_failed(compact_terminal(
            "compact-failed",
            id(&[1]),
            SpineOperation::Next,
            1,
            3,
            "failed",
        ))
        .expect("append compact failed");
    store
        .append_compact_started(compact_started(
            "compact-interrupted",
            id(&[1, 1]),
            SpineOperation::Next,
            3,
            5,
        ))
        .expect("append interrupted compact started");
    store
        .append_compact_interrupted(compact_terminal(
            "compact-interrupted",
            id(&[1, 1]),
            SpineOperation::Next,
            3,
            5,
            "interrupted",
        ))
        .expect("append compact interrupted");
    append_meminstall_evidence(
        &store,
        "compact-ok",
        id(&[1, 2]),
        SpineOperation::Next,
        5,
        8,
        4,
        "sha1:ok",
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
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_failed(compact_terminal(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_interrupted(compact_terminal(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
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
            "op": "next",
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
fn compact_index_terminal_without_started_fails_load() {
    let (_temp, store) = temp_store();
    store.create().expect("create sidecar");
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:abc",
        ))
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
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1]),
            SpineOperation::Close,
            4,
            9,
            7,
            "sha1:abc",
        ))
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
        .append_compact_started(compact_started(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
        ))
        .expect("append compact started");
    store
        .append_compact_installed(compact_installed(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            7,
            "sha1:abc",
        ))
        .expect("append compact installed");
    store
        .append_compact_failed(compact_terminal(
            "compact-1",
            id(&[1]),
            SpineOperation::Next,
            4,
            9,
            "late failure",
        ))
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
