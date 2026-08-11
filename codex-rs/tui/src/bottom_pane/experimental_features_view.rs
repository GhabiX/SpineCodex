use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::key_hint;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::keymap::primary_binding;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;

use codex_features::Feature;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;

const MIN_SPINE_SPAWN_CONCURRENT_THREADS: usize = 3;

pub(crate) struct ExperimentalFeatureItem {
    pub feature: Feature,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub max_concurrent_threads_per_session: Option<usize>,
}

pub(crate) struct ExperimentalFeaturesView {
    features: Vec<ExperimentalFeatureItem>,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    header: Box<dyn Renderable>,
    keymap: ListKeymap,
}

impl ExperimentalFeaturesView {
    pub(crate) fn new(
        mut features: Vec<ExperimentalFeatureItem>,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        for item in &mut features {
            if let Some(max_threads) = item.max_concurrent_threads_per_session.as_mut() {
                *max_threads = (*max_threads).max(MIN_SPINE_SPAWN_CONCURRENT_THREADS);
            }
        }

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Experimental features".bold()));
        header.push(Line::from(
            "Changes are saved to config.toml and apply to new sessions.".dim(),
        ));

        let mut view = Self {
            features,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            header: Box::new(header),
            keymap,
        };
        view.initialize_selection();
        view
    }

    fn initialize_selection(&mut self) {
        if self.visible_len() == 0 {
            self.state.selected_idx = None;
        } else if self.state.selected_idx.is_none() {
            self.state.selected_idx = Some(0);
        }
    }

    fn visible_len(&self) -> usize {
        self.features.len()
    }

    fn selected_capacity_is_adjustable(&self) -> bool {
        self.state
            .selected_idx
            .and_then(|idx| self.features.get(idx))
            .is_some_and(|item| item.max_concurrent_threads_per_session.is_some())
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        let mut rows = Vec::with_capacity(self.features.len());
        let selected_idx = self.state.selected_idx;
        for (idx, item) in self.features.iter().enumerate() {
            let prefix = if selected_idx == Some(idx) {
                '›'
            } else {
                ' '
            };
            let marker = if item.enabled { 'x' } else { ' ' };
            let name = match item.max_concurrent_threads_per_session {
                Some(max_threads) => {
                    let max_branches = max_threads.saturating_sub(1);
                    format!(
                        "{prefix} [{marker}] {}  Concurrent branch agents: {max_branches}",
                        item.name
                    )
                }
                None => format!("{prefix} [{marker}] {}", item.name),
            };
            rows.push(GenericDisplayRow {
                name,
                description: Some(item.description.clone()),
                ..Default::default()
            });
        }

        rows
    }

    fn move_up(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn move_down(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn page_up(&mut self) {
        let len = self.visible_len();
        let visible = MAX_POPUP_ROWS.min(len);
        self.state.page_up_clamped(len, visible);
    }

    fn page_down(&mut self) {
        let len = self.visible_len();
        let visible = MAX_POPUP_ROWS.min(len);
        self.state.page_down_clamped(len, visible);
    }

    fn jump_top(&mut self) {
        let len = self.visible_len();
        let visible = MAX_POPUP_ROWS.min(len);
        self.state.jump_top(len, visible);
    }

    fn jump_bottom(&mut self) {
        let len = self.visible_len();
        let visible = MAX_POPUP_ROWS.min(len);
        self.state.jump_bottom(len, visible);
    }

    fn toggle_selected(&mut self) {
        let Some(selected_idx) = self.state.selected_idx else {
            return;
        };

        if let Some(item) = self.features.get_mut(selected_idx) {
            item.enabled = !item.enabled;
        }
    }

    fn decrement_selected_capacity(&mut self) {
        let Some(selected_idx) = self.state.selected_idx else {
            return;
        };
        let Some(max_threads) = self
            .features
            .get_mut(selected_idx)
            .and_then(|item| item.max_concurrent_threads_per_session.as_mut())
        else {
            return;
        };
        if *max_threads > MIN_SPINE_SPAWN_CONCURRENT_THREADS {
            *max_threads -= 1;
        }
    }

    fn increment_selected_capacity(&mut self) {
        let Some(selected_idx) = self.state.selected_idx else {
            return;
        };
        let Some(max_threads) = self
            .features
            .get_mut(selected_idx)
            .and_then(|item| item.max_concurrent_threads_per_session.as_mut())
        else {
            return;
        };
        if let Some(next) = max_threads.checked_add(1) {
            *max_threads = next;
        }
    }

    fn rows_width(total_width: u16) -> u16 {
        total_width.saturating_sub(2)
    }
}

impl BottomPaneView for ExperimentalFeaturesView {
    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::List)
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            _ if self.keymap.move_up.is_pressed(key_event) => self.move_up(),
            _ if self.keymap.move_down.is_pressed(key_event) => self.move_down(),
            _ if self.keymap.page_up.is_pressed(key_event) => self.page_up(),
            _ if self.keymap.page_down.is_pressed(key_event) => self.page_down(),
            _ if self.keymap.jump_top.is_pressed(key_event) => self.jump_top(),
            _ if self.keymap.jump_bottom.is_pressed(key_event) => self.jump_bottom(),
            _ if self.keymap.move_left.is_pressed(key_event) => {
                self.decrement_selected_capacity();
            }
            _ if self.keymap.move_right.is_pressed(key_event) => {
                self.increment_selected_capacity();
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle_selected(),
            _ if self.keymap.accept.is_pressed(key_event)
                || self.keymap.cancel.is_pressed(key_event) =>
            {
                self.on_ctrl_c();
            }
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        // Save the updates
        if !self.features.is_empty() {
            let updates = self
                .features
                .iter()
                .map(|item| (item.feature, item.enabled))
                .collect();
            let spine_spawn_max_concurrent_threads_per_session = self
                .features
                .iter()
                .find(|item| item.feature == Feature::SpineSpawn)
                .and_then(|item| item.max_concurrent_threads_per_session)
                .map(|max_threads| max_threads.max(MIN_SPINE_SPAWN_CONCURRENT_THREADS));
            self.app_event_tx.send(AppEvent::UpdateFeatureFlags {
                updates,
                spine_spawn_max_concurrent_threads_per_session,
            });
        }

        self.complete = true;
        CancellationEvent::Handled
    }
}

impl Renderable for ExperimentalFeaturesView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        Block::default()
            .style(user_message_style())
            .render(content_area, buf);

        let header_height = self
            .header
            .desired_height(content_area.width.saturating_sub(4));
        let rows = self.build_rows();
        let rows_width = Self::rows_width(content_area.width);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );
        let [header_area, _, list_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(rows_height),
        ])
        .areas(content_area.inset(Insets::vh(/*v*/ 1, /*h*/ 2)));

        self.header.render(header_area, buf);

        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: rows_width.max(1),
                height: list_area.height,
            };
            render_rows(
                render_area,
                buf,
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                "  No experimental features available for now",
            );
        }

        let hint_area = Rect {
            x: footer_area.x + 2,
            y: footer_area.y,
            width: footer_area.width.saturating_sub(2),
            height: footer_area.height,
        };
        experimental_popup_hint_line(
            &self.keymap,
            self.selected_capacity_is_adjustable(),
            hint_area.width,
        )
        .dim()
        .render(hint_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let rows = self.build_rows();
        let rows_width = Self::rows_width(width);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );

        let mut height = self.header.desired_height(width.saturating_sub(4));
        height = height.saturating_add(rows_height + 3);
        height.saturating_add(1)
    }
}

fn experimental_popup_hint_line(
    keymap: &ListKeymap,
    show_capacity_controls: bool,
    available_width: u16,
) -> Line<'static> {
    let accept = keymap
        .primary_hint(ListAction::Accept)
        .unwrap_or_else(|| key_hint::plain(KeyCode::Enter).into());
    let toggle: Vec<Span<'static>> =
        vec![key_hint::plain(KeyCode::Char(' ')).into(), " toggle".into()];
    let save: Vec<Span<'static>> = vec![accept.into(), " save".into()];
    let save_for_next_session: Vec<Span<'static>> =
        vec![accept.into(), " save for next session".into()];
    let mut candidates = Vec::new();

    if show_capacity_controls
        && let (Some(move_left), Some(move_right)) = (
            primary_binding(&keymap.move_left),
            primary_binding(&keymap.move_right),
        )
    {
        let binding_pair: Vec<Span<'static>> =
            vec![move_left.into(), "/".into(), move_right.into()];
        let mut capacity = binding_pair.clone();
        capacity.push(" branch agents".into());

        candidates.extend([
            join_hint_groups(&[&toggle, &capacity, &save_for_next_session]),
            join_hint_groups(&[&toggle, &capacity, &save]),
            join_hint_groups(&[&capacity, &save]),
            Line::from(capacity),
            Line::from(binding_pair),
        ]);
    }

    candidates.extend([
        join_hint_groups(&[&toggle, &save_for_next_session]),
        join_hint_groups(&[&toggle, &save]),
        Line::from(toggle),
        Line::from(save),
        Line::default(),
    ]);
    candidates
        .into_iter()
        .find(|hint| hint.width() <= usize::from(available_width))
        .unwrap_or_default()
}

fn join_hint_groups(groups: &[&[Span<'static>]]) -> Line<'static> {
    let mut spans = Vec::new();
    for group in groups {
        if !spans.is_empty() {
            spans.push("  ".into());
        }
        spans.extend_from_slice(group);
    }
    Line::from(spans)
}

#[cfg(test)]
#[path = "experimental_features_view_tests.rs"]
mod tests;
