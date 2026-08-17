use crate::config::Config;
use codex_features::Feature as CodexFeature;

#[derive(Clone)]
pub(crate) struct SpineSessionConfig {
    sdk: spine_core::host::SpineConfig,
}

impl SpineSessionConfig {
    pub(crate) fn from_config(config: &Config) -> Self {
        let jit_enabled = config.features.enabled(CodexFeature::SpineJit);
        let trim_enabled = config.features.enabled(CodexFeature::SpineTrim);
        let spawn_enabled = config.features.enabled(CodexFeature::SpineSpawn);
        let mut features = Vec::new();
        if jit_enabled {
            features.push(spine_core::host::Feature::Jit);
        }
        if trim_enabled {
            features.push(spine_core::host::Feature::Trim);
        }
        if spawn_enabled {
            features.push(spine_core::host::Feature::Spawn);
        }
        let sdk = config
            .spine_config
            .clone()
            .with_features(features)
            .expect("validated session Spine features must remain valid");
        Self { sdk }
    }

    pub(crate) fn disabled() -> Self {
        Self {
            sdk: spine_core::host::SpineConfig::v1(),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.jit_enabled() || self.sdk.is_enabled(spine_core::host::Feature::Trim)
    }

    pub(crate) fn jit_enabled(&self) -> bool {
        self.sdk.is_enabled(spine_core::host::Feature::Jit)
    }

    pub(crate) fn sdk(&self) -> &spine_core::host::SpineConfig {
        &self.sdk
    }
}
