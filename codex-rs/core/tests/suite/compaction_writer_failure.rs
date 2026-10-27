use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

const COMPACTED_CONTEXT: &str = "CLOSED_WRITER_COMPACTED_CONTEXT";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_compaction_publishes_after_writer_shutdown() -> Result<()> {
    assert_closed_writer_compaction(CompactionMode::Native).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_compaction_keeps_history_after_writer_shutdown() -> Result<()> {
    assert_closed_writer_compaction(CompactionMode::Spine).await
}

#[derive(Clone, Copy)]
enum CompactionMode {
    Native,
    Spine,
}

async fn assert_closed_writer_compaction(mode: CompactionMode) -> Result<()> {
    let server = responses::start_mock_server().await;
    let responses = vec![
        responses::sse(vec![
            responses::ev_assistant_message("before", "BEFORE_COMPACTION"),
            responses::ev_completed("before-response"),
        ]),
        responses::sse(vec![
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": COMPACTED_CONTEXT,
                },
            }),
            responses::ev_completed("compact-response"),
        ]),
    ];
    let requests = responses::mount_sse_sequence(&server, responses).await;
    let builder = match mode {
        CompactionMode::Native => test_codex(),
        CompactionMode::Spine => spine_test_codex(),
    };
    let test = builder
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            let _ = config.features.disable(Feature::SpineSpawn);
            let _ = config.features.disable(Feature::SpinetreeMemoryProjection);
            match mode {
                CompactionMode::Native => {
                    let _ = config.features.disable(Feature::SpineJit);
                }
                CompactionMode::Spine => {
                    let _ = config.features.enable(Feature::SpineJit);
                }
            }
            let _ = config.features.enable(Feature::RemoteCompactionV2);
        })
        .build(&server)
        .await?;
    assert_eq!(
        test.config.features.enabled(Feature::SpineJit),
        matches!(mode, CompactionMode::Spine),
    );
    test.submit_text_turn("before closing the writer").await?;
    let before = test
        .codex
        .conversation_history_snapshot()
        .await
        .items()
        .cloned()
        .collect::<Vec<_>>();
    let thread_id = test.session_configured.session_id.into();
    test.thread_store.shutdown_thread(thread_id).await?;
    test.thread_store
        .flush_thread(thread_id)
        .await
        .expect_err("the actual local writer is closed");

    test.codex.submit(Op::Compact).await?;
    let mut completed_compactions = 0;
    let mut errors = Vec::new();
    loop {
        let event =
            tokio::time::timeout(std::time::Duration::from_secs(30), test.codex.next_event())
                .await??;
        match event.msg {
            EventMsg::ItemCompleted(event)
                if matches!(event.item, TurnItem::ContextCompaction(_)) =>
            {
                completed_compactions += 1;
            }
            EventMsg::Error(event) => errors.push(event),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    let after = test
        .codex
        .conversation_history_snapshot()
        .await
        .items()
        .cloned()
        .collect::<Vec<_>>();
    match mode {
        CompactionMode::Native => {
            assert_eq!(completed_compactions, 1);
            assert_eq!(errors, vec![]);
            assert!(after.iter().any(|item| matches!(
                item,
                ResponseItem::Compaction { encrypted_content, .. }
                    if encrypted_content == COMPACTED_CONTEXT
            )));
            assert_eq!(requests.requests().len(), 2);
            let followup = responses::mount_sse_once(
                &server,
                responses::sse(vec![responses::ev_completed("after-response")]),
            )
            .await;
            test.codex
                .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
                    codex_protocol::user_input::UserInput::Text {
                        text: "after compaction".into(),
                        text_elements: vec![],
                    },
                ]))
                .await?;
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnComplete(_))
            })
            .await;
            assert!(followup.single_request().input().iter().any(|item| {
                item["type"] == "compaction" && item["encrypted_content"] == COMPACTED_CONTEXT
            }));
        }
        CompactionMode::Spine => {
            assert_eq!(completed_compactions, 0);
            assert_eq!(errors.len(), 1);
            assert_eq!(requests.requests().len(), 2);
            assert_eq!(after, before);
        }
    }
    Ok(())
}
