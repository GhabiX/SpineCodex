use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::wait_for_event_match;
use core_test_support::wait_for_event_with_timeout;
use tokio::time::Duration;

const REMOTE_COMPACT_TURN_COMPLETE_TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_turn_complete(codex: &codex_core::CodexThread) {
    wait_for_event_with_timeout(
        codex,
        |event| matches!(event, EventMsg::TurnComplete(_)),
        REMOTE_COMPACT_TURN_COMPLETE_TIMEOUT,
    )
    .await;
}

async fn submit_text(codex: &codex_core::CodexThread, text: &str) -> Result<()> {
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_turn_complete(codex).await;
    Ok(())
}

async fn wait_for_spine_tree_update(
    codex: &codex_core::CodexThread,
) -> codex_protocol::protocol::SpineTreeUpdateEvent {
    wait_for_event_match(codex, |event| match event {
        EventMsg::SpineTreeUpdate(snapshot) => Some(snapshot.clone()),
        _ => None,
    })
    .await
}

fn assert_followup_preserves_spine_projection(request: &responses::ResponsesRequest) {
    let body = request.body_json().to_string();
    assert!(
        body.contains("[U"),
        "expected follow-up request to contain rollout-derived user anchors: {body}"
    );
    assert!(
        body.contains("<spine_view>"),
        "expected follow-up request to carry Spine instructions: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_installs_spine_root_compact_for_followups() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        spine_test_codex()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(|config| {
                let _ = config.features.disable(Feature::RemoteCompactionV2);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", "FIRST_REMOTE_REPLY"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({
            "output": [{
                "type": "compaction",
                "encrypted_content": "ENCRYPTED_SPINE_COMPACTION_SUMMARY"
            }]
        }),
    )
    .await;

    submit_text(&codex, "before Spine compact").await?;
    codex.submit(Op::Compact).await?;
    let spine_update = wait_for_spine_tree_update(&codex).await;
    assert_eq!(spine_update.active_node_id, "2");
    wait_for_turn_complete(&codex).await;
    submit_text(&codex, "after Spine compact").await?;

    assert_eq!(
        compact_mock.single_request().path(),
        "/v1/responses/compact"
    );
    let requests = responses_mock.requests();
    let followup = requests.last().expect("follow-up response request");
    assert_followup_preserves_spine_projection(followup);
    assert!(
        followup
            .body_json()
            .to_string()
            .contains("ENCRYPTED_SPINE_COMPACTION_SUMMARY"),
        "expected native compact replacement to remain in the projected follow-up"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_v2_installs_spine_root_compact_for_followups() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        spine_test_codex()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(|config| {
                let _ = config.features.enable(Feature::RemoteCompactionV2);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", "FIRST_REMOTE_REPLY"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "compaction",
                        "encrypted_content": "ENCRYPTED_SPINE_V2_COMPACTION_SUMMARY"
                    }
                }),
                responses::ev_completed("resp-compact"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    submit_text(&codex, "before Spine v2 compact").await?;
    codex.submit(Op::Compact).await?;
    let spine_update = wait_for_spine_tree_update(&codex).await;
    assert_eq!(spine_update.active_node_id, "2");
    wait_for_turn_complete(&codex).await;
    submit_text(&codex, "after Spine v2 compact").await?;

    let requests = responses_mock.requests();
    let followup = requests.last().expect("follow-up response request");
    assert_followup_preserves_spine_projection(followup);
    assert!(
        followup
            .body_json()
            .to_string()
            .contains("ENCRYPTED_SPINE_V2_COMPACTION_SUMMARY"),
        "expected native v2 compact replacement to remain in the projected follow-up"
    );
    Ok(())
}
