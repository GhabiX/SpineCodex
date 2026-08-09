use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;

mod loader;

pub use loader::ConfigLoadError;
pub use loader::SpineConfigLoader;

const MAX_TRIM_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum serialized bytes owned by one Spine model-visible provider value.
///
/// Every supported provider path receives the same UTF-8 Responses value. A
/// byte-level tokenizer can emit at most one token per non-empty input byte;
/// keeping the complete framed value at or below this ceiling leaves the
/// explicit provider-framing reserve below the strict 10K-token item limit.
pub const MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES: usize = 9_500;
/// Reserved tokens for provider-created sentinels around one already framed
/// Spine value. Provider adapters must preserve this contract when translating
/// the Responses representation.
pub const MAX_PROVIDER_ADDED_FRAME_TOKENS: usize = 499;
pub const MAX_MODEL_VISIBLE_ITEM_TOKENS: usize =
    MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES + MAX_PROVIDER_ADDED_FRAME_TOKENS;
const _: () = assert!(MAX_MODEL_VISIBLE_ITEM_TOKENS < 10_000);
/// Hard ceiling for one configured Spine text item that may be sent to a model.
pub const MAX_MODEL_VISIBLE_TEXT_BYTES: usize = 7 * 1024;
pub(crate) const MAX_TOOL_DESCRIPTION_BYTES: usize = 4 * 1024;
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../config/spine.toml");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    Jit,
    Trim,
    Spawn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnPromptMode {
    ExplicitRequestOnly,
    Proactive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpineConfig {
    trim_threshold_bytes: usize,
    jit_prompt: String,
    node_prompt: String,
    trim_prompt: String,
    spawn_prompt: String,
    spawn_explicit_request_only_prompt: String,
    spawn_proactive_prompt: String,
    tool_descriptions: ToolDescriptions,
    features: BTreeSet<Feature>,
}

impl Default for SpineConfig {
    fn default() -> Self {
        Self::v1()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolDescriptions {
    open: Option<String>,
    close: Option<String>,
    next: Option<String>,
    trim: Option<String>,
    spawn: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    schema_version: u32,
    #[serde(default)]
    limits: FileLimits,
    #[serde(default)]
    prompt: FilePrompt,
    #[serde(default)]
    tools: FileTools,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileLimits {
    #[serde(default = "default_trim_threshold")]
    trim_threshold_bytes: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FilePrompt {
    #[serde(default)]
    jit: Option<String>,
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    trim: Option<String>,
    #[serde(default)]
    spawn: Option<String>,
    #[serde(default)]
    spawn_explicit_request_only: Option<String>,
    #[serde(default)]
    spawn_proactive: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileTools {
    #[serde(default)]
    open: Option<FileToolDescription>,
    #[serde(default)]
    close: Option<FileToolDescription>,
    #[serde(default)]
    next: Option<FileToolDescription>,
    #[serde(default)]
    trim: Option<FileToolDescription>,
    #[serde(default)]
    spawn: Option<FileToolDescription>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToolDescription {
    description: String,
}

const fn default_trim_threshold() -> u64 {
    10_000
}

impl SpineConfig {
    pub fn v1() -> Self {
        match Self::parse_toml(DEFAULT_CONFIG_TOML) {
            Ok(config) => config,
            Err(error) => panic!("embedded Spine config is invalid: {error}"),
        }
    }

    pub fn parse_toml(source: &str) -> Result<Self, ConfigError> {
        let parsed: FileConfig =
            toml::from_str(source).map_err(|error| ConfigError::InvalidToml(error.to_string()))?;
        Self::from_file_config(parsed)
    }

    fn from_file_config(parsed: FileConfig) -> Result<Self, ConfigError> {
        if parsed.schema_version != 1 {
            return Err(ConfigError::UnsupportedSchemaVersion(parsed.schema_version));
        }
        if parsed.limits.trim_threshold_bytes == 0
            || parsed.limits.trim_threshold_bytes > MAX_TRIM_THRESHOLD_BYTES
        {
            return Err(ConfigError::InvalidTrimThreshold(
                parsed.limits.trim_threshold_bytes,
            ));
        }
        for (name, value, max_bytes) in [
            (
                "prompt.jit",
                parsed.prompt.jit.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.node",
                parsed.prompt.node.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.trim",
                parsed.prompt.trim.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.spawn",
                parsed.prompt.spawn.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.spawn_explicit_request_only",
                parsed.prompt.spawn_explicit_request_only.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.spawn_proactive",
                parsed.prompt.spawn_proactive.as_deref(),
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "tools.open.description",
                parsed
                    .tools
                    .open
                    .as_ref()
                    .map(|tool| tool.description.as_str()),
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.close.description",
                parsed
                    .tools
                    .close
                    .as_ref()
                    .map(|tool| tool.description.as_str()),
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.next.description",
                parsed
                    .tools
                    .next
                    .as_ref()
                    .map(|tool| tool.description.as_str()),
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.trim.description",
                parsed
                    .tools
                    .trim
                    .as_ref()
                    .map(|tool| tool.description.as_str()),
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.spawn.description",
                parsed
                    .tools
                    .spawn
                    .as_ref()
                    .map(|tool| tool.description.as_str()),
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
        ] {
            validate_model_visible_text(name, value, max_bytes)?;
        }
        Ok(Self {
            trim_threshold_bytes: parsed.limits.trim_threshold_bytes as usize,
            jit_prompt: parsed.prompt.jit.unwrap_or_default(),
            node_prompt: parsed.prompt.node.unwrap_or_default(),
            trim_prompt: parsed.prompt.trim.unwrap_or_default(),
            spawn_prompt: parsed.prompt.spawn.unwrap_or_default(),
            spawn_explicit_request_only_prompt: parsed
                .prompt
                .spawn_explicit_request_only
                .unwrap_or_default(),
            spawn_proactive_prompt: parsed.prompt.spawn_proactive.unwrap_or_default(),
            tool_descriptions: ToolDescriptions {
                open: parsed.tools.open.map(|tool| tool.description),
                close: parsed.tools.close.map(|tool| tool.description),
                next: parsed.tools.next.map(|tool| tool.description),
                trim: parsed.tools.trim.map(|tool| tool.description),
                spawn: parsed.tools.spawn.map(|tool| tool.description),
            },
            features: BTreeSet::new(),
        })
    }

    pub fn with_feature(mut self, feature: Feature) -> Result<Self, crate::InitError> {
        self.features.insert(feature);
        self.validate_features()?;
        Ok(self)
    }

    pub fn with_features<I>(mut self, features: I) -> Result<Self, crate::InitError>
    where
        I: IntoIterator<Item = Feature>,
    {
        self.features = features.into_iter().collect();
        self.validate_features()?;
        Ok(self)
    }

    pub fn is_enabled(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    pub fn is_feature_off(&self) -> bool {
        self.features.is_empty()
    }

    pub const fn schema_version(&self) -> u32 {
        1
    }

    pub const fn trim_threshold_bytes(&self) -> usize {
        self.trim_threshold_bytes
    }

    pub fn extend_system_prompt(&self, base: &str) -> String {
        crate::prompt::extend(base.to_owned(), self)
    }

    pub fn spawn_prompt(&self, mode: SpawnPromptMode) -> Option<&str> {
        if !self.is_enabled(Feature::Spawn) {
            return None;
        }
        let prompt = match mode {
            SpawnPromptMode::ExplicitRequestOnly => &self.spawn_explicit_request_only_prompt,
            SpawnPromptMode::Proactive => &self.spawn_proactive_prompt,
        };
        (!prompt.trim().is_empty()).then_some(prompt.as_str())
    }

    pub fn node_prompt(&self) -> Option<&str> {
        if !self.is_enabled(Feature::Jit) {
            return None;
        }
        (!self.node_prompt.trim().is_empty()).then_some(self.node_prompt.as_str())
    }

    pub(crate) fn prompt(&self, feature: crate::Feature) -> &str {
        match feature {
            crate::Feature::Jit => &self.jit_prompt,
            crate::Feature::Trim => &self.trim_prompt,
            crate::Feature::Spawn => &self.spawn_prompt,
        }
    }

    pub(crate) fn tool_description(&self, name: &str) -> Option<&str> {
        match name {
            "open" => self.tool_descriptions.open.as_deref(),
            "close" => self.tool_descriptions.close.as_deref(),
            "next" => self.tool_descriptions.next.as_deref(),
            "trim" => self.tool_descriptions.trim.as_deref(),
            "spawn" => self.tool_descriptions.spawn.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), crate::InitError> {
        self.validate_features()
    }

    fn validate_features(&self) -> Result<(), crate::InitError> {
        if self.is_enabled(Feature::Jit) {
            require_prompt(self.prompt(Feature::Jit), Feature::Jit)?;
            require_configured_prompt(self.node_prompt(), Feature::Jit)?;
            for name in ["open", "close", "next"] {
                require_tool(self.tool_description(name), name)?;
            }
        }
        if self.is_enabled(Feature::Trim) {
            require_tool(self.tool_description("trim"), "trim")?;
        }
        if self.is_enabled(Feature::Spawn) {
            if !self.is_enabled(Feature::Jit) {
                return Err(crate::InitError::SpawnRequiresJit);
            }
            require_configured_prompt(
                self.spawn_prompt(SpawnPromptMode::ExplicitRequestOnly),
                Feature::Spawn,
            )?;
            require_configured_prompt(
                self.spawn_prompt(SpawnPromptMode::Proactive),
                Feature::Spawn,
            )?;
            require_tool(self.tool_description("spawn"), "spawn")?;
        }
        let prompt_segments = [Feature::Jit, Feature::Trim, Feature::Spawn]
            .into_iter()
            .filter(|feature| self.is_enabled(*feature))
            .map(|feature| self.prompt(feature))
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let prompt_bytes = prompt_segments
            .iter()
            .map(|segment| segment.len())
            .fold(0usize, |total, bytes| {
                total.saturating_add(bytes).saturating_add(2)
            })
            .saturating_sub(2);
        if prompt_bytes > MAX_MODEL_VISIBLE_TEXT_BYTES {
            return Err(crate::InitError::ModelVisiblePromptTooLong {
                max_bytes: MAX_MODEL_VISIBLE_TEXT_BYTES,
                actual_bytes: prompt_bytes,
            });
        }
        let prompt = prompt_segments.join("\n\n");
        let prompt_bytes = model_visible_prompt_provider_value_bytes(&prompt);
        if prompt_bytes > MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES {
            return Err(crate::InitError::ModelVisiblePromptTooLong {
                max_bytes: MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES,
                actual_bytes: prompt_bytes,
            });
        }
        Ok(())
    }
}

fn validate_model_visible_text(
    name: &'static str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), ConfigError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > max_bytes {
        return Err(ConfigError::PromptTooLong {
            name,
            max: max_bytes,
            actual: value.len(),
        });
    }
    let actual_bytes = model_visible_json_bytes(value);
    if actual_bytes > MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES {
        return Err(ConfigError::ModelVisibleProviderValueTooLong {
            name,
            max_bytes: MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES,
            actual_bytes,
        });
    }
    Ok(())
}

fn model_visible_json_bytes(value: &str) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |serialized| serialized.len())
}

fn model_visible_prompt_provider_value_bytes(value: &str) -> usize {
    let responses = serde_json::to_vec(&serde_json::json!({ "instructions": value }))
        .map_or(usize::MAX, |serialized| serialized.len());
    let responses_lite = serde_json::to_vec(&serde_json::json!({
        "input": [{
            "type": "message",
            "role": "developer",
            "content": [{ "type": "input_text", "text": value }]
        }]
    }))
    .map_or(usize::MAX, |serialized| serialized.len());
    responses.max(responses_lite)
}

fn require_prompt(value: &str, feature: crate::Feature) -> Result<(), crate::InitError> {
    if value.trim().is_empty() {
        return Err(crate::InitError::MissingPrompt(feature));
    }
    Ok(())
}

fn require_configured_prompt(
    value: Option<&str>,
    feature: crate::Feature,
) -> Result<(), crate::InitError> {
    if value.is_none() {
        return Err(crate::InitError::MissingPrompt(feature));
    }
    Ok(())
}

fn require_tool(value: Option<&str>, name: &'static str) -> Result<(), crate::InitError> {
    if value.is_none_or(|value| value.trim().is_empty()) {
        return Err(crate::InitError::MissingToolDescription(name));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    InvalidToml(String),
    UnsupportedSchemaVersion(u32),
    InvalidTrimThreshold(u64),
    PromptTooLong {
        name: &'static str,
        max: usize,
        actual: usize,
    },
    ModelVisibleProviderValueTooLong {
        name: &'static str,
        max_bytes: usize,
        actual_bytes: usize,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToml(error) => write!(formatter, "invalid Spine TOML: {error}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported Spine config schema version {version}"
                )
            }
            Self::InvalidTrimThreshold(value) => {
                write!(formatter, "invalid Spine trim threshold {value}")
            }
            Self::PromptTooLong { name, max, actual } => {
                write!(
                    formatter,
                    "Spine model-visible {name} is {actual} bytes; maximum is {max}"
                )
            }
            Self::ModelVisibleProviderValueTooLong {
                name,
                max_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "Spine model-visible {name} provider value is {actual_bytes} bytes; maximum is {max_bytes}"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
schema_version = 1
[limits]
trim_threshold_bytes = 2048
[prompt]
jit = "jit prompt"
node = "node prompt"
trim = "trim prompt"
spawn = "spawn prompt"
spawn_explicit_request_only = "spawn explicit request only prompt"
spawn_proactive = "spawn proactive prompt"
[tools.open]
description = "open description"
[tools.close]
description = "close description"
[tools.next]
description = "next description"
[tools.trim]
description = "trim description"
[tools.spawn]
description = "spawn description"
"#;

    #[test]
    fn parses_and_exposes_typed_config() {
        let config = SpineConfig::parse_toml(VALID).unwrap();
        assert_eq!(config.schema_version(), 1);
        assert_eq!(config.trim_threshold_bytes(), 2048);
        assert_eq!(config.prompt(crate::Feature::Jit), "jit prompt");
        assert_eq!(config.node_prompt(), None);
        assert_eq!(
            config.spawn_prompt(SpawnPromptMode::ExplicitRequestOnly),
            None
        );
        assert_eq!(config.tool_description("open"), Some("open description"));
        let config = config
            .with_features([crate::Feature::Jit, crate::Feature::Spawn])
            .unwrap();
        assert_eq!(config.node_prompt(), Some("node prompt"));
        assert_eq!(
            config.spawn_prompt(SpawnPromptMode::ExplicitRequestOnly),
            Some("spawn explicit request only prompt")
        );
        assert_eq!(
            config.spawn_prompt(SpawnPromptMode::Proactive),
            Some("spawn proactive prompt")
        );
    }

    #[test]
    fn rejects_unknown_fields_and_invalid_limits() {
        assert!(matches!(
            SpineConfig::parse_toml("schema_version = 1\nunknown = true"),
            Err(ConfigError::InvalidToml(_))
        ));
        assert!(matches!(
            SpineConfig::parse_toml("schema_version = 1\n[limits]\ntrim_threshold_bytes = 0"),
            Err(ConfigError::InvalidTrimThreshold(0))
        ));
        for (name, marker, max_bytes) in [
            ("prompt.jit", "jit prompt", MAX_MODEL_VISIBLE_TEXT_BYTES),
            ("prompt.node", "node prompt", MAX_MODEL_VISIBLE_TEXT_BYTES),
            ("prompt.trim", "trim prompt", MAX_MODEL_VISIBLE_TEXT_BYTES),
            ("prompt.spawn", "spawn prompt", MAX_MODEL_VISIBLE_TEXT_BYTES),
            (
                "prompt.spawn_explicit_request_only",
                "spawn explicit request only prompt",
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "prompt.spawn_proactive",
                "spawn proactive prompt",
                MAX_MODEL_VISIBLE_TEXT_BYTES,
            ),
            (
                "tools.open.description",
                "open description",
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.close.description",
                "close description",
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.next.description",
                "next description",
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.trim.description",
                "trim description",
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "tools.spawn.description",
                "spawn description",
                MAX_TOOL_DESCRIPTION_BYTES,
            ),
        ] {
            let source = VALID.replacen(marker, &"x".repeat(max_bytes + 1), 1);
            assert_eq!(
                SpineConfig::parse_toml(&source),
                Err(ConfigError::PromptTooLong {
                    name,
                    max: max_bytes,
                    actual: max_bytes + 1,
                }),
                "failed to bound {name}"
            );
        }
    }

    #[test]
    fn rejects_json_escaping_that_exceeds_the_safety_token_budget() {
        let source = VALID.replacen(
            "node prompt",
            &"\\u0000".repeat(MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES / 6 + 1),
            1,
        );

        assert!(matches!(
            SpineConfig::parse_toml(&source),
            Err(ConfigError::ModelVisibleProviderValueTooLong {
                name: "prompt.node",
                max_bytes: MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES,
                ..
            })
        ));
    }

    #[test]
    fn bundled_large_model_visible_items_are_manually_reviewed_and_hard_bounded() {
        let config = SpineConfig::v1();
        let configured = [
            config.prompt(Feature::Jit),
            config.prompt(Feature::Trim),
            config.prompt(Feature::Spawn),
            config.node_prompt().unwrap_or_default(),
            config
                .spawn_prompt(SpawnPromptMode::ExplicitRequestOnly)
                .unwrap_or_default(),
            config
                .spawn_prompt(SpawnPromptMode::Proactive)
                .unwrap_or_default(),
            config.tool_description("open").unwrap_or_default(),
            config.tool_description("close").unwrap_or_default(),
            config.tool_description("next").unwrap_or_default(),
            config.tool_description("trim").unwrap_or_default(),
            config.tool_description("spawn").unwrap_or_default(),
        ];

        assert!(
            configured
                .iter()
                .all(|value| value.len() <= MAX_MODEL_VISIBLE_TEXT_BYTES)
        );
        // The bundled JIT prompt and Spawn description are intentionally rich
        // enough to exceed 1 KiB. Keep them in this explicit review test so a
        // future expansion cannot silently bypass the shared hard bound.
        assert!(config.prompt(Feature::Jit).len() > 1024);
        assert!(config.tool_description("spawn").unwrap().len() > 1024);
    }

    #[test]
    fn enabled_prompt_segments_share_one_hard_boundary() {
        let source = VALID
            .replace("jit prompt", &"j".repeat(MAX_MODEL_VISIBLE_TEXT_BYTES / 2))
            .replace("trim prompt", &"t".repeat(MAX_MODEL_VISIBLE_TEXT_BYTES / 2));
        let config = SpineConfig::parse_toml(&source).unwrap();
        assert!(matches!(
            config.with_features([Feature::Jit, Feature::Trim]),
            Err(crate::InitError::ModelVisiblePromptTooLong { .. })
        ));
    }

    #[test]
    fn enabled_prompt_segments_share_one_final_safety_token_budget() {
        let escaped = "\\u0000".repeat(900);
        let source = VALID
            .replace("jit prompt", &escaped)
            .replace("trim prompt", &escaped);
        let config = SpineConfig::parse_toml(&source).unwrap();

        assert!(matches!(
            config.with_features([Feature::Jit, Feature::Trim]),
            Err(crate::InitError::ModelVisiblePromptTooLong {
                max_bytes: MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES,
                ..
            })
        ));
    }

    #[test]
    fn provider_value_budget_leaves_a_strict_framing_reserve() {
        assert_eq!(
            MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES + MAX_PROVIDER_ADDED_FRAME_TOKENS,
            MAX_MODEL_VISIBLE_ITEM_TOKENS
        );
    }

    #[test]
    fn default_v1_satisfies_jit_and_spawn_registration() {
        let config = SpineConfig::v1();
        let config = config
            .with_features([Feature::Jit, Feature::Spawn])
            .unwrap();
        config.validate().unwrap();
    }
}
