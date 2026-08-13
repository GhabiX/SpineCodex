use super::PreviousSectionState;
use super::WorldStateHash;
use super::WorldStateSection;
use super::multi_agent_usage_hint::MultiAgentUsageHintState;
use crate::context::ContextualUserFragment;
use crate::context::SpineMultiAgentModeInstructions;
use crate::context::multi_agent_mode_instructions::MultiAgentModeInstructions;
use codex_protocol::config_types::MultiAgentMode;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use serde::Deserialize;
use serde::Serialize;
use spine_core::SpawnPromptMode;
use spine_core::SpineConfig;

const MULTI_AGENT_MODE_MAX_TOKENS: usize = 400;

/// Effective multi-agent mode currently visible to the model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct MultiAgentModeState {
    mode: Option<MultiAgentMode>,
    #[serde(skip)]
    configured_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    configured_prompt_hash: Option<WorldStateHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage_hint_hash: Option<WorldStateHash>,
}

impl MultiAgentModeState {
    pub(crate) fn new(mode: Option<MultiAgentMode>) -> Self {
        Self {
            mode: mode.map(|mode| match mode {
                MultiAgentMode::Custom(hint_text) => MultiAgentMode::Custom(truncate_text(
                    &hint_text,
                    TruncationPolicy::Tokens(MULTI_AGENT_MODE_MAX_TOKENS),
                )),
                mode @ (MultiAgentMode::ExplicitRequestOnly | MultiAgentMode::Proactive) => mode,
            }),
            configured_prompt: None,
            configured_prompt_hash: None,
            usage_hint_hash: None,
        }
    }

    /// Selects the SDK-owned mode prompt only when the typed Spine Spawn feature is active.
    ///
    /// An inactive mode retains the explicit prompt identity so a proactive-to-inactive
    /// transition can emit the correct cancellation text without consulting stale history.
    pub(crate) fn with_spine_config(mut self, config: &SpineConfig) -> Self {
        let configured_prompt = match self.mode.as_ref() {
            Some(MultiAgentMode::Proactive) => config.spawn_prompt(SpawnPromptMode::Proactive),
            Some(MultiAgentMode::Custom(_)) => None,
            Some(MultiAgentMode::ExplicitRequestOnly) | None => {
                config.spawn_prompt(SpawnPromptMode::ExplicitRequestOnly)
            }
        };
        self.configured_prompt_hash = configured_prompt.map(|prompt| {
            WorldStateHash::from_fragment(&SpineMultiAgentModeInstructions::new(prompt))
        });
        self.configured_prompt = configured_prompt.map(str::to_string);
        self
    }

    pub(crate) fn with_usage_hint(mut self, usage_hint: &MultiAgentUsageHintState) -> Self {
        self.usage_hint_hash = Some(usage_hint.snapshot());
        self
    }
}

impl WorldStateSection for MultiAgentModeState {
    const ID: &'static str = "multi_agent_mode";
    type Snapshot = Self;

    fn snapshot(&self) -> Self::Snapshot {
        self.clone()
    }

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "developer" && MultiAgentModeInstructions::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let mode = match (&self.mode, previous) {
            (Some(mode), PreviousSectionState::Known(previous))
                if previous.mode.as_ref() == Some(mode)
                    && previous.configured_prompt_hash == self.configured_prompt_hash
                    && previous.usage_hint_hash == self.usage_hint_hash =>
            {
                return None;
            }
            (Some(mode), _) => mode.clone(),
            (None, PreviousSectionState::Known(previous))
                if previous.mode == Some(MultiAgentMode::Proactive) =>
            {
                MultiAgentMode::ExplicitRequestOnly
            }
            (None, PreviousSectionState::Unknown) => MultiAgentMode::ExplicitRequestOnly,
            (None, PreviousSectionState::Absent | PreviousSectionState::Known(_)) => return None,
        };

        if let Some(prompt) = self.configured_prompt.as_deref() {
            return Some(Box::new(SpineMultiAgentModeInstructions::new(prompt)));
        }
        MultiAgentModeInstructions::from_mode(mode)
            .map(|instructions| Box::new(instructions) as Box<dyn ContextualUserFragment>)
    }
}

#[cfg(test)]
#[path = "multi_agent_mode_tests.rs"]
mod tests;
