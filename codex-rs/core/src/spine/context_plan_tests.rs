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
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::SourceLedger;
use spine_core::host::SpineChar;
use spine_core::host::ThreadNamespace;
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
        source_snapshot_digest: None,
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: Vec::new(),
        }],
        memory_slots: Vec::new(),
        plan_digest: None,
    }
    .finalize()
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
        &BTreeMap::from([(source_id, item.into())]),
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
        source_snapshot_digest: None,
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: vec![ContextLabel::UserAnchor(42)],
        }],
        memory_slots: Vec::new(),
        plan_digest: None,
    }
    .finalize()
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
        &BTreeMap::from([(source_id, source_item.clone().into())]),
        &BTreeMap::new(),
        "",
    )
    .expect("the bounded anchor must not claim or rewrite the Base source item");

    assert_eq!(prepared.items.len(), 2);
    let ResponseItem::Message { content, .. } = &prepared.items[0].item else {
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
    assert_eq!(
        prepared.items[1],
        codex_history::ResponseItemEnvelope::new(source_item)
    );
    assert!(
        spine_model_item_wire_bytes(&prepared.items[1]).unwrap() > MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
}

#[test]
fn canonical_context_preserves_oversized_native_tool_output() {
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
        source_snapshot_digest: None,
        cells: vec![ContextPlanCell::Source {
            source_id: source_id.clone(),
            labels: Vec::new(),
        }],
        memory_slots: Vec::new(),
        plan_digest: None,
    }
    .finalize()
    .expect("recipe");
    let item = ResponseItem::FunctionCallOutput {
        name: None,
        namespace: None,
        id: None,
        call_id: Some("large-output".to_string()),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".repeat(50_000)),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    let prepared = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, item.clone().into())]),
        &BTreeMap::new(),
        "",
    )
    .expect("native tool output must remain Base-owned source");

    assert_eq!(
        prepared.items,
        vec![codex_history::ResponseItemEnvelope::new(item)]
    );
    assert!(
        spine_model_item_wire_bytes(&prepared.items[0]).unwrap() > MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
}
