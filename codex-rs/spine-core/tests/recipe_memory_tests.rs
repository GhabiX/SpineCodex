use pretty_assertions::assert_eq;
use spine_core::host::CanonicalReplay;
use spine_core::host::ContextEpoch;
use spine_core::host::ContextItem;
use spine_core::host::ExecutionOrigin;
use spine_core::host::Feature;
use spine_core::host::MemorySlot;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::NodeId;
use spine_core::host::PlannerError;
use spine_core::host::RawBoundary;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpawnArrival;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnTask;
use spine_core::host::SpineChar;
use spine_core::host::SpineCompactBarrierV1;
use spine_core::host::SpineConfig;
use spine_core::host::SpineOperationFact;
use spine_core::host::ThreadNamespace;

struct History {
    runtime: SamplingRuntime,
    inputs: Vec<ReplayInput>,
    boundary: u64,
    thread: ThreadNamespace,
    config: SpineConfig,
}

impl History {
    fn new(name: &str) -> Self {
        let thread = ThreadNamespace::parse(name).expect("thread");
        let config = SpineConfig::v1()
            .with_features([Feature::Jit, Feature::Spawn])
            .expect("config");
        Self {
            runtime: SamplingRuntime::new(thread.clone(), ContextEpoch::ZERO, config.clone())
                .expect("runtime"),
            inputs: Vec::new(),
            boundary: 0,
            thread,
            config,
        }
    }

    fn user(&mut self, content: &str) {
        self.boundary += 1;
        let character = SpineChar::Message(Message {
            boundary: RawBoundary(self.boundary),
            role: MessageRole::User,
            content: content.to_string(),
        });
        self.runtime
            .observe_source([character.clone()])
            .expect("source");
        self.inputs.push(ReplayInput::Source(character));
    }

    fn step(&mut self, operation: SpineOperationFact) {
        self.boundary += 1;
        let output = SpineChar::Opaque {
            boundary: RawBoundary(self.boundary),
        };
        self.sample(Some(operation), output).expect("prepare");
    }

    /// A sampling without a Spine fact whose only output is one message.
    fn say(&mut self, role: MessageRole, content: &str) {
        self.boundary += 1;
        let output = SpineChar::Message(Message {
            boundary: RawBoundary(self.boundary),
            role,
            content: content.to_string(),
        });
        self.sample(None, output).expect("prepare");
    }

    fn arrival(&mut self, producer: &[u32], outcome: SpawnOutcome, memory: Option<&str>) {
        self.boundary += 1;
        let character = SpineChar::Arrival {
            boundary: RawBoundary(self.boundary),
            arrival: SpawnArrival {
                producer: node(producer),
                outcome,
                memory: memory.map(str::to_string),
                diagnostic: (outcome != SpawnOutcome::Completed).then(|| "cancelled".to_string()),
                execution_ref: Some(node(producer).to_string()),
            },
        };
        self.runtime
            .observe_source([character.clone()])
            .expect("source");
        self.inputs.push(ReplayInput::Source(character));
    }

    fn sample(
        &mut self,
        operation: Option<SpineOperationFact>,
        output: SpineChar,
    ) -> Result<(), PlannerError> {
        let sampling = self.runtime.begin_sampling().expect("begin");
        let started = self
            .runtime
            .sampling_started_record(&sampling)
            .expect("started");
        self.inputs.push(ReplayInput::Archive(started));
        if let Some(operation) = operation.clone() {
            self.runtime
                .register_execution("control")
                .expect("register");
            self.runtime
                .stage_execution(
                    "control",
                    ExecutionOrigin::Direct {
                        execution_ref: "control".to_string(),
                    },
                    operation,
                )
                .expect("stage");
        }
        self.runtime
            .observe_source([output.clone()])
            .expect("source");
        self.inputs.push(ReplayInput::Source(output));
        if operation.is_some() {
            self.runtime
                .finish_execution("control", true)
                .expect("execution");
        }
        let SamplingFinish::Prepared(prepared) = self
            .runtime
            .finish_sampling(sampling, SamplingTerminal::Completed)?
        else {
            panic!("completed sampling must prepare");
        };
        let record = SamplingArchiveRecord::SamplingCommit(prepared.durable_record().clone());
        // Persistent authority remains the existing typed archive, not a recipe snapshot.
        let encoded = serde_json::to_vec(&record).expect("archive encode");
        let decoded = serde_json::from_slice(&encoded).expect("archive decode");
        assert_eq!(record, decoded);
        self.inputs.push(ReplayInput::Archive(decoded));
        self.runtime.install_prepared(prepared).expect("install");
        Ok(())
    }

    fn compact(&mut self) {
        let barrier = SpineCompactBarrierV1::new(
            self.thread.clone(),
            self.runtime.epoch(),
            self.runtime.epoch().checked_next().expect("epoch"),
            RawBoundary(self.boundary),
            vec![RawBoundary(self.boundary + 1)],
        )
        .expect("barrier");
        self.boundary += 1;
        self.runtime.compact(barrier.clone()).expect("compact");
        self.inputs.push(ReplayInput::Compact(barrier));
    }

    fn replay(&self) -> SamplingRuntime {
        let replay = CanonicalReplay::new(self.thread.clone())
            .expect("replay")
            .with_runtime_config(self.config.clone())
            .expect("config");
        let full = replay.prepare(self.inputs.clone()).expect("full replay");
        let mut builder = replay.builder().expect("incremental replay");
        for chunk in self.inputs.chunks(17) {
            builder.apply(chunk.to_vec()).expect("replay chunk");
        }
        let chunked = builder.finish().expect("finish replay");
        assert_eq!(full.projection, chunked.projection);
        assert_eq!(full.live_plan, chunked.live_plan);
        assert_eq!(full.live_context, chunked.live_context);
        assert_eq!(full.tree, chunked.tree);
        assert_eq!(full.applied_commits, chunked.applied_commits);
        assert_eq!(&full.projection, self.runtime.projection());
        let restored = full.into_runtime();
        assert_eq!(restored.source_snapshot(), self.runtime.source_snapshot());
        let current = self
            .runtime
            .preview_context_plan()
            .expect("live plan")
            .resolve(&self.runtime.source_snapshot())
            .expect("live resolve");
        let replayed = restored
            .preview_context_plan()
            .expect("replayed plan")
            .resolve(&restored.source_snapshot())
            .expect("replayed resolve");
        // Projection ordinals are presentation identities; compare complete ordered semantic cells.
        assert_eq!(
            current
                .cells
                .into_iter()
                .map(|cell| (cell.item, cell.labels))
                .collect::<Vec<_>>(),
            replayed
                .cells
                .into_iter()
                .map(|cell| (cell.item, cell.labels))
                .collect::<Vec<_>>()
        );
        restored
    }
}

#[test]
fn current_recipe_stays_bounded_across_historical_memory_and_compacts() {
    let mut history = History::new("recipe-history");
    let memory = "m".repeat(4096);
    // This crosses the former failure sequence without declaring a product node-count limit.
    for closed in 1..=520 {
        history.step(SpineOperationFact::Open {
            summary: "scope".to_string(),
        });
        history.step(SpineOperationFact::Close {
            memory: memory.clone(),
        });
        if closed % 8 == 0 {
            history.compact();
            let plan = history
                .runtime
                .preview_context_plan()
                .expect("compact plan");
            assert_eq!(plan.cells.len(), 1);
            assert!(serde_json::to_vec(&plan).expect("encode").len() < 512);
        }
    }
    let summaries = history
        .runtime
        .projection()
        .nodes
        .iter()
        .flat_map(|node| node.memory.iter().flatten())
        .filter_map(|slot| match slot {
            MemorySlot::Summary { body, .. } => Some(body),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries, vec![&memory; 520]);
    let restored = history.replay();
    assert_eq!(
        restored
            .preview_context_plan()
            .expect("replayed compact plan"),
        history
            .runtime
            .preview_context_plan()
            .expect("live compact plan")
    );

    history.step(SpineOperationFact::Open {
        summary: "pending close".to_string(),
    });
    let sampling = history.runtime.begin_sampling().expect("orphan begin");
    history.inputs.push(ReplayInput::Archive(
        history
            .runtime
            .sampling_started_record(&sampling)
            .expect("orphan started"),
    ));
    history.user("durable tail before failed close");
    history.runtime = history.replay();
    history.step(SpineOperationFact::Close {
        memory: "recovered close".to_string(),
    });
    history.replay();
}

#[test]
fn recipe_preserves_nested_user_child_and_spawn_memory_owners() {
    let mut history = History::new("recipe-nested");
    history.user("outer user evidence");
    history.step(SpineOperationFact::Open {
        summary: "parent".to_string(),
    });
    history.step(SpineOperationFact::Open {
        summary: "child".to_string(),
    });
    history.user("inner user evidence");
    history.step(SpineOperationFact::Close {
        memory: "same body".to_string(),
    });
    history.step(SpineOperationFact::Next {
        closed_memory: "same body".to_string(),
        next_summary: "sibling".to_string(),
    });
    history.step(spawn(["left", "right"]));
    history.arrival(&[1, 2, 1], SpawnOutcome::Completed, Some("same body"));
    history.arrival(&[1, 2, 2], SpawnOutcome::Completed, Some("same body"));
    history.step(SpineOperationFact::Close {
        memory: "same body".to_string(),
    });
    let plan = history.runtime.preview_context_plan().expect("plan");
    let resolved = plan
        .resolve(&history.runtime.source_snapshot())
        .expect("resolve");
    let summaries = resolved
        .cells
        .iter()
        .filter_map(|cell| match &cell.item {
            ContextItem::MemorySlot(MemorySlot::Summary {
                owner_node, body, ..
            }) => Some((owner_node.to_string(), body.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summaries,
        vec![
            ("1.1.1".to_string(), "same body"),
            ("1.1".to_string(), "same body"),
            ("1.2".to_string(), "same body"),
        ]
    );
    let users = resolved
        .cells
        .iter()
        .filter_map(|cell| match &cell.item {
            ContextItem::Message { message, .. } if message.role == MessageRole::User => {
                Some(message.content.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(users, vec!["outer user evidence", "inner user evidence"]);
    let births = resolved
        .cells
        .iter()
        .filter_map(|cell| match &cell.item {
            ContextItem::MemorySlot(MemorySlot::Birth { owner_node, .. }) => {
                Some(owner_node.to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(births, vec!["1.2.1", "1.2.2"]);
    assert_eq!(
        memory_shape(&history, &[1, 2]),
        vec![
            "birth 1.2.1",
            "birth 1.2.2",
            "arrival 1.2.1",
            "arrival 1.2.2",
            "summary"
        ]
    );
    history.replay();
}

#[test]
fn recipe_replay_keeps_namespace_and_rejects_broken_typed_chain() {
    let mut history = History::new("recipe-parent");
    history.user("inherited source");
    history.step(SpineOperationFact::Open {
        summary: "scope".to_string(),
    });
    let inherited = history.runtime.source_snapshot();
    history
        .runtime
        .continue_in_namespace(ThreadNamespace::parse("recipe-fork").expect("fork namespace"))
        .expect("continue namespace");
    history.step(SpineOperationFact::Close {
        memory: "fork memory".to_string(),
    });
    let source = history.runtime.source_snapshot();
    assert_eq!(
        &source.cells()[..inherited.cells().len()],
        inherited.cells()
    );
    history.replay();

    let first_started = history
        .inputs
        .iter()
        .position(|input| {
            matches!(
                input,
                ReplayInput::Archive(SamplingArchiveRecord::SamplingStarted(_))
            )
        })
        .expect("first start");
    let broken = history
        .inputs
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != first_started)
        .map(|(_, input)| input.clone())
        .collect::<Vec<_>>();
    assert!(
        CanonicalReplay::new(history.thread)
            .expect("replay")
            .with_runtime_config(history.config)
            .expect("config")
            .prepare(broken)
            .is_err()
    );
}

fn node(parts: &[u32]) -> NodeId {
    parts[1..]
        .iter()
        .fold(NodeId::root_epoch(parts[0]), |node, ordinal| {
            node.child(*ordinal)
        })
}

fn spawn<const N: usize>(summaries: [&str; N]) -> SpineOperationFact {
    SpineOperationFact::Spawn {
        tasks: summaries
            .into_iter()
            .map(|summary| SpawnTask {
                summary: summary.to_string(),
                prompt: format!("inspect {summary}"),
            })
            .collect(),
    }
}

fn open(summary: &str) -> SpineOperationFact {
    SpineOperationFact::Open {
        summary: summary.to_string(),
    }
}

fn close(memory: &str) -> SpineOperationFact {
    SpineOperationFact::Close {
        memory: memory.to_string(),
    }
}

/// The assembled memory of a closed node, one label per slot.
fn memory_shape(history: &History, id: &[u32]) -> Vec<String> {
    let id = node(id);
    history
        .runtime
        .projection()
        .nodes
        .iter()
        .find(|candidate| candidate.id == id)
        .and_then(|candidate| candidate.memory.clone())
        .expect("closed node memory")
        .iter()
        .map(|slot| match slot {
            MemorySlot::User { message, .. } => format!("user {}", message.content),
            MemorySlot::Summary { .. } => "summary".to_string(),
            MemorySlot::Birth { owner_node, .. } => format!("birth {owner_node}"),
            MemorySlot::Arrival { arrival, .. } => format!("arrival {}", arrival.producer),
            MemorySlot::Reply { message, .. } => format!("reply {}", message.content),
        })
        .collect()
}

#[test]
fn arrival_is_received_at_the_cursor_of_its_admission() {
    // P = 1.1 spawns 1.1.1 and 1.1.2, then opens Q = 1.1.3.
    let mut history = History::new("arrival-receiver");
    history.step(open("P"));
    history.step(spawn(["left", "right"]));
    history.step(open("Q"));
    // Admitted before the sampling that closes Q: received by Q, kept in Q's memory.
    history.arrival(&[1, 1, 1], SpawnOutcome::Completed, Some("left memory"));
    history.step(close("Q memory"));
    assert_eq!(
        memory_shape(&history, &[1, 1, 3]),
        vec!["arrival 1.1.1", "summary"]
    );
    // Admitted after Q closed: received by P.
    history.arrival(&[1, 1, 2], SpawnOutcome::Completed, Some("right memory"));
    history.step(close("P memory"));
    assert_eq!(
        memory_shape(&history, &[1, 1]),
        vec![
            "birth 1.1.1",
            "birth 1.1.2",
            "arrival 1.1.1",
            "summary",
            "arrival 1.1.2",
            "summary"
        ]
    );
    history.replay();
}

#[test]
fn arrival_after_open_is_received_by_the_new_node() {
    let mut history = History::new("arrival-open");
    history.step(spawn(["left", "right"]));
    history.step(open("R"));
    history.arrival(&[1, 1], SpawnOutcome::Completed, Some("left memory"));
    history.arrival(&[1, 2], SpawnOutcome::Aborted, None);
    history.step(close("R memory"));
    assert_eq!(
        memory_shape(&history, &[1, 3]),
        vec!["arrival 1.1", "arrival 1.2", "summary"]
    );
    let outcomes = history
        .runtime
        .projection()
        .nodes
        .iter()
        .filter(|candidate| candidate.kind == spine_core::host::NodeKind::Spawn)
        .map(|candidate| candidate.spawn_outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![Some(SpawnOutcome::Completed), Some(SpawnOutcome::Aborted)]
    );
    history.replay();
}

#[test]
fn close_and_compact_wait_for_every_birth_in_scope() {
    let mut history = History::new("arrival-guard");
    history.step(open("P"));
    history.step(spawn(["left", "right"]));
    history.arrival(&[1, 1, 1], SpawnOutcome::Completed, Some("left memory"));
    history.boundary += 1;
    let output = SpineChar::Opaque {
        boundary: RawBoundary(history.boundary),
    };
    let error = history
        .sample(Some(close("too early")), output)
        .expect_err("close must wait for 1.1.2");
    assert!(error.to_string().contains("1.1.2"), "{error}");

    let mut history = History::new("arrival-compact");
    history.step(spawn(["left", "right"]));
    history.arrival(&[1, 1], SpawnOutcome::Completed, Some("left memory"));
    let barrier = SpineCompactBarrierV1::new(
        history.thread.clone(),
        history.runtime.epoch(),
        history.runtime.epoch().checked_next().expect("epoch"),
        RawBoundary(history.boundary),
        vec![RawBoundary(history.boundary + 1)],
    )
    .expect("barrier");
    let error = history
        .runtime
        .compact(barrier)
        .expect_err("compact must wait for 1.2");
    assert!(error.to_string().contains("1.2"), "{error}");
    history.arrival(&[1, 2], SpawnOutcome::Errored, None);
    history.compact();
    history.replay();
}

#[test]
fn arrival_must_name_one_unarrived_birth() {
    let mut history = History::new("arrival-identity");
    history.step(spawn(["left", "right"]));
    history.arrival(&[1, 1], SpawnOutcome::Completed, Some("left memory"));
    history.arrival(&[1, 1], SpawnOutcome::Completed, Some("left memory"));
    let error = history
        .runtime
        .preview_context_plan()
        .expect_err("duplicate arrival");
    assert!(error.to_string().contains("already arrived"), "{error}");

    let mut history = History::new("arrival-unknown");
    history.step(open("P"));
    history.arrival(&[1, 1], SpawnOutcome::Completed, Some("memory"));
    let error = history
        .runtime
        .preview_context_plan()
        .expect_err("not a birth");
    assert!(error.to_string().contains("not a birth"), "{error}");
}

#[test]
fn close_keeps_replies_and_drops_other_assistant_text() {
    let mut history = History::new("reply-memory");
    history.user("outer request");
    history.step(open("P"));
    history.say(MessageRole::Assistant, "progress narration");
    history.say(MessageRole::Reply, "answer to the user");
    history.user("follow-up");
    history.step(close("P memory"));
    assert_eq!(
        memory_shape(&history, &[1, 1]),
        vec!["reply answer to the user", "user follow-up", "summary"]
    );
    history.replay();
}

#[test]
fn arrival_inside_a_sampling_is_rejected() {
    let mut history = History::new("arrival-inside");
    history.step(spawn(["left", "right"]));
    history.boundary += 1;
    let inside = SpineChar::Arrival {
        boundary: RawBoundary(history.boundary),
        arrival: SpawnArrival {
            producer: node(&[1, 1]),
            outcome: SpawnOutcome::Completed,
            memory: Some("left memory".to_string()),
            diagnostic: None,
            execution_ref: None,
        },
    };
    let error = history
        .sample(None, inside)
        .expect_err("arrival must wait for the sampling boundary");
    assert!(error.to_string().contains("inside a sampling"), "{error}");
}
