use crate::ContextEpoch;
use crate::ContextItem;
use crate::ContextLabel;
use crate::MemorySlot;
use crate::Message;
use crate::MessageRole;
use crate::NodeId;
use crate::NodeStatus;
use crate::ProjectionCellId;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RecordDigest;
use crate::SourceCellId;
use crate::SpawnOutcome;
use crate::SpawnTask;
use crate::ThreadNamespace;
use crate::context_plan::CONTEXT_PLAN_SCHEMA_V1;
use crate::context_plan::ContextCellProvenance;
use crate::context_plan::ContextPlanCell;
use crate::context_plan::ContextPlanError;
use crate::context_plan::ContextPlanRecipe;
use crate::context_plan::ContextPlanSource;
use crate::context_plan::ResolvedContextCell;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

#[derive(Clone)]
struct TestSource {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    digest: RecordDigest,
    cells: BTreeMap<SourceCellId, ContextItem>,
}

impl ContextPlanSource for TestSource {
    fn thread(&self) -> &ThreadNamespace {
        &self.thread
    }

    fn epoch(&self) -> ContextEpoch {
        self.epoch
    }

    fn digest(&self) -> &RecordDigest {
        &self.digest
    }

    fn resolve(&self, source_id: &SourceCellId) -> Option<&ContextItem> {
        self.cells.get(source_id)
    }
}

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-1").expect("valid namespace")
}

fn digest(value: char) -> RecordDigest {
    RecordDigest::parse(value.to_string().repeat(64)).expect("valid digest")
}

fn source_id(ordinal: u64) -> SourceCellId {
    SourceCellId::new(namespace(), ContextEpoch::new(3), ordinal)
}

fn projection_id(ordinal: u64) -> ProjectionCellId {
    ProjectionCellId::new(namespace(), ContextEpoch::new(3), ordinal)
}

fn user_item(boundary: u64, body: &str) -> ContextItem {
    ContextItem::Message {
        message: Message {
            boundary: RawBoundary(boundary),
            role: MessageRole::User,
            content: body.to_string(),
        },
        user_anchor: Some(boundary),
    }
}

fn memory_slots() -> Vec<MemorySlot> {
    vec![
        MemorySlot::User {
            owner_node: NodeId::root_epoch(3),
            message: Message {
                boundary: RawBoundary(1),
                role: MessageRole::User,
                content: "request".to_string(),
            },
            anchor: 1,
        },
        MemorySlot::Summary {
            owner_node: NodeId::root_epoch(3).child(1),
            source: RawSpan {
                start: RawBoundary(2),
                end: RawBoundary(4),
            },
            body: "closed memory".to_string(),
        },
        MemorySlot::SpawnEvidence {
            owner_node: NodeId::root_epoch(3),
            source: RawSpan {
                start: RawBoundary(5),
                end: RawBoundary(6),
            },
            task: SpawnTask {
                summary: "audit".to_string(),
                prompt: "inspect".to_string(),
            },
            outcome: SpawnOutcome::Completed,
            diagnostic: None,
            execution_ref: Some("exec-1".to_string()),
        },
    ]
}

fn source() -> TestSource {
    TestSource {
        thread: namespace(),
        epoch: ContextEpoch::new(3),
        digest: digest('a'),
        cells: BTreeMap::from([
            (source_id(1), user_item(1, "request")),
            (
                source_id(2),
                ContextItem::Native {
                    source: crate::NativeItemRef::Rollout {
                        ordinal: RawBoundary(2),
                    },
                },
            ),
        ]),
    }
}

fn recipe() -> ContextPlanRecipe {
    ContextPlanRecipe {
        schema: CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread: namespace(),
        epoch: ContextEpoch::new(3),
        source_snapshot_digest: digest('a'),
        cells: vec![
            ContextPlanCell::Source {
                source_id: source_id(1),
                labels: vec![ContextLabel::UserAnchor(1)],
            },
            ContextPlanCell::Projection {
                projection_id: projection_id(1),
                item: ContextItem::SyntheticNode {
                    node_id: NodeId::root_epoch(3).child(1),
                    summary: "task".to_string(),
                    status: NodeStatus::Opened,
                },
            },
            ContextPlanCell::Source {
                source_id: source_id(2),
                labels: vec![ContextLabel::ToolOutput(crate::TrimEdit::Snipped)],
            },
        ],
        memory_slots: memory_slots(),
        plan_digest: digest('0'),
    }
    .finalize_digest()
    .expect("valid recipe")
}

#[test]
fn context_plan_roundtrips_and_resolves_complete_memory_slots() {
    let recipe = recipe();
    let encoded = serde_json::to_vec(&recipe).expect("serialize");
    let decoded: ContextPlanRecipe = serde_json::from_slice(&encoded).expect("deserialize");

    assert_eq!(decoded, recipe);
    assert_eq!(
        decoded.resolve(&source()).expect("resolve"),
        crate::context_plan::ResolvedContextPlan {
            cells: vec![
                ResolvedContextCell {
                    provenance: ContextCellProvenance::Source(source_id(1)),
                    item: user_item(1, "request"),
                    labels: vec![ContextLabel::UserAnchor(1)],
                },
                ResolvedContextCell {
                    provenance: ContextCellProvenance::Projection(projection_id(1)),
                    item: ContextItem::SyntheticNode {
                        node_id: NodeId::root_epoch(3).child(1),
                        summary: "task".to_string(),
                        status: NodeStatus::Opened,
                    },
                    labels: Vec::new(),
                },
                ResolvedContextCell {
                    provenance: ContextCellProvenance::Source(source_id(2)),
                    item: ContextItem::Native {
                        source: crate::NativeItemRef::Rollout {
                            ordinal: RawBoundary(2),
                        },
                    },
                    labels: vec![ContextLabel::ToolOutput(crate::TrimEdit::Snipped)],
                },
            ],
            memory_slots: memory_slots(),
        }
    );
    let json = String::from_utf8(encoded).expect("utf8");
    assert!(!json.contains("source_index"));
}

#[test]
fn context_plan_keeps_source_identity_across_close_and_next_projection_changes() {
    let close = recipe();
    let mut next = close.clone();
    next.cells[1] = ContextPlanCell::Projection {
        projection_id: projection_id(2),
        item: ContextItem::SyntheticNode {
            node_id: NodeId::root_epoch(3).child(2),
            summary: "next task".to_string(),
            status: NodeStatus::Live,
        },
    };
    next.plan_digest = digest('0');
    let next = next.finalize_digest().expect("valid next recipe");

    assert_eq!(close.cells.first(), next.cells.first());
    assert_eq!(close.cells.last(), next.cells.last());
    assert_eq!(
        close.resolve(&source()).expect("resolve close").cells[0].provenance,
        next.resolve(&source()).expect("resolve next").cells[0].provenance
    );
}

#[test]
fn context_plan_rejects_missing_stale_or_wrong_scope_source() {
    let recipe = recipe();
    let mut missing = source();
    missing.cells.remove(&source_id(2));
    assert_eq!(
        recipe.resolve(&missing),
        Err(ContextPlanError::MissingSourceCell(source_id(2)))
    );

    let mut stale = source();
    stale.digest = digest('b');
    assert_eq!(
        recipe.resolve(&stale),
        Err(ContextPlanError::SourceSnapshotDigestMismatch)
    );

    let mut wrong_scope = source();
    wrong_scope.epoch = ContextEpoch::new(4);
    assert_eq!(
        recipe.resolve(&wrong_scope),
        Err(ContextPlanError::SourceSnapshotScopeMismatch)
    );
}

#[test]
fn context_plan_rejects_duplicate_projection_identity() {
    let mut recipe = recipe();
    recipe.cells.push(ContextPlanCell::Projection {
        projection_id: projection_id(1),
        item: ContextItem::SyntheticNode {
            node_id: NodeId::root_epoch(3).child(2),
            summary: "duplicate".to_string(),
            status: NodeStatus::Live,
        },
    });
    recipe.plan_digest = digest('0');

    assert_eq!(
        recipe.finalize_digest(),
        Err(ContextPlanError::DuplicateProjectionCell(projection_id(1)))
    );
}

#[test]
fn context_plan_rejects_duplicate_or_excess_source_labels() {
    let mut duplicate = recipe();
    let ContextPlanCell::Source { source_id, labels } = &mut duplicate.cells[0] else {
        panic!("first recipe cell must be source-backed");
    };
    let source_id = source_id.clone();
    labels.push(ContextLabel::UserAnchor(1));
    duplicate.plan_digest = digest('0');
    assert_eq!(
        duplicate.finalize_digest(),
        Err(ContextPlanError::InvalidSourceLabels {
            source_id: source_id.clone(),
        })
    );

    let mut excess = recipe();
    let ContextPlanCell::Source { labels, .. } = &mut excess.cells[0] else {
        panic!("first recipe cell must be source-backed");
    };
    labels.extend([
        ContextLabel::ToolOutput(crate::TrimEdit::Snipped),
        ContextLabel::SpawnOutput { succeeded: true },
    ]);
    excess.plan_digest = digest('0');
    assert_eq!(
        excess.finalize_digest(),
        Err(ContextPlanError::InvalidSourceLabels { source_id })
    );
}

#[test]
fn context_plan_digest_covers_order_and_semantic_items() {
    let recipe = recipe();
    let mut changed = recipe;
    changed.cells.swap(0, 2);

    assert!(matches!(
        changed.validate(),
        Err(ContextPlanError::DigestMismatch { .. })
    ));
}
