use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::ThreadItem;

const ITEM_ID_PREFIX: &str = "spine-ui-";
const SERVER_NAME: &str = "__codex_internal_spine_tree_ui__";
const TOOL_NAME: &str = "spine_tree";
const RESOURCE_URI: &str = "ui://spine/tree.html";

pub(crate) fn is_item(item: &ThreadItem) -> bool {
    let ThreadItem::McpToolCall {
        id,
        server,
        tool,
        mcp_app_resource_uri,
        ..
    } = item
    else {
        return false;
    };
    id.starts_with(ITEM_ID_PREFIX)
        && server == SERVER_NAME
        && tool == TOOL_NAME
        && mcp_app_resource_uri.as_deref() == Some(RESOURCE_URI)
}

pub(crate) fn is_server_status(status: &McpServerStatus) -> bool {
    status.name == SERVER_NAME
        && status.tools.get(TOOL_NAME).is_some_and(|tool| {
            tool.meta
                .as_ref()
                .and_then(|meta| meta.pointer("/ui/resourceUri"))
                .and_then(serde_json::Value::as_str)
                == Some(RESOURCE_URI)
        })
}
