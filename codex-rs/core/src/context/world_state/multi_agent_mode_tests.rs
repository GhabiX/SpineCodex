use super::super::test_support::render_section_cases;
use super::*;
use crate::context::world_state::WorldState;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;
use spine_core::host::Feature;
use spine_core::host::SpawnPromptMode;
use spine_core::host::SpineConfig;

fn state(mode: Option<MultiAgentMode>) -> MultiAgentModeState {
    MultiAgentModeState::new(mode)
}

fn spawn_config() -> SpineConfig {
    SpineConfig::v1()
        .with_features([Feature::Jit, Feature::Spawn])
        .expect("enable typed Spine Spawn test config")
}

fn render(
    state: &MultiAgentModeState,
    previous: PreviousSectionState<'_, MultiAgentModeState>,
) -> String {
    state
        .render_diff(previous)
        .expect("multi-agent mode should render")
        .render()
}

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let inactive = state(/*mode*/ None);
    let explicit = state(Some(MultiAgentMode::ExplicitRequestOnly));
    let proactive = state(Some(MultiAgentMode::Proactive));
    let custom = state(Some(MultiAgentMode::Custom(
        "use a custom policy".to_string(),
    )));
    let empty = state(Some(MultiAgentMode::Custom(String::new())));

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&inactive)),
        (Absent, Known(&explicit)),
        (Known(&explicit), Known(&explicit)),
        (Known(&explicit), Known(&proactive)),
        (Known(&proactive), Known(&inactive)),
        (Known(&explicit), Known(&inactive)),
        (Known(&explicit), Known(&custom)),
        (Known(&custom), Known(&empty)),
        (Unknown, Known(&explicit)),
        (Unknown, Known(&inactive)),
    ]));
}

#[test]
fn persisted_mode_is_restored_only_when_missing_from_history() {
    let state = state(Some(MultiAgentMode::ExplicitRequestOnly));
    let retained: ResponseItem = ContextualUserFragment::into(
        MultiAgentModeInstructions::from_mode(MultiAgentMode::ExplicitRequestOnly)
            .expect("explicit mode should render"),
    );
    let mut world_state = WorldState::default();
    world_state.add_section(state);
    let snapshot = world_state.snapshot();

    assert_eq!(
        world_state
            .render_history_diff(/*previous*/ None, std::slice::from_ref(&retained))
            .len(),
        1,
    );
    assert_eq!(
        world_state.render_history_diff(Some(&snapshot), &[]).len(),
        1
    );
    assert!(
        world_state
            .render_history_diff(Some(&snapshot), &[retained])
            .is_empty()
    );
}

/// Active mode instructions must follow a newly migrated multi-agent usage hint.
#[test]
fn unchanged_mode_is_reemitted_after_usage_hint_migration() {
    let previous = state(Some(MultiAgentMode::Proactive));
    let current = MultiAgentModeState::new(Some(MultiAgentMode::Proactive)).with_usage_hint(
        &MultiAgentUsageHintState::new("Current usage instructions."),
    );

    let instructions = current
        .render_diff(PreviousSectionState::Known(&previous))
        .expect("unchanged mode should follow migrated usage instructions");

    assert_eq!(
        instructions.render(),
        MultiAgentModeInstructions::from_mode(MultiAgentMode::Proactive)
            .expect("proactive mode should render")
            .render()
    );
}

#[test]
fn custom_mode_is_bounded_before_snapshot_and_rendering() {
    let state = state(Some(MultiAgentMode::Custom("custom mode ".repeat(1_000))));
    let Some(MultiAgentMode::Custom(snapshot_mode)) = state.snapshot().mode else {
        panic!("expected custom multi-agent mode")
    };
    assert!(approx_token_count(&snapshot_mode) < 1_000);

    let rendered = state
        .render_diff(PreviousSectionState::Absent)
        .expect("custom mode should render")
        .render();
    assert!(approx_token_count(&rendered) < 1_000);
}

#[test]
fn feature_off_modes_remain_byte_identical_to_native_fragments() {
    let config = SpineConfig::v1();
    for mode in [
        MultiAgentMode::ExplicitRequestOnly,
        MultiAgentMode::Proactive,
    ] {
        let state = state(Some(mode.clone())).with_spine_config(&config);
        let expected = MultiAgentModeInstructions::from_mode(mode)
            .expect("native mode should render")
            .render();
        assert_eq!(render(&state, PreviousSectionState::Absent), expected);
    }
}

#[test]
fn enabled_spawn_uses_typed_explicit_and_proactive_prompts() {
    let config = spawn_config();
    for (mode, prompt_mode) in [
        (
            MultiAgentMode::ExplicitRequestOnly,
            SpawnPromptMode::ExplicitRequestOnly,
        ),
        (MultiAgentMode::Proactive, SpawnPromptMode::Proactive),
    ] {
        let state = state(Some(mode)).with_spine_config(&config);
        let body = config
            .spawn_prompt(prompt_mode)
            .expect("enabled Spawn config should expose its mode prompt");
        assert_eq!(
            render(&state, PreviousSectionState::Absent),
            format!("{MULTI_AGENT_MODE_OPEN_TAG}{body}{MULTI_AGENT_MODE_CLOSE_TAG}")
        );
        let item = state
            .render_diff(PreviousSectionState::Absent)
            .expect("configured mode should render")
            .into_boxed_response_item();
        crate::context::validate_spine_model_item(&item)
            .expect("configured mode prompt must satisfy the Spine provider value bound");
    }
}

#[test]
fn custom_mode_remains_native_when_spawn_is_enabled() {
    let mode = MultiAgentMode::Custom("use only the requested collaboration role".to_string());
    let state = state(Some(mode.clone())).with_spine_config(&spawn_config());
    assert_eq!(
        render(&state, PreviousSectionState::Absent),
        MultiAgentModeInstructions::from_mode(mode)
            .expect("custom mode should render")
            .render()
    );
}

#[test]
fn configured_prompt_identity_and_proactive_reset_are_persisted() {
    let config = spawn_config();
    let native = state(Some(MultiAgentMode::ExplicitRequestOnly));
    let configured = state(Some(MultiAgentMode::ExplicitRequestOnly)).with_spine_config(&config);
    let persisted = serde_json::to_value(configured.snapshot()).expect("serialize mode snapshot");
    assert_eq!(persisted.get("configured_prompt"), None);
    assert!(persisted.get("configured_prompt_hash").is_some());
    assert!(
        configured
            .render_diff(PreviousSectionState::Known(&native))
            .is_some(),
        "enabling Spawn must replace a same-mode native policy"
    );

    let proactive = state(Some(MultiAgentMode::Proactive)).with_spine_config(&config);
    let inactive = state(/*mode*/ None).with_spine_config(&config);
    let expected = config
        .spawn_prompt(SpawnPromptMode::ExplicitRequestOnly)
        .expect("enabled Spawn config should expose explicit reset prompt");
    assert_eq!(
        render(&inactive, PreviousSectionState::Known(&proactive)),
        format!("{MULTI_AGENT_MODE_OPEN_TAG}{expected}{MULTI_AGENT_MODE_CLOSE_TAG}")
    );
}

#[test]
fn configured_mode_reconciles_retained_history_and_legacy_snapshots() {
    let config = spawn_config();
    let state = state(Some(MultiAgentMode::ExplicitRequestOnly)).with_spine_config(&config);
    let retained = state
        .render_diff(PreviousSectionState::Absent)
        .expect("configured mode should render")
        .into_boxed_response_item();
    let mut world_state = WorldState::default();
    world_state.add_section(state.clone());
    let snapshot = world_state.snapshot();
    assert!(
        world_state
            .render_history_diff(Some(&snapshot), std::slice::from_ref(&retained))
            .is_empty()
    );
    assert_eq!(
        world_state.render_history_diff(Some(&snapshot), &[]).len(),
        1
    );

    let legacy_snapshot: MultiAgentModeState = serde_json::from_value(serde_json::json!({
        "mode": "explicitRequestOnly"
    }))
    .expect("old snapshots without configured_prompt should deserialize");
    assert!(
        state
            .render_diff(PreviousSectionState::Known(&legacy_snapshot))
            .is_some(),
        "legacy snapshots must migrate by emitting the configured prompt once"
    );
}
