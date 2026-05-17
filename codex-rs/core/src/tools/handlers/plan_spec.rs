use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub fn create_update_plan_tool() -> ToolSpec {
    create_update_plan_tool_with_options(UpdatePlanToolOptions {
        include_task_projection: false,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdatePlanToolOptions {
    pub include_task_projection: bool,
}

pub fn create_update_plan_tool_with_options(options: UpdatePlanToolOptions) -> ToolSpec {
    let plan_item_properties = BTreeMap::from([
        ("step".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
    ]);
    let plan_item_schema = JsonSchema::object(
        plan_item_properties,
        Some(vec!["step".to_string(), "status".to_string()]),
        Some(false.into()),
    );
    let mut properties = BTreeMap::from([
        (
            "explanation".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
        (
            "plan".to_string(),
            JsonSchema::array(
                plan_item_schema.clone(),
                Some(
                    "The current checklist. When Spine is enabled, this is the current real Spine node's checklist."
                        .to_string(),
                ),
            ),
        ),
    ]);
    if options.include_task_projection {
        let projection_current_properties = BTreeMap::from([
            (
                "node_id".to_string(),
                JsonSchema::string(Some(
                    "Current real Spine node id. For the MVP this must match the runtime cursor."
                        .to_string(),
                )),
            ),
            (
                "checklist".to_string(),
                JsonSchema::array(
                    plan_item_schema.clone(),
                    Some(
                        "Checklist for the current real Spine node; this is normalized to the flat plan."
                            .to_string(),
                    ),
                ),
            ),
        ]);
        let projection_current_schema = JsonSchema::object(
            projection_current_properties,
            Some(vec!["node_id".to_string()]),
            Some(false.into()),
        );
        let projection_draft_properties = BTreeMap::from([
            (
                "draft_id".to_string(),
                JsonSchema::string(Some(
                    "Optional local draft id for nested draft references. If provided, it must start with '~' and is not a real Spine node id."
                        .to_string(),
                )),
            ),
            (
                "parent".to_string(),
                JsonSchema::string(Some(
                    "Editable real Spine node id or earlier draft_id that this draft scope is under."
                        .to_string(),
                )),
            ),
            (
                "summary".to_string(),
                JsonSchema::string(Some("Draft scope summary".to_string())),
            ),
            (
                "checklist".to_string(),
                JsonSchema::array(
                    plan_item_schema,
                    Some("Checklist items for this future draft scope".to_string()),
                ),
            ),
        ]);
        let projection_draft_schema = JsonSchema::object(
            projection_draft_properties,
            Some(vec!["parent".to_string(), "summary".to_string()]),
            Some(false.into()),
        );
        let task_projection_properties = BTreeMap::from([
            ("current".to_string(), projection_current_schema),
            (
                "draft_nodes".to_string(),
                JsonSchema::array(
                    projection_draft_schema,
                    Some(
                        "Future draft scopes. Order among entries with the same parent is sibling order."
                            .to_string(),
                    ),
                ),
            ),
        ]);
        properties.insert(
            "task_projection".to_string(),
            JsonSchema::object(
                task_projection_properties,
                Some(vec!["current".to_string()]),
                Some(false.into()),
            ),
        );
    }

    let description = if options.include_task_projection {
        r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
When Spine is enabled, use task_projection for model-authored planning. task_projection.current.checklist is normalized to the current real Spine node's flat plan, and task_projection.draft_nodes is normalized to the editable task tree draft. task_projection is a draft projection only: it does not create, finish, close, compact, or move Spine nodes. Do not combine task_projection with top-level plan.
Successful writable Spine updates return JSON containing the updated spine_tree; treat that returned tree as the authoritative planning state.
The returned tree may contain a normalized spine_plantree; that is runtime-normalized draft state, not a model-authored input path.
Future draft scopes may display as ~<predicted-id> to distinguish them from real Spine nodes.
"#
    } else {
        r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
"#
    };

    ToolSpec::Function(ResponsesApiTool {
        name: "update_plan".to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            if options.include_task_projection {
                None
            } else {
                Some(vec!["plan".to_string()])
            },
            Some(false.into()),
        ),
        output_schema: None,
    })
}
