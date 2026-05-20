#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used)]

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_features::Feature;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_segment_integration_close_compact_resume_reads_real_sidecar() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-close"),
                ev_spine_transition_call("close-1", "close", "segment done"),
                ev_completed("resp-close"),
            ]),
            sse(vec![
                ev_response_created("resp-close-compact"),
                ev_assistant_message("msg-close-compact", "Segment close memory."),
                ev_completed("resp-close-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-resume"),
                ev_assistant_message("msg-resume", "resumed from close memory"),
                ev_completed("resp-resume"),
            ]),
        ],
    )
    .await;

    let mut builder = spine_builder();
    let test = builder.build(&server).await?;
    let home = test.home.clone();
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("session should expose rollout path");

    test.submit_turn("finish first segment").await?;

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    assert_sidecar_core_files(&sidecar_dir)?;
    assert_state_cursor(&sidecar_dir, "1")?;
    assert_tree_transition(&sidecar_dir, "close", "1.1", "1")?;
    assert_mem_install_committed(&sidecar_dir, "1.1", "close")?;
    assert_memory_contains(
        &sidecar_dir.join("nodes/1/1/memory.md"),
        "Segment close memory.",
    )?;
    assert_raw_mirror_nonempty(&sidecar_dir)?;

    let mut resume_builder = spine_builder();
    let resumed = resume_builder
        .resume(&server, home, rollout_path.clone())
        .await?;
    resumed.submit_turn("resume after compact").await?;

    let requests = responses.requests();
    let resume_request = requests
        .last()
        .expect("resume turn should send a model request");
    assert!(
        resume_request.body_contains_text("Segment close memory."),
        "resume request should hydrate compacted sidecar memory: {resume_request:?}"
    );
    assert_sidecar_core_files(&sidecar_dir)?;
    assert_state_cursor(&sidecar_dir, "1")?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_segment_integration_close_compact_resume_preserves_child_and_parent_mem()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_spine_transition_call("open-1", "open", "open child"),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-close-child"),
                ev_spine_transition_call("close-child-1", "close", "child leaf done"),
                ev_completed("resp-close-child"),
            ]),
            sse(vec![
                ev_response_created("resp-close-child-compact"),
                ev_assistant_message("msg-close-child-compact", "Segment close child memory."),
                ev_completed("resp-close-child-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-close-parent"),
                ev_spine_transition_call("close-parent-1", "close", "parent scope done"),
                ev_completed("resp-close-parent"),
            ]),
            sse(vec![
                ev_response_created("resp-close-parent-compact"),
                ev_assistant_message("msg-close-parent-compact", "Segment close parent memory."),
                ev_completed("resp-close-parent-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-close-resume"),
                ev_assistant_message("msg-close-resume", "resumed from close memory"),
                ev_completed("resp-close-resume"),
            ]),
        ],
    )
    .await;

    let mut builder = spine_builder();
    let test = builder.build(&server).await?;
    let home = test.home.clone();
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("session should expose rollout path");

    test.submit_turn("open child segment").await?;
    test.submit_turn("close child").await?;
    test.submit_turn("close parent").await?;

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    assert_sidecar_core_files(&sidecar_dir)?;
    assert_state_cursor(&sidecar_dir, "1")?;
    assert_tree_transition(&sidecar_dir, "open", "1.1", "1.1.1")?;
    assert_tree_transition(&sidecar_dir, "close", "1.1.1", "1.1")?;
    assert_tree_transition(&sidecar_dir, "close", "1.1", "1")?;
    assert_mem_install_committed(&sidecar_dir, "1.1.1", "close")?;
    assert_mem_install_committed(&sidecar_dir, "1.1", "close")?;
    assert_memory_contains(
        &sidecar_dir.join("nodes/1/1/1/memory.md"),
        "Segment close child memory.",
    )?;
    assert_memory_contains(
        &sidecar_dir.join("nodes/1/1/memory.md"),
        "Segment close parent memory.",
    )?;

    let mut resume_builder = spine_builder();
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    resumed.submit_turn("resume after close compact").await?;

    let requests = responses.requests();
    let resume_request = requests
        .last()
        .expect("resume turn should send a model request");
    assert!(
        resume_request.body_contains_text("Segment close parent memory."),
        "resume should hydrate parent memory: {resume_request:?}"
    );
    assert!(
        resume_request.body_contains_text("Segment close child memory."),
        "current close-scoped compaction keeps child memory visible and auditable: {resume_request:?}"
    );

    Ok(())
}

fn spine_builder() -> core_test_support::test_codex::TestCodexBuilder {
    test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .features
            .enable(Feature::SpineTaskTree)
            .expect("enable spine task tree");
    })
}

fn ev_spine_transition_call(call_id: &str, name: &str, summary: &str) -> Value {
    let arguments = match name {
        "open" => "{}".to_string(),
        "close" => json!({
            "summary": summary,
        })
        .to_string(),
        _ => json!({
            "summary": summary,
        })
        .to_string(),
    };
    ev_function_call_with_namespace(call_id, "spine", name, &arguments)
}

fn sidecar_dir_for_rollout_path(rollout_path: &Path) -> PathBuf {
    let parent = rollout_path
        .parent()
        .expect("rollout path should have parent");
    let stem = rollout_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("rollout path should have UTF-8 stem");
    parent.join(format!("spine-{stem}"))
}

fn assert_sidecar_core_files(sidecar_dir: &Path) -> anyhow::Result<()> {
    for relative in [
        "tree.jsonl",
        "compact.index.jsonl",
        "trajs.index.jsonl",
        "raw/rollout.raw.jsonl",
    ] {
        let path = sidecar_dir.join(relative);
        assert!(path.exists(), "expected sidecar file {}", path.display());
    }
    Ok(())
}

fn assert_state_cursor(sidecar_dir: &Path, expected: &str) -> anyhow::Result<()> {
    let events = read_json_lines(sidecar_dir.join("tree.jsonl"))?;
    let state = events
        .iter()
        .rev()
        .find_map(|event| match event.get("type").and_then(Value::as_str) {
            Some("spine_initialized") | Some("projection_reset") => event.get("state"),
            Some("transition_applied") | Some("root_epoch_reset") => Some(event),
            _ => None,
        })
        .context("tree ledger should contain cursor-bearing state")?;
    let cursor = match state.get("type").and_then(Value::as_str) {
        Some("transition_applied") => state.get("to_node"),
        Some("root_epoch_reset") => state.get("next_leaf_id"),
        _ => state.get("cursor"),
    };
    assert_eq!(
        cursor.and_then(Value::as_str),
        Some(expected),
        "unexpected state cursor: {state:?}"
    );
    Ok(())
}

fn assert_tree_transition(
    sidecar_dir: &Path,
    op: &str,
    from_node: &str,
    to_node: &str,
) -> anyhow::Result<()> {
    let tree = read_json_lines(sidecar_dir.join("tree.jsonl"))?;
    assert!(
        tree.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("transition_applied")
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event.get("from_node").and_then(Value::as_str) == Some(from_node)
                && event.get("to_node").and_then(Value::as_str) == Some(to_node)
        }),
        "missing {op} transition {from_node} -> {to_node}: {tree:?}"
    );
    Ok(())
}

fn assert_mem_install_committed(sidecar_dir: &Path, node_id: &str, op: &str) -> anyhow::Result<()> {
    let index = read_json_lines(sidecar_dir.join("compact.index.jsonl"))?;
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("compact_started")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
        }),
        "missing compact start for {node_id} {op}: {index:?}"
    );
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("mem_install_committed")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event
                    .get("compact_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("sha1:"))
        }),
        "missing MemInstall for {node_id} {op}: {index:?}"
    );
    Ok(())
}

fn assert_memory_contains(path: &Path, expected: &str) -> anyhow::Result<()> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read memory {}", path.display()))?;
    assert!(
        contents.contains("spine:auto-compact-generated") && contents.contains(expected),
        "memory {} should contain generated marker and {expected:?}: {contents}",
        path.display()
    );
    Ok(())
}

fn assert_raw_mirror_nonempty(sidecar_dir: &Path) -> anyhow::Result<()> {
    let raw_mirror = read_json_lines(sidecar_dir.join("raw/rollout.raw.jsonl"))?;
    assert!(
        raw_mirror.iter().any(|event| {
            event.get("kind").and_then(Value::as_str) == Some("response_item")
                || event.get("type").and_then(Value::as_str) == Some("response_item")
        }),
        "raw mirror should contain response item facts: {raw_mirror:?}"
    );
    Ok(())
}

fn read_json_lines(path: impl AsRef<Path>) -> anyhow::Result<Vec<Value>> {
    let path = path.as_ref();
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse jsonl line"))
        .collect()
}
