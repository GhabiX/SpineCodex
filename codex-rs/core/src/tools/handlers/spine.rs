use crate::function_tool::FunctionCallError;
use crate::spine::tool_response;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ModelVisibleToolOwner;
use crate::tools::registry::ToolExecutor;
use codex_protocol::config_types::ModeKind;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::parse_tool_input_schema_without_compaction;
use spine_core::host::SpineOperationFact;
use spine_core::host::SpineTool;
use spine_core::host::ToolCatalog;
use spine_core::host::ToolDefinition;

pub(crate) struct SpineHandler {
    definition: ToolDefinition,
}

impl SpineHandler {
    pub(crate) fn add_tools(catalog: &ToolCatalog, mode: ModeKind, mut add: impl FnMut(Self)) {
        for definition in catalog.definitions() {
            if mode == ModeKind::Plan && definition.tool == SpineTool::Spawn {
                continue;
            }
            add(Self {
                definition: definition.clone(),
            });
        }
    }

    fn name(&self) -> &'static str {
        self.definition.tool.name()
    }
}

fn create_spine_tool(definition: &ToolDefinition) -> ToolSpec {
    // Tool descriptions are bounded by SpineConfig. Do not reject the merged
    // namespace with a byte proxy here; the final provider request owns total
    // context-budget enforcement.
    spine_tool_spec(definition)
}

fn spine_tool_spec(definition: &ToolDefinition) -> ToolSpec {
    let parameters: JsonSchema = parse_tool_input_schema_without_compaction(&definition.parameters)
        .expect("Spine SDK emits valid JSON schemas");
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: spine_core::host::SPINE_NAMESPACE.to_string(),
        description: spine_core::host::SPINE_NAMESPACE_DESCRIPTION.to_string(),
        tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
            name: definition.tool.name().to_string(),
            description: definition.description.clone(),
            strict: false,
            defer_loading: None,
            parameters,
            output_schema: None,
        })],
    })
}

#[cfg(test)]
fn validate_arguments(tool: SpineTool, arguments: &str) -> Result<(), FunctionCallError> {
    validate_control_fact(tool, arguments).map(|_| ())
}

fn validate_control_fact(
    tool: SpineTool,
    arguments: &str,
) -> Result<SpineOperationFact, FunctionCallError> {
    crate::spine::validated_control_fact(tool, arguments).map_err(|error| {
        let message = match error {
            spine_core::host::ToolValidationError::InvalidJson(error) => {
                format!("failed to parse function arguments: {error}")
            }
            spine_core::host::ToolValidationError::EmptyField(_) => {
                format!("{} requires a non-empty argument", tool.name())
            }
            error => error.to_string(),
        };
        FunctionCallError::RespondToModel(message)
    })
}

impl ToolExecutor<ToolInvocation> for SpineHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(spine_core::host::SPINE_NAMESPACE, self.name())
    }

    fn spec(&self) -> ToolSpec {
        create_spine_tool(&self.definition)
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::DirectModelOnly
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
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
            step_context,
            call_id,
            cancellation_token,
            payload,
            ..
        } = invocation;
        let origin = spine_core::host::ExecutionOrigin::Direct {
            execution_ref: call_id.clone(),
        };
        if step_context.turn.collaboration_mode().mode == ModeKind::Plan {
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

        let response_tool = match self.definition.tool {
            tool @ (SpineTool::Open | SpineTool::Close | SpineTool::Next) => {
                let operation = validate_control_fact(tool, &arguments)?;
                let validation = session
                    .lock_spine_coordinator()
                    .as_ref()
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "Spine is not enabled for this session".to_string(),
                        )
                    })?
                    .validate_control(tool);
                match validation {
                    Ok(()) => {
                        session.stage_spine_fact(&call_id, origin.clone(), operation);
                        crate::tools::parallel::provision_current_spine_control().await?;
                        tool
                    }
                    Err(error) => {
                        return Err(FunctionCallError::RespondToModel(error));
                    }
                }
            }
            SpineTool::Spawn => {
                let call = crate::tools::parallel::await_current_spine_spawn_call(
                    &call_id,
                    &cancellation_token,
                )
                .await
                .map_err(FunctionCallError::RespondToModel)?;
                let (tasks, receipt) = crate::spine::spawn::execute(
                    session.clone(),
                    step_context,
                    call.call_id,
                    call.arguments,
                    cancellation_token,
                )
                .await
                .map_err(FunctionCallError::RespondToModel)?;
                session.stage_spine_fact(
                    &call_id,
                    origin,
                    SpineOperationFact::Spawn {
                        tasks,
                        terminal_results: receipt.results,
                    },
                );
                return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    r#"{"status":"success"}"#.to_string(),
                    Some(true),
                )));
            }
        };

        Ok(boxed_tool_output(tool_response::success(response_tool)))
    }
}

impl CoreToolRuntime for SpineHandler {
    fn model_visible_owner(&self) -> ModelVisibleToolOwner {
        ModelVisibleToolOwner::Spine
    }

    fn waits_for_runtime_cancellation(&self) -> bool {
        self.definition.tool == SpineTool::Spawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn catalog() -> ToolCatalog {
        let config = spine_core::host::SpineConfig::v1()
            .with_features([
                spine_core::host::Feature::Jit,
                spine_core::host::Feature::Spawn,
            ])
            .unwrap();
        ToolCatalog::new(&config).unwrap()
    }

    fn handlers(mode: ModeKind) -> Vec<SpineHandler> {
        let mut handlers = Vec::new();
        SpineHandler::add_tools(&catalog(), mode, |handler| handlers.push(handler));
        handlers
    }

    #[test]
    fn validates_control_argument_matrix() {
        for (kind, arguments) in [
            (SpineTool::Open, r#"{"summary":"task"}"#),
            (SpineTool::Close, r#"{"memory":"done"}"#),
            (SpineTool::Next, r#"{"summary":"sibling","memory":"done"}"#),
        ] {
            assert!(validate_arguments(kind, arguments).is_ok());
        }

        for (kind, arguments) in [
            (SpineTool::Open, r#"{"summary":" "}"#),
            (SpineTool::Close, r#"{"memory":""}"#),
            (SpineTool::Next, r#"{"summary":"sibling","memory":" "}"#),
            (SpineTool::Open, r#"{"summary":"task","extra":1}"#),
            (SpineTool::Close, "not-json"),
        ] {
            assert!(validate_arguments(kind, arguments).is_err());
        }

        assert!(matches!(
            validate_arguments(SpineTool::Open, r#"{"summary":" "}"#),
            Err(FunctionCallError::RespondToModel(message))
                if message == "open requires a non-empty argument"
        ));
        assert!(matches!(
            validate_arguments(SpineTool::Close, "not-json"),
            Err(FunctionCallError::RespondToModel(message))
                if message.starts_with("failed to parse function arguments:")
        ));
    }

    #[test]
    fn tool_registration_follows_sdk_catalog() {
        let catalog = catalog();
        let mut handlers = Vec::new();
        SpineHandler::add_tools(&catalog, ModeKind::Default, |handler| {
            handlers.push(handler);
        });
        assert_eq!(
            handlers
                .iter()
                .map(codex_tools::ToolExecutor::tool_name)
                .collect::<Vec<_>>(),
            catalog
                .definitions()
                .iter()
                .map(|definition| {
                    ToolName::namespaced(spine_core::host::SPINE_NAMESPACE, definition.tool.name())
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn plan_mode_suppresses_only_spawn() {
        assert_eq!(
            handlers(ModeKind::Plan)
                .iter()
                .map(codex_tools::ToolExecutor::tool_name)
                .collect::<Vec<_>>(),
            [SpineTool::Open, SpineTool::Close, SpineTool::Next]
                .map(|tool| ToolName::namespaced(spine_core::host::SPINE_NAMESPACE, tool.name()))
        );
    }

    #[test]
    fn spine_tools_are_direct_model_only() {
        assert!(
            handlers(ModeKind::Default)
                .iter()
                .all(|handler| handler.exposure() == ToolExposure::DirectModelOnly)
        );
    }
}
