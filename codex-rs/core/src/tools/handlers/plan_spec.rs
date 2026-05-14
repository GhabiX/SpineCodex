use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub fn create_update_plan_tool() -> ToolSpec {
    let checkpoint_properties = BTreeMap::from([
        (
            "task".to_string(),
            JsonSchema::string(Some("Concrete checkpoint/task".to_string())),
        ),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
    ]);
    let checkpoint_schema = JsonSchema::object(
        checkpoint_properties,
        Some(vec!["task".to_string(), "status".to_string()]),
        Some(false.into()),
    );
    let leaf_scope_properties = BTreeMap::from([
        (
            "node".to_string(),
            JsonSchema::string(Some(
                "Existing editable Spine node id, such as 1.2. Omit for a future scope proposal."
                    .to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some("Scope summary".to_string())),
        ),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
        (
            "checkpoints".to_string(),
            JsonSchema::array(
                checkpoint_schema.clone(),
                Some("Concrete checkpoints/tasks inside this scope".to_string()),
            ),
        ),
    ]);
    let leaf_scope_schema = JsonSchema::object(
        leaf_scope_properties,
        Some(vec!["summary".to_string()]),
        Some(false.into()),
    );
    let child_scope_properties = BTreeMap::from([
        (
            "node".to_string(),
            JsonSchema::string(Some(
                "Existing editable Spine node id, such as 1.2. Omit for a future scope proposal."
                    .to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some("Scope summary".to_string())),
        ),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
        (
            "checkpoints".to_string(),
            JsonSchema::array(
                checkpoint_schema.clone(),
                Some("Concrete checkpoints/tasks inside this scope".to_string()),
            ),
        ),
        (
            "children".to_string(),
            JsonSchema::array(
                leaf_scope_schema.clone(),
                Some("Nested child scopes".to_string()),
            ),
        ),
    ]);
    let child_scope_schema = JsonSchema::object(
        child_scope_properties,
        Some(vec!["summary".to_string()]),
        Some(false.into()),
    );
    let root_scope_properties = BTreeMap::from([
        (
            "node".to_string(),
            JsonSchema::string(Some(
                "Existing editable Spine node id, such as 1.2. Omit to use the resolved anchor."
                    .to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some("Scope summary".to_string())),
        ),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
        (
            "checkpoints".to_string(),
            JsonSchema::array(
                checkpoint_schema,
                Some("Concrete checkpoints/tasks inside this scope".to_string()),
            ),
        ),
        (
            "children".to_string(),
            JsonSchema::array(child_scope_schema, Some("Nested child scopes".to_string())),
        ),
    ]);
    let root_scope_schema = JsonSchema::object(
        root_scope_properties,
        Some(vec!["summary".to_string()]),
        Some(false.into()),
    );
    let plan_item_properties = BTreeMap::from([
        ("step".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
    ]);
    let plantree_properties = BTreeMap::from([
        (
            "anchor".to_string(),
            JsonSchema::string(Some(
                "Editable Spine anchor node id. Omit to use the current editable scope."
                    .to_string(),
            )),
        ),
        ("root".to_string(), root_scope_schema),
    ]);

    let properties = BTreeMap::from([
        (
            "explanation".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
        (
            "plan".to_string(),
            JsonSchema::array(
                JsonSchema::object(
                    plan_item_properties,
                    Some(vec!["step".to_string(), "status".to_string()]),
                    Some(false.into()),
                ),
                Some("The list of steps".to_string()),
            ),
        ),
        (
            "spine_plantree".to_string(),
            JsonSchema::object(
                plantree_properties,
                Some(vec!["root".to_string()]),
                Some(false.into()),
            ),
        ),
        (
            "clear_spine_plantree".to_string(),
            JsonSchema::boolean(Some(
                "Set true only to explicitly clear the current Spine PlanTree.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "update_plan".to_string(),
        description: r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
When Spine is enabled, use spine_plantree to maintain the current editable task tree draft. This is planning only; it does not create or move Spine nodes. Omitting spine_plantree preserves the previous draft; use clear_spine_plantree only to clear it.
Future planned scopes may display as ~<predicted-id> to distinguish them from real Spine nodes.
"#
        .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["plan".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}
