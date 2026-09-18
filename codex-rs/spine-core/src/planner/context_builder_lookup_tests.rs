use super::build_context_plan;
use crate::ContextEpoch;
use crate::ContextItem;
use crate::ContextLabel;
use crate::ContextPlanCell;
use crate::MemorySlot;
use crate::Message;
use crate::MessageRole;
use crate::NativeItemRef;
use crate::NodeId;
use crate::PlannerError;
use crate::RawBoundary;
use crate::RawSpan;
use crate::SourceLedger;
use crate::SpineChar;
use crate::SpineProjection;
use crate::ThreadNamespace;
use pretty_assertions::assert_eq;

fn message(boundary: u64) -> Message {
    Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::User,
        content: "same text at distinct boundaries".to_string(),
    }
}

fn projection(visible_context: Vec<ContextItem>) -> SpineProjection {
    SpineProjection {
        nodes: Vec::new(),
        cursor: NodeId::root_epoch(1),
        visible_context,
        last_boundary: None,
    }
}

#[test]
fn lookup_preserves_synthetic_priority_duplicates_and_user_labels() {
    let mut source = SourceLedger::new(
        ThreadNamespace::parse("lookup").expect("thread"),
        ContextEpoch::ZERO,
    )
    .expect("ledger");
    let item = ContextItem::Message {
        message: message(2),
        user_anchor: Some(9),
    };
    let ids = source
        .append([
            SpineChar::Message(message(2)),
            SpineChar::Synthetic {
                boundary: RawBoundary(4),
                item: item.clone(),
            },
            SpineChar::Synthetic {
                boundary: RawBoundary(6),
                item: item.clone(),
            },
            SpineChar::Message(message(8)),
        ])
        .expect("append");
    let visible = projection(vec![
        item.clone(),
        item.clone(),
        item,
        ContextItem::MemorySlot(MemorySlot::User {
            owner_node: NodeId::root_epoch(1),
            message: message(8),
            anchor: 10,
        }),
    ]);
    let plan = build_context_plan(&source.snapshot(), &visible, &[], None, &mut 0).expect("plan");
    assert_eq!(
        plan.cells,
        vec![
            ContextPlanCell::Source {
                source_id: ids[1].clone(),
                labels: vec![]
            },
            ContextPlanCell::Source {
                source_id: ids[2].clone(),
                labels: vec![]
            },
            ContextPlanCell::Source {
                source_id: ids[0].clone(),
                labels: vec![ContextLabel::UserAnchor(9)]
            },
            ContextPlanCell::Source {
                source_id: ids[3].clone(),
                labels: vec![ContextLabel::UserAnchor(10)]
            },
        ]
    );
}

#[test]
fn lookup_preserves_inclusive_spans_gaps_consumption_and_pending_order_after_fork() {
    let mut source = SourceLedger::new(
        ThreadNamespace::parse("parent").expect("thread"),
        ContextEpoch::ZERO,
    )
    .expect("ledger");
    source
        .append([2, 4, 6, 8, 10].map(|boundary| SpineChar::Opaque {
            boundary: RawBoundary(boundary),
        }))
        .expect("append");
    source
        .continue_in_namespace(ThreadNamespace::parse("child").expect("thread"), 2)
        .expect("fork");
    let snapshot = source.snapshot();
    let visible = projection(vec![
        ContextItem::Native {
            source: NativeItemRef::Rollout {
                ordinal: RawBoundary(4),
            },
        },
        ContextItem::SourceSpan {
            span: RawSpan {
                start: RawBoundary(1),
                end: RawBoundary(6),
            },
        },
    ]);
    let plan = build_context_plan(
        &snapshot,
        &visible,
        &[RawBoundary(10), RawBoundary(8)],
        None,
        &mut 0,
    )
    .expect("plan");
    assert_eq!(
        plan.cells,
        [1, 0, 2, 4, 3].map(|index| ContextPlanCell::Source {
            source_id: snapshot.cells()[index].id.clone(),
            labels: vec![],
        })
    );
}

#[test]
fn lookup_preserves_missing_mismatched_and_consumed_source_errors() {
    let mut source = SourceLedger::new(
        ThreadNamespace::parse("errors").expect("thread"),
        ContextEpoch::ZERO,
    )
    .expect("ledger");
    source
        .append([SpineChar::Message(message(2))])
        .expect("append");
    let snapshot = source.snapshot();
    let item = ContextItem::Message {
        message: message(2),
        user_anchor: None,
    };
    let mut mismatch = message(2);
    mismatch.content = "different".to_string();
    for (items, pending, boundary) in [
        (
            vec![ContextItem::Message {
                message: mismatch,
                user_anchor: None,
            }],
            vec![],
            2,
        ),
        (vec![item.clone(), item.clone()], vec![], 2),
        (vec![item], vec![RawBoundary(2)], 2),
        (vec![], vec![RawBoundary(3)], 3),
        (
            vec![ContextItem::SourceSpan {
                span: RawSpan {
                    start: RawBoundary(4),
                    end: RawBoundary(1),
                },
            }],
            vec![],
            4,
        ),
    ] {
        let error = build_context_plan(&snapshot, &projection(items), &pending, None, &mut 0)
            .expect_err("lookup should reject the invalid source request");
        assert!(matches!(
            error,
            PlannerError::MissingSourceBoundary(actual) if actual == RawBoundary(boundary)
        ));
    }
}
