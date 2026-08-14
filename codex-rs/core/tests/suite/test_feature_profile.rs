use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::test_codex::test_codex;

#[tokio::test]
async fn native_codex_test_profile_disables_spine_features_and_model_surfaces() -> Result<()> {
    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-native-profile", "done"),
            ev_completed("resp-native-profile"),
        ]),
    )
    .await;
    let test = test_codex().build(&server).await?;

    for feature in [Feature::SpineJit, Feature::SpineTrim, Feature::SpineSpawn] {
        assert!(!test.config.features.enabled(feature));
    }
    assert!(test.config.spine_config.is_feature_off());
    assert!(test.config.spine_tools.definitions().is_empty());

    test.submit_turn("native profile request").await?;
    let request = response_mock.single_request();
    let request_json = request.body_json();
    let request_text = request_json.to_string();
    assert!(!request_text.contains("<spine_instruction>"));
    assert!(!request_text.contains("<spine_view>"));
    assert!(!request_text.contains("\"spine\""));
    Ok(())
}

#[tokio::test]
async fn spine_test_profile_rebuilds_tools_from_typed_feature_config() -> Result<()> {
    let server = start_mock_server().await;
    let test = spine_test_codex()
        .with_spine_trim()
        .with_spine_spawn()
        .build(&server)
        .await?;

    for feature in [Feature::SpineJit, Feature::SpineTrim, Feature::SpineSpawn] {
        assert!(test.config.features.enabled(feature));
    }
    assert!(!test.config.spine_config.is_feature_off());
    assert!(!test.config.spine_tools.definitions().is_empty());
    Ok(())
}
