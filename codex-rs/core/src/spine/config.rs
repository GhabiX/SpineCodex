use crate::config::ManagedFeatures;
use codex_features::Feature as CodexFeature;
use codex_utils_absolute_path::AbsolutePathBuf;
use spine_core::SpineConfig;
use spine_core::SpineConfigLoader;
use spine_core::ToolCatalog;
use std::path::Path;

pub(crate) fn load(
    path: Option<&AbsolutePathBuf>,
    working_directory: &Path,
    home_directory: Option<&Path>,
    enabled_features: &ManagedFeatures,
    project_config_trusted: bool,
) -> std::io::Result<(SpineConfig, ToolCatalog)> {
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
    let mut features = Vec::new();
    if enabled_features.enabled(CodexFeature::SpineJit) {
        features.push(spine_core::Feature::Jit);
    }
    if enabled_features.enabled(CodexFeature::SpineTrim) {
        features.push(spine_core::Feature::Trim);
    }
    if enabled_features.enabled(CodexFeature::SpineSpawn) {
        features.push(spine_core::Feature::Spawn);
    }
    let config = loader
        .load()
        .map_err(std::io::Error::from)?
        .with_features(features)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let tools = ToolCatalog::new(&config)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    Ok((config, tools))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
