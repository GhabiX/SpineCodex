use crate::spine::SpineControlKind;
use crate::tools::context::FunctionToolOutput;
use crate::tools::handlers::spine_spec::SPINE_CLOSE;
use crate::tools::handlers::spine_spec::SPINE_NAMESPACE;
use crate::tools::handlers::spine_spec::SPINE_NEXT;
use crate::tools::handlers::spine_spec::SPINE_OPEN;
use crate::tools::handlers::spine_spec::SPINE_TRIM;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_spine_core::ToolOutcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineToolResponse {
    Open,
    Close,
    Next,
    Trim,
}

impl From<SpineControlKind> for SpineToolResponse {
    fn from(kind: SpineControlKind) -> Self {
        match kind {
            SpineControlKind::Open => Self::Open,
            SpineControlKind::Close => Self::Close,
            SpineControlKind::Next => Self::Next,
        }
    }
}

impl SpineToolResponse {
    pub(crate) fn success(self) -> FunctionToolOutput {
        FunctionToolOutput::from_text(self.success_carrier(), Some(true))
    }

    pub(crate) fn outcome(tool_name: &str, payload: &FunctionCallOutputPayload) -> ToolOutcome {
        match payload.success {
            Some(true) => ToolOutcome::Succeeded,
            Some(false) => ToolOutcome::Failed,
            None => {
                let Some(tool) = Self::from_qualified_name(tool_name) else {
                    return ToolOutcome::Unknown;
                };
                if matches!(
                    &payload.body,
                    FunctionCallOutputBody::Text(body) if body == &tool.success_carrier()
                ) {
                    ToolOutcome::Succeeded
                } else {
                    ToolOutcome::Unknown
                }
            }
        }
    }

    fn from_qualified_name(name: &str) -> Option<Self> {
        let (namespace, tool_name) = name.split_once('.')?;
        if namespace != SPINE_NAMESPACE {
            return None;
        }
        match tool_name {
            SPINE_OPEN => Some(Self::Open),
            SPINE_CLOSE => Some(Self::Close),
            SPINE_NEXT => Some(Self::Next),
            SPINE_TRIM => Some(Self::Trim),
            _ => None,
        }
    }

    #[cfg(test)]
    fn qualified_name(self) -> String {
        format!("{SPINE_NAMESPACE}.{}", self.tool_name())
    }

    fn tool_name(self) -> &'static str {
        match self {
            Self::Open => SPINE_OPEN,
            Self::Close => SPINE_CLOSE,
            Self::Next => SPINE_NEXT,
            Self::Trim => SPINE_TRIM,
        }
    }

    fn success_carrier(self) -> String {
        format!("Spine {} accepted.", self.tool_name())
    }
}

#[cfg(test)]
#[path = "tool_response_tests.rs"]
mod tests;
