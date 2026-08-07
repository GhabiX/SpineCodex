use super::*;
use crate::CellId;
use crate::ContextEvent;
use crate::ContextInsert;
use crate::ContextItem;
use crate::ContextLabel;
use crate::Feature;
use crate::MemorySlot;
use crate::Message;
use crate::MessageRole;
use crate::NodeId;
use crate::NodeStatus;
use crate::ParseCell;
use crate::RawSpan;
use crate::SpineConfig;
use crate::SpineContextEventHandler;
use crate::SpineRecoveryInput;
use crate::SpineSignal;
use crate::ToolOutcome;
use crate::ToolRequestChar;
use crate::ToolResponseChar;
use pretty_assertions::assert_eq;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TestError;

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("test handler rejected context")
    }
}

impl std::error::Error for TestError {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TestHistory {
    cells: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
struct TestHandler {
    reject: bool,
    commits: usize,
}

impl SpineContextEventHandler for TestHandler {
    type History = TestHistory;
    type PreparedContext = TestHistory;
    type Error = TestError;

    fn context_size(&self, history: &Self::History) -> usize {
        history.cells.len()
    }

    fn prepare_context(
        &self,
        history: &Self::History,
        stack: &ParseStack,
        events: &[ContextEvent],
    ) -> Result<Self::PreparedContext, Self::Error> {
        if self.reject {
            return Err(TestError);
        }
        let mut prepared = history.clone();
        for event in events {
            match event {
                ContextEvent::Tag { .. } => {}
                ContextEvent::Splice {
                    start,
                    delete,
                    insert,
                } => {
                    let values = insert
                        .iter()
                        .map(|insert| match insert {
                            ContextInsert::Existing { source_index, .. } => {
                                prepared.cells[*source_index]
                            }
                            ContextInsert::Synthetic { cell_id, .. } => cell_id.value(),
                        })
                        .collect::<Vec<_>>();
                    prepared.cells.splice(*start..start + delete, values);
                }
            }
        }
        assert_eq!(prepared.cells.len(), stack.len());
        Ok(prepared)
    }

    fn commit_context(&mut self, history: &mut Self::History, prepared: Self::PreparedContext) {
        *history = prepared;
        self.commits = self.commits.saturating_add(1);
    }
}

#[derive(Clone, Debug, Default)]
struct RecordingObserver {
    observed: Rc<RefCell<Vec<(SpineObserverEffectKind, usize, usize)>>>,
}

impl SpineObserverEffectHandler<TestHandler> for RecordingObserver {
    fn handle(&mut self, effect: SpineObserverEffect<'_>, handler: &TestHandler) {
        self.observed.borrow_mut().push((
            effect.kind(),
            handler.commits,
            effect.projection().usage_samples().len(),
        ));
    }
}

fn config(features: &[Feature]) -> SpineConfig {
    SpineConfig::v1()
        .with_features(features.iter().copied())
        .expect("valid test configuration")
}

fn message(boundary: u64, role: MessageRole, content: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role,
        content: content.to_string(),
    })
}

fn request(boundary: u64, call_id: &str, name: &str, arguments: &str) -> SpineChar {
    SpineChar::ToolRequest(ToolRequestChar {
        boundary: RawBoundary(boundary),
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: arguments.to_string(),
    })
}

fn response(boundary: u64, call_id: &str, output: &str) -> SpineChar {
    response_with_outcome(boundary, call_id, ToolOutcome::Succeeded, output)
}

fn response_with_outcome(
    boundary: u64,
    call_id: &str,
    outcome: ToolOutcome,
    output: &str,
) -> SpineChar {
    SpineChar::ToolResponse(ToolResponseChar {
        boundary: RawBoundary(boundary),
        call_id: call_id.to_string(),
        outcome,
        output: output.to_string(),
    })
}

#[test]
fn live_append_tags_user_and_preserves_one_cell_per_input() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();

    let output = runtime
        .append([message(1, MessageRole::User, "hello")], &mut history)
        .unwrap();

    assert_eq!(runtime.projection().stack().len(), 1);
    assert_eq!(history.cells.len(), 1);
    assert_eq!(
        output.events(),
        &[ContextEvent::Tag {
            index: 0,
            label: ContextLabel::UserAnchor(1),
        }]
    );
}

#[test]
fn pending_tool_group_stays_in_the_live_stack_until_completion() {
    let mut history = TestHistory { cells: vec![0, 1] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();

    runtime
        .append(
            [
                message(1, MessageRole::Assistant, "working"),
                request(2, "call", "shell", "{}"),
            ],
            &mut history,
        )
        .unwrap();
    assert_eq!(runtime.projection().stack().len(), 2);
    assert!(runtime.projection().spine().visible_context.is_empty());

    history.cells.push(2);
    runtime
        .append([response(3, "call", "done")], &mut history)
        .unwrap();
    assert_eq!(runtime.projection().stack().len(), 3);
    assert_eq!(history.cells.len(), 3);
}

#[test]
fn live_spawn_labels_are_scoped_to_response_boundaries_when_call_ids_repeat() {
    let mut history = TestHistory { cells: vec![0, 1] };
    let mut runtime = SpineContextRuntime::new(
        config(&[Feature::Jit, Feature::Spawn]),
        TestHandler::default(),
    )
    .unwrap();

    runtime
        .append(
            [
                request(1, "reused-spawn", "spine.spawn", "{}"),
                response(2, "reused-spawn", "ok"),
            ],
            &mut history,
        )
        .unwrap();
    history.cells.extend([2, 3]);
    runtime
        .append(
            [
                request(3, "reused-spawn", "spine.spawn", "{}"),
                response_with_outcome(4, "reused-spawn", ToolOutcome::Failed, "failed"),
            ],
            &mut history,
        )
        .unwrap();

    let labels = runtime
        .projection()
        .stack()
        .cells()
        .iter()
        .filter_map(|cell| match cell.character() {
            SpineChar::ToolResponse(response) => Some((response.boundary, cell.labels())),
            SpineChar::Message(_)
            | SpineChar::TurnAborted(_)
            | SpineChar::ToolRequest(_)
            | SpineChar::Opaque { .. }
            | SpineChar::Synthetic { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            (
                RawBoundary(2),
                &[ContextLabel::SpawnOutput { succeeded: true }][..],
            ),
            (
                RawBoundary(4),
                &[ContextLabel::SpawnOutput { succeeded: false }][..],
            ),
        ]
    );
}

#[test]
fn event_driven_runtime_does_not_infer_structural_facts_from_tool_text() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();

    runtime
        .append([message(1, MessageRole::User, "hello")], &mut history)
        .unwrap();
    history.cells.extend([1, 2]);
    runtime
        .append(
            [
                request(2, "open", "spine.open", r#"{"summary":"task"}"#),
                response(3, "open", "accepted"),
            ],
            &mut history,
        )
        .unwrap();

    assert!(
        runtime
            .projection()
            .stack()
            .cells()
            .iter()
            .all(|cell| !matches!(cell.character(), SpineChar::Synthetic { .. }))
    );
    assert!(
        runtime
            .projection()
            .spine()
            .visible_context
            .iter()
            .all(|item| !matches!(item, ContextItem::SyntheticNode { .. }))
    );
}

#[test]
fn trim_labels_only_the_completed_tool_response_cell() {
    let mut history = TestHistory { cells: vec![0, 1] };
    let mut runtime = SpineContextRuntime::new(
        config(&[Feature::Jit, Feature::Trim]),
        TestHandler::default(),
    )
    .unwrap();

    let output = runtime
        .append(
            [
                request(1, "call", "shell", "{}"),
                response(2, "call", &"large".repeat(3_000)),
            ],
            &mut history,
        )
        .unwrap();

    assert!(matches!(
        output.events(),
        [ContextEvent::Tag {
            index: 1,
            label: ContextLabel::ToolOutput(crate::TrimEdit::Tagged { .. }),
        }]
    ));
    assert!(runtime.projection().stack().cells()[0].labels().is_empty());
    assert!(matches!(
        runtime.projection().stack().cells()[1].labels(),
        [ContextLabel::ToolOutput(crate::TrimEdit::Tagged { .. })]
    ));
}

#[test]
fn handler_rejection_does_not_commit_runtime_state() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    runtime
        .append([message(1, MessageRole::User, "before")], &mut history)
        .unwrap();
    let before_projection = runtime.projection().clone();
    let before_history = history.clone();
    runtime.handler_mut().reject = true;
    history.cells.push(99);

    let result = runtime.append([message(2, MessageRole::User, "after")], &mut history);

    assert!(matches!(
        result,
        Err(SpineContextRuntimeError::Handler(TestError))
    ));
    assert_eq!(runtime.projection(), &before_projection);
    assert_eq!(history, TestHistory { cells: vec![0, 99] });
    assert_ne!(history, before_history);
}

#[test]
fn projection_limit_failure_discards_candidate_and_runtime_remains_usable() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    runtime
        .append([message(1, MessageRole::User, "before")], &mut history)
        .unwrap();
    let before_projection = runtime.projection().clone();
    let before_history = history.clone();
    let oversized = (0..33)
        .map(|index| {
            let boundary = RawBoundary(index + 2);
            SpineChar::Synthetic {
                boundary,
                item: ContextItem::MemorySlot(MemorySlot::Summary {
                    owner_node: NodeId::root_epoch(1),
                    source: RawSpan {
                        start: boundary,
                        end: boundary,
                    },
                    body: "x".repeat(crate::MAX_MEMORY_BYTES),
                }),
            }
        })
        .collect::<Vec<_>>();
    history.cells.extend(1..=oversized.len() as u64);

    let result = runtime.append(oversized, &mut history);

    assert!(matches!(
        result,
        Err(SpineContextRuntimeError::Spine(SpineError::ContextLimit {
            kind: "synthetic context bytes",
            ..
        }))
    ));
    assert_eq!(runtime.projection(), &before_projection);

    history = before_history;
    history.cells.push(1);
    runtime
        .append([message(2, MessageRole::User, "after")], &mut history)
        .unwrap();
    assert_eq!(runtime.projection().stack().len(), 2);
}

#[test]
fn observer_runs_after_successful_commits_and_positive_usage_updates() {
    let observer = RecordingObserver::default();
    let observed = Rc::clone(&observer.observed);
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime = SpineContextRuntime::new_with_observer(
        config(&[Feature::Jit]),
        TestHandler::default(),
        observer,
    )
    .unwrap();

    runtime
        .append([message(1, MessageRole::User, "before")], &mut history)
        .unwrap();
    runtime.handler_mut().reject = true;
    history.cells.push(1);
    assert!(matches!(
        runtime.append([message(2, MessageRole::User, "rejected")], &mut history),
        Err(SpineContextRuntimeError::Handler(TestError))
    ));
    runtime.observe_usage(TokenUsageSample {
        boundary: RawBoundary(2),
        input_tokens: 0,
    });
    runtime.observe_usage(TokenUsageSample {
        boundary: RawBoundary(2),
        input_tokens: 42,
    });

    assert_eq!(
        observed.borrow().as_slice(),
        &[
            (SpineObserverEffectKind::ContextCommitted, 1, 0),
            (SpineObserverEffectKind::UsageUpdated, 1, 1),
        ]
    );
}

#[test]
fn label_reset_after_structural_splice_uses_original_source_index() {
    let first = ParseCell::new(CellId::new(0), message(1, MessageRole::User, "first"))
        .with_labels(vec![ContextLabel::UserAnchor(1)]);
    let removed = ParseCell::new(CellId::new(1), message(2, MessageRole::User, "removed"));
    let moved = ParseCell::new(CellId::new(2), message(3, MessageRole::User, "moved"))
        .with_labels(vec![ContextLabel::UserAnchor(2)]);
    let before = ParseStack::from_cells(vec![first.clone(), removed, moved.clone()]);
    let after = ParseStack::from_cells(vec![first, moved.with_labels(Vec::new())]);

    let events = context_events_between::<TestError>(&before, &after).unwrap();

    assert_eq!(
        events,
        vec![
            ContextEvent::Splice {
                start: 1,
                delete: 1,
                insert: Vec::new(),
            },
            ContextEvent::Splice {
                start: 1,
                delete: 1,
                insert: vec![ContextInsert::Existing {
                    cell_id: CellId::new(2),
                    source_index: 2,
                }],
            },
        ]
    );
}

#[test]
fn structurally_reinserted_cell_is_retagged_from_raw_source() {
    let first = ParseCell::new(CellId::new(0), message(1, MessageRole::User, "first"));
    let removed = ParseCell::new(CellId::new(1), message(2, MessageRole::User, "removed"));
    let reinserted = ParseCell::new(CellId::new(2), message(3, MessageRole::User, "reinserted"))
        .with_labels(vec![ContextLabel::UserAnchor(2)]);
    let synthetic = ParseCell::new(
        CellId::new(4),
        SpineChar::Synthetic {
            boundary: RawBoundary(5),
            item: ContextItem::SyntheticNode {
                node_id: crate::NodeId::root_epoch(1),
                summary: "synthetic".to_string(),
                status: crate::NodeStatus::Live,
            },
        },
    );
    let trailing = ParseCell::new(CellId::new(3), message(4, MessageRole::User, "trailing"));
    let before = ParseStack::from_cells(vec![
        first.clone(),
        removed,
        reinserted.clone(),
        trailing.clone(),
    ]);
    let after = ParseStack::from_cells(vec![first, reinserted, synthetic, trailing]);

    let events = context_events_between::<TestError>(&before, &after).unwrap();

    assert_eq!(
        events,
        vec![
            ContextEvent::Splice {
                start: 1,
                delete: 2,
                insert: vec![
                    ContextInsert::Existing {
                        cell_id: CellId::new(2),
                        source_index: 2,
                    },
                    ContextInsert::Synthetic {
                        cell_id: CellId::new(4),
                        item: ContextItem::SyntheticNode {
                            node_id: crate::NodeId::root_epoch(1),
                            summary: "synthetic".to_string(),
                            status: crate::NodeStatus::Live,
                        },
                    }
                ],
            },
            ContextEvent::Tag {
                index: 1,
                label: ContextLabel::UserAnchor(2),
            },
        ]
    );
}

#[test]
fn compact_live_archives_the_old_root_and_compiles_the_installed_context() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    runtime
        .append([message(1, MessageRole::User, "before")], &mut history)
        .unwrap();

    history.cells = vec![10];
    runtime
        .compact_live(
            RawBoundary(2),
            [SpineChar::Opaque {
                boundary: RawBoundary(2),
            }],
            &mut history,
        )
        .unwrap();
    history.cells.push(11);
    let output = runtime
        .append([message(3, MessageRole::User, "after")], &mut history)
        .unwrap();

    assert_eq!(
        output
            .projection()
            .spine()
            .nodes
            .iter()
            .map(|node| (&node.id, node.status))
            .collect::<Vec<_>>(),
        vec![
            (&NodeId::root_epoch(1), NodeStatus::Compacted),
            (&NodeId::root_epoch(2), NodeStatus::Live),
        ]
    );
    assert!(output.events().contains(&ContextEvent::Tag {
        index: 1,
        label: ContextLabel::UserAnchor(2),
    }));
}

#[test]
fn archived_recovery_matches_live_compact_projection() {
    let mut live_history = TestHistory { cells: vec![0] };
    let mut live =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    live.append([message(1, MessageRole::User, "before")], &mut live_history)
        .unwrap();
    live_history.cells = vec![10];
    live.compact_live(
        RawBoundary(2),
        [SpineChar::Opaque {
            boundary: RawBoundary(2),
        }],
        &mut live_history,
    )
    .unwrap();
    live_history.cells.push(11);
    live.append([message(3, MessageRole::User, "after")], &mut live_history)
        .unwrap();

    let mut recovered_history = TestHistory {
        cells: vec![20, 21],
    };
    let mut recovered =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    recovered
        .recover(
            [
                SpineRecoveryInput::Char(message(1, MessageRole::User, "before")),
                SpineRecoveryInput::Signal(SpineSignal::Compact {
                    boundary: RawBoundary(2),
                }),
            ],
            [
                SpineChar::Opaque {
                    boundary: RawBoundary(2),
                },
                message(3, MessageRole::User, "after"),
            ],
            &mut recovered_history,
        )
        .unwrap();

    assert_eq!(recovered.projection().spine(), live.projection().spine());
    assert_eq!(recovered_history.cells.len(), 2);
}

#[test]
fn sampling_archive_fixture_preserves_current_compact_recovery_contract() {
    let archived = [
        SpineRecoveryInput::Char(message(1, MessageRole::User, "archived request")),
        SpineRecoveryInput::Signal(SpineSignal::Compact {
            boundary: RawBoundary(2),
        }),
        SpineRecoveryInput::Signal(SpineSignal::Usage(TokenUsageSample {
            boundary: RawBoundary(3),
            input_tokens: 37,
        })),
    ];
    let installed = [
        SpineChar::Opaque {
            boundary: RawBoundary(2),
        },
        message(4, MessageRole::User, "live request"),
    ];
    let mut first_history = TestHistory {
        cells: vec![20, 21],
    };
    let mut second_history = first_history.clone();
    let mut first =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    let mut second =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();

    first
        .recover(archived.clone(), installed.clone(), &mut first_history)
        .unwrap();
    second
        .recover(archived, installed, &mut second_history)
        .unwrap();

    assert_eq!(first.projection(), second.projection());
    assert_eq!(first_history, second_history);
    assert_eq!(
        first.projection().usage_samples(),
        &[TokenUsageSample {
            boundary: RawBoundary(3),
            input_tokens: 37,
        }]
    );
}

#[test]
fn recovery_rejects_archived_live_tail_without_committing() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    let before = runtime.projection().clone();

    let result = runtime.recover(
        [SpineRecoveryInput::Char(message(
            1,
            MessageRole::User,
            "live tail",
        ))],
        [message(1, MessageRole::User, "installed")],
        &mut history,
    );

    assert!(matches!(
        result,
        Err(SpineContextRuntimeError::ArchivedTraceHasLiveTail)
    ));
    assert_eq!(runtime.projection(), &before);
}

#[test]
fn recovery_restores_usage_after_the_archived_compact() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();

    runtime
        .recover(
            [
                SpineRecoveryInput::Signal(SpineSignal::Compact {
                    boundary: RawBoundary(1),
                }),
                SpineRecoveryInput::Signal(SpineSignal::Usage(TokenUsageSample {
                    boundary: RawBoundary(2),
                    input_tokens: 42,
                })),
            ],
            [SpineChar::Opaque {
                boundary: RawBoundary(1),
            }],
            &mut history,
        )
        .unwrap();

    assert_eq!(
        runtime.projection().usage_samples(),
        &[TokenUsageSample {
            boundary: RawBoundary(2),
            input_tokens: 42,
        }]
    );
}

#[test]
fn recovery_handler_failure_preserves_runtime_state() {
    let mut history = TestHistory { cells: vec![0] };
    let mut runtime =
        SpineContextRuntime::new(config(&[Feature::Jit]), TestHandler::default()).unwrap();
    runtime
        .append([message(1, MessageRole::User, "before")], &mut history)
        .unwrap();
    let before = runtime.projection().clone();
    runtime.handler_mut().reject = true;

    let result = runtime.recover(
        std::iter::empty(),
        [message(1, MessageRole::User, "installed")],
        &mut history,
    );

    assert!(matches!(
        result,
        Err(SpineContextRuntimeError::Handler(TestError))
    ));
    assert_eq!(runtime.projection(), &before);
}
