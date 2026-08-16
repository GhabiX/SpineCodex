use super::context_plan::prepare_codex_context_plan;
use crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES;
use crate::context::spine_model_item_wire_bytes;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use spine_core::host::ContextEpoch;
use spine_core::host::ContextLabel;
use spine_core::host::ContextPlanCell;
use spine_core::host::ContextPlanRecipe;
use spine_core::host::ContextPlanSource;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::RecordDigest;
use spine_core::host::SourceLedger;
use spine_core::host::SpineChar;
use spine_core::host::ThreadNamespace;
use spine_core::host::TrimEdit;
use std::collections::BTreeMap;

#[test]
fn canonical_context_preserves_oversized_base_item() {
    let thread = ThreadNamespace::parse("thread").expect("thread");
    let mut source = SourceLedger::new(thread.clone(), ContextEpoch::ZERO).expect("source ledger");
    let message = Message {
        boundary: RawBoundary(1),
        role: MessageRole::User,
        content: "x".repeat(50_000),
    };
    let source_id = source
        .append([SpineChar::Message(message.clone())])
        .expect("append source")
        .remove(0);
    let snapshot = source.snapshot();
    let recipe = ContextPlanRecipe {
        schema: spine_core::host::CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread,
        epoch: ContextEpoch::ZERO,
        source_snapshot_digest: snapshot.digest().clone(),
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: Vec::new(),
        }],
        memory_slots: Vec::new(),
        plan_digest: RecordDigest::parse("0".repeat(64)).expect("placeholder digest"),
    }
    .finalize_digest()
    .expect("recipe");
    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: message.content,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let prepared = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, item)]),
        &BTreeMap::new(),
        "",
    )
    .expect("Base-owned source items are not rewritten by the Spine gate");

    assert!(
        spine_model_item_wire_bytes(&prepared.items[0]).unwrap() > MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
}

#[test]
fn user_anchor_is_a_separate_bounded_item_before_an_unchanged_base_source() {
    let thread = ThreadNamespace::parse("thread").expect("thread");
    let mut source = SourceLedger::new(thread.clone(), ContextEpoch::ZERO).expect("source ledger");
    let message = Message {
        boundary: RawBoundary(1),
        role: MessageRole::User,
        content: "x".repeat(MAX_SPINE_MODEL_ITEM_WIRE_BYTES),
    };
    let source_id = source
        .append([SpineChar::Message(message.clone())])
        .expect("append source")
        .remove(0);
    let snapshot = source.snapshot();
    let recipe = ContextPlanRecipe {
        schema: spine_core::host::CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread,
        epoch: ContextEpoch::ZERO,
        source_snapshot_digest: snapshot.digest().clone(),
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: vec![ContextLabel::UserAnchor(42)],
        }],
        memory_slots: Vec::new(),
        plan_digest: RecordDigest::parse("0".repeat(64)).expect("placeholder digest"),
    }
    .finalize_digest()
    .expect("recipe");
    let source_item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: message.content,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let prepared = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, source_item.clone())]),
        &BTreeMap::new(),
        "",
    )
    .expect("the bounded anchor must not claim or rewrite the Base source item");

    assert_eq!(prepared.items.len(), 2);
    let ResponseItem::Message { content, .. } = &prepared.items[0] else {
        panic!("anchor must be a user message");
    };
    assert_eq!(
        content,
        &[ContentItem::InputText {
            text: "[U42]\n".to_string(),
        }]
    );
    assert!(
        spine_model_item_wire_bytes(&prepared.items[0]).unwrap() <= MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
    assert_eq!(prepared.items[1], source_item);
    assert!(
        spine_model_item_wire_bytes(&prepared.items[1]).unwrap() > MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
}

#[test]
fn canonical_context_truncates_oversized_tool_output_before_projection() {
    let thread = ThreadNamespace::parse("thread").expect("thread");
    let mut source = SourceLedger::new(thread.clone(), ContextEpoch::ZERO).expect("source ledger");
    let source_id = source
        .append([SpineChar::Opaque {
            boundary: RawBoundary(1),
        }])
        .expect("append source")
        .remove(0);
    let snapshot = source.snapshot();
    let recipe = ContextPlanRecipe {
        schema: spine_core::host::CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread,
        epoch: ContextEpoch::ZERO,
        source_snapshot_digest: snapshot.digest().clone(),
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: vec![ContextLabel::ToolOutput(TrimEdit::Tagged {
                trim_id: "trim-large-output".to_string(),
                body: "x".repeat(50_000),
                eligible: true,
            })],
        }],
        memory_slots: Vec::new(),
        plan_digest: RecordDigest::parse("0".repeat(64)).expect("placeholder digest"),
    }
    .finalize_digest()
    .expect("recipe");
    let item = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "large-output".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".repeat(50_000)),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    let prepared = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, item)]),
        &BTreeMap::new(),
        "",
    )
    .expect("tool output should be bounded, not reject the turn");

    assert_eq!(prepared.items.len(), 1);
    let encoded = serde_json::to_string(&prepared.items[0]).expect("serialize projected item");
    assert!(
        spine_model_item_wire_bytes(&prepared.items[0]).unwrap() <= MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
    assert!(encoded.contains("tokens truncated"));
}

#[test]
fn canonical_context_accounts_for_json_escaping_when_truncating_tool_output() {
    let thread = ThreadNamespace::parse("thread").expect("thread");
    let mut source = SourceLedger::new(thread.clone(), ContextEpoch::ZERO).expect("source ledger");
    let source_id = source
        .append([SpineChar::Opaque {
            boundary: RawBoundary(1),
        }])
        .expect("append source")
        .remove(0);
    let snapshot = source.snapshot();
    let escaped_json = serde_json::json!({
        "items": (0..700)
            .map(|index| serde_json::json!({
                "path": format!("src/module_{index}/file.rs"),
                "patch": "\\\"quoted\\\"\\nline\\\\tail",
            }))
            .collect::<Vec<_>>(),
    })
    .to_string();
    let recipe = ContextPlanRecipe {
        schema: spine_core::host::CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread,
        epoch: ContextEpoch::ZERO,
        source_snapshot_digest: snapshot.digest().clone(),
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: vec![ContextLabel::ToolOutput(TrimEdit::Sliced(
                escaped_json.clone(),
            ))],
        }],
        memory_slots: Vec::new(),
        plan_digest: RecordDigest::parse("0".repeat(64)).expect("placeholder digest"),
    }
    .finalize_digest()
    .expect("recipe");
    let item = ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "escaped-output".to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(escaped_json),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    let prepared = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, item)]),
        &BTreeMap::new(),
        "",
    )
    .expect("escaped tool output should be bounded after serialization");

    let [prepared_item] = prepared.items.as_slice() else {
        panic!("expected one projected item");
    };
    let encoded = serde_json::to_string(prepared_item).expect("serialize projected item");
    assert!(spine_model_item_wire_bytes(prepared_item).unwrap() <= MAX_SPINE_MODEL_ITEM_WIRE_BYTES);
    assert!(encoded.contains("tokens truncated"));
}
