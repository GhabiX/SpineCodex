use pretty_assertions::assert_eq;
use spine_core::host::CanonicalReplay;
use spine_core::host::ContextEpoch;
use spine_core::host::ContextItem;
use spine_core::host::ExecutionOrigin;
use spine_core::host::Feature;
use spine_core::host::MemorySlot;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnResult;
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
        let sampling = self.runtime.begin_sampling().expect("begin");
        let started = self
            .runtime
            .sampling_started_record(&sampling)
            .expect("started");
        self.inputs.push(ReplayInput::Archive(started));
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
        self.boundary += 1;
        let output = SpineChar::Opaque {
            boundary: RawBoundary(self.boundary),
        };
        self.runtime
            .observe_source([output.clone()])
            .expect("source");
        self.inputs.push(ReplayInput::Source(output));
        self.runtime
            .finish_execution("control", true)
            .expect("execution");
        let SamplingFinish::Prepared(prepared) = self
            .runtime
            .finish_sampling(sampling, SamplingTerminal::Completed)
            .expect("prepare")
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
    history.step(SpineOperationFact::Spawn {
        tasks: ["left", "right"]
            .into_iter()
            .map(|summary| SpawnTask {
                summary: summary.to_string(),
                prompt: format!("inspect {summary}"),
            })
            .collect(),
        terminal_results: ["left", "right"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, reference)| SpawnResult {
                ordinal: u32::try_from(ordinal).expect("task ordinal"),
                outcome: SpawnOutcome::Completed,
                memory_body: "same body".to_string(),
                diagnostic: None,
                execution_ref: Some(reference.to_string()),
            })
            .collect(),
    });
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
            ("1.2.1".to_string(), "same body"),
            ("1.2.2".to_string(), "same body"),
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
    let references = resolved
        .cells
        .iter()
        .filter_map(|cell| match &cell.item {
            ContextItem::MemorySlot(MemorySlot::SpawnEvidence { execution_ref, .. }) => {
                execution_ref.as_deref()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(references, vec!["left", "right"]);
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
