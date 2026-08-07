use super::ConfigError;
use super::DEFAULT_CONFIG_TOML;
use super::FileConfig;
use super::SpineConfig;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

const CONFIG_FILE_NAME: &str = "spine.toml";
const CONFIG_DIRECTORY_NAME: &str = ".spine";

/// Discovers, merges, and validates Spine configuration layers.
///
/// The working and home directories are environment inputs supplied by the
/// host. Path discovery, file loading, precedence, merge semantics, and typed
/// validation remain owned by the Spine SDK.
///
/// Layers are recursively merged from the bundled config through home
/// `.spine/spine.toml`, working-directory `.spine/spine.toml`,
/// working-directory `spine.toml`, and finally the custom path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpineConfigLoader {
    working_directory: PathBuf,
    home_directory: Option<PathBuf>,
    custom_path: Option<PathBuf>,
    load_working_directory_layers: bool,
}

impl SpineConfigLoader {
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
            home_directory: None,
            custom_path: None,
            load_working_directory_layers: true,
        }
    }

    pub fn with_home_directory(mut self, home_directory: impl Into<PathBuf>) -> Self {
        self.home_directory = Some(home_directory.into());
        self
    }

    pub fn with_custom_path(mut self, custom_path: impl Into<PathBuf>) -> Self {
        self.custom_path = Some(custom_path.into());
        self
    }

    /// Disables implicit configuration discovery beneath the working directory.
    ///
    /// Hosts should use this for workspaces whose project configuration is not
    /// trusted. Bundled, home, and explicit configuration layers still load.
    pub fn without_working_directory_layers(mut self) -> Self {
        self.load_working_directory_layers = false;
        self
    }

    pub fn load(self) -> Result<SpineConfig, ConfigLoadError> {
        let mut merged = toml::from_str(DEFAULT_CONFIG_TOML)
            .map_err(|error| ConfigLoadError::InvalidBundled(error.to_string()))?;

        if let Some(home_directory) = &self.home_directory {
            merge_file_if_present(
                &mut merged,
                &home_directory
                    .join(CONFIG_DIRECTORY_NAME)
                    .join(CONFIG_FILE_NAME),
            )?;
        }
        if self.load_working_directory_layers {
            merge_file_if_present(
                &mut merged,
                &self
                    .working_directory
                    .join(CONFIG_DIRECTORY_NAME)
                    .join(CONFIG_FILE_NAME),
            )?;
            merge_file_if_present(&mut merged, &self.working_directory.join(CONFIG_FILE_NAME))?;
        }
        if let Some(custom_path) = &self.custom_path {
            merge_required_file(&mut merged, custom_path)?;
        }

        parse_merged_config(merged).map_err(ConfigLoadError::InvalidMerged)
    }
}

fn merge_file_if_present(merged: &mut TomlValue, path: &Path) -> Result<(), ConfigLoadError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ConfigLoadError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    merge_source(merged, path, &source)
}

fn merge_required_file(merged: &mut TomlValue, path: &Path) -> Result<(), ConfigLoadError> {
    let source = fs::read_to_string(path).map_err(|source| ConfigLoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    merge_source(merged, path, &source)
}

fn merge_source(merged: &mut TomlValue, path: &Path, source: &str) -> Result<(), ConfigLoadError> {
    let overlay = toml::from_str(source).map_err(|error| ConfigLoadError::InvalidLayer {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    merge_toml_values(merged, overlay);
    Ok(())
}

fn merge_toml_values(base: &mut TomlValue, overlay: TomlValue) {
    match (base, overlay) {
        (TomlValue::Table(base), TomlValue::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(base_value) => merge_toml_values(base_value, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn parse_merged_config(merged: TomlValue) -> Result<SpineConfig, ConfigError> {
    let parsed: FileConfig = merged
        .try_into()
        .map_err(|error: toml::de::Error| ConfigError::InvalidToml(error.to_string()))?;
    SpineConfig::from_file_config(parsed)
}

#[derive(Debug)]
pub enum ConfigLoadError {
    Read { path: PathBuf, source: io::Error },
    InvalidLayer { path: PathBuf, message: String },
    InvalidBundled(String),
    InvalidMerged(ConfigError),
}

impl fmt::Display for ConfigLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read Spine config {}: {source}",
                    path.display()
                )
            }
            Self::InvalidLayer { path, message } => {
                write!(
                    formatter,
                    "invalid Spine config {}: {message}",
                    path.display()
                )
            }
            Self::InvalidBundled(message) => {
                write!(formatter, "invalid bundled Spine config: {message}")
            }
            Self::InvalidMerged(source) => {
                write!(formatter, "invalid merged Spine configuration: {source}")
            }
        }
    }
}

impl std::error::Error for ConfigLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::InvalidMerged(source) => Some(source),
            Self::InvalidLayer { .. } | Self::InvalidBundled(_) => None,
        }
    }
}

impl From<ConfigLoadError> for io::Error {
    fn from(error: ConfigLoadError) -> Self {
        let kind = match &error {
            ConfigLoadError::Read { source, .. } => source.kind(),
            ConfigLoadError::InvalidLayer { .. }
            | ConfigLoadError::InvalidBundled(_)
            | ConfigLoadError::InvalidMerged(_) => io::ErrorKind::InvalidData,
        };
        Self::new(kind, error)
    }
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
