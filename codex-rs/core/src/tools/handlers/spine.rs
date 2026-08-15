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
use codex_protocol::models::ResponseItem;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::create_tools_json_for_responses_lite;
use codex_tools::parse_tool_input_schema_without_compaction;
use spine_core::SpineOperationFact;
use spine_core::SpineTool;
use spine_core::ToolCatalog;
use spine_core::ToolDefinition;
#[cfg(test)]
use spine_core::TrimOperation;
use spine_core::TrimRequest;
#[cfg(test)]
use spine_core::TrimSlice;

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
    let spec = spine_tool_spec(definition);
    validate_spine_tool_spec(&spec)
        .expect("validated Spine configuration must produce a bounded final ToolSpec");
    spec
}

fn spine_tool_spec(definition: &ToolDefinition) -> ToolSpec {
    let parameters: JsonSchema = parse_tool_input_schema_without_compaction(&definition.parameters)
        .expect("Spine SDK emits valid JSON schemas");
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: spine_core::SPINE_NAMESPACE.to_string(),
        description: spine_core::SPINE_NAMESPACE_DESCRIPTION.to_string(),
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

fn spine_tool_model_item_wire_bytes(spec: &ToolSpec) -> Result<usize, String> {
    let responses_api = serde_json::to_vec(&serde_json::json!({ "tools": [spec] }))
        .map_err(|error| format!("failed to serialize Spine Responses ToolSpec: {error}"))?
        .len();
    let tools = create_tools_json_for_responses_lite(std::slice::from_ref(spec))
        .map_err(|error| format!("failed to serialize Spine Responses Lite ToolSpec: {error}"))?;
    let responses_lite_item = ResponseItem::AdditionalTools {
        id: None,
        role: "developer".to_string(),
        tools,
    };
    let responses_lite = crate::context::spine_model_item_wire_bytes(&responses_lite_item)?;
    Ok(responses_api.max(responses_lite))
}

fn validate_spine_tool_spec(spec: &ToolSpec) -> Result<(), String> {
    let provider_value_bytes = spine_tool_model_item_wire_bytes(spec)?;
    if provider_value_bytes > crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES {
        return Err(format!(
            "Spine ToolSpec provider value is {provider_value_bytes} bytes; maximum is {}",
            crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES
        ));
    }
    Ok(())
}

pub(crate) fn validate_spine_namespace(namespace: &ResponsesApiNamespace) -> Result<(), String> {
    validate_spine_tool_spec(&ToolSpec::Namespace(namespace.clone()))
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
            spine_core::ToolValidationError::InvalidJson(error) => {
                format!("failed to parse function arguments: {error}")
            }
            spine_core::ToolValidationError::EmptyField(_) => {
                format!("{} requires a non-empty argument", tool.name())
            }
            error => error.to_string(),
        };
        FunctionCallError::RespondToModel(message)
    })
}

impl ToolExecutor<ToolInvocation> for SpineHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(spine_core::SPINE_NAMESPACE, self.name())
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
            ..
        } = invocation;
        let origin = spine_core::ExecutionOrigin::Direct {
            call_id: call_id.clone(),
        };
        if turn.collaboration_mode().mode == ModeKind::Plan {
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
                    turn,
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
                        terminal_results: receipt.results.clone(),
                    },
                );
                let body = receipt.encode_json().map_err(|error| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to encode spine.spawn receipt: {error}"
                    ))
                })?;
                return Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    body,
                    Some(true),
                )));
            }
            SpineTool::Trim => {
                let request =
                    TrimRequest::parse(&arguments).map_err(FunctionCallError::RespondToModel)?;
                let operation = session
                    .lock_spine_coordinator()
                    .as_ref()
                    .ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "Spine trim runtime is unavailable".to_string(),
                        )
                    })?
                    .prepare_trim(&call_id, &request);
                match operation {
                    Ok(operation) => {
                        session.stage_spine_fact(&call_id, origin, operation);
                        SpineTool::Trim
                    }
                    Err(error) => {
                        return Err(FunctionCallError::RespondToModel(error));
                    }
                }
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
        let config = spine_core::SpineConfig::v1()
            .with_features([
                spine_core::Feature::Jit,
                spine_core::Feature::Trim,
                spine_core::Feature::Spawn,
            ])
            .unwrap();
        ToolCatalog::new(&config).unwrap()
    }

    fn handlers(mode: ModeKind) -> Vec<SpineHandler> {
        let mut handlers = Vec::new();
        SpineHandler::add_tools(&catalog(), mode, |handler| handlers.push(handler));
        handlers
    }

    fn merged_namespace(catalog: &ToolCatalog, mode: ModeKind) -> ResponsesApiNamespace {
        let mut merged = ResponsesApiNamespace {
            name: spine_core::SPINE_NAMESPACE.to_string(),
            description: spine_core::SPINE_NAMESPACE_DESCRIPTION.to_string(),
            tools: Vec::new(),
        };
        SpineHandler::add_tools(catalog, mode, |handler| {
            let ToolSpec::Namespace(mut namespace) = spine_tool_spec(&handler.definition) else {
                panic!("Spine tools must use the Spine namespace");
            };
            merged.tools.append(&mut namespace.tools);
        });
        merged.tools.sort_by_key(|tool| match tool {
            ResponsesApiNamespaceTool::Function(tool) => tool.name.clone(),
            ResponsesApiNamespaceTool::Custom(tool) => tool.name.clone(),
        });
        merged
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
                    ToolName::namespaced(spine_core::SPINE_NAMESPACE, definition.tool.name())
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
            [
                SpineTool::Open,
                SpineTool::Close,
                SpineTool::Next,
                SpineTool::Trim,
            ]
            .map(|tool| ToolName::namespaced(spine_core::SPINE_NAMESPACE, tool.name()))
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

    #[test]
    fn final_tool_spec_gate_covers_max_config_runtime_suffix_and_schema() {
        let description = "x".repeat(4 * 1024);
        let source = format!(
            r#"schema_version = 1
[limits]
trim_threshold_bytes = 100
[prompt]
jit = "jit"
node = "node"
spawn_explicit_request_only = "explicit"
spawn_proactive = "proactive"
[tools.open]
description = "open"
[tools.close]
description = "close"
[tools.next]
description = "next"
[tools.spawn]
description = "{description}"
"#
        );
        let config = spine_core::SpineConfig::parse_toml(&source)
            .unwrap()
            .with_features([spine_core::Feature::Jit, spine_core::Feature::Spawn])
            .unwrap();
        let catalog = ToolCatalog::new(&config).unwrap().with_spawn_max_items(16);
        let namespace = merged_namespace(&catalog, ModeKind::Default);
        let spec = ToolSpec::Namespace(namespace.clone());

        let provider_value_bytes =
            spine_tool_model_item_wire_bytes(&ToolSpec::Namespace(namespace.clone())).unwrap();
        assert!(
            validate_spine_namespace(&namespace).is_ok(),
            "merged max-description namespace is {provider_value_bytes} bytes"
        );
        assert!(
            spine_tool_model_item_wire_bytes(&spec).unwrap()
                <= crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES
        );
    }

    #[test]
    fn final_merged_namespace_gate_has_exact_just_under_and_over_boundaries() {
        let catalog = catalog().with_spawn_max_items(16);
        let mut namespace = merged_namespace(&catalog, ModeKind::Default);
        let ResponsesApiNamespaceTool::Function(spawn) = namespace
            .tools
            .iter_mut()
            .find(|tool| matches!(tool, ResponsesApiNamespaceTool::Function(tool) if tool.name == "spawn"))
            .unwrap()
        else {
            panic!("Spawn must be a function tool");
        };
        spawn.description.clear();
        let fixed_bytes =
            spine_tool_model_item_wire_bytes(&ToolSpec::Namespace(namespace.clone())).unwrap();
        let description_bytes = crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES - fixed_bytes;
        let ResponsesApiNamespaceTool::Function(spawn) = namespace
            .tools
            .iter_mut()
            .find(|tool| matches!(tool, ResponsesApiNamespaceTool::Function(tool) if tool.name == "spawn"))
            .unwrap()
        else {
            panic!("Spawn must be a function tool");
        };
        spawn.description = "x".repeat(description_bytes);
        let accepted = ToolSpec::Namespace(namespace.clone());
        assert_eq!(
            spine_tool_model_item_wire_bytes(&accepted).unwrap(),
            crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES
        );
        assert!(validate_spine_namespace(&namespace).is_ok());

        let ResponsesApiNamespaceTool::Function(spawn) = namespace
            .tools
            .iter_mut()
            .find(|tool| matches!(tool, ResponsesApiNamespaceTool::Function(tool) if tool.name == "spawn"))
            .unwrap()
        else {
            panic!("Spawn must be a function tool");
        };
        spawn.description.push('x');
        assert!(validate_spine_namespace(&namespace).is_err());
    }

    #[test]
    fn final_merged_namespace_gate_covers_each_enabled_catalog() {
        for features in [
            vec![spine_core::Feature::Jit],
            vec![spine_core::Feature::Jit, spine_core::Feature::Trim],
            vec![spine_core::Feature::Jit, spine_core::Feature::Spawn],
            vec![
                spine_core::Feature::Jit,
                spine_core::Feature::Trim,
                spine_core::Feature::Spawn,
            ],
        ] {
            let config = spine_core::SpineConfig::v1()
                .with_features(features)
                .unwrap();
            let catalog = ToolCatalog::new(&config).unwrap().with_spawn_max_items(16);
            let namespace = merged_namespace(&catalog, ModeKind::Default);
            let provider_value_bytes =
                spine_tool_model_item_wire_bytes(&ToolSpec::Namespace(namespace.clone())).unwrap();
            assert!(
                validate_spine_namespace(&namespace).is_ok(),
                "merged namespace is {provider_value_bytes} bytes"
            );
        }
    }

    #[test]
    fn default_spawn_namespace_uses_the_complete_final_wire_budget() {
        let catalog = catalog().with_spawn_max_items(16);
        let namespace = merged_namespace(&catalog, ModeKind::Default);
        let spec = ToolSpec::Namespace(namespace);
        let provider_value_bytes = spine_tool_model_item_wire_bytes(&spec).unwrap();

        assert!(provider_value_bytes > crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES);
        assert!(provider_value_bytes <= crate::context::MAX_SPINE_TOOL_SPEC_WIRE_BYTES);
        assert!(validate_spine_tool_spec(&spec).is_ok());
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
