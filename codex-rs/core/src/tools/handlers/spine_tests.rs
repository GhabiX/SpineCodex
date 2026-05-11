use super::SpineHandler;
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
use crate::tools::registry::ToolHandler;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_protocol::config_types::ModeKind;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

async fn session_and_turn_with_spine() -> (TempDir, Session, TurnContext) {
    let (mut session, turn) = make_session_and_context().await;
    let temp = tempfile::tempdir().expect("create tempdir");
    let rollout_path = temp.path().join("rollout-test.jsonl");
    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("create store");
    let runtime = SpineRuntime::create(store).expect("create spine runtime");
    session.spine = Some(Arc::new(Mutex::new(runtime)));
    (temp, session, turn)
}

fn invocation(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: &str,
    arguments: serde_json::Value,
) -> ToolInvocation {
    ToolInvocation {
        session,
        turn,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
        call_id: call_id.to_string(),
        tool_name: codex_tools::ToolName::plain("spine"),
        source: ToolCallSource::Direct,
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    }
}

fn valid_args(op: &str) -> serde_json::Value {
    json!({
        "op": op,
        "summary": "root scope",
    })
}

#[tokio::test]
async fn valid_open_stages_transition_without_advancing_cursor() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-spine",
            valid_args("open"),
        ))
        .await
        .expect("spine open should stage");

    assert_eq!(
        output.log_preview(),
        "Spine updated: open\n\ncurrent: [1.1] live\n\n[1] opened root scope (nodes/1/worklog.md)\n|-- [1.1] live current (nodes/1/1/worklog.md)"
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert_eq!(runtime.cursor().bracketed(), "[1]");
    let staged = runtime
        .staged_transition()
        .expect("transition should be staged");
    assert_eq!(staged.call_id.as_str(), "call-spine");
    assert_eq!(staged.turn_id.as_str(), turn.sub_id.as_str());
    assert_eq!(staged.from_node.bracketed(), "[1]");
    assert_eq!(staged.to_node.bracketed(), "[1.1]");
    assert_eq!(
        staged
            .visible_spine
            .iter()
            .map(|node| node.bracketed())
            .collect::<Vec<_>>(),
        vec!["[1]".to_string(), "[1.1]".to_string()]
    );
}

#[tokio::test]
async fn valid_next_returns_compact_tree_view() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    {
        let spine = session.spine.as_ref().expect("spine runtime");
        let mut runtime = spine.lock().await;
        let mut state = runtime.state().clone();
        runtime
            .store()
            .record_transition(
                &mut state,
                crate::spine::store::SpineOperation::Open,
                "root scope",
                0,
            )
            .expect("record open");
        let store = runtime.store().clone();
        *runtime = SpineRuntime::from_parts(store, state, 0);
    }
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let output = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            Arc::clone(&turn),
            "call-next",
            json!({
                "op": "next",
                "summary": "Completed reproduction and patch verification",
            }),
        ))
        .await
        .expect("spine next should stage");

    assert_eq!(
        output.log_preview(),
        "Spine updated: next\n\ncurrent: [1.2] live\n\n[1] opened root scope (nodes/1/worklog.md)\n|-- [1.1] finished Completed reproduction and patch verification (nodes/1/1/worklog.md)\n|-- [1.2] live current (nodes/1/2/worklog.md)"
    );
}

#[tokio::test]
async fn plan_mode_rejects_before_staging() {
    let (_temp, session, mut turn) = session_and_turn_with_spine().await;
    turn.collaboration_mode.mode = ModeKind::Plan;
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let err = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            turn,
            "call-spine",
            valid_args("open"),
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
    let mut invocation = invocation(Arc::clone(&session), turn, "call-spine", valid_args("open"));
    invocation.source = ToolCallSource::CodeMode {
        cell_id: "cell-1".to_string(),
        runtime_tool_call_id: "runtime-call-1".to_string(),
    };

    let err = SpineHandler
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
    let err = SpineHandler
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "call-spine",
            valid_args("open"),
        ))
        .await
        .expect_err("missing runtime should reject");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("spine task tree is not enabled".to_string())
    );
}

#[tokio::test]
async fn invalid_operation_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            valid_args("jump"),
        ))
        .await
        .expect_err("invalid op should reject");

    let FunctionCallError::RespondToModel(message) = err else {
        panic!("expected model-visible parse error");
    };
    assert!(message.contains("failed to parse function arguments"));
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}

#[tokio::test]
async fn empty_summary_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            json!({
                "op": "open",
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
async fn close_on_root_rejects_without_staging() {
    let (_temp, session, turn) = session_and_turn_with_spine().await;
    let session = Arc::new(session);
    let err = SpineHandler
        .handle(invocation(
            Arc::clone(&session),
            Arc::new(turn),
            "call-spine",
            valid_args("close"),
        ))
        .await
        .expect_err("root close should reject");

    assert_eq!(
        err,
        FunctionCallError::RespondToModel("cannot close the root spine node".to_string())
    );
    let runtime = session.spine.as_ref().expect("spine runtime").lock().await;
    assert!(runtime.staged_transition().is_none());
}
