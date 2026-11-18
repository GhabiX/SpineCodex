use codex_features::Feature;
use codex_features::Features;

#[test]
fn explicit_fixture_enables_obey_process_spine_gate() {
    let spine = [
        Feature::SpineJit,
        Feature::SpineStatus,
        Feature::SpineSpawn,
        Feature::SpinetreeMemoryProjection,
    ];
    let mut features = Features::with_defaults();
    for feature in spine {
        features.enable(feature);
    }
    features.enable(Feature::WebSearchRequest);
    let forced_off = cfg!(debug_assertions)
        && std::env::var_os("CODEX_TEST_SPINE_OFF").as_deref() == Some(std::ffi::OsStr::new("1"));
    assert_eq!(
        spine.map(|feature| features.enabled(feature)),
        [!forced_off; 4]
    );
    assert!(features.enabled(Feature::WebSearchRequest));
}
