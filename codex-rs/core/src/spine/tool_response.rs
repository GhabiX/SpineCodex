use crate::tools::context::FunctionToolOutput;
use spine_core::host::SpineTool;

pub(crate) fn success(tool: SpineTool) -> FunctionToolOutput {
    FunctionToolOutput::from_text(success_carrier(tool).to_string(), Some(true))
}

pub(crate) fn success_carrier(tool: SpineTool) -> &'static str {
    spine_core::host::success_carrier(tool)
        .unwrap_or_else(|| unreachable!("spawn has a structured receipt, not a text carrier"))
}
