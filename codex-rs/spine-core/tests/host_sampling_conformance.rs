use spine_core::host::CanonicalReplay;
use spine_core::host::ContextEpoch;
use spine_core::host::Feature;
use spine_core::host::Message;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpineChar;
use spine_core::host::SpineConfig;
use spine_core::host::ThreadNamespace;

#[test]
fn native_items_are_opaque_to_the_sampling_host_contract() {
    let thread = ThreadNamespace::parse("host-sampling").expect("valid thread");
    let mut runtime = SamplingRuntime::new(thread, ContextEpoch::new(0), SpineConfig::v1())
        .expect("valid runtime");
    runtime
        .observe_source([
            SpineChar::Opaque {
                boundary: RawBoundary(1),
            },
            SpineChar::Message(Message {
                boundary: RawBoundary(2),
                role: MessageRole::User,
                content: "interrupting user message".to_string(),
            }),
            SpineChar::Opaque {
                boundary: RawBoundary(3),
            },
        ])
        .expect("native tool items must not create a parser group");
    assert_eq!(runtime.source_snapshot().cells().len(), 3);
}

#[test]
fn native_items_survive_sampling_and_canonical_replay_as_opaque_source() {
    let thread = ThreadNamespace::parse("host-opaque-replay").expect("valid thread");
    let config = SpineConfig::v1()
        .with_features([Feature::Jit])
        .expect("valid config");
    let user = SpineChar::Message(Message {
        boundary: RawBoundary(1),
        role: MessageRole::User,
        content: "produce native output".to_string(),
    });
    let output = SpineChar::Opaque {
        boundary: RawBoundary(2),
    };
    let mut runtime = SamplingRuntime::new(thread.clone(), ContextEpoch::new(0), config.clone())
        .expect("valid runtime");
    runtime.observe_source([user.clone()]).expect("user source");
    let sampling = runtime.begin_sampling().expect("begin sampling");
    let started = runtime
        .sampling_started_record(&sampling)
        .expect("sampling started");
    runtime
        .observe_source([output.clone()])
        .expect("native source");
    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(sampling, SamplingTerminal::Completed)
        .expect("prepare sampling")
    else {
        panic!("completed sampling must prepare");
    };
    let committed = SamplingArchiveRecord::SamplingCommit(prepared.durable_record().clone());
    runtime
        .install_prepared(prepared)
        .expect("install sampling");
    let replayed = CanonicalReplay::new(thread)
        .expect("replay")
        .with_runtime_config(config)
        .expect("replay config")
        .prepare([
            ReplayInput::Source(user.clone()),
            ReplayInput::Archive(started),
            ReplayInput::Source(output.clone()),
            ReplayInput::Archive(committed),
        ])
        .expect("canonical replay")
        .into_runtime();
    let cells = replayed.source_snapshot();
    assert_eq!(cells.cells().len(), 2);
    assert_eq!(cells.cells()[0].character(), user);
    assert_eq!(cells.cells()[1].character(), output);
}

#[test]
fn incremental_canonical_replay_matches_prepare() {
    let thread = ThreadNamespace::parse("host-opaque-replay-incremental").expect("valid thread");
    let config = SpineConfig::v1()
        .with_features([Feature::Jit])
        .expect("valid config");
    let user = SpineChar::Message(Message {
        boundary: RawBoundary(1),
        role: MessageRole::User,
        content: "produce native output".to_string(),
    });
    let output = SpineChar::Opaque {
        boundary: RawBoundary(2),
    };
    let mut runtime = SamplingRuntime::new(thread.clone(), ContextEpoch::new(0), config.clone())
        .expect("valid runtime");
    runtime.observe_source([user.clone()]).expect("user source");
    let sampling = runtime.begin_sampling().expect("begin sampling");
    let started = runtime
        .sampling_started_record(&sampling)
        .expect("sampling started");
    runtime
        .observe_source([output.clone()])
        .expect("native source");
    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(sampling, SamplingTerminal::Completed)
        .expect("prepare sampling")
    else {
        panic!("completed sampling must prepare");
    };
    let committed = SamplingArchiveRecord::SamplingCommit(prepared.durable_record().clone());
    runtime
        .install_prepared(prepared)
        .expect("install sampling");
    let one_shot = CanonicalReplay::new(thread.clone())
        .expect("replay")
        .with_runtime_config(config.clone())
        .expect("replay config")
        .prepare([
            ReplayInput::Source(user.clone()),
            ReplayInput::Archive(started.clone()),
            ReplayInput::Source(output.clone()),
            ReplayInput::Archive(committed.clone()),
        ])
        .expect("one-shot replay");
    let mut builder = CanonicalReplay::new(thread)
        .expect("replay")
        .with_runtime_config(config)
        .expect("replay config")
        .builder()
        .expect("builder");
    builder
        .apply([
            ReplayInput::Source(user.clone()),
            ReplayInput::Archive(started),
        ])
        .expect("apply prefix");
    builder
        .apply([
            ReplayInput::Source(output.clone()),
            ReplayInput::Archive(committed),
        ])
        .expect("apply suffix");
    let incremental = builder.finish().expect("finish replay");
    assert_eq!(incremental.projection, one_shot.projection);
    assert_eq!(incremental.live_plan, one_shot.live_plan);
    assert_eq!(incremental.applied_commits, one_shot.applied_commits);
    let cells = incremental.into_runtime().source_snapshot();
    assert_eq!(cells.cells().len(), 2);
    assert_eq!(cells.cells()[0].character(), user);
    assert_eq!(cells.cells()[1].character(), output);
}
