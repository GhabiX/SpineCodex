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
use wiremock::ResponseTemplate;

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

fn window_generation(request: &responses::ResponsesRequest) -> (String, u64) {
    let window_id = request
        .header("x-codex-window-id")
        .expect("request must include x-codex-window-id");
    let (thread_id, generation) = window_id
        .rsplit_once(':')
        .expect("window id must contain a generation");
    let generation = generation
        .parse::<u64>()
        .expect("window generation must be numeric");
    (thread_id.to_string(), generation)
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
            .contains("ENCRYPTED_SPINE_COMPACTION_SUMMARY")
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
            .contains("ENCRYPTED_SPINE_V2_COMPACTION_SUMMARY")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_v1_retry_installs_base_window_and_spine_projection_atomically() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        spine_test_codex()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(|config| {
                let _ = config.features.disable(Feature::RemoteCompactionV2);
                config.model_provider.request_max_retries = Some(1);
                config.model_provider.stream_max_retries = Some(0);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("v1-atomic-before", "BEFORE_V1_ATOMIC"),
                responses::ev_completed("v1-atomic-before-response"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("v1-atomic-after", "AFTER_V1_ATOMIC"),
                responses::ev_completed("v1-atomic-after-response"),
            ]),
        ],
    )
    .await;
    let compact_mock = responses::mount_compact_response_sequence(
        harness.server(),
        vec![
            ResponseTemplate::new(503).set_body_string("retry compact without installing"),
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(serde_json::json!({
                    "output": [{
                        "type": "compaction",
                        "encrypted_content": "V1_ATOMIC_SUCCESS_SUMMARY"
                    }]
                })),
        ],
    )
    .await;

    submit_text(&codex, "before v1 atomic compact").await?;
    codex.submit(Op::Compact).await?;
    let spine_update = wait_for_spine_tree_update(&codex).await;
    assert_eq!(spine_update.active_node_id, "2");
    wait_for_turn_complete(&codex).await;
    submit_text(&codex, "after v1 atomic compact").await?;

    let response_requests = responses_mock.requests();
    let initial = response_requests.first().expect("initial request");
    let followup = response_requests.last().expect("follow-up request");
    let compact_requests = compact_mock.requests();
    assert_eq!(compact_requests.len(), 2);
    let (thread_id, initial_generation) = window_generation(initial);
    assert_eq!(initial_generation, 0);
    for compact_request in &compact_requests {
        assert_eq!(
            window_generation(compact_request),
            (thread_id.clone(), initial_generation),
            "failed and retried compact attempts must stay on the old window"
        );
    }
    assert_eq!(window_generation(followup), (thread_id, 1));
    assert_followup_preserves_spine_projection(followup);
    let followup_body = followup.body_json().to_string();
    assert!(followup_body.contains("V1_ATOMIC_SUCCESS_SUMMARY"));
    assert!(!followup_body.contains("retry compact without installing"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_v2_retry_installs_base_window_and_spine_projection_atomically() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        spine_test_codex()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(|config| {
                let _ = config.features.enable(Feature::RemoteCompactionV2);
                config.model_provider.request_max_retries = Some(0);
                config.model_provider.stream_max_retries = Some(1);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let response_mock = responses::mount_response_sequence(
        harness.server(),
        vec![
            responses::sse_response(responses::sse(vec![
                responses::ev_assistant_message("v2-atomic-before", "BEFORE_V2_ATOMIC"),
                responses::ev_completed("v2-atomic-before-response"),
            ])),
            responses::sse_response(responses::sse(vec![serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "V2_ATOMIC_FAILED_SUMMARY"
                }
            })])),
            responses::sse_response(responses::sse(vec![
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "compaction",
                        "encrypted_content": "V2_ATOMIC_SUCCESS_SUMMARY"
                    }
                }),
                responses::ev_completed("v2-atomic-compact-response"),
            ])),
            responses::sse_response(responses::sse(vec![
                responses::ev_assistant_message("v2-atomic-after", "AFTER_V2_ATOMIC"),
                responses::ev_completed("v2-atomic-after-response"),
            ])),
        ],
    )
    .await;

    submit_text(&codex, "before v2 atomic compact").await?;
    codex.submit(Op::Compact).await?;
    let spine_update = wait_for_spine_tree_update(&codex).await;
    assert_eq!(spine_update.active_node_id, "2");
    wait_for_turn_complete(&codex).await;
    submit_text(&codex, "after v2 atomic compact").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 4);
    let initial = &requests[0];
    let failed_compact = &requests[1];
    let successful_compact = &requests[2];
    let followup = &requests[3];
    let (thread_id, initial_generation) = window_generation(initial);
    assert_eq!(initial_generation, 0);
    assert_eq!(
        window_generation(failed_compact),
        (thread_id.clone(), initial_generation)
    );
    assert_eq!(
        window_generation(successful_compact),
        (thread_id.clone(), initial_generation)
    );
    assert_eq!(window_generation(followup), (thread_id, 1));
    assert_followup_preserves_spine_projection(followup);
    let followup_body = followup.body_json().to_string();
    assert!(followup_body.contains("V2_ATOMIC_SUCCESS_SUMMARY"));
    assert!(!followup_body.contains("V2_ATOMIC_FAILED_SUMMARY"));
    Ok(())
}
