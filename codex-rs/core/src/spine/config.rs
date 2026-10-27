use crate::config::ManagedFeatures;
use codex_features::Feature as CodexFeature;
use codex_utils_absolute_path::AbsolutePathBuf;
use spine_core::host::SpineConfig;
use spine_core::host::SpineConfigLoader;
use spine_core::host::ToolCatalog;
use std::io;
use std::path::Path;

/// SDK configuration is selected at session initialization, after resume history is available.
#[derive(Clone, Debug, PartialEq)]
pub struct SpineConfiguration {
    state: ConfigurationState,
}

#[derive(Clone, Debug, PartialEq)]
enum ConfigurationState {
    Sources(SpineConfigLoader),
    Resolved(Box<ResolvedConfiguration>),
}

#[derive(Clone, Debug, PartialEq)]
struct ResolvedConfiguration {
    sdk: SpineConfig,
    tools: ToolCatalog,
}

impl SpineConfiguration {
    pub(crate) fn pending(
        path: Option<&AbsolutePathBuf>,
        working_directory: &Path,
        home_directory: Option<&Path>,
        project_config_trusted: bool,
    ) -> Self {
        Self {
            state: ConfigurationState::Sources(loader(
                path,
                working_directory,
                home_directory,
                project_config_trusted,
            )),
        }
    }

    /// Creates an already resolved SDK configuration for an embedding host.
    pub fn from_sdk(sdk: SpineConfig) -> anyhow::Result<Self> {
        let tools = ToolCatalog::new(&sdk)?;
        Ok(Self {
            state: ConfigurationState::Resolved(Box::new(ResolvedConfiguration { sdk, tools })),
        })
    }

    /// Returns the SDK after the session initialization boundary has resolved it.
    pub fn sdk(&self) -> &SpineConfig {
        match &self.state {
            ConfigurationState::Resolved(resolved) => &resolved.sdk,
            ConfigurationState::Sources(_) => {
                panic!(
                    "session initialization must resolve Spine configuration before using the SDK"
                )
            }
        }
    }

    /// Returns tools from the same resolved configuration as the SDK.
    pub fn tools(&self) -> &ToolCatalog {
        match &self.state {
            ConfigurationState::Resolved(resolved) => &resolved.tools,
            ConfigurationState::Sources(_) => {
                panic!("session initialization must resolve Spine configuration before using tools")
            }
        }
    }

    pub(crate) fn resolve(
        &self,
        saved: Option<&str>,
        features: &ManagedFeatures,
    ) -> anyhow::Result<Self> {
        let sdk = match saved {
            Some(saved) => SpineConfig::parse_toml(saved)?,
            None => match &self.state {
                ConfigurationState::Sources(loader) => {
                    loader.clone().load().map_err(io::Error::from)?
                }
                ConfigurationState::Resolved(resolved) => resolved.sdk.clone(),
            },
        };
        let enabled = [
            (CodexFeature::SpineJit, spine_core::host::Feature::Jit),
            (CodexFeature::SpineSpawn, spine_core::host::Feature::Spawn),
        ]
        .into_iter()
        .filter(|(host, _)| features.enabled(*host))
        .map(|(_, sdk)| sdk);
        let sdk = sdk.with_features(enabled)?;
        let tools = ToolCatalog::new(&sdk)?;
        Ok(Self {
            state: ConfigurationState::Resolved(Box::new(ResolvedConfiguration { sdk, tools })),
        })
    }
}

fn loader(
    path: Option<&AbsolutePathBuf>,
    working_directory: &Path,
    home_directory: Option<&Path>,
    project_config_trusted: bool,
) -> SpineConfigLoader {
    let mut loader = SpineConfigLoader::new(working_directory);
    if !project_config_trusted {
        loader = loader.without_working_directory_layers();
    }
    if let Some(home_directory) = home_directory {
        loader = loader.with_home_directory(home_directory);
    }
    if let Some(path) = path {
        loader = loader.with_custom_path(path.as_path());
    }
    loader
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

/// Reuse the SDK configuration captured at the effective transaction boundary on resume or fork.
pub(crate) fn restore_sampling_config(
    config: &mut crate::config::Config,
    history: &codex_history::InitialHistory,
) -> anyhow::Result<()> {
    let effective = super::effective_rollout(history.get_spine_rollout_items());
    let snapshot = effective.iter().rev().find_map(|(_, item)| match item {
        codex_history::RolloutItem::SpineSamplingStarted(started) => started.sdk_config.as_deref(),
        _ => None,
    });
    config.spine = config.spine.resolve(snapshot, &config.features)?;
    Ok(())
}
