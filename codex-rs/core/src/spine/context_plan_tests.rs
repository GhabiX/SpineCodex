use super::context_plan::prepare_codex_context_plan;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;
use spine_core::ContextEpoch;
use spine_core::ContextPlanCell;
use spine_core::ContextPlanRecipe;
use spine_core::ContextPlanSource;
use spine_core::Message;
use spine_core::MessageRole;
use spine_core::RawBoundary;
use spine_core::RecordDigest;
use spine_core::SourceLedger;
use spine_core::SpineChar;
use spine_core::ThreadNamespace;
use std::collections::BTreeMap;

#[test]
fn canonical_context_rejects_oversized_model_item() {
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
        schema: spine_core::CONTEXT_PLAN_SCHEMA_V1.to_string(),
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
    let error = prepare_codex_context_plan(
        &recipe,
        &snapshot,
        &BTreeMap::from([(source_id, item)]),
        &BTreeMap::new(),
        "",
    )
    .expect_err("oversized item must fail");

    assert!(error.to_string().contains("maximum is 9999"));
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
        schema: spine_core::CONTEXT_PLAN_SCHEMA_V1.to_string(),
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
    assert!(approx_token_count(&encoded) < 10_000);
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
    let recipe = ContextPlanRecipe {
        schema: spine_core::CONTEXT_PLAN_SCHEMA_V1.to_string(),
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
    let escaped_json = serde_json::json!({
        "items": (0..700)
            .map(|index| serde_json::json!({
                "path": format!("src/module_{index}/file.rs"),
                "patch": "\\\"quoted\\\"\\nline\\\\tail",
            }))
            .collect::<Vec<_>>(),
    })
    .to_string();
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
    assert!(approx_token_count(&encoded) < 10_000);
    assert!(encoded.contains("tokens truncated"));
}
