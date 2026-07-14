use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_OPEN: &str = "open";
pub(crate) const SPINE_CLOSE: &str = "close";
pub(crate) const SPINE_NEXT: &str = "next";
pub(crate) const SPINE_TRIM: &str = "trim";

const NODE_MEMORY_DESCRIPTION: &str = "Compiled continuation state for the node being finalized. This memory replaces the node's local working content for future continuation. Preserve only continuation-relevant state: completed or confirmed progress, key decisions and constraints, confirmed findings, validation results, unresolved factual gaps or risks, remaining work, and the logic linking evidence and findings to decisions and next steps. Use compact supporting evidence or precise, recoverable references wherever they clarify that logic. For source code, prefer repository-relative paths, symbols, and relevant line ranges; for program output, cite commands, artifact paths, and decisive results when needed to avoid replaying completed investigation. Treat inherited ancestor context as already available. Runtime preserves user messages and child memories, so do not copy them verbatim. Preserve the continuation-relevant evolution of user intent: use [U#] anchors to resolve approvals, corrections, rejections, and elliptical replies to their concrete referents, and record the resulting semantic deltas in task scope, decisions, and progress.";

const OPEN_SUMMARY_DESCRIPTION: &str = "Concise, actionable, completable goal for the child node being opened. The transition call carrying this goal is retained in the child node's context.";
const NEXT_SUMMARY_DESCRIPTION: &str = "Concise goal for the next sibling node. Make it actionable and completable. The transition call carrying this goal is retained in the sibling's context; continuation state from the node being finalized belongs in memory.";
const TRIM_DESCRIPTION: &str = "Conservatively clean up one tagged tool-response projection; this never changes the Spine tree or creates memory. A TRIM_ID is live only for the immediately previous tool-result batch, and only in your next assistant tool request. After any later tool request it expires; if trim misses, treat the id as expired and continue. Use slice for needed visible evidence, snip only when useful facts are preserved elsewhere, and leave untrimmed if the original may still be needed.";

pub(crate) fn create_spine_tool(name: &str) -> ToolSpec {
    let function = match name {
        SPINE_OPEN => ResponsesApiTool {
            name: SPINE_OPEN.to_string(),
            description: "Open a concrete child node for one appropriately scoped goal under the current Spine cursor and set it as the scope for the next ReAct step.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "summary".to_string(),
                    JsonSchema::string(Some(OPEN_SUMMARY_DESCRIPTION.to_string())),
                )]),
                Some(vec!["summary".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        },
        SPINE_CLOSE => ResponsesApiTool {
            name: SPINE_CLOSE.to_string(),
            description: "Finalize the current Spine node with the supplied continuation memory and resume its immediate parent for the next ReAct step. Root-epoch nodes cannot be closed.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "memory".to_string(),
                    JsonSchema::string(Some(NODE_MEMORY_DESCRIPTION.to_string())),
                )]),
                Some(vec!["memory".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        },
        SPINE_NEXT => ResponsesApiTool {
            name: SPINE_NEXT.to_string(),
            description: "Finalize the current Spine node with the supplied continuation memory and continue in a fresh sibling under the same parent for the next ReAct step.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    (
                        "summary".to_string(),
                        JsonSchema::string(Some(NEXT_SUMMARY_DESCRIPTION.to_string())),
                    ),
                    (
                        "memory".to_string(),
                        JsonSchema::string(Some(NODE_MEMORY_DESCRIPTION.to_string())),
                    ),
                ]),
                Some(vec!["summary".to_string(), "memory".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        },
        _ => panic!("unknown Spine tool: {name}"),
    };

    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the Spine task tree.".to_string(),
        tools: vec![ResponsesApiNamespaceTool::Function(function)],
    })
}

pub(crate) fn create_spine_trim_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "TRIM_ID".to_string(),
            JsonSchema::string(Some(
                "Trim id attached to a tool response in the immediately previous tool-result batch; it expires after your next assistant tool request."
                    .to_string(),
            )),
        ),
        (
            "op".to_string(),
            JsonSchema::string_enum(
                vec![serde_json::json!("snip"), serde_json::json!("slice")],
                Some("Use snip only when useful facts are preserved elsewhere; use slice to keep the needed head, tail, or anchor window.".to_string()),
            ),
        ),
        (
            "head".to_string(),
            JsonSchema::integer(Some("For op=\"slice\", keep this many characters from the start of the current visible body. Mutually exclusive with tail and anchor.".to_string())),
        ),
        (
            "tail".to_string(),
            JsonSchema::integer(Some("For op=\"slice\", keep this many characters from the end of the current visible body. Mutually exclusive with head and anchor.".to_string())),
        ),
        (
            "anchor".to_string(),
            JsonSchema::string(Some("For op=\"slice\", locate this non-empty text in the current visible body and keep an anchor window. Mutually exclusive with head and tail.".to_string())),
        ),
        (
            "preceding".to_string(),
            JsonSchema::integer(Some("For anchor slice, keep this many complete lines before the anchor line.".to_string())),
        ),
        (
            "following".to_string(),
            JsonSchema::integer(Some("For anchor slice, keep this many complete lines after the anchor line.".to_string())),
        ),
    ]);
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the Spine task tree.".to_string(),
        tools: vec![ResponsesApiNamespaceTool::Function(ResponsesApiTool {
            name: SPINE_TRIM.to_string(),
            description: TRIM_DESCRIPTION.to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["TRIM_ID".to_string(), "op".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_schema_exposes_only_control_tools() {
        for name in [SPINE_OPEN, SPINE_CLOSE, SPINE_NEXT] {
            let ToolSpec::Namespace(namespace) = create_spine_tool(name) else {
                panic!("expected namespace spec");
            };
            assert_eq!(namespace.tools.len(), 1);
            let ResponsesApiNamespaceTool::Function(function) = &namespace.tools[0];
            assert_eq!(function.name, name);
            assert!(!function.name.contains("tree"));
        }
    }

    #[test]
    fn close_and_next_require_memory() {
        for (name, required) in [
            (SPINE_CLOSE, vec!["memory"]),
            (SPINE_NEXT, vec!["summary", "memory"]),
        ] {
            let ToolSpec::Namespace(namespace) = create_spine_tool(name) else {
                panic!("expected namespace spec");
            };
            let ResponsesApiNamespaceTool::Function(function) = &namespace.tools[0];
            let schema = serde_json::to_value(&function.parameters).unwrap();
            assert_eq!(schema["required"], serde_json::json!(required));
            assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        }
    }

    #[test]
    fn trim_schema_requires_id_and_operation() {
        let ToolSpec::Namespace(namespace) = create_spine_trim_tool() else {
            panic!("expected namespace spec");
        };
        let ResponsesApiNamespaceTool::Function(function) = &namespace.tools[0];
        assert_eq!(function.name, SPINE_TRIM);
        let schema = serde_json::to_value(&function.parameters).unwrap();
        assert_eq!(schema["required"], serde_json::json!(["TRIM_ID", "op"]));
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    }
}
