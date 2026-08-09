use super::coordinator::CodexSpineCoordinator;
use super::coordinator::CoordinatorError;
use super::coordinator::InstalledCanonicalCommit;
use super::coordinator::ReplayMode;
use super::coordinator::SpineSamplingAttempt;
use super::coordinator::decode_spine_rollout_item;
use super::coordinator::replay_mode;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use pretty_assertions::assert_eq;
use spine_core::ExecutionOrigin;
use spine_core::Feature;
use spine_core::SamplingTerminal;
use spine_core::SpineConfig;
use spine_core::SpineOperationFact;
use spine_core::ThreadNamespace;

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
    CodexSpineCoordinator::new("thread-shadow", config).expect("coordinator")
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
            id: Some(ResponseItemId::from_server("open-output".to_string())),
            call_id: "open-call".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
    ]
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
        .observe_response_items(std::slice::from_ref(&user))
        .expect("observe prompt source");
    let attempt = coordinator.begin_sampling().expect("begin");
    let started = coordinator
        .sampling_started_rollout_item(&attempt, std::slice::from_ref(&user))
        .expect("sampling started");
    coordinator
        .observe_response_items(&[message("assistant", "answer")])
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
        RolloutItem::ResponseItem(message("user", "legacy prefix")),
        RolloutItem::ResponseItem(message("user", "canonical turn")),
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
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(&[message("assistant", "answer")])
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
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let failed = begin_sampling_for_test(&mut coordinator).expect("begin failed attempt");
    coordinator
        .abort_sampling(&failed)
        .expect("abort failed attempt");

    let retry = begin_sampling_for_test(&mut coordinator).expect("begin retry");
    coordinator
        .observe_response_items(&[message("assistant", "answer")])
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
        .observe_response_items(&expected[..1])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(&expected[1..])
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
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("execution-open")
        .expect("register execution");
    coordinator
        .stage_execution(
            "execution-open",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
fn spine_sampling_commit_is_self_contained() {
    let mut coordinator = coordinator();
    let user = message("user", "question");
    coordinator
        .observe_response_items(std::slice::from_ref(&user))
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
    let spine_core::SamplingArchiveRecord::SamplingCommit(record) =
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
        .observe_response_items(&[message("assistant", "answer")])
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
        spine_core::SamplingArchiveRecord::SamplingCommit(_)
    ));
}

#[test]
fn spine_compatibility_release_replays_and_continues_canonical_rollout() {
    let user = message("user", "question");
    let mut live = coordinator();
    live.observe_response_items(std::slice::from_ref(&user))
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
            call_id: "open-call".to_string(),
        },
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    )
    .expect("stage fact");
    live.observe_response_items(&open_source())
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
    let mut rollout = vec![RolloutItem::ResponseItem(user), started];
    rollout.extend(open_source().into_iter().map(RolloutItem::ResponseItem));
    rollout.push(prepared.rollout_item());
    rollout.push(token_count(10_001, 80_000));
    let installed = live.install_canonical_sampling(prepared).expect("install");
    let installed_context = serde_json::to_string(&installed.context.items).expect("context json");
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
        .sampling_started_rollout_item(&continued, &replayed.context.items)
        .expect("continued sampling started");
    resumed
        .observe_response_items(&[message("assistant", "answer")])
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
        spine_core::SamplingArchiveRecord::SamplingCommit(_)
    ));
}

#[test]
fn ordinary_observation_and_token_accounting_preserve_the_model_context_prefix() {
    let user = message("user", "question");
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(std::slice::from_ref(&user))
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
        .observe_response_items(&[message("assistant", "follow-up")])
        .expect("ordinary observation");

    assert_eq!(&second.items[..first.len()], first.as_slice());
}

#[test]
fn canonical_replay_continues_after_orphan_sampling_started() {
    let user = message("user", "question");
    let mut interrupted = coordinator();
    interrupted
        .observe_response_items(std::slice::from_ref(&user))
        .expect("observe prompt source");
    let orphan_attempt = interrupted.begin_sampling().expect("begin orphan sampling");
    let orphan_started = interrupted
        .sampling_started_rollout_item(&orphan_attempt, std::slice::from_ref(&user))
        .expect("orphan sampling started");
    let mut rollout = vec![RolloutItem::ResponseItem(user.clone()), orphan_started];

    let effective = rollout.iter().enumerate().collect::<Vec<_>>();
    let ReplayMode::Canonical { thread, records } =
        replay_mode(&effective).expect("canonical replay mode")
    else {
        panic!("rollout must be canonical");
    };
    let mut resumed = coordinator();
    let replayed = resumed
        .replay_canonical(&effective, std::slice::from_ref(&user), thread, records)
        .expect("replay orphan sampling start");

    let continued_attempt = resumed.begin_sampling().expect("continue after orphan");
    let continued_started = resumed
        .sampling_started_rollout_item(&continued_attempt, &replayed.context.items)
        .expect("continued sampling started");
    resumed
        .observe_response_items(&[message("assistant", "answer")])
        .expect("observe continued source");
    let prepared = resumed
        .prepare_canonical_sampling(continued_attempt)
        .expect("prepare continued commit");
    rollout.push(continued_started);
    rollout.push(RolloutItem::ResponseItem(message("assistant", "answer")));
    rollout.push(prepared.rollout_item());
    let installed = resumed
        .install_canonical_sampling(prepared)
        .expect("install");

    let started_attempts = rollout
        .iter()
        .filter_map(
            |item| match decode_spine_rollout_item(item).ok().flatten()? {
                spine_core::SamplingArchiveRecord::SamplingStarted(started) => {
                    Some(started.attempt_id)
                }
                spine_core::SamplingArchiveRecord::SamplingCommit(_) => None,
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
    live.observe_response_items(std::slice::from_ref(&user))
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
            call_id: "open-call".to_string(),
        },
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    )
    .expect("stage fact");
    live.observe_response_items(std::slice::from_ref(&reasoning))
        .expect("observe live reasoning");
    live.observe_response_items(&open_source())
        .expect("observe transition source");
    live.finish_execution("open-call", true)
        .expect("finish execution");
    let prepared = live
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");
    let mut rollout = vec![
        RolloutItem::ResponseItem(user),
        started,
        RolloutItem::ResponseItem(persisted_reasoning),
    ];
    rollout.extend(open_source().into_iter().map(RolloutItem::ResponseItem));
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
        id: Some(ResponseItemId::from_server("large-output".to_string())),
        call_id: "large-call".to_string(),
        output: FunctionCallOutputPayload::from_text("host-truncated".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    live.observe_response_items(&[request.clone(), processed_output])
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
        id: Some(ResponseItemId::from_server("large-output".to_string())),
        call_id: "large-call".to_string(),
        output: FunctionCallOutputPayload::from_text(raw_body),
        internal_chat_message_metadata_passthrough: None,
    };
    let mut rollout = vec![
        RolloutItem::ResponseItem(request.clone()),
        RolloutItem::ResponseItem(raw_output.clone()),
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
    assert_eq!(replayed.context.items, vec![request, raw_output]);
}

#[test]
fn canonical_fork_preserves_prefix_ids_and_uses_child_suffix_namespace() {
    let user = message("user", "question");
    let mut parent = coordinator();
    parent
        .observe_response_items(std::slice::from_ref(&user))
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
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    parent
        .observe_response_items(&open_source())
        .expect("observe transition source");
    parent
        .finish_execution("open-call", true)
        .expect("finish execution");
    let prepared = parent
        .prepare_canonical_sampling(attempt)
        .expect("prepare parent commit");
    let mut rollout = vec![RolloutItem::ResponseItem(user), started];
    rollout.extend(open_source().into_iter().map(RolloutItem::ResponseItem));
    rollout.push(prepared.rollout_item());
    let installed = parent
        .install_canonical_sampling(prepared)
        .expect("install");

    let config = SpineConfig::v1()
        .with_feature(Feature::Jit)
        .expect("JIT config");
    let mut child = CodexSpineCoordinator::new("thread-child", config).expect("child coordinator");
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
        .observe_response_items(&[message("assistant", "child answer")])
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
        spine_core::SamplingArchiveRecord::SamplingCommit(record) => record,
        spine_core::SamplingArchiveRecord::SamplingStarted(_) => {
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
            .map(spine_core::SamplingCommitId::thread),
        Some(&parent_namespace)
    );
}

#[test]
fn spine_sampling_atomic_prepare_is_not_visible_until_install() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
            .validate_control(spine_core::SpineTool::Close)
            .is_err()
    );

    let installed = coordinator
        .install_canonical_sampling(prepared)
        .expect("install");
    assert_eq!(coordinator.runtime.projection(), &installed.projection);
    assert_eq!(installed.projection.nodes.len(), 2);
    assert!(
        coordinator
            .validate_control(spine_core::SpineTool::Close)
            .is_ok()
    );
}

#[test]
fn codex_context_materialization_failure_discards_sdk_candidate() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let open = begin_sampling_for_test(&mut coordinator).expect("begin open");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage open fact");
    coordinator
        .observe_response_items(&open_source())
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
                call_id: "close-call".to_string(),
            },
            SpineOperationFact::Close {
                memory: "\0".repeat(20_000),
            },
        )
        .expect("stage bounded close fact");
    coordinator
        .observe_response_items(&[
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
                id: Some(ResponseItemId::from_server("close-output".to_string())),
                call_id: "close-call".to_string(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("closed".to_string()),
                    success: Some(true),
                },
                internal_chat_message_metadata_passthrough: None,
            },
        ])
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
        .observe_response_items(&[message("assistant", "ordinary retry")])
        .expect("observe retry source");
    install_sampling_for_test(&mut coordinator, retry).expect("runtime remains reusable");
}

#[test]
fn spine_prepared_commit_rejects_racing_source_until_install() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .observe_response_items(&[message("assistant", "answer")])
        .expect("observe response source");
    let prepared = coordinator
        .prepare_canonical_sampling(attempt)
        .expect("prepare canonical commit");

    assert!(matches!(
        coordinator.observe_response_items(&[message("assistant", "racing source")]),
        Err(super::coordinator::CoordinatorError::Planner(
            spine_core::PlannerError::SamplingCommitPendingInstall
        ))
    ));
    coordinator
        .install_canonical_sampling(prepared)
        .expect("install prepared commit");
    coordinator
        .observe_response_items(&[message("assistant", "source after install")])
        .expect("source after install");
}

#[test]
fn spine_compact_live_advances_the_epoch_atomically() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "before compact")])
        .expect("observe source");
    let replacement = [message("assistant", "compact summary")];
    coordinator
        .compact_live(&replacement)
        .expect("compact live context");
    assert!(
        coordinator
            .runtime
            .projection()
            .nodes
            .iter()
            .any(|node| { node.status == spine_core::NodeStatus::Compacted })
    );
}

#[test]
fn spine_execution_fact_commits_only_after_lifecycle_success() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");
    coordinator
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage fact");
    coordinator
        .observe_response_items(&open_source())
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
        .observe_response_items(&[message("user", "question")])
        .expect("observe prompt source");
    let attempt = begin_sampling_for_test(&mut coordinator).expect("begin");
    coordinator
        .register_execution("open-call")
        .expect("register execution");

    let error = install_sampling_for_test(&mut coordinator, attempt)
        .expect_err("pending execution must reject seal");

    assert!(matches!(
        error,
        super::coordinator::CoordinatorError::Planner(spine_core::PlannerError::PendingExecutions(
            1
        ))
    ));
}

#[test]
fn spine_sampling_rejects_success_without_staged_fact() {
    let mut coordinator = coordinator();
    coordinator
        .observe_response_items(&[message("user", "question")])
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
        super::coordinator::CoordinatorError::Planner(spine_core::PlannerError::Sampling(
            spine_core::SamplingError::TransactionAborted
        ))
    ));
}
