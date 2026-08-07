use crate::ContextEpoch;
use crate::ContextItem;
use crate::ContextPlanSource;
use crate::Message;
use crate::MessageRole;
use crate::RawBoundary;
use crate::SourceCellId;
use crate::SpineChar;
use crate::ThreadNamespace;
use crate::ToolOutcome;
use crate::ToolRequestChar;
use crate::ToolResponseChar;
use crate::source_ledger::SourceCellPayload;
use crate::source_ledger::SourceLedger;
use crate::source_ledger::SourceLedgerError;
use pretty_assertions::assert_eq;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-1").expect("valid namespace")
}

fn ledger() -> SourceLedger {
    SourceLedger::new(namespace(), ContextEpoch::new(4)).expect("source ledger")
}

fn message(boundary: u64, body: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::User,
        content: body.to_string(),
    })
}

#[test]
fn source_ledger_append_is_atomic_and_assigns_stable_identities() {
    let mut ledger = ledger();
    let inserted = ledger
        .append([message(1, "first"), message(2, "second")])
        .expect("append");
    let before = ledger.snapshot();

    assert_eq!(
        inserted,
        vec![
            SourceCellId::new(namespace(), ContextEpoch::new(4), 0),
            SourceCellId::new(namespace(), ContextEpoch::new(4), 1),
        ]
    );
    assert_eq!(
        ledger.append([message(4, "candidate"), message(3, "stale")]),
        Err(SourceLedgerError::NonMonotonicBoundary {
            previous: RawBoundary(4),
            next: RawBoundary(3),
        })
    );
    assert_eq!(ledger.snapshot(), before);
}

#[test]
fn source_ledger_rejects_duplicate_boundary_without_mutation() {
    let mut ledger = ledger();
    ledger
        .append([message(1, "first")])
        .expect("append initial source");
    let before = ledger.snapshot();

    assert_eq!(
        ledger.append([message(1, "duplicate")]),
        Err(SourceLedgerError::NonMonotonicBoundary {
            previous: RawBoundary(1),
            next: RawBoundary(1),
        })
    );
    assert_eq!(ledger.snapshot(), before);
}

#[test]
fn source_snapshot_is_immutable_and_digest_tracks_semantic_source() {
    let mut ledger = ledger();
    let source_id = ledger
        .append([message(1, "first")])
        .expect("append")
        .remove(0);
    let snapshot = ledger.snapshot();
    let digest = snapshot.digest().clone();

    ledger.append([message(2, "second")]).expect("append");

    assert_eq!(snapshot.cells().len(), 1);
    assert_eq!(snapshot.digest(), &digest);
    assert_ne!(ledger.digest(), &digest);
    assert_eq!(
        snapshot.resolve(&source_id),
        Some(&ContextItem::Message {
            message: Message {
                boundary: RawBoundary(1),
                role: MessageRole::User,
                content: "first".to_string(),
            },
            user_anchor: None,
        })
    );
}

#[test]
fn source_digest_ignores_host_tool_output_presentation() {
    let request = SpineChar::ToolRequest(ToolRequestChar {
        boundary: RawBoundary(1),
        call_id: "call-1".to_string(),
        name: "exec".to_string(),
        arguments: r#"{"cmd":"produce-output"}"#.to_string(),
    });
    let response = |output: &str, outcome| {
        SpineChar::ToolResponse(ToolResponseChar {
            boundary: RawBoundary(2),
            call_id: "call-1".to_string(),
            outcome,
            output: output.to_string(),
        })
    };

    let mut live = ledger();
    live.append([
        request.clone(),
        response("host-truncated", ToolOutcome::Unknown),
    ])
    .expect("append live source");

    let mut replayed = ledger();
    replayed
        .append([
            request.clone(),
            response("raw persisted output", ToolOutcome::Unknown),
        ])
        .expect("append replayed source");
    assert_eq!(live.digest(), replayed.digest());

    let mut different_outcome = ledger();
    different_outcome
        .append([
            request,
            response("raw persisted output", ToolOutcome::Succeeded),
        ])
        .expect("append source with different outcome");
    assert_ne!(live.digest(), different_outcome.digest());
}

#[test]
fn compact_epoch_archives_old_source_and_rejects_stale_epoch_progression() {
    let mut ledger = ledger();
    let old_id = ledger
        .append([message(1, "old")])
        .expect("append")
        .remove(0);
    let archived = ledger.advance_epoch(ContextEpoch::new(5)).expect("advance");

    assert_eq!(archived.resolve(&old_id).is_some(), true);
    assert_eq!(ledger.snapshot().resolve(&old_id), None);
    assert_eq!(ledger.epoch(), ContextEpoch::new(5));
    assert_eq!(
        ledger.advance_epoch(ContextEpoch::new(7)),
        Err(SourceLedgerError::EpochNotNext {
            current: ContextEpoch::new(5),
            next: ContextEpoch::new(7),
        })
    );
}

#[test]
fn direct_and_code_mode_tool_source_remains_opaque_to_transition_state() {
    let mut ledger = ledger();
    let ids = ledger
        .append([
            SpineChar::ToolRequest(ToolRequestChar {
                boundary: RawBoundary(1),
                call_id: "outer".to_string(),
                name: "spine.open".to_string(),
                arguments: r#"{"summary":"direct"}"#.to_string(),
            }),
            SpineChar::ToolResponse(ToolResponseChar {
                boundary: RawBoundary(2),
                call_id: "outer".to_string(),
                outcome: ToolOutcome::Succeeded,
                output: "carrier".to_string(),
            }),
        ])
        .expect("append");
    let snapshot = ledger.snapshot();

    assert!(matches!(
        snapshot.payload(&ids[0]),
        Some(SourceCellPayload::ToolRequest { name, .. }) if name == "spine.open"
    ));
    assert_eq!(
        snapshot.payload(&ids[1]),
        Some(&SourceCellPayload::ToolResponse {
            call_id: "outer".to_string(),
            outcome: ToolOutcome::Succeeded,
            output: "carrier".to_string(),
        })
    );
}
