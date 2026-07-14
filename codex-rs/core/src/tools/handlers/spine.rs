use crate::function_tool::FunctionCallError;
use crate::spine::SpineControlKind;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::spine_spec::SPINE_CLOSE;
use crate::tools::handlers::spine_spec::SPINE_NAMESPACE;
use crate::tools::handlers::spine_spec::SPINE_NEXT;
use crate::tools::handlers::spine_spec::SPINE_OPEN;
use crate::tools::handlers::spine_spec::create_spine_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::config_types::ModeKind;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;

pub(crate) struct SpineHandler {
    kind: SpineControlKind,
}

impl SpineHandler {
    pub(crate) fn all() -> [Self; 3] {
        [
            Self {
                kind: SpineControlKind::Open,
            },
            Self {
                kind: SpineControlKind::Close,
            },
            Self {
                kind: SpineControlKind::Next,
            },
        ]
    }

    fn name(&self) -> &'static str {
        match self.kind {
            SpineControlKind::Open => SPINE_OPEN,
            SpineControlKind::Close => SPINE_CLOSE,
            SpineControlKind::Next => SPINE_NEXT,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    memory: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NextArgs {
    summary: String,
    memory: String,
}

fn non_empty(value: String, name: &str) -> Result<String, FunctionCallError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{name} requires a non-empty argument"
        )));
    }
    Ok(value)
}

fn validate_arguments(kind: SpineControlKind, arguments: &str) -> Result<(), FunctionCallError> {
    match kind {
        SpineControlKind::Open => {
            let args: OpenArgs = parse_arguments(arguments)?;
            non_empty(args.summary, SPINE_OPEN)?;
        }
        SpineControlKind::Close => {
            let args: CloseArgs = parse_arguments(arguments)?;
            non_empty(args.memory, SPINE_CLOSE)?;
        }
        SpineControlKind::Next => {
            let args: NextArgs = parse_arguments(arguments)?;
            non_empty(args.summary, SPINE_NEXT)?;
            non_empty(args.memory, SPINE_NEXT)?;
        }
    }
    Ok(())
}

impl ToolExecutor<ToolInvocation> for SpineHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(SPINE_NAMESPACE, self.name())
    }

    fn spec(&self) -> ToolSpec {
        create_spine_tool(self.name())
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl SpineHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            source,
            ..
        } = invocation;
        if matches!(source, ToolCallSource::CodeMode { .. }) {
            return Err(FunctionCallError::RespondToModel(
                "Spine is not available as a Code Mode nested tool".to_string(),
            ));
        }
        if turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "Spine transitions are not allowed in Plan mode".to_string(),
            ));
        }
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "Spine handler received unsupported payload".to_string(),
                ));
            }
        };

        validate_arguments(self.kind, &arguments)?;

        session
            .validate_spine_control(self.kind)
            .await
            .map_err(FunctionCallError::RespondToModel)?;

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            format!("Spine {} accepted.", self.name()),
            Some(true),
        )))
    }
}

impl CoreToolRuntime for SpineHandler {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_required_arguments() {
        assert_eq!(non_empty(" task ".to_string(), SPINE_OPEN).unwrap(), "task");
        assert!(non_empty(" \n".to_string(), SPINE_CLOSE).is_err());
    }

    #[test]
    fn validates_control_argument_matrix() {
        for (kind, arguments) in [
            (SpineControlKind::Open, r#"{"summary":"task"}"#),
            (SpineControlKind::Close, r#"{"memory":"done"}"#),
            (
                SpineControlKind::Next,
                r#"{"summary":"sibling","memory":"done"}"#,
            ),
        ] {
            assert!(validate_arguments(kind, arguments).is_ok());
        }

        for (kind, arguments) in [
            (SpineControlKind::Open, r#"{"summary":" "}"#),
            (SpineControlKind::Close, r#"{"memory":""}"#),
            (
                SpineControlKind::Next,
                r#"{"summary":"sibling","memory":" "}"#,
            ),
            (SpineControlKind::Open, r#"{"summary":"task","extra":1}"#),
            (SpineControlKind::Close, "not-json"),
        ] {
            assert!(validate_arguments(kind, arguments).is_err());
        }
    }

    #[test]
    fn tool_names_are_namespaced() {
        let handlers = SpineHandler::all();
        assert_eq!(
            handlers[0].tool_name(),
            ToolName::namespaced(SPINE_NAMESPACE, SPINE_OPEN)
        );
        assert_eq!(
            handlers[1].tool_name(),
            ToolName::namespaced(SPINE_NAMESPACE, SPINE_CLOSE)
        );
        assert_eq!(
            handlers[2].tool_name(),
            ToolName::namespaced(SPINE_NAMESPACE, SPINE_NEXT)
        );
    }
}
