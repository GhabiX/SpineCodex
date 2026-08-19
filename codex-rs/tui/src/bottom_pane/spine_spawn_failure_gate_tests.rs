use super::*;

use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::keymap::RuntimeKeymap;
use codex_app_server_protocol::ToolRequestUserInputOption;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputQuestion;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tokio::sync::mpsc::unbounded_channel;

fn request() -> ToolRequestUserInputParams {
    ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        item_id: "call-1".to_string(),
        questions: vec![ToolRequestUserInputQuestion {
            id: QUESTION_ID.to_string(),
            header: "Spawn failed".to_string(),
            question: "2 of 3 spawned branches failed. Choose what to do with the failed branches."
                .to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                ToolRequestUserInputOption {
                    label: "Continue".to_string(),
                    description: "Resume with each failed branch's existing context.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "Retry".to_string(),
                    description: "Start each failed branch again in a new agent.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "Abandon".to_string(),
                    description: "Return the failures to the parent agent.".to_string(),
                },
            ]),
        }],
        is_blocking: true,
        auto_resolution_ms: None,
    }
}

fn view_and_receiver() -> (
    SpineSpawnFailureGate,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (raw_tx, raw_rx) = unbounded_channel();
    let sender = AppEventSender::new(raw_tx);
    let view = SpineSpawnFailureGate::new(request(), sender, RuntimeKeymap::defaults().list);
    (view, raw_rx)
}

#[test]
fn enter_returns_the_selected_action() {
    let (mut view, mut rx) = view_and_receiver();
    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let event = rx.try_recv().expect("selection should emit an event");
    let AppEvent::CodexOp(AppCommand::UserInputAnswer { id, response }) = event else {
        panic!("expected user input answer event");
    };
    assert_eq!(id, "turn-1");
    assert_eq!(
        response
            .answers
            .get(QUESTION_ID)
            .map(|answer| answer.answers.as_slice()),
        Some(["Continue".to_string()].as_slice())
    );
    assert!(view.is_complete());
}

#[test]
fn number_key_selects_retry_and_esc_is_a_noop() {
    let (mut view, mut rx) = view_and_receiver();
    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!view.is_complete());
    assert!(rx.try_recv().is_err());
    assert_eq!(view.on_ctrl_c(), CancellationEvent::NotHandled);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    let event = rx
        .try_recv()
        .expect("number selection should emit an event");
    let AppEvent::CodexOp(AppCommand::UserInputAnswer { response, .. }) = event else {
        panic!("expected user input answer event");
    };
    assert_eq!(
        response
            .answers
            .get(QUESTION_ID)
            .map(|answer| answer.answers.as_slice()),
        Some(["Retry".to_string()].as_slice())
    );
}

#[test]
fn renders_native_white_summary_and_explained_actions() {
    let (view, _rx) = view_and_receiver();
    let width = 100;
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    let rendered = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered);

    let summary = "2 of 3 spawned branches failed. Choose what to do with the failed branches.";
    let summary_y = (0..height)
        .find(|y| {
            (0..width)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains(summary)
        })
        .expect("rendered gate should include its failure summary");
    let summary_start = (0..width)
        .map(|x| buffer[(x, summary_y)].symbol())
        .collect::<String>()
        .find(summary)
        .expect("failure summary start") as u16;
    for x in summary_start..summary_start + summary.len() as u16 {
        assert_eq!(
            buffer[(x, summary_y)].fg,
            Color::Reset,
            "failure summary must use the native terminal foreground color"
        );
    }
}
