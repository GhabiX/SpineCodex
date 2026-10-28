#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::test_codex_exec::test_codex_exec;
use std::time::Duration;

const BRANCH_PROMPT_MARKER: &str = "You are a spawned execution branch.";
const PARENT_PROMPT: &str = "split the work into two spawned branches";
const FINAL_ANSWER: &str = "both branches arrived";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|entry| entry.trim() == "zstd"));
    let body = if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).unwrap()
    } else {
        request.body.clone()
    };
    String::from_utf8_lossy(&body).contains(text)
}

async fn mount_child(server: &wiremock::MockServer, marker: &'static str, delay: Duration) {
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path_regex(".*/responses$"))
        .and(move |request: &wiremock::Request| {
            body_contains(request, BRANCH_PROMPT_MARKER) && body_contains(request, marker)
        })
        .respond_with(
            sse_response(sse(vec![
                ev_response_created(marker),
                ev_assistant_message(marker, &format!("{marker} memory")),
                ev_completed(marker),
            ]))
            .set_delay(delay),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// `codex exec` ends with its turn, and a turn that bore Spawn children stays
/// open until every child has arrived: the last message is the parent's answer
/// after both arrivals, not the reply it gave while waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exec_exits_only_after_every_spawned_child_arrives() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;

    // Any other parent sampling only waits.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path_regex(".*/responses$"))
        .and(|request: &wiremock::Request| !body_contains(request, BRANCH_PROMPT_MARKER))
        .respond_with(sse_response(sse(vec![
            ev_response_created("parent-wait"),
            ev_assistant_message("parent-wait", "waiting for the branches"),
            ev_completed("parent-wait"),
        ])))
        .with_priority(u8::MAX)
        .mount(&server)
        .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PARENT_PROMPT) && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("parent-spawn"),
            ev_function_call_with_namespace(
                "exec-spawn-call",
                "spine",
                "spawn",
                &serde_json::json!({"tasks": [
                    {"summary": "first", "prompt": "exec-first-branch"},
                    {"summary": "second", "prompt": "exec-second-branch"},
                ]})
                .to_string(),
            ),
            ev_completed("parent-spawn"),
        ]),
    )
    .await;
    mount_child(&server, "exec-first-branch", Duration::from_millis(300)).await;
    mount_child(&server, "exec-second-branch", Duration::from_millis(900)).await;
    let parent_final_request = |request: &wiremock::Request| {
        !body_contains(request, BRANCH_PROMPT_MARKER)
            && body_contains(request, "exec-first-branch memory")
            && body_contains(request, "exec-second-branch memory")
    };
    responses::mount_sse_once_match(
        &server,
        parent_final_request,
        sse(vec![
            ev_response_created("parent-final"),
            ev_assistant_message("parent-final", FINAL_ANSWER),
            ev_completed("parent-final"),
        ]),
    )
    .await;

    let last_message = test.cwd_path().join("last-message.txt");
    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--output-last-message")
        .arg(&last_message)
        .arg(PARENT_PROMPT)
        .timeout(Duration::from_secs(60))
        .assert()
        .success();

    assert_eq!(std::fs::read_to_string(&last_message)?.trim(), FINAL_ANSWER);
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests
            .iter()
            .filter(|request| parent_final_request(request))
            .count(),
        1
    );
    Ok(())
}
