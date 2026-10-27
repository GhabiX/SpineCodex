//! Spine features must not change native question delivery, answers, or replay identity.
use super::*;
use codex_context_fragments::AnsweredQuestion;
use codex_context_fragments::ContextualUserFragment;
use codex_protocol::items::AsyncUserInputQuestion;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn spine_async_question_answer_and_replay_match_native() {
    let thread_id = ThreadId::new();
    let item = AppServerThreadItem::AgentMessage {
        id: "question-message".into(),
        text: String::new(),
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: Some(vec![AsyncUserInputQuestion {
            title: "Which route?".into(),
            options: None,
        }]),
    };
    let notification = ServerNotification::ItemCompleted(ItemCompletedNotification {
        item: item.clone(),
        thread_id: thread_id.to_string(),
        turn_id: "question-turn".into(),
        completed_at_ms: 0,
    });
    let mut outcomes = Vec::new();
    for spine_enabled in [false, true] {
        let (mut chat, _rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(thread_id);
        chat.set_feature_enabled(Feature::SpineJit, spine_enabled);
        chat.set_feature_enabled(Feature::SpineSpawn, spine_enabled);
        chat.bottom_pane
            .set_composer_text("unfinished main draft".into(), Vec::new(), Vec::new());
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 1);
        let arrival = chat
            .pending_notification
            .take()
            .expect("question notification");
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        assert!(chat.pending_notification.is_none());
        assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 1);
        chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        let editor = render_bottom_popup(&chat, /*width*/ 80);
        chat.bottom_pane.handle_paste("Left route".into());
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let Op::UserTurn { items, .. } = ops.try_recv().expect("one native answer") else {
            panic!("question answer must be an ordinary user turn")
        };
        let question_id =
            serde_json::json!(["request_user_input_async", "question-message", 0]).to_string();
        assert_eq!(
            items,
            vec![UserInput::Text {
                text: AnsweredQuestion::new(&question_id, "Which route?", "Left route").render(),
                text_elements: Vec::new(),
            }]
        );
        assert!(ops.try_recv().is_err());
        assert_eq!(chat.bottom_pane.composer_text(), "unfinished main draft");
        assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 0);
        chat.replay_thread_item(
            item.clone(),
            "question-turn".into(),
            ReplayKind::ResumeInitialMessages,
        );
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 0);
        assert!(ops.try_recv().is_err());
        outcomes.push((
            arrival.display(),
            editor,
            items,
            chat.bottom_pane.composer_text(),
        ));
    }
    assert_eq!(outcomes[0], outcomes[1]);
}
