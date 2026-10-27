use std::collections::HashMap;

use crate::app::app_server_requests::ResolvedAppServerRequest;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::bottom_pane_view::ViewCompletion;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::history_cell;
use crate::key_hint;
use crate::keymap::ListKeymap;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use codex_app_server_protocol::ToolRequestUserInputAnswer;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::text::Span;

pub(crate) const QUESTION_ID: &str = "spine_spawn_failure_action";

pub(crate) struct SpineSpawnFailureGate {
    request: ToolRequestUserInputParams,
    selection: ListSelectionView,
    dismissed: bool,
}

impl SpineSpawnFailureGate {
    pub(crate) fn new(
        request: ToolRequestUserInputParams,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let question = request
            .questions
            .first()
            .expect("Spine failure gate must contain one question");
        let question_id = question.id.clone();
        let turn_id = request.turn_id.clone();
        let questions = request.questions.clone();
        let items = question
            .options
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|option| {
                let label = option.label;
                let response_label = label.clone();
                let answer_question_id = question_id.clone();
                let answer_turn_id = turn_id.clone();
                let answer_questions = questions.clone();
                SelectionItem {
                    name: format!("{label}:"),
                    description: Some(option.description),
                    actions: vec![Box::new(move |event_tx| {
                        let answers = HashMap::from([(
                            answer_question_id.clone(),
                            ToolRequestUserInputAnswer {
                                answers: vec![response_label.clone()],
                            },
                        )]);
                        event_tx.user_input_answer(
                            answer_turn_id.clone(),
                            ToolRequestUserInputResponse {
                                answers: answers.clone(),
                            },
                        );
                        event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::RequestUserInputResultCell {
                                questions: answer_questions.clone(),
                                answers,
                                interrupted: false,
                            },
                        )));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from(Span::raw(question.header.clone())));
        header.push(Line::from(Span::raw(question.question.clone())));
        let footer_hint = Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Enter).into(),
            " to confirm".into(),
        ]);
        let selection = ListSelectionView::new(
            SelectionViewParams {
                header: Box::new(header),
                footer_hint: Some(footer_hint),
                items,
                allow_cancel: false,
                initial_selected_idx: Some(0),
                ..Default::default()
            },
            app_event_tx,
            keymap,
        );
        Self {
            request,
            selection,
            dismissed: false,
        }
    }
}

impl BottomPaneView for SpineSpawnFailureGate {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        self.selection.handle_key_event(key_event);
    }

    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        self.selection.keymap_contexts()
    }

    fn is_complete(&self) -> bool {
        self.dismissed || self.selection.is_complete()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        if self.dismissed {
            Some(ViewCompletion::Cancelled)
        } else {
            self.selection.completion()
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.selection.selected_index()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.selection.on_ctrl_c()
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn dismiss_app_server_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let ResolvedAppServerRequest::UserInput { call_id } = request else {
            return false;
        };
        if self.request.item_id != *call_id {
            return false;
        }
        self.dismissed = true;
        true
    }
}

impl Renderable for SpineSpawnFailureGate {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.selection.render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.selection.desired_height(width)
    }
}

#[cfg(test)]
#[path = "spine_spawn_failure_gate_tests.rs"]
mod tests;
