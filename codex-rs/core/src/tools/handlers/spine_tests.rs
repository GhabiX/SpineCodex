use super::SpineHandler;
use super::SpineTool;
use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::tests::make_session_and_context;
use crate::session::turn_context::TurnContext;
use crate::spine::runtime::SpineRuntime;
use crate::spine::store::SpineSidecarStore;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::spine_spec::create_spine_namespace_tool;
use crate::tools::registry::ToolHandler;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_tools::JsonSchemaPrimitiveType;
use codex_tools::JsonSchemaType;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

async fn session_and_turn_with_spine() -> (TempDir, Session, TurnContext) {
    let (mut session, turn) = make_session_and_context().await;
    let temp = tempfile::tempdir().expect("create tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("create store");
    let runtime = SpineRuntime::create(store).expect("create spine runtime");
    session.spine = Some(Arc::new(Mutex::new(runtime)));
    (temp, session, turn)
}

fn spine_base(temp: &TempDir) -> String {
    temp.path().join("spine-rollout-test").display().to_string()
}

fn invocation(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: &str,
    tool: SpineTool,
    arguments: serde_json::Value,
) -> ToolInvocation {
    ToolInvocation {
        session,
        turn,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
        call_id: call_id.to_string(),
        tool_name: codex_tools::ToolName::namespaced(crate::spine::SPINE_NAMESPACE, tool.name()),
        source: ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}

fn transition_args() -> serde_json::Value {
    json!({
        "summary": "root scope",
    })
}

fn close_args() -> serde_json::Value {
    json!({
        "summary": "current scope",
    })
}

fn open_args() -> serde_json::Value {
    json!({})
}

fn handler(tool: SpineTool) -> SpineHandler {
    SpineHandler { tool }
}

fn spine_call(tool: SpineTool, call_id: &str, arguments: serde_json::Value) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: tool.name().to_string(),
        namespace: Some(crate::spine::SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("Spine updated.".to_string()),
            success: Some(true),
        },
    }
}

#[test]
fn transition_schema_exposes_open_close_only() {
    let ToolSpec::Namespace(namespace) = create_spine_namespace_tool() else {
        panic!("expected namespace tool");
    };
    let expected_tools = [
        (crate::spine::SPINE_TOOL_TREE, false, false),
        (crate::spine::SPINE_TOOL_OPEN, false, false),
        (crate::spine::SPINE_TOOL_CLOSE, true, true),
    ];
    assert_eq!(namespace.tools.len(), expected_tools.len());

    for (name, expect_summary, expect_instruction) in expected_tools {
        let tool = namespace
            .tools
            .iter()
            .find_map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool) if tool.name == name => Some(tool),
                ResponsesApiNamespaceTool::Function(_) => None,
            })
            .unwrap_or_else(|| panic!("expected spine tool {name}"));
        let properties = tool
            .parameters
            .properties
            .as_ref()
            .expect("transition tool should have properties");

        assert_eq!(properties.contains_key("summary"), expect_summary);
        assert!(!properties.contains_key("child_summary"));
        assert_eq!(properties.contains_key("instruction"), expect_instruction);
        let mut expected_required = Vec::new();
        if expect_summary {
            expected_required.push("summary".to_string());
        }
        assert_eq!(tool.parameters.required.as_ref(), Some(&expected_required));
        if expect_instruction {
            assert_eq!(
                properties
                    .get("instruction")
                    .expect("instruction property")
                    .schema_type,
                Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::String))
            );
        }
    }
}

#[tokio::test]
async fn valid_open_stages_transition_without_advancing_cursor() {
    let (temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = handler(SpineTool::Open)
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-spine",
            SpineTool::Open,
            open_args(),
        ))
        .await
        .expect("spine open should stage");

    assert_eq!(
        output.log_preview(),
        format!(
            "Current:  1.1.1\nBase: {}\n\n1: suspended\n    1.1: suspended\n        1.1.1: Current",
            spine_base(&temp)
        )
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert_eq!(runtime.cursor().bracketed(), "[1.1]");
    let staged = runtime
        .staged_transition()
        .expect("transition should be staged");
    assert_eq!(staged.call_id.as_str(), "call-spine");
    assert_eq!(staged.turn_id.as_str(), turn.sub_id.as_str());
    assert_eq!(staged.from_node.bracketed(), "[1.1]");
    assert_eq!(staged.to_node.bracketed(), "[1.1.1]");
    assert_eq!(
        staged
            .visible_spine
            .iter()
            .map(|node| node.bracketed())
            .collect::<Vec<_>>(),
        vec![
            "[1]".to_string(),
            "[1.1]".to_string(),
            "[1.1.1]".to_string()
        ]
    );
}

#[tokio::test]
async fn valid_close_returns_to_parent_tree_view() {
    let (temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    handler(SpineTool::Open)
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-open",
            SpineTool::Open,
            open_args(),
        ))
        .await
        .expect("spine open should stage");
    {
        let mut runtime = session.spine.as_ref().expect("spine runtime").lock().await;
        runtime
            .after_response_items_recorded(
                "turn-open",
                &[
                    spine_call(SpineTool::Open, "call-open", open_args()),
                    function_call_output("call-open"),
                ],
                0,
                2,
            )
            .expect("commit open");
        runtime.take_last_committed_transition();
    }

    let output = handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-close",
            SpineTool::Close,
            json!({
                "summary": "Completed reproduction and patch verification",
            }),
        ))
        .await
        .expect("spine close should stage");

    assert_eq!(
        output.log_preview(),
        format!(
            "Current:  1.1\nBase: {}\n\n1: suspended\n    1.1: Current\n        1.1.1: closed Completed reproduction and patch verification [memory already in context]",
            spine_base(&temp)
        )
    );
}

#[tokio::test]
async fn close_accepts_instruction_and_stages_it() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-close",
            SpineTool::Close,
            json!({
                "summary": "Completed reproduction and patch verification",
                "instruction": " preserve failing command and verification result ",
            }),
        ))
        .await
        .expect("spine close should stage");

    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    let staged = runtime
        .staged_transition()
        .expect("transition should be staged");
    assert_eq!(
        staged.compact_instruction.as_deref(),
        Some("preserve failing command and verification result")
    );
}

#[tokio::test]
async fn plan_mode_rejects_before_staging() {
    let (_temp, session, mut turn) = session_and_turn_with_spine().await;
    turn.collaboration_mode.mode = ModeKind::Plan;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let err = handler(SpineTool::Open)
        .handle(invocation(
            Arc::clone(&session),
            turn,
            "call-spine",
            SpineTool::Open,
            open_args(),
        ))
        .await
        .expect_err("plan mode should reject spine");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("spine is not allowed in Plan mode".to_string())
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn code_mode_rejects_before_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let mut invocation = invocation(
        Arc::clone(&session),
        turn,
        "call-spine",
        SpineTool::Open,
        open_args(),
    );
    invocation.source = ToolCallSource::CodeMode {
        cell_id: "cell-1".to_string(),
        runtime_tool_call_id: "runtime-call-1".to_string(),
    };

    let err = handler(SpineTool::Open)
        .handle(invocation)
        .await
        .expect_err("code mode should reject spine");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "spine is not available as a Code Mode nested tool".to_string()
        )
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn missing_runtime_rejects() {
    let (session, turn) = make_session_and_context().await;
    let err = handler(SpineTool::Open)
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Open,
            transition_args(),
        ))
        .await
        .expect_err("missing runtime should reject");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("spine is not enabled".to_string())
    );
}

#[tokio::test]
async fn unexpected_transition_arg_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = handler(SpineTool::Open)
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Open,
            json!({
                "summary": "root scope",
                "op": "jump",
            }),
        ))
        .await
        .expect_err("unexpected arg should reject");

    let FunctionCallError::RespondToModel(message) = err else {
        panic!("expected model-visible parse error");
    };
    assert!(message.contains("failed to parse function arguments"));
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn open_rejects_instruction_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = handler(SpineTool::Open)
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Open,
            json!({
                "summary": "root scope",
                "instruction": "no compact happens on open",
            }),
        ))
        .await
        .expect_err("open instruction should reject");

    let FunctionCallError::RespondToModel(message) = err else {
        panic!("expected model-visible parse error");
    };
    assert!(message.contains("failed to parse function arguments"));
    assert!(message.contains("unknown field"));
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn empty_instruction_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Close,
            json!({
                "summary": "root done",
                "instruction": "   ",
            }),
        ))
        .await
        .expect_err("empty instruction should reject");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel(
            "spine instruction must not be empty when provided".to_string()
        )
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn empty_summary_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Close,
            json!({
                "summary": "",
            }),
        ))
        .await
        .expect_err("empty summary should reject");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("spine summary must not be empty".to_string())
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn close_root_child_stages_parent_cursor() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            turn,
            "call-spine",
            SpineTool::Close,
            close_args(),
        ))
        .await
        .expect("root child close should stage");

    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    let staged = runtime
        .staged_transition()
        .expect("transition should be staged");
    assert_eq!(staged.from_node.bracketed(), "[1.1]");
    assert_eq!(staged.to_node.bracketed(), "[1]");
}

#[tokio::test]
async fn close_rejects_child_summary_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = handler(SpineTool::Close)
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            SpineTool::Close,
            json!({
                "child_summary": "legacy child",
                "summary": "current scope",
            }),
        ))
        .await
        .expect_err("child_summary should reject");

    let FunctionCallError::RespondToModel(message) = err else {
        panic!("expected model-visible parse error");
    };
    assert!(message.contains("failed to parse function arguments"));
    assert!(message.contains("unknown field"));
    assert!(message.contains("child_summary"));
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn tree_prints_current_tree_without_staging() {
    let (temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = handler(SpineTool::Tree)
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-tree",
            SpineTool::Tree,
            json!({}),
        ))
        .await
        .expect("spine tree should render");

    assert_eq!(
        output.log_preview(),
        format!(
            "Current:  1.1\nBase: {}\n\n1: suspended\n    1.1: Current",
            spine_base(&temp)
        )
    );
    assert_eq!(
        output.code_mode_result(&ToolPayload::Function {
            arguments: "{}".to_string()
        }),
        json!({
            "op": null,
            "cursor": "[1.1]",
            "tree": "1: suspended\n    1.1: Current",
        })
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn tree_is_allowed_in_plan_mode() {
    let (_temp, session, mut turn) = session_and_turn_with_spine().await;
    turn.collaboration_mode.mode = ModeKind::Plan;

    handler(SpineTool::Tree)
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "call-tree",
            SpineTool::Tree,
            json!({}),
        ))
        .await
        .expect("read-only tree should be allowed in Plan mode");
}
