//! Check configuration changes and feature toggles are represented without stale overrides.

use super::*;
use codex_models_manager::model_info::model_info_from_slug;
use codex_models_manager::model_info::with_config_overrides;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn configured_and_effective_context_are_distinct_and_unset_is_explicit() {
    let mut config = crate::config::test_config().await;
    config.model_context_window = Some(872_000);
    let mut model = model_info_from_slug("test-model");
    model.context_window = Some(272_000);
    model.max_context_window = Some(400_000);
    model.effective_context_window_percent = 95;
    let resolved = with_config_overrides(model.clone(), &config.to_models_manager_config());
    let tags = usage_tags(&config, &config.features, &resolved, Some("priority"));
    assert_eq!(
        [
            tags["model_context_window"].as_str(),
            tags["model_context_window_override"].as_str(),
            tags["model_context_window_effective"].as_str(),
            tags["service_tier"].as_str(),
        ],
        ["872000", "true", "380000", "priority"]
    );

    config.model_context_window = None;
    let mut updated = tags;
    updated.extend(usage_tags(
        &config,
        &config.features,
        &model,
        /*service_tier*/ None,
    ));
    assert_eq!(
        [
            updated["model_context_window"].as_str(),
            updated["model_context_window_override"].as_str(),
            updated["model_context_window_effective"].as_str(),
            updated["service_tier"].as_str(),
        ],
        ["unset", "false", "258400", "unset"]
    );
}

#[tokio::test]
async fn reported_features_have_boolean_tags_that_change_with_their_state() {
    let config = crate::config::test_config().await;
    let model = model_info_from_slug("test-model");
    let mut features = Features::with_defaults();
    for spec in FEATURES {
        features.disable(spec.id);
    }
    let disabled = usage_tags(&config, &features, &model, /*service_tier*/ None);
    let native_features: Vec<_> = FEATURES
        .iter()
        .filter(|spec| {
            !matches!(
                spec.id,
                Feature::SpineJit
                    | Feature::SpineStatus
                    | Feature::SpineSpawn
                    | Feature::SpinetreeMemoryProjection
            )
        })
        .collect();
    for spec in &native_features {
        features.enable(spec.id);
    }
    let enabled = usage_tags(&config, &features, &model, /*service_tier*/ None);
    assert_feature_matrix(&disabled, &enabled, &native_features);
}

#[tokio::test]
async fn spine_reported_features_have_boolean_tags_that_change_with_their_state() {
    let config = crate::config::test_config().await;
    let model = model_info_from_slug("test-model");
    let mut features = Features::with_defaults();
    let spine_features = [
        Feature::SpineJit,
        Feature::SpineStatus,
        Feature::SpineSpawn,
        Feature::SpinetreeMemoryProjection,
    ];
    for spec in FEATURES {
        features.disable(spec.id);
    }
    let disabled = usage_tags(&config, &features, &model, /*service_tier*/ None);
    features.enable(Feature::SpineJit);
    features.enable(Feature::SpineStatus);
    features.enable(Feature::SpineSpawn);
    features.enable(Feature::SpinetreeMemoryProjection);
    let enabled = usage_tags(&config, &features, &model, /*service_tier*/ None);
    let enabled_specs: Vec<_> = FEATURES
        .iter()
        .filter(|spec| spine_features.contains(&spec.id))
        .collect();
    assert_feature_matrix(&disabled, &enabled, &enabled_specs);
}

fn assert_feature_matrix(
    disabled: &BTreeMap<String, String>,
    enabled: &BTreeMap<String, String>,
    enabled_specs: &[&codex_features::FeatureSpec],
) {
    let expected: BTreeMap<_, _> = FEATURES
        .iter()
        .filter(|spec| !matches!(spec.id, Feature::Collab | Feature::SpawnCsv))
        .map(|spec| {
            (
                format!("feature.{}", spec.key),
                if enabled_specs.iter().any(|enabled| enabled.id == spec.id) {
                    "true".to_string()
                } else {
                    "false".to_string()
                },
            )
        })
        .collect();
    let actual = |tags: &BTreeMap<String, String>| -> BTreeMap<String, String> {
        tags.iter()
            .filter(|(key, _)| key.starts_with("feature."))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    };
    assert_eq!(actual(enabled), expected);
    assert_eq!(
        actual(disabled),
        expected
            .into_keys()
            .map(|key| (key, "false".to_string()))
            .collect()
    );
}
