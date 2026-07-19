use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;

#[tokio::test]
async fn native_codex_test_profile_disables_spine_features() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    for feature in [Feature::SpineJit, Feature::SpineTrim, Feature::SpineSpawn] {
        assert!(
            !test.config.features.enabled(feature),
            "native Codex test profile unexpectedly enabled {}",
            feature.key()
        );
    }

    Ok(())
}
