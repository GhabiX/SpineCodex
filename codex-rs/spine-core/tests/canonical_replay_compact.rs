use spine_core::host::CanonicalReplay;
use spine_core::host::ContextEpoch;
use spine_core::host::ContextItem;
use spine_core::host::Feature;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::RecordDigest;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpineChar;
use spine_core::host::SpineCompactBarrierV1;
use spine_core::host::SpineConfig;
use spine_core::host::ThreadNamespace;

const BEFORE: &str = "BEFORE_COMPACT_UNIQUE_TOKEN_9f3a";
const AFTER: &str = "AFTER_COMPACT_UNIQUE_TOKEN_c21b";
const PENDING: &str = "PENDING_AFTER_COMPACT_UNIQUE_TOKEN_e70d";

fn message(boundary: u64, role: MessageRole, content: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role,
        content: content.to_string(),
    })
}

fn message_texts(items: &[ContextItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            ContextItem::Message { message, .. } => Some(message.content.clone()),
            _ => None,
        })
        .collect()
}

fn run_turn(
    runtime: &mut SamplingRuntime,
    inputs: &mut Vec<ReplayInput>,
    user: SpineChar,
    assistant: SpineChar,
    prompt_tag: &[u8],
) {
    runtime
        .observe_source([user.clone()])
        .expect("observe user");
    inputs.push(ReplayInput::Source(user));
    let sampling = runtime.begin_sampling().expect("begin sampling");
    let started = runtime
        .sampling_started_record(&sampling, RecordDigest::digest(prompt_tag))
        .expect("sampling started");
    inputs.push(ReplayInput::Archive(started));
    runtime
        .observe_source([assistant.clone()])
        .expect("observe assistant");
    inputs.push(ReplayInput::Source(assistant));
    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(sampling, SamplingTerminal::Completed)
        .expect("prepare sampling")
    else {
        panic!("completed sampling must prepare");
    };
    let committed = SamplingArchiveRecord::SamplingCommit(prepared.durable_record().clone());
    inputs.push(ReplayInput::Archive(committed));
    runtime
        .install_prepared(prepared)
        .expect("install sampling");
}

fn compact(
    runtime: &mut SamplingRuntime,
    inputs: &mut Vec<ReplayInput>,
    thread: ThreadNamespace,
    boundary: u64,
    replacement: u64,
) {
    let barrier = SpineCompactBarrierV1::new(
        thread,
        runtime.epoch(),
        runtime.epoch().checked_next().expect("next epoch"),
        RawBoundary(boundary),
        vec![RawBoundary(replacement)],
    )
    .expect("compact barrier");
    runtime.compact(barrier.clone()).expect("live compact");
    inputs.push(ReplayInput::Compact(barrier));
}

fn prepare_replay(
    thread: ThreadNamespace,
    config: SpineConfig,
    inputs: Vec<ReplayInput>,
) -> (Vec<String>, Vec<String>, bool) {
    let prepared = CanonicalReplay::new(thread)
        .expect("replay")
        .with_runtime_config(config)
        .expect("replay config")
        .prepare(inputs)
        .expect("canonical replay");
    (
        message_texts(&prepared.projection.visible_context),
        message_texts(&prepared.live_context),
        prepared.live_plan.is_some(),
    )
}

#[test]
fn compact_only_resume_installs_current_projection() {
    let thread = ThreadNamespace::parse("compact-only-resume").expect("valid thread");
    let config = SpineConfig::v1()
        .with_features([Feature::Jit])
        .expect("valid config");
    let mut runtime = SamplingRuntime::new(thread.clone(), ContextEpoch::new(0), config.clone())
        .expect("valid runtime");
    let mut inputs = Vec::new();

    run_turn(
        &mut runtime,
        &mut inputs,
        message(1, MessageRole::User, BEFORE),
        message(2, MessageRole::Assistant, "ack-before"),
        b"before-prompt",
    );
    compact(&mut runtime, &mut inputs, thread.clone(), 2, 3);

    let (projection_texts, live_context_texts, has_plan) = prepare_replay(thread, config, inputs);

    assert!(has_plan, "compact-only resume must produce a plan");
    assert_eq!(live_context_texts, projection_texts);
    assert!(
        !live_context_texts.iter().any(|text| text.contains(BEFORE)),
        "opaque compact replacement must drop pre-compact messages"
    );
}

#[test]
fn compact_then_commit_resume_installs_post_compact_messages() {
    let thread = ThreadNamespace::parse("compact-then-commit-resume").expect("valid thread");
    let config = SpineConfig::v1()
        .with_features([Feature::Jit])
        .expect("valid config");
    let mut runtime = SamplingRuntime::new(thread.clone(), ContextEpoch::new(0), config.clone())
        .expect("valid runtime");
    let mut inputs = Vec::new();

    run_turn(
        &mut runtime,
        &mut inputs,
        message(1, MessageRole::User, BEFORE),
        message(2, MessageRole::Assistant, "ack-before"),
        b"before-prompt",
    );
    compact(&mut runtime, &mut inputs, thread.clone(), 2, 3);
    run_turn(
        &mut runtime,
        &mut inputs,
        message(4, MessageRole::User, AFTER),
        message(5, MessageRole::Assistant, "ack-after"),
        b"after-prompt",
    );

    let live_projection_texts = message_texts(&runtime.projection().visible_context);
    let (_, live_context_texts, _) = prepare_replay(thread, config, inputs);

    assert!(
        live_projection_texts
            .iter()
            .any(|text| text.contains(AFTER)),
        "live runtime projection missing {AFTER}"
    );
    assert!(
        live_context_texts.iter().any(|text| text.contains(AFTER)),
        "resume live_context omitted post-compact user message {AFTER}"
    );
    assert!(
        live_context_texts
            .iter()
            .any(|text| text.contains("ack-after")),
        "resume live_context omitted post-compact assistant message"
    );
    assert!(
        !live_context_texts.iter().any(|text| text.contains(BEFORE)),
        "resume live_context kept pre-compact message {BEFORE}"
    );
}

#[test]
fn compact_then_pending_source_resume_previews_trailing_user_message() {
    let thread = ThreadNamespace::parse("compact-then-pending-resume").expect("valid thread");
    let config = SpineConfig::v1()
        .with_features([Feature::Jit])
        .expect("valid config");
    let mut runtime = SamplingRuntime::new(thread.clone(), ContextEpoch::new(0), config.clone())
        .expect("valid runtime");
    let mut inputs = Vec::new();

    run_turn(
        &mut runtime,
        &mut inputs,
        message(1, MessageRole::User, BEFORE),
        message(2, MessageRole::Assistant, "ack-before"),
        b"before-prompt",
    );
    compact(&mut runtime, &mut inputs, thread.clone(), 2, 3);

    let pending = message(4, MessageRole::User, PENDING);
    runtime
        .observe_source([pending.clone()])
        .expect("observe pending user");
    inputs.push(ReplayInput::Source(pending));

    let (_, live_context_texts, _) = prepare_replay(thread, config, inputs);
    assert!(
        live_context_texts.iter().any(|text| text.contains(PENDING)),
        "trailing source after compact must appear in resume preview"
    );
}
