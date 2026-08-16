use crate::tools::context::FunctionToolOutput;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use spine_core::host::SpineTool;
use spine_core::host::ToolOutcome;

pub(crate) fn success(tool: SpineTool) -> FunctionToolOutput {
    FunctionToolOutput::from_text(success_carrier(tool).to_string(), Some(true))
}

pub(crate) fn outcome(tool_name: &str, payload: &FunctionCallOutputPayload) -> ToolOutcome {
    let Some((namespace, name)) = tool_name.split_once('.') else {
        return ToolOutcome::Unknown;
    };
    if namespace != spine_core::host::SPINE_NAMESPACE {
        return ToolOutcome::Unknown;
    }
    let Some(tool) = SpineTool::all()
        .into_iter()
        .find(|tool| tool.name() == name)
    else {
        return ToolOutcome::Unknown;
    };
    let Some(carrier) = spine_core::host::success_carrier(tool) else {
        return ToolOutcome::Unknown;
    };
    if matches!(
        &payload.body,
        FunctionCallOutputBody::Text(body) if body == carrier
    ) {
        ToolOutcome::Succeeded
    } else {
        ToolOutcome::Unknown
    }
}

pub(crate) fn success_carrier(tool: SpineTool) -> &'static str {
    spine_core::host::success_carrier(tool)
        .unwrap_or_else(|| unreachable!("spawn has a structured receipt, not a text carrier"))
}

#[cfg(test)]
#[path = "tool_response_tests.rs"]
mod tests;
