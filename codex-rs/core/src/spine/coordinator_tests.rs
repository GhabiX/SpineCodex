use super::coordinator::CodexSpineCoordinator;
use super::coordinator::CoordinatorError;
use super::coordinator::InstalledCanonicalCommit;
use super::coordinator::ReplayMode;
use super::coordinator::SpineSamplingAttempt;
use super::coordinator::decode_spine_rollout_item;
use super::coordinator::replay_mode;
use super::observer::CodexSpineObserverHandler;
use codex_history::RolloutItem;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use pretty_assertions::assert_eq;
use spine_core::host::ExecutionOrigin;
use spine_core::host::Feature;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnResult;
use spine_core::host::SpawnTask;
use spine_core::host::SpineChar;
use spine_core::host::SpineConfig;
use spine_core::host::SpineOperationFact;
use spine_core::host::ThreadNamespace;

fn message(role: &str, text: &str) -> ResponseItem {
    let content = if role == "assistant" {
        vec![ContentItem::OutputText {
            text: text.to_string(),
        }]
    } else {
        vec![ContentItem::InputText {
            text: text.to_string(),
        }]
    };
    ResponseItem::Message {
        id: Some(ResponseItemId::from_server(format!("{role}-id"))),
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn coordinator() -> CodexSpineCoordinator {
    let config = SpineConfig::v1()
        .with_feature(Feature::Jit)
        .expect("JIT config");
    CodexSpineCoordinator::new_with_observer(
        "thread-shadow",
        config,
        CodexSpineObserverHandler::default(),
    )
    .expect("coordinator")
}

fn spawn_coordinator() -> CodexSpineCoordinator {
    let config = SpineConfig::v1()
        .with_feature(Feature::Jit)
        .and_then(|config| config.with_feature(Feature::Spawn))
        .expect("JIT and Spawn config");
    CodexSpineCoordinator::new_with_observer(
        "thread-spawn",
        config,
        CodexSpineObserverHandler::default(),
    )
    .expect("spawn coordinator")
}

fn spawn_operation() -> SpineOperationFact {
    SpineOperationFact::Spawn {
        tasks: vec![
            SpawnTask {
                summary: "first".to_string(),
                prompt: "first task".to_string(),
            },
            SpawnTask {
                summary: "second".to_string(),
                prompt: "second task".to_string(),
            },
        ],
        terminal_results: vec![
            SpawnResult {
                ordinal: 0,
                outcome: SpawnOutcome::Completed,
                memory_body: "first memory".to_string(),
                diagnostic: None,
                execution_ref: Some("00000000-0000-0000-0000-000000000001".to_string()),
            },
            SpawnResult {
                ordinal: 1,
                outcome: SpawnOutcome::Completed,
                memory_body: "second memory".to_string(),
                diagnostic: None,
                execution_ref: Some("00000000-0000-0000-0000-000000000002".to_string()),
            },
        ],
    }
}

fn install_spawn_sampling(
    coordinator: &mut CodexSpineCoordinator,
    call_id: &str,
) -> InstalledCanonicalCommit {
    coordinator
        .observe_response_items(
            &[message("user", "spawn request")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe spawn prompt source");
    let attempt = begin_sampling_for_test(coordinator).expect("begin spawn sampling");
    coordinator
        .register_execution(call_id)
        .expect("register spawn execution");
    coordinator
        .stage_execution(
            call_id,
            ExecutionOrigin::Direct {
                execution_ref: call_id.to_string(),
            },
            spawn_operation(),
        )
        .expect("stage spawn fact");
    coordinator
        .observe_response_items(
            &[message("assistant", "spawn completed")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe spawn sampling source");
    coordinator
        .finish_execution(call_id, true)
        .expect("finish spawn execution");
    install_sampling_for_test(coordinator, attempt).expect("install spawn sampling")
}

fn install_sampling_for_test(
    coordinator: &mut CodexSpineCoordinator,
    attempt: SpineSamplingAttempt,
) -> Result<InstalledCanonicalCommit, CoordinatorError> {
    let commit = coordinator.prepare_canonical_sampling(attempt)?;
    coordinator.install_canonical_sampling(commit)
}

fn begin_sampling_for_test(
    coordinator: &mut CodexSpineCoordinator,
) -> Result<SpineSamplingAttempt, CoordinatorError> {
    let attempt = coordinator.begin_sampling()?;
    coordinator.sampling_started_rollout_item(&attempt, &[])?;
    Ok(attempt)
}

fn open_source() -> [ResponseItem; 2] {
    [
        ResponseItem::FunctionCall {
            id: Some(ResponseItemId::from_server("open-request".to_string())),
            name: "open".to_string(),
            namespace: Some("spine".to_string()),
            arguments: r#"{"summary":"scope"}"#.to_string(),
            call_id: "open-call".to_string(),
            encrypted_function_args: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            name: None,
            namespace: None,
            id: Some(ResponseItemId::from_server("open-output".to_string())),
            call_id: Some("open-call".to_string()),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
    ]
}

#[test]
fn native_tool_items_are_opaque_across_sampling_interruption() {
    let mut coordinator = coordinator();
    let source = open_source();
    let items = [
        source[0].clone(),
        message("user", "interrupt"),
        source[1].clone(),
    ];

    coordinator
        .observe_response_items(&items.iter().cloned().map(Into::into).collect::<Vec<_>>())
        .expect("native tool interruption must remain opaque to Spine");

    let snapshot = coordinator.runtime.source_snapshot();
    let cells = snapshot.cells();
    assert!(matches!(cells[0].character(), SpineChar::Opaque { .. }));
    assert!(matches!(cells[1].character(), SpineChar::Message(_)));
    assert!(matches!(cells[2].character(), SpineChar::Opaque { .. }));
}

fn token_count(input_tokens: i64, model_context_window: i64) -> RolloutItem {
    let usage = TokenUsage {
        input_tokens,
        total_tokens: input_tokens,
        ..TokenUsage::default()
    };
    RolloutItem::EventMsg(EventMsg::TokenCount(TokenCountEvent {
        info: Some(TokenUsageInfo {
            total_token_usage: usage.clone(),
            last_token_usage: usage,
            model_context_window: Some(model_context_window),
        }),
        rate_limits: None,
    }))
}

fn canonical_rollout_items() -> Vec<RolloutItem> {
    let mut coordinator = coordinator();
    let user = message("user", "question");
    coordinator
        .observe_response_items(
            &(std::slice::from_ref(&user))
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = coordinator.begin_sampling().expect("begin");
    let started = coordinator
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe response source");
    let commit = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare");
    let mut rollout = vec![started];
    rollout.push(commit.rollout_item());
    rollout
}

#[test]
fn canonical_replay_mode_fails_closed_for_malformed_or_unsupported_started_record() {
    let mut records = canonical_rollout_items();
    let mut malformed = records.remove(0);
    let RolloutItem::SpineSamplingStarted(item) = &mut malformed else {
        panic!("sampling-started record must use its named rollout item");
    };
    item.payload = serde_json::json!({
        "type": "sampling_started",
        "record": {}
    });
    let malformed_effective = [(0, &malformed)];
    assert!(replay_mode(&malformed_effective).is_err());

    let mut unsupported = canonical_rollout_items().remove(0);
    let RolloutItem::SpineSamplingStarted(item) = &mut unsupported else {
        panic!("sampling-started record must use its named rollout item");
    };
    item.version = item.version.saturating_add(1);
    let unsupported_effective = [(0, &unsupported)];
    assert!(replay_mode(&unsupported_effective).is_err());
}

#[test]
fn canonical_replay_mode_uses_the_rollback_selected_prefix() {
    let started = canonical_rollout_items().remove(0);
    let rollout = vec![
        RolloutItem::ResponseItem(message("user", "legacy prefix").into()),
        RolloutItem::ResponseItem(message("user", "canonical turn").into()),
        started,
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let effective = super::effective_rollout(&rollout);
    assert_eq!(
        replay_mode(&effective).expect("rolled-back sampling start is absent"),
        ReplayMode::Native
    );
}

#[test]
fn spine_sampling_coordinator_seals_zero_fact_attempt() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe response source");

    let commit = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare");
    assert!(matches!(
        commit.rollout_item(),
        RolloutItem::SpineTransition(_)
    ));
    let installed = coordinator
        .install_canonical_sampling(commit)
        .expect("install");
    assert_eq!(installed.projection.nodes.len(), 1);
}

#[test]
fn spine_sampling_coordinator_retry_abort_isolated() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let failed = begin_sampling_for_test(&mut coordinator).expect("begin failed attempt");
    coordinator
        .abort_sampling(&failed)
        .expect("abort failed attempt");

    let retry = begin_sampling_for_test(&mut coordinator).expect("begin retry");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe retry response");
    let commit = coordinator
        .prepare_canonical_sampling(retry)
        .expect("prepare retry");
    let installed = coordinator
        .install_canonical_sampling(commit)
        .expect("install");
    assert_eq!(installed.projection.nodes.len(), 1);
}

#[test]
fn spine_durability_fault_is_sticky_and_rejects_sampling() {
    let mut coordinator = coordinator();
    coordinator.latch_durability_fault("write failed");
    coordinator.latch_durability_fault("later failure");

    assert_eq!(
        coordinator.durability_fault.as_deref(),
        Some("write failed")
    );
    assert!(coordinator.begin_sampling().is_err());
}

#[test]
fn spine_canonical_equivalence_preserves_ordinary_context() {
    let mut coordinator = coordinator();
    let expected = [message("user", "question"), message("assistant", "answer")];
    coordinator
        .observe_response_items(
            &expected[..1]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(
            &expected[1..]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe response source");

    let commit = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare");
    let installed = coordinator
        .install_canonical_sampling(commit)
        .expect("install");
    assert_eq!(installed.projection.cursor.to_string(), "1");
    assert_eq!(installed.projection.nodes.len(), 1);
    assert_eq!(installed.projection.visible_context.len(), 2);
    assert_eq!(installed.context.items.len(), expected.len());
}

#[test]
fn spine_canonical_equivalence_uses_explicit_fact_for_transition() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("execution-open")
        .expect("register execution");
    coordinator
        .stage_execution(
            "execution-open",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("execution-open", true)
        .expect("finish execution");

    let commit = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare");
    let installed = coordinator
        .install_canonical_sampling(commit)
        .expect("install");
    assert_eq!(installed.projection.cursor.to_string(), "1.1");
    assert_eq!(installed.projection.nodes.len(), 2);
}

#[test]
fn spawn_settlement_is_local_to_the_live_sampling_commit() {
    let (tx_event, rx_event) = async_channel::unbounded();
    let config = SpineConfig::v1()
        .with_feature(Feature::Jit)
        .and_then(|config| config.with_feature(Feature::Spawn))
        .expect("JIT and Spawn config");
    let observer = CodexSpineObserverHandler::new(
        tx_event,
        "fallback-event".to_string(),
        None,
        /*jit_enabled*/ true,
    );
    let mut coordinator =
        CodexSpineCoordinator::new_with_observer("thread-spawn-events", config, observer)
            .expect("spawn coordinator with observer");

    let spawn = install_spawn_sampling(&mut coordinator, "reused-call");
    assert_eq!(spawn.settled_spawn_call_ids, ["reused-call"]);
    coordinator.publish_canonical_sampling(&spawn);
    let EventMsg::SpineTreeUpdate(spawn_update) = rx_event.try_recv().expect("spawn update").msg
    else {
        panic!("spawn commit must publish a tree update");
    };
    assert_eq!(spawn_update.settled_spawn_call_ids, ["reused-call"]);
    assert_eq!(spawn_update.settled_spawn_thread_ids.len(), 2);

    let open = begin_sampling_for_test(&mut coordinator).expect("begin open sampling");
    coordinator
        .register_execution("open-execution")
        .expect("register open execution");
    coordinator
        .stage_execution(
            "open-execution",
            ExecutionOrigin::Direct {
                execution_ref: "reused-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage open fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe open source");
    coordinator
        .finish_execution("open-execution", true)
        .expect("finish open execution");
    let open = install_sampling_for_test(&mut coordinator, open).expect("install open sampling");
    assert_eq!(open.settled_spawn_call_ids, Vec::<String>::new());
    coordinator.publish_canonical_sampling(&open);
    let EventMsg::SpineTreeUpdate(open_update) = rx_event.try_recv().expect("open update").msg
    else {
        panic!("open commit must publish a tree update");
    };
    assert_eq!(open_update.settled_spawn_call_ids, Vec::<String>::new());
    assert_eq!(
        open_update.settled_spawn_thread_ids,
        spawn_update.settled_spawn_thread_ids
    );

    let compact = coordinator.prepare_compact(&[]).expect("prepare compact");
    coordinator.install_compact(compact);
    coordinator.publish_canonical_compact();
    let EventMsg::SpineTreeUpdate(compact_update) =
        rx_event.try_recv().expect("compact update").msg
    else {
        panic!("compact publication must emit a tree update");
    };
    assert_eq!(compact_update.settled_spawn_call_ids, Vec::<String>::new());
    assert_eq!(compact_update.active_node_id, "2");
    assert_eq!(
        compact_update.settled_spawn_thread_ids,
        spawn_update.settled_spawn_thread_ids
    );

    let RolloutItem::EventMsg(EventMsg::TokenCount(usage)) = token_count(10_001, 80_000) else {
        panic!("token count helper must produce a token event");
    };
    coordinator.observe_token_count(&usage, "usage-turn");
    let usage_event = rx_event.try_recv().expect("usage update");
    let EventMsg::SpineTreeUpdate(usage_update) = usage_event.msg else {
        panic!("usage publication must emit a tree update");
    };
    assert_eq!(
        (usage_event.id, usage_update.settled_spawn_call_ids),
        ("usage-turn".to_string(), Vec::<String>::new())
    );
    assert_eq!(
        usage_update.settled_spawn_thread_ids,
        spawn_update.settled_spawn_thread_ids
    );
}

#[test]
fn canonical_replay_does_not_resettle_historical_spawn_calls() {
    let user = message("user", "question");
    let response = message("assistant", "spawn completed");
    let mut live = spawn_coordinator();
    live.observe_response_items(
        &(std::slice::from_ref(&user))
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe prompt source");
    let attempt = live.begin_sampling().expect("begin spawn sampling");
    let started = live
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    live.register_execution("spawn-call")
        .expect("register spawn execution");
    live.stage_execution(
        "spawn-call",
        ExecutionOrigin::Direct {
            execution_ref: "spawn-call".to_string(),
        },
        spawn_operation(),
    )
    .expect("stage spawn fact");
    live.observe_response_items(
        &(std::slice::from_ref(&response))
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe spawn source");
    live.finish_execution("spawn-call", true)
        .expect("finish spawn execution");
    let prepared = live
        .prepare_canonical_sampling(attempt)
        .expect("prepare spawn commit");
    let rollout = [
        RolloutItem::ResponseItem(user.into()),
        started,
        RolloutItem::ResponseItem(response.into()),
        prepared.rollout_item(),
    ];
    let live_commit = live
        .install_canonical_sampling(prepared)
        .expect("install live spawn commit");
    assert_eq!(live_commit.settled_spawn_call_ids, ["spawn-call"]);

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let mut resumed = spawn_coordinator();
    let replayed = resumed
        .replay_canonical(&effective, &live_commit.context.items, thread, records)
        .expect("replay spawn commit");

    assert_eq!(replayed.projection, live_commit.projection);
    assert_eq!(replayed.settled_spawn_call_ids, Vec::<String>::new());
    let snapshot = super::observer::tree_update_from_parts(&replayed.projection, &[], &[]);
    assert_eq!(
        snapshot
            .settled_spawn_thread_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002"
        ]
    );
}

#[test]
fn spine_sampling_commit_is_self_contained() {
    let mut coordinator = coordinator();
    let user = message("user", "question");
    coordinator
        .observe_response_items(
            &(std::slice::from_ref(&user))
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("open-call", true)
        .expect("finish execution");

    let prepared = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");
    let item = prepared.rollout_item();
    let RolloutItem::SpineTransition(_) = &item else {
        panic!("sampling must emit one canonical commit");
    };
    let spine_core::host::SamplingArchiveRecord::SamplingCommit(record) =
        decode_spine_rollout_item(&item)
            .expect("decode")
            .expect("Spine transition")
    else {
        panic!("sampling must emit a commit record");
    };
    let [execution] = record.executions.as_slice() else {
        panic!("open commit must archive exactly one execution");
    };
    assert_eq!(
        execution.operation,
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        }
    );
    let installed = coordinator
        .install_canonical_sampling(prepared)
        .expect("install");
    assert_eq!(installed.projection.cursor.to_string(), "1.1");
    assert_eq!(installed.projection.nodes.len(), 2);
    assert_eq!(installed.context.items.len(), 4);

    let second = begin_sampling_for_test(&mut coordinator).expect("begin second sampling");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe second response");
    let second = coordinator
        .prepare_canonical_sampling(second)
        .expect("prepare second canonical commit");
    let item = second.rollout_item();
    let item @ RolloutItem::SpineTransition(_) = &item else {
        panic!("canonical record must use a Spine transition");
    };
    assert!(matches!(
        decode_spine_rollout_item(item)
            .expect("decode")
            .expect("Spine transition"),
        spine_core::host::SamplingArchiveRecord::SamplingCommit(_)
    ));
}

#[test]
fn spine_compatibility_release_replays_and_continues_canonical_rollout() {
    let user = message("user", "question");
    let mut live = coordinator();
    live.observe_response_items(
        &(std::slice::from_ref(&user))
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe prompt source");
    let attempt = live.begin_sampling().expect("begin");
    let started = live
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    live.register_execution("open-call")
        .expect("register execution");
    live.stage_execution(
        "open-call",
        ExecutionOrigin::Direct {
            execution_ref: "open-call".to_string(),
        },
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    )
    .expect("stage fact");
    live.observe_response_items(
        &open_source()
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe transition source");
    live.finish_execution("open-call", true)
        .expect("finish execution");
    live.record_context_window(80_000);
    let prepared = live
        .finish_canonical_sampling_with_input_tokens(
            attempt,
            SamplingTerminal::Completed,
            /*input_tokens*/ Some(10_001),
        )
        .expect("prepare canonical commit")
        .expect("completed sampling commit");
    let mut rollout = vec![RolloutItem::ResponseItem(user.into()), started];
    rollout.extend(
        open_source()
            .into_iter()
            .map(|item| RolloutItem::ResponseItem(item.into())),
    );
    rollout.push(prepared.rollout_item());
    rollout.push(token_count(10_001, 80_000));
    let installed = live.install_canonical_sampling(prepared).expect("install");
    let installed_context = serde_json::to_string(
        &installed
            .context
            .items
            .iter()
            .map(|envelope| &envelope.item)
            .collect::<Vec<_>>(),
    )
    .expect("context json");
    assert!(installed_context.contains("<spine_node id=\\\"1.1\\\""));
    assert!(!installed_context.contains("Current Remaining Context Windows"));

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let mut resumed = coordinator();
    let replayed = resumed
        .replay_canonical(&effective, &installed.context.items, thread, records)
        .expect("replay canonical rollout");
    assert_eq!(replayed.projection, installed.projection);
    assert_eq!(replayed.context, installed.context);

    let continued = resumed.begin_sampling().expect("continue after replay");
    resumed
        .sampling_started_rollout_item(
            &continued,
            &replayed
                .context
                .items
                .iter()
                .map(|envelope| envelope.item.clone())
                .collect::<Vec<_>>(),
        )
        .expect("continued sampling started");
    resumed
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe continued source");
    let continued = resumed
        .prepare_canonical_sampling(continued)
        .expect("prepare continued commit");
    let item = continued.rollout_item();
    let item @ RolloutItem::SpineTransition(_) = &item else {
        panic!("continued canonical record must use a Spine transition");
    };
    assert!(matches!(
        decode_spine_rollout_item(item)
            .expect("decode")
            .expect("Spine transition"),
        spine_core::host::SamplingArchiveRecord::SamplingCommit(_)
    ));
}

#[test]
fn ordinary_observation_and_token_accounting_preserve_the_model_context_prefix() {
    let user = message("user", "question");
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &(std::slice::from_ref(&user))
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("open-call", true)
        .expect("finish execution");
    coordinator.record_context_window(80_000);
    let first = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare first commit");
    let first = coordinator
        .install_canonical_sampling(first)
        .expect("install first commit")
        .context
        .items;

    coordinator.record_context_window(40_000);
    let second = coordinator
        .observe_response_items(
            &[message("assistant", "follow-up")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("ordinary observation");

    assert_eq!(&second.items[..first.len()], first.as_slice());
}

#[test]
fn canonical_replay_continues_after_orphan_sampling_started() {
    let user = message("user", "question");
    let mut interrupted = coordinator();
    interrupted
        .observe_response_items(
            &(std::slice::from_ref(&user))
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let orphan_attempt = interrupted.begin_sampling().expect("begin orphan sampling");
    let orphan_started = interrupted
        .sampling_started_rollout_item(&orphan_attempt, std::slice::from_ref(&user))
        .expect("orphan sampling started");
    let mut rollout = vec![
        RolloutItem::ResponseItem(user.clone().into()),
        orphan_started,
    ];

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let mut resumed = coordinator();
    let replayed = resumed
        .replay_canonical(&effective, &[user.clone().into()], thread, records)
        .expect("replay orphan sampling start");

    let continued_attempt = resumed.begin_sampling().expect("continue after orphan");
    let continued_started = resumed
        .sampling_started_rollout_item(
            &continued_attempt,
            &replayed
                .context
                .items
                .iter()
                .map(|envelope| envelope.item.clone())
                .collect::<Vec<_>>(),
        )
        .expect("continued sampling started");
    resumed
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe continued source");
    let prepared = resumed
        .prepare_canonical_sampling(continued_attempt)
        .expect("prepare continued commit");
    rollout.push(continued_started);
    rollout.push(RolloutItem::ResponseItem(
        message("assistant", "answer").into(),
    ));
    rollout.push(prepared.rollout_item());
    let installed = resumed
        .install_canonical_sampling(prepared)
        .expect("install");

    let started_attempts = rollout
        .iter()
        .filter_map(
            |item| match decode_spine_rollout_item(item).ok().flatten()? {
                spine_core::host::SamplingArchiveRecord::SamplingStarted(started) => {
                    Some(started.attempt_id)
                }
                spine_core::host::SamplingArchiveRecord::SamplingCommit(_) => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(started_attempts.len(), 2);
    assert_ne!(started_attempts[0], started_attempts[1]);

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let replayed = coordinator()
        .replay_canonical(&effective, &installed.context.items, thread, records)
        .expect("replay continued sampling after orphan");
    assert_eq!(replayed.projection, installed.projection);
    assert_eq!(replayed.context, installed.context);
}

#[test]
fn canonical_replay_accepts_persisted_reasoning_with_omitted_empty_content() {
    let user = message("user", "question");
    let reasoning = ResponseItem::Reasoning {
        id: Some(ResponseItemId::from_server("reasoning-id".to_string())),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "planning".to_string(),
        }],
        content: Some(Vec::new()),
        encrypted_content: Some("ciphertext".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let persisted_reasoning =
        serde_json::from_value(serde_json::to_value(&reasoning).expect("serialize live reasoning"))
            .expect("deserialize persisted reasoning");
    assert!(matches!(
        persisted_reasoning,
        ResponseItem::Reasoning { content: None, .. }
    ));

    let mut live = coordinator();
    live.observe_response_items(
        &(std::slice::from_ref(&user))
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe prompt source");
    let attempt = live.begin_sampling().expect("begin");
    let started = live
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    live.register_execution("open-call")
        .expect("register execution");
    live.stage_execution(
        "open-call",
        ExecutionOrigin::Direct {
            execution_ref: "open-call".to_string(),
        },
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    )
    .expect("stage fact");
    live.observe_response_items(
        &(std::slice::from_ref(&reasoning))
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe live reasoning");
    live.observe_response_items(
        &open_source()
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe transition source");
    live.finish_execution("open-call", true)
        .expect("finish execution");
    let prepared = live
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");
    let mut rollout = vec![
        RolloutItem::ResponseItem(user.into()),
        started,
        RolloutItem::ResponseItem(persisted_reasoning.into()),
    ];
    rollout.extend(
        open_source()
            .into_iter()
            .map(|item| RolloutItem::ResponseItem(item.into())),
    );
    rollout.push(prepared.rollout_item());
    let installed = live.install_canonical_sampling(prepared).expect("install");

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let mut resumed = coordinator();
    let replayed = resumed
        .replay_canonical(&effective, &installed.context.items, thread, records)
        .expect("persisted reasoning must preserve canonical source identity");

    assert_eq!(replayed.projection, installed.projection);
}

#[test]
fn canonical_replay_accepts_host_tool_output_presentation_difference() {
    let mut live = coordinator();
    let request = ResponseItem::FunctionCall {
        id: Some(ResponseItemId::from_server("large-request".to_string())),
        name: "shell".to_string(),
        namespace: None,
        arguments: r#"{"cmd":"large-output"}"#.to_string(),
        call_id: "large-call".to_string(),
        encrypted_function_args: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let processed_output = ResponseItem::FunctionCallOutput {
        name: None,
        namespace: None,
        id: Some(ResponseItemId::from_server("large-output".to_string())),
        call_id: Some("large-call".to_string()),
        output: FunctionCallOutputPayload::from_text("host-truncated".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    live.observe_response_items(
        &[request.clone(), processed_output.clone()]
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
    )
    .expect("observe host-processed source");
    let attempt = live.begin_sampling().expect("begin");
    let started = live
        .sampling_started_rollout_item(&attempt, &[])
        .expect("sampling started");
    let prepared = live
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");
    let transition = prepared.rollout_item();
    let expected = live
        .install_canonical_sampling(prepared)
        .expect("install canonical commit");

    let raw_body = "raw persisted output".repeat(1_000);
    let raw_output = ResponseItem::FunctionCallOutput {
        name: None,
        namespace: None,
        id: Some(ResponseItemId::from_server("large-output".to_string())),
        call_id: Some("large-call".to_string()),
        output: FunctionCallOutputPayload::from_text(raw_body),
        internal_chat_message_metadata_passthrough: None,
    };
    let mut rollout = vec![
        RolloutItem::ResponseItem(request.clone().into()),
        RolloutItem::ResponseItem(raw_output.into()),
        started,
    ];
    rollout.push(transition);
    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let replayed = coordinator()
        .replay_canonical(&effective, &expected.context.items, thread, records)
        .expect("replay canonical rollout");
    assert_eq!(replayed.projection, expected.projection);
    assert_eq!(
        replayed.context.items,
        vec![
            codex_history::ResponseItemEnvelope::new(request),
            codex_history::ResponseItemEnvelope::new(processed_output)
        ]
    );
}

#[test]
fn canonical_fork_preserves_prefix_ids_and_uses_child_suffix_namespace() {
    let user = message("user", "question");
    let mut parent = coordinator();
    parent
        .observe_response_items(
            &(std::slice::from_ref(&user))
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = parent.begin_sampling().expect("begin");
    let started = parent
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    parent
        .register_execution("open-call")
        .expect("register execution");
    parent
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    parent
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    parent
        .finish_execution("open-call", true)
        .expect("finish execution");
    let prepared = parent
        .prepare_canonical_sampling(attempt)
        .expect("prepare parent commit");
    let mut rollout = vec![RolloutItem::ResponseItem(user.into()), started];
    rollout.extend(
        open_source()
            .into_iter()
            .map(|item| RolloutItem::ResponseItem(item.into())),
    );
    rollout.push(prepared.rollout_item());
    let installed = parent
        .install_canonical_sampling(prepared)
        .expect("install");

    let config = SpineConfig::v1()
        .with_feature(Feature::Jit)
        .expect("JIT config");
    let mut child = CodexSpineCoordinator::new_with_observer(
        "thread-child",
        config,
        CodexSpineObserverHandler::default(),
    )
    .expect("child coordinator");
    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    child
        .replay_canonical(&effective, &installed.context.items, thread, records)
        .expect("replay parent prefix");
    let attempt = begin_sampling_for_test(&mut child).expect("continue child");
    child
        .observe_response_items(
            &[message("assistant", "child answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe child source");
    let prepared = child
        .prepare_canonical_sampling(attempt)
        .expect("prepare child commit");
    let item = prepared.rollout_item();
    let item @ RolloutItem::SpineTransition(_) = &item else {
        panic!("child commit must use a Spine transition");
    };
    let record = match decode_spine_rollout_item(item)
        .expect("decode child record")
        .expect("child sampling record")
    {
        spine_core::host::SamplingArchiveRecord::SamplingCommit(record) => record,
        spine_core::host::SamplingArchiveRecord::SamplingStarted(_) => {
            panic!("child continuation must produce a sampling commit")
        }
    };
    let child_namespace = ThreadNamespace::parse("thread-child").expect("child namespace");
    let parent_namespace = ThreadNamespace::parse("thread-shadow").expect("parent namespace");

    assert_eq!(record.attempt_id.thread(), &child_namespace);
    assert_eq!(record.commit_id.thread(), &child_namespace);
    assert_eq!(
        record
            .previous_commit_id
            .as_ref()
            .map(spine_core::host::SamplingCommitId::thread),
        Some(&parent_namespace)
    );
}

#[test]
fn spine_sampling_atomic_prepare_is_not_visible_until_install() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("open-call", true)
        .expect("finish execution");

    let prepared = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");
    assert_eq!(coordinator.runtime.projection().nodes.len(), 1);
    assert!(
        coordinator
            .validate_control(spine_core::host::SpineTool::Close)
            .is_err()
    );

    let installed = coordinator
        .install_canonical_sampling(prepared)
        .expect("install");
    assert_eq!(coordinator.runtime.projection(), &installed.projection);
    assert_eq!(installed.projection.nodes.len(), 2);
    assert!(
        coordinator
            .validate_control(spine_core::host::SpineTool::Close)
            .is_ok()
    );
}

#[test]
fn codex_context_materialization_failure_discards_sdk_candidate() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let open = begin_sampling_for_test(&mut coordinator).expect("begin open");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage open fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe open source");
    coordinator
        .finish_execution("open-call", true)
        .expect("finish execution");
    install_sampling_for_test(&mut coordinator, open).expect("install open");

    let close = begin_sampling_for_test(&mut coordinator).expect("begin close");
    coordinator
        .register_execution("close-call")
        .expect("register close execution");
    coordinator
        .stage_execution(
            "close-call",
            ExecutionOrigin::Direct {
                execution_ref: "close-call".to_string(),
            },
            SpineOperationFact::Close {
                memory: "\0".repeat(20_000),
            },
        )
        .expect("stage bounded close fact");
    coordinator
        .observe_response_items(
            &[
                ResponseItem::FunctionCall {
                    id: Some(ResponseItemId::from_server("close-request".to_string())),
                    name: "close".to_string(),
                    namespace: Some("spine".to_string()),
                    arguments: r#"{"memory":"finished"}"#.to_string(),
                    call_id: "close-call".to_string(),
                    encrypted_function_args: None,
                    internal_chat_message_metadata_passthrough: None,
                },
                ResponseItem::FunctionCallOutput {
                    name: None,
                    namespace: None,
                    id: Some(ResponseItemId::from_server("close-output".to_string())),
                    call_id: Some("close-call".to_string()),
                    output: FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text("closed".to_string()),
                        success: Some(true),
                    },
                    internal_chat_message_metadata_passthrough: None,
                },
            ]
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<_>>(),
        )
        .expect("observe close source");
    coordinator
        .finish_execution("close-call", true)
        .expect("finish close execution");

    let projection_before = coordinator.runtime.projection().clone();
    assert!(matches!(
        coordinator.prepare_canonical_sampling(close),
        Err(CoordinatorError::ContextPlan(_))
    ));
    assert_eq!(coordinator.runtime.projection(), &projection_before);
    assert!(!coordinator.runtime.has_pending_durable_sampling());

    let retry = begin_sampling_for_test(&mut coordinator).expect("begin valid close");
    coordinator
        .observe_response_items(
            &[message("assistant", "ordinary retry")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe retry source");
    install_sampling_for_test(&mut coordinator, retry).expect("runtime remains reusable");
}

#[test]
fn spine_prepared_commit_rejects_racing_source_until_install() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe response source");
    let prepared = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");

    assert!(matches!(
        coordinator.observe_response_items(
            &[message("assistant", "racing source")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>()
        ),
        Err(super::coordinator::CoordinatorError::Planner(
            spine_core::host::PlannerError::SamplingCommitPendingInstall
        ))
    ));
    coordinator
        .install_canonical_sampling(prepared)
        .expect("install prepared commit");
    coordinator
        .observe_response_items(
            &[message("assistant", "source after install")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("source after install");
}

#[test]
fn spine_compact_live_advances_the_epoch_atomically() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "before compact")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe source");
    let replacement = [message("assistant", "compact summary")];
    let previous = coordinator.runtime.projection().clone();
    let prepared = coordinator
        .prepare_compact(
            &replacement
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("prepare compact context");
    assert_eq!(coordinator.runtime.projection(), &previous);
    coordinator.install_compact(prepared);
    assert!(
        coordinator
            .runtime
            .projection()
            .nodes
            .iter()
            .any(|node| { node.status == spine_core::host::NodeStatus::Compacted })
    );
}

#[test]
fn spine_compact_live_preserves_session_user_message_projection() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "before compact")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin sampling");
    coordinator
        .observe_response_items(
            &[message("assistant", "answer")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe response source");
    let installed = install_sampling_for_test(&mut coordinator, attempt).expect("install");
    coordinator.publish_canonical_sampling(&installed);
    assert_eq!(coordinator.user_message_projection().len(), 1);

    let prepared = coordinator
        .prepare_compact(
            &[message("assistant", "compact summary")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("prepare compact context");
    coordinator.install_compact(prepared);

    assert_eq!(coordinator.user_message_projection().len(), 1);
    assert_eq!(
        coordinator.user_message_projection()[0].body,
        "before compact"
    );
}

#[test]
fn spine_execution_fact_commits_only_after_lifecycle_success() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("open-call", true)
        .expect("finish execution");

    let commit = install_sampling_for_test(&mut coordinator, attempt).expect("install");
    assert_eq!(commit.projection.cursor.to_string(), "1.1");
    assert_eq!(commit.projection.nodes.len(), 2);
}

#[test]
fn spine_execution_fact_is_discarded_when_lifecycle_rejects_result() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                execution_ref: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(
            &open_source()
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe transition source");
    coordinator
        .finish_execution("open-call", false)
        .expect("discard execution");

    let commit = install_sampling_for_test(&mut coordinator, attempt).expect("install");

    assert_eq!(commit.projection.nodes.len(), 1);
}

#[test]
fn spine_sampling_rejects_unfinished_execution_slot() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");

    let error = install_sampling_for_test(&mut coordinator, attempt)
        .expect_err("pending execution must reject seal");

    assert!(matches!(
        error,
        super::coordinator::CoordinatorError::Planner(
            spine_core::host::PlannerError::PendingExecutions(1)
        )
    ));
}

#[test]
fn spine_sampling_rejects_success_without_staged_fact() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(
            &[message("user", "question")]
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    assert!(
        coordinator.finish_execution("open-call", true).is_err(),
        "successful execution without a typed fact must fail the batch"
    );

    let error = install_sampling_for_test(&mut coordinator, attempt)
        .expect_err("failed execution batch must reject seal");

    assert!(matches!(
        error,
        super::coordinator::CoordinatorError::Planner(spine_core::host::PlannerError::Sampling(
            spine_core::host::SamplingError::TransactionAborted
        ))
    ));
}
