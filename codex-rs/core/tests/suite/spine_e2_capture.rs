//! Opt-in wire capture for the reusable E2 differential driver. No live account is used.
use anyhow::Context;
use anyhow::Result;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::{self};
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::spine_test_codex;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

fn builder(root: &Path) -> TestCodexBuilder {
    let root = root.to_path_buf();
    spine_test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |info| {
            info.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(move |config| {
            config.cwd = root
                .join("workspace")
                .try_into()
                .expect("absolute workspace");
            config.codex_home = root.join("home").try_into().expect("absolute home");
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.supports_websockets = false;
        })
}

fn answer(id: &str, text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_assistant_message(&format!("{id}-message"), text),
        responses::ev_completed(id),
    ])
}

fn control(id: &str, tool: &str, args: Value) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_function_call_with_namespace(
            &format!("{id}-call"),
            "spine",
            tool,
            &args.to_string(),
        ),
        responses::ev_completed(id),
    ])
}

fn add_requests(records: &mut Vec<Value>, scenario: &str, channel: &str, mock: &ResponseMock) {
    records.extend(mock.requests().iter().enumerate().map(|(index, request)| {
        json!({"scenario": scenario, "channel": channel, "index": index, "body": request.body_json()})
    }));
}

async fn archive(test: &TestCodex, output: &Path, name: &str) -> Result<PathBuf> {
    test.codex.flush_rollout().await?;
    let source = test.codex.rollout_path().context("rollout path")?;
    let target = output.join(format!("{name}.jsonl"));
    std::fs::copy(source, &target)?;
    Ok(target)
}

async fn capture(root: &Path, output: &Path) -> Result<()> {
    std::fs::create_dir_all(output)?;
    let mut records = Vec::new();
    let server = responses::start_mock_server().await;
    let fold_mock = responses::mount_sse_sequence(
        &server,
        vec![
            control("open", "open", json!({"summary": "E2 work branch"})),
            answer("opened", "Branch is open."),
            answer("work", "Private branch evidence."),
            control("close", "close", json!({"memory": "E2 retained result"})),
            answer("closed", "Parent continued."),
        ],
    )
    .await;
    let test = builder(root).build_with_auto_env(&server).await?;
    test.submit_turn("Open a work branch.").await?;
    test.submit_turn("Do work inside the branch.").await?;
    test.submit_turn("Close with the retained result.").await?;
    let fold_rollout = archive(&test, output, "fold-rollout").await?;
    add_requests(&mut records, "fold", "root", &fold_mock);
    anyhow::ensure!(
        fold_mock.requests().len() == 5,
        "expected five actual fold requests"
    );
    test.codex.shutdown_and_wait().await?;

    let resume_server = responses::start_mock_server().await;
    let resume_mock =
        responses::mount_sse_once(&resume_server, answer("resumed", "Resumed parent.")).await;
    let replay = std::env::var_os("E2_RESUME_ROLLOUT")
        .map(PathBuf::from)
        .unwrap_or(fold_rollout);
    // A resumed session appends to its rollout: never hand it the immutable fixture.
    let replay_working = output.join("resume-working.jsonl");
    std::fs::copy(replay, &replay_working)?;
    let mut home = tempfile::tempdir()?;
    home.disable_cleanup(true);
    let resumed = builder(root)
        .resume(&resume_server, Arc::new(home), replay_working)
        .await?;
    resumed.submit_turn("Continue after cold resume.").await?;
    archive(&resumed, output, "resume-rollout").await?;
    add_requests(&mut records, "resume", "root", &resume_mock);
    anyhow::ensure!(
        resume_mock.requests().len() == 1,
        "expected one actual resume request"
    );
    resumed.codex.shutdown_and_wait().await?;

    let spawn_server = responses::start_mock_server().await;
    let decoded = |request: &wiremock::Request| -> String {
        let bytes = if request
            .headers
            .get("content-encoding")
            .is_some_and(|v| v == "zstd")
        {
            zstd::stream::decode_all(std::io::Cursor::new(&request.body))
                .expect("mock zstd request")
        } else {
            request.body.clone()
        };
        String::from_utf8(bytes).expect("JSON request")
    };
    let _parent = responses::mount_sse_once_match(
        &spawn_server,
        move |r: &wiremock::Request| {
            let text = decoded(r);
            !text.contains("You are a spawned execution branch.")
                && !text.contains("E2 first memory")
        },
        control(
            "spawn",
            "spawn",
            json!({"tasks": [
                {"summary": "first", "prompt": "E2 child first assignment"},
                {"summary": "second", "prompt": "E2 child second assignment"}
            ]}),
        ),
    )
    .await;
    let _first = responses::mount_sse_once_match(
        &spawn_server,
        move |r: &wiremock::Request| {
            let text = decoded(r);
            text.contains("You are a spawned execution branch.")
                && text.contains("E2 child first assignment")
        },
        answer("first", "E2 first memory"),
    )
    .await;
    let _second = responses::mount_sse_once_match(
        &spawn_server,
        move |r: &wiremock::Request| {
            let text = decoded(r);
            text.contains("You are a spawned execution branch.")
                && text.contains("E2 child second assignment")
        },
        answer("second", "E2 second memory"),
    )
    .await;
    let _followup = responses::mount_sse_once_match(
        &spawn_server,
        move |r: &wiremock::Request| {
            let text = decoded(r);
            !text.contains("You are a spawned execution branch.")
                && text.contains("E2 first memory")
                && text.contains("E2 second memory")
        },
        answer("spawn-done", "Spawn parent continued."),
    )
    .await;
    let spawned = builder(root)
        .with_spine_spawn()
        .build_with_auto_env(&spawn_server)
        .await?;
    spawned
        .submit_turn("Run the two E2 child assignments.")
        .await?;
    archive(&spawned, output, "spawn-rollout").await?;
    // ResponseMock records before its later route matcher runs, so a rejected
    // match can still appear in that recorder. The server log is authoritative.
    let actual = spawn_server
        .received_requests()
        .await
        .context("server log")?
        .into_iter()
        .filter(|request| request.method == "POST" && request.url.path().ends_with("/responses"))
        .map(|request| ResponsesRequest::from(request).body_json())
        .collect::<Vec<_>>();
    std::fs::write(
        output.join("spawn-raw-requests.json"),
        serde_json::to_string_pretty(&actual)?,
    )?;
    let channel_for = |body: &Value| {
        let texts = body["input"]
            .as_array()
            .expect("input array")
            .iter()
            .filter(|item| item["role"] == "user")
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>();
        if texts.iter().any(|text| {
            text.contains("You are a spawned execution branch.")
                && text.contains("E2 child first assignment")
        }) {
            "child-first"
        } else if texts.iter().any(|text| {
            text.contains("You are a spawned execution branch.")
                && text.contains("E2 child second assignment")
        }) {
            "child-second"
        } else if body["input"]
            .as_array()
            .expect("input array")
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "spawn-call")
        {
            "parent-return"
        } else {
            "parent-start"
        }
    };
    for channel in [
        "parent-start",
        "child-first",
        "child-second",
        "parent-return",
    ] {
        let requests = actual
            .iter()
            .filter(|body| channel_for(body) == channel)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            requests.len() == 1,
            "expected one actual spawn request on {channel}, got {}",
            requests.len()
        );
        records.push(
            json!({"scenario": "spawn", "channel": channel, "index": 0, "body": requests[0]}),
        );
    }
    spawned.codex.shutdown_and_wait().await?;
    std::fs::write(
        output.join("requests.json"),
        serde_json::to_string_pretty(&records)?,
    )?;
    Ok(())
}

#[test]
#[ignore = "E2 wire probe: set E2_CAPTURE_DIR and E2_WORKSPACE, run explicitly"]
fn spine_e2_capture() -> Result<()> {
    let output =
        PathBuf::from(std::env::var_os("E2_CAPTURE_DIR").context("E2_CAPTURE_DIR required")?);
    let root = PathBuf::from(std::env::var_os("E2_WORKSPACE").context("E2_WORKSPACE required")?);
    std::fs::create_dir_all(root.join("workspace"))?;
    std::fs::create_dir_all(root.join("home"))?;
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(16 * 1024 * 1024)
                .enable_all()
                .build()?
                .block_on(capture(&root, &output))
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("capture thread panicked"))?
}
