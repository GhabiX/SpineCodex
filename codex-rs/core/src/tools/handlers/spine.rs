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
use crate::tools::handlers::spine_spec::SPINE_SPAWN;
use crate::tools::handlers::spine_spec::SPINE_TRIM;
use crate::tools::handlers::spine_spec::create_spine_tool;
use crate::tools::handlers::spine_spec::create_spine_trim_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::config_types::ModeKind;
#[cfg(test)]
use codex_spine_core::TrimOperation;
use codex_spine_core::TrimRequest;
#[cfg(test)]
use codex_spine_core::TrimSlice;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;

pub(crate) struct SpineHandler {
    kind: SpineHandlerKind,
}

#[derive(Clone, Copy)]
enum SpineHandlerKind {
    Control(SpineControlKind),
    Spawn,
    Trim,
}

impl SpineHandler {
    pub(crate) fn all() -> [Self; 3] {
        [
            Self {
                kind: SpineHandlerKind::Control(SpineControlKind::Open),
            },
            Self {
                kind: SpineHandlerKind::Control(SpineControlKind::Close),
            },
            Self {
                kind: SpineHandlerKind::Control(SpineControlKind::Next),
            },
        ]
    }

    pub(crate) fn trim() -> Self {
        Self {
            kind: SpineHandlerKind::Trim,
        }
    }

    pub(crate) fn spawn() -> Self {
        Self {
            kind: SpineHandlerKind::Spawn,
        }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            SpineHandlerKind::Control(SpineControlKind::Open) => SPINE_OPEN,
            SpineHandlerKind::Control(SpineControlKind::Close) => SPINE_CLOSE,
            SpineHandlerKind::Control(SpineControlKind::Next) => SPINE_NEXT,
            SpineHandlerKind::Spawn => SPINE_SPAWN,
            SpineHandlerKind::Trim => SPINE_TRIM,
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
        match self.kind {
            SpineHandlerKind::Control(_) | SpineHandlerKind::Spawn => {
                create_spine_tool(self.name())
            }
            SpineHandlerKind::Trim => create_spine_trim_tool(),
        }
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::DirectModelOnly
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
            call_id,
            cancellation_token,
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

        match self.kind {
            SpineHandlerKind::Control(kind) => {
                validate_arguments(kind, &arguments)?;
                session
                    .validate_spine_control(kind)
                    .await
                    .map_err(FunctionCallError::RespondToModel)?;
            }
            SpineHandlerKind::Spawn => {
                let tasks = crate::spine::spawn::parse_tasks(&arguments)
                    .map_err(FunctionCallError::RespondToModel)?;
                let receipt =
                    crate::spine::spawn::execute(session, turn, call_id, cancellation_token, tasks)
                        .await
                        .map_err(FunctionCallError::RespondToModel)?;
                let body = crate::spine::spawn::encode_receipt(&receipt).map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to encode spine.spawn receipt: {error}"
                    ))
                })?;
                return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    body,
                    Some(true),
                )));
            }
            SpineHandlerKind::Trim => {
                let request =
                    TrimRequest::parse(&arguments).map_err(FunctionCallError::RespondToModel)?;
                session
                    .validate_spine_trim(&request)
                    .await
                    .map_err(FunctionCallError::RespondToModel)?;
            }
        }

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            format!("Spine {} accepted.", self.name()),
            Some(true),
        )))
    }
}

impl CoreToolRuntime for SpineHandler {
    fn waits_for_runtime_cancellation(&self) -> bool {
        matches!(self.kind, SpineHandlerKind::Spawn)
    }
}

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

    #[test]
    fn spine_tools_are_direct_model_only() {
        assert!(
            SpineHandler::all()
                .iter()
                .all(|handler| handler.exposure() == ToolExposure::DirectModelOnly)
        );
        assert_eq!(
            SpineHandler::trim().exposure(),
            ToolExposure::DirectModelOnly
        );
        assert_eq!(
            SpineHandler::spawn().exposure(),
            ToolExposure::DirectModelOnly
        );
    }

    #[test]
    fn trim_arguments_cover_snip_and_slice_shapes() {
        let snip = TrimRequest::parse(r#"{"TRIM_ID":"trim_4","op":"snip"}"#).unwrap();
        assert_eq!(snip.trim_id, "trim_4");
        assert_eq!(snip.operation, TrimOperation::Snip);
        let slice = TrimRequest::parse(r#"{"TRIM_ID":"trim_4","op":"slice","tail":3}"#).unwrap();
        assert_eq!(
            slice.operation,
            TrimOperation::Slice(TrimSlice::Tail { tail: 3 })
        );
        assert!(TrimRequest::parse(r#"{"TRIM_ID":"trim_4","op":"slice"}"#).is_err());
    }
}
