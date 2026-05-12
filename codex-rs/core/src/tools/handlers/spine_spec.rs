use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_spine_namespace_tool() -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the feature-gated Spine task tree cursor.".to_string(),
        tools: vec![
            ResponsesApiNamespaceTool::Function(spine_tree_tool()),
            ResponsesApiNamespaceTool::Function(spine_transition_tool(
                SPINE_TOOL_OPEN,
                "Enter a child scope for a focused Spine subproblem.",
                CompactInstructionParam::Excluded,
            )),
            ResponsesApiNamespaceTool::Function(spine_transition_tool(
                SPINE_TOOL_NEXT,
                "Finish the current Spine leaf and move to its next sibling.",
                CompactInstructionParam::Included,
            )),
            ResponsesApiNamespaceTool::Function(spine_transition_tool(
                SPINE_TOOL_CLOSE,
                "Finish the current Spine leaf, close its non-root parent scope, and continue at the parent's next sibling.",
                CompactInstructionParam::Included,
            )),
        ],
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompactInstructionParam {
    Excluded,
    Included,
}

fn spine_transition_tool(
    name: &str,
    description: &str,
    compact_instruction: CompactInstructionParam,
) -> ResponsesApiTool {
    let mut properties = BTreeMap::from([(
        "summary".to_string(),
        JsonSchema::string(Some(
            "Short Spine Tree display label for the transition".to_string(),
        )),
    )]);
    if compact_instruction == CompactInstructionParam::Included {
        properties.insert(
            "instruction".to_string(),
            JsonSchema::string(Some(
                "Optional guidance for the automatic compact pass after this transition; use it to name what must be preserved or emphasized from the completed node or scope.".to_string(),
            )),
        );
    }

    ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["summary".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn spine_tree_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: SPINE_TOOL_TREE.to_string(),
        description: "Print the current Spine node and task tree without changing the cursor."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), Some(Vec::new()), Some(false.into())),
        output_schema: None,
    }
}
