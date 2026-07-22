use super::ContextualUserFragment;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;

const EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT: &str = "Do not use the MultiAgent collaboration tools to spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents, delegation, or parallel agent work. This restriction applies only to the MultiAgent collaboration surface and does not restrict independently available tools such as spine.spawn.";
const PROACTIVE_MULTI_AGENT_MODE_TEXT: &str = "Proactive MultiAgent collaboration is active. Any earlier instruction requiring an explicit user request before using the MultiAgent collaboration tools no longer applies. Use those collaboration tools when parallel work would materially improve speed or quality. This policy does not govern independent tools such as spine.spawn. This mode remains active until a later multi-agent mode developer message changes it.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MultiAgentModeInstructions {
    multi_agent_mode: MultiAgentMode,
}

impl MultiAgentModeInstructions {
    pub(crate) fn new(multi_agent_mode: MultiAgentMode) -> Self {
        Self { multi_agent_mode }
    }
}

impl ContextualUserFragment for MultiAgentModeInstructions {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (MULTI_AGENT_MODE_OPEN_TAG, MULTI_AGENT_MODE_CLOSE_TAG)
    }

    fn body(&self) -> String {
        match &self.multi_agent_mode {
            MultiAgentMode::Custom(hint_text) => hint_text.clone(),
            MultiAgentMode::ExplicitRequestOnly => {
                EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT.to_string()
            }
            MultiAgentMode::Proactive => PROACTIVE_MULTI_AGENT_MODE_TEXT.to_string(),
        }
    }
}
