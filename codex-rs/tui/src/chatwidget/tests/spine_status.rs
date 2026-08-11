use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn organic_working_status_is_scoped_to_spine_jit_agent_turns() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.turn_lifecycle.last_turn_id = Some("turn-1".to_string());
    chat.set_feature_enabled(Feature::SpineJit, /*enabled*/ true);
    chat.turn_lifecycle.start(Instant::now());
    chat.update_task_running_state();

    assert_eq!(
        chat.bottom_pane
            .status_widget()
            .and_then(|status| status.organic_working_word()),
        Some(crate::motion::activity_word_for_identity("turn-1"))
    );

    chat.set_feature_enabled(Feature::SpineJit, /*enabled*/ false);
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.organic_working_word(), None);
    assert_eq!(status.header(), "Working");
    assert!(!status.header_is_reasoning());
}

#[tokio::test]
async fn reasoning_status_provenance_survives_operational_override_and_restore() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.turn_lifecycle.last_turn_id = Some("turn-1".to_string());
    chat.set_feature_enabled(Feature::SpineJit, /*enabled*/ true);
    chat.turn_lifecycle.start(Instant::now());
    chat.update_task_running_state();

    chat.on_agent_reasoning_delta("**Planning memory rollout inspection**".to_string());
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.header(), "Planning memory rollout inspection");
    assert!(status.header_is_reasoning());

    chat.bottom_pane.hide_status_indicator();
    chat.status_state.pending_status_indicator_restore = true;
    chat.maybe_restore_status_indicator_after_stream_idle();
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("restored status widget");
    assert_eq!(status.header(), "Planning memory rollout inspection");
    assert!(status.header_is_reasoning());

    chat.status_state.remember_retry_status_header();
    chat.set_status_header("Reconnecting... 2/5".to_string());
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.header(), "Reconnecting... 2/5");
    assert!(!status.header_is_reasoning());

    chat.restore_retry_status_header_if_present();
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.header(), "Planning memory rollout inspection");
    assert!(status.header_is_reasoning());

    chat.set_status_header("Waiting for background terminal".to_string());
    chat.restore_reasoning_status_header();
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.header(), "Planning memory rollout inspection");
    assert!(status.header_is_reasoning());

    chat.reasoning_buffer.clear();
    chat.reasoning_header = None;
    chat.restore_reasoning_status_header();
    let status = chat.bottom_pane.status_widget().expect("status widget");
    assert_eq!(status.header(), "Working");
    assert!(!status.header_is_reasoning());
}
