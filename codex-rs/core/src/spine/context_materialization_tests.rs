use super::*;
use crate::spine::ids::NodeId;
use crate::spine::store::CompactAttemptRecord;
use crate::spine::store::CompactStartedRecord;
use crate::spine::store::MemInstallCommittedRecord;
use crate::spine::store::NoteEvidenceCommittedRecord;
use crate::spine::store::NotePlacement;
use crate::spine::store::SpineOperation;
use crate::spine::store::SpineSidecarStore;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineCompactedCheckpoint;
use codex_protocol::protocol::SpineCompactedCheckpointKind;

fn user_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

fn assistant_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

fn response_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![if role == "assistant" {
            ContentItem::OutputText {
                text: text.to_string(),
            }
        } else {
            ContentItem::InputText {
                text: text.to_string(),
            }
        }],
        phase: None,
    }
}

fn spine_call(call_id: &str, op: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: op.to_string(),
        namespace: Some(crate::spine::SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    })
}

fn call_output(call_id: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("ok".to_string()),
            success: Some(true),
        },
    })
}

#[test]
fn materialize_suffix_compact_from_sidecar_without_replacement_history() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let rollout_path = temp.path().join("rollout.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path)?;
    let mut state = store.create()?;
    store.record_transition(
        &mut state,
        SpineOperation::Open,
        None::<String>,
        3,
        "turn-open",
    )?;
    store.record_transition(
        &mut state,
        SpineOperation::Close,
        "child done",
        7,
        "turn-close",
    )?;
    let node_id = NodeId::from_segments(vec![1, 1, 1]);
    let attempt = CompactAttemptRecord {
        compact_id: "compact-child".to_string(),
        node_id: node_id.clone(),
        op: SpineOperation::Close,
        cut_ordinal: 3,
        fold_end_ordinal: 7,
    };
    store.append_compact_started(CompactStartedRecord {
        attempt: attempt.clone(),
        strategy: "test".to_string(),
        rollout: "../rollout.jsonl".to_string(),
    })?;
    store.append_memory_section(&node_id, "\n\n## Auto Compact\n\nchild facts\n")?;
    let body_ref = store
        .generated_memory_sections(&node_id)?
        .last()
        .expect("memory section")
        .body_ref();
    store.append_mem_install_committed(MemInstallCommittedRecord {
        attempt,
        body_ref,
        projection_ref: "projection:test".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    })?;

    let replay_items = vec![
        user_message("root prelude"),
        spine_call("open-1", crate::spine::SPINE_TOOL_OPEN, "{}"),
        call_output("open-1"),
        user_message("child raw"),
        assistant_message("child result"),
        spine_call(
            "close-1",
            crate::spine::SPINE_TOOL_CLOSE,
            r#"{"summary":"child done"}"#,
        ),
        call_output("close-1"),
        RolloutItem::Compacted(CompactedItem {
            message: "Spine compacted 1.1.1 [3, 7)".to_string(),
            replacement_history: None,
            spine: Some(SpineCompactedCheckpoint {
                compact_id: "compact-child".to_string(),
                kind: SpineCompactedCheckpointKind::Suffix,
            }),
        }),
        assistant_message("parent tail"),
    ];
    let materialized = materialize_spine_context(SpineMaterializationInput {
        replay_items: &replay_items,
        branch_ref: rollout_path.to_string_lossy().into_owned(),
        persisted_prefix_items: &replay_items,
        store: &store,
    })?;
    let rendered = serde_json::to_string(&materialized.history)?;

    assert!(rendered.contains("root prelude"));
    assert!(rendered.contains("Node: 1.1.1"));
    assert!(rendered.contains("Summary: child done"));
    assert!(rendered.contains("child facts"));
    assert!(rendered.contains("<spine_handoff>"));
    assert!(rendered.contains("parent tail"));
    assert!(!rendered.contains("child raw"));
    assert!(!rendered.contains("child result"));
    Ok(())
}

#[test]
fn materialize_root_epoch_from_sidecar_without_replacement_history() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let rollout_path = temp.path().join("rollout.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path)?;
    let mut state = store.create()?;
    store.record_root_epoch_archive(
        &mut state,
        "Context compacted",
        2,
        "compact-root",
        "turn-root",
    )?;
    let root_id = NodeId::from_segments(vec![1]);
    let attempt = CompactAttemptRecord {
        compact_id: "compact-root".to_string(),
        node_id: root_id.clone(),
        op: SpineOperation::Archive,
        cut_ordinal: 0,
        fold_end_ordinal: 2,
    };
    store.append_compact_started(CompactStartedRecord {
        attempt: attempt.clone(),
        strategy: "test".to_string(),
        rollout: "../rollout.jsonl".to_string(),
    })?;
    store.append_note_evidence_committed(NoteEvidenceCommittedRecord {
        compact_id: "compact-root".to_string(),
        placement: NotePlacement::BeforeMem,
        kind: "initial_context_0".to_string(),
        items: vec![response_message(
            "developer",
            "<spine_initial_context>structured initial context</spine_initial_context>",
        )],
        projection_ref: "projection:test:root".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    })?;
    store.append_memory_section(&root_id, "\n\n## Auto Compact\n\nroot facts\n")?;
    let body_ref = store
        .generated_memory_sections(&root_id)?
        .last()
        .expect("memory section")
        .body_ref();
    store.append_mem_install_committed(MemInstallCommittedRecord {
        attempt,
        body_ref,
        projection_ref: "projection:test:root".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    })?;

    let replay_items = vec![
        user_message("root raw one"),
        assistant_message("root raw two"),
        RolloutItem::Compacted(CompactedItem {
            message: "Spine compacted root epoch 1 [0, 2)".to_string(),
            replacement_history: None,
            spine: Some(SpineCompactedCheckpoint {
                compact_id: "compact-root".to_string(),
                kind: SpineCompactedCheckpointKind::RootEpoch,
            }),
        }),
        user_message("new epoch tail"),
    ];
    let materialized = materialize_spine_context(SpineMaterializationInput {
        replay_items: &replay_items,
        branch_ref: rollout_path.to_string_lossy().into_owned(),
        persisted_prefix_items: &replay_items,
        store: &store,
    })?;
    let rendered = serde_json::to_string(&materialized.history)?;

    assert!(rendered.contains("structured initial context"));
    assert!(rendered.contains("Node: 1"));
    assert!(rendered.contains("root facts"));
    assert!(rendered.contains("new epoch tail"));
    assert!(!rendered.contains("root raw one"));
    assert!(!rendered.contains("root raw two"));
    Ok(())
}

#[test]
fn materialize_root_epoch_without_note_evidence_fails_meminstall() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let rollout_path = temp.path().join("rollout.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path)?;
    let mut state = store.create()?;
    store.record_root_epoch_archive(
        &mut state,
        "Context compacted",
        2,
        "compact-root",
        "turn-root",
    )?;
    let root_id = NodeId::from_segments(vec![1]);
    let attempt = CompactAttemptRecord {
        compact_id: "compact-root".to_string(),
        node_id: root_id.clone(),
        op: SpineOperation::Archive,
        cut_ordinal: 0,
        fold_end_ordinal: 2,
    };
    store.append_compact_started(CompactStartedRecord {
        attempt: attempt.clone(),
        strategy: "test".to_string(),
        rollout: "../rollout.jsonl".to_string(),
    })?;
    store.append_memory_section(&root_id, "\n\n## Auto Compact\n\nroot facts\n")?;
    let body_ref = store
        .generated_memory_sections(&root_id)?
        .last()
        .expect("memory section")
        .body_ref();
    let error = store
        .append_mem_install_committed(MemInstallCommittedRecord {
            attempt,
            body_ref,
            projection_ref: "projection:test:root".to_string(),
            source_rollout_ref: "../rollout.jsonl".to_string(),
        })
        .expect_err("root MemInstall without Note evidence must fail closed");
    assert!(error.to_string().contains("missing NoteEvidenceCommitted"));
    Ok(())
}

#[test]
fn materialize_root_epoch_with_empty_context_note_replays_memory() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let rollout_path = temp.path().join("rollout.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path)?;
    let mut state = store.create()?;
    store.record_root_epoch_archive(
        &mut state,
        "Context compacted",
        2,
        "compact-root",
        "turn-root",
    )?;
    let root_id = NodeId::from_segments(vec![1]);
    let attempt = CompactAttemptRecord {
        compact_id: "compact-root".to_string(),
        node_id: root_id.clone(),
        op: SpineOperation::Archive,
        cut_ordinal: 0,
        fold_end_ordinal: 2,
    };
    store.append_compact_started(CompactStartedRecord {
        attempt: attempt.clone(),
        strategy: "test".to_string(),
        rollout: "../rollout.jsonl".to_string(),
    })?;
    store.append_note_evidence_committed(NoteEvidenceCommittedRecord {
        compact_id: "compact-root".to_string(),
        placement: NotePlacement::BeforeMem,
        kind: "initial_context_empty".to_string(),
        items: vec![response_message(
            "developer",
            r#"<spine_initial_context_empty runtime_generated="true" />"#,
        )],
        projection_ref: "projection:test:root".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    })?;
    store.append_memory_section(&root_id, "\n\n## Auto Compact\n\nroot facts\n")?;
    let body_ref = store
        .generated_memory_sections(&root_id)?
        .last()
        .expect("memory section")
        .body_ref();
    store.append_mem_install_committed(MemInstallCommittedRecord {
        attempt,
        body_ref,
        projection_ref: "projection:test:root".to_string(),
        source_rollout_ref: "../rollout.jsonl".to_string(),
    })?;

    let replay_items = vec![
        user_message("root raw one"),
        assistant_message("root raw two"),
        RolloutItem::Compacted(CompactedItem {
            message: "Spine compacted root epoch 1 [0, 2)".to_string(),
            replacement_history: None,
            spine: Some(SpineCompactedCheckpoint {
                compact_id: "compact-root".to_string(),
                kind: SpineCompactedCheckpointKind::RootEpoch,
            }),
        }),
    ];
    let materialized = materialize_spine_context(SpineMaterializationInput {
        replay_items: &replay_items,
        branch_ref: rollout_path.to_string_lossy().into_owned(),
        persisted_prefix_items: &replay_items,
        store: &store,
    })?;
    let rendered = serde_json::to_string(&materialized.history)?;

    assert!(rendered.contains("spine_initial_context_empty"));
    assert!(rendered.contains("Node: 1"));
    assert!(rendered.contains("root facts"));
    assert!(!rendered.contains("root raw one"));
    assert!(!rendered.contains("root raw two"));
    Ok(())
}
