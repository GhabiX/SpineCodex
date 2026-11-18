use std::borrow::Cow;
use std::cell::RefCell;
use std::path::Path;

use codex_protocol::ThreadId;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::clipboard_paste::PreparedFeedbackScreenshot;
use crate::clipboard_paste::SPINE_FEEDBACK_MAX_SCREENSHOTS;
use crate::clipboard_paste::normalize_pasted_path;
use crate::clipboard_paste::paste_feedback_image_as_png;
use crate::clipboard_paste::prepare_feedback_image_path;
use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::selection_popup_common::menu_surface_inset;
use super::selection_popup_common::menu_surface_padding_height;
use super::selection_popup_common::render_menu_surface;
use super::selection_popup_common::wrap_styled_line;
use super::textarea::TextArea;
use super::textarea::TextAreaState;

const SPINE_FEEDBACK_MAX_NOTE_BYTES: usize = 8 * 1024;
const NOTE_INPUT_MAX_HEIGHT: u16 = 6;
pub(crate) const SPINE_FEEDBACK_VIEW_ID: &str = "spine-feedback";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineFeedbackDraft {
    pub(crate) thread_id: ThreadId,
    pub(crate) note: String,
    pub(crate) screenshots: Vec<PreparedFeedbackScreenshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Editing,
    Consent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Note,
    Screenshot(usize),
}

struct EditingLayout {
    before_input: Vec<Line<'static>>,
    input_height: u16,
    after_input: Vec<Line<'static>>,
}

impl EditingLayout {
    fn height(&self) -> u16 {
        u16::try_from(self.before_input.len())
            .unwrap_or(u16::MAX)
            .saturating_add(self.input_height)
            .saturating_add(u16::try_from(self.after_input.len()).unwrap_or(u16::MAX))
    }
}

pub(crate) struct SpineFeedbackView {
    draft: SpineFeedbackDraft,
    app_event_tx: AppEventSender,
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    stage: Stage,
    focus: Focus,
    error: Option<String>,
    complete: bool,
}

impl SpineFeedbackView {
    pub(crate) fn new(thread_id: ThreadId, app_event_tx: AppEventSender) -> Self {
        Self::with_draft(
            SpineFeedbackDraft {
                thread_id,
                note: String::new(),
                screenshots: Vec::new(),
            },
            None,
            app_event_tx,
        )
    }

    pub(crate) fn with_draft(
        draft: SpineFeedbackDraft,
        error: Option<String>,
        app_event_tx: AppEventSender,
    ) -> Self {
        let mut textarea = TextArea::new();
        textarea.set_text_clearing_elements(&draft.note);
        textarea.set_cursor(draft.note.len());
        Self {
            draft,
            app_event_tx,
            textarea,
            textarea_state: RefCell::new(TextAreaState::default()),
            stage: Stage::Editing,
            focus: Focus::Note,
            error,
            complete: false,
        }
    }

    fn sync_note(&mut self) {
        self.draft.note = self.textarea.text().to_string();
    }

    fn existing_png_bytes(&self) -> usize {
        self.draft
            .screenshots
            .iter()
            .map(|screenshot| screenshot.png.len())
            .sum()
    }

    fn add_screenshot(&mut self, screenshot: PreparedFeedbackScreenshot) {
        if self.draft.screenshots.len() >= SPINE_FEEDBACK_MAX_SCREENSHOTS {
            self.error = Some(format!(
                "You can attach at most {SPINE_FEEDBACK_MAX_SCREENSHOTS} screenshots."
            ));
            return;
        }
        self.draft.screenshots.push(screenshot);
        self.focus = Focus::Screenshot(self.draft.screenshots.len() - 1);
        self.error = None;
    }

    fn paste_clipboard_screenshot(&mut self) {
        if self.draft.screenshots.len() >= SPINE_FEEDBACK_MAX_SCREENSHOTS {
            self.error = Some(format!(
                "You can attach at most {SPINE_FEEDBACK_MAX_SCREENSHOTS} screenshots."
            ));
            return;
        }
        match paste_feedback_image_as_png(self.existing_png_bytes()) {
            Ok(screenshot) => self.add_screenshot(screenshot),
            Err(err) => self.error = Some(format!("Could not attach clipboard image: {err}")),
        }
    }

    fn add_path_screenshot(&mut self, path: &Path) {
        if self.draft.screenshots.len() >= SPINE_FEEDBACK_MAX_SCREENSHOTS {
            self.error = Some(format!(
                "You can attach at most {SPINE_FEEDBACK_MAX_SCREENSHOTS} screenshots."
            ));
            return;
        }
        match prepare_feedback_image_path(path, self.existing_png_bytes()) {
            Ok(screenshot) => self.add_screenshot(screenshot),
            Err(err) => self.error = Some(format!("Could not attach screenshot: {err}")),
        }
    }

    fn remove_selected_screenshot(&mut self) {
        let Focus::Screenshot(index) = self.focus else {
            return;
        };
        if index >= self.draft.screenshots.len() {
            self.focus = Focus::Note;
            return;
        }
        self.draft.screenshots.remove(index);
        self.focus = if self.draft.screenshots.is_empty() {
            Focus::Note
        } else {
            Focus::Screenshot(index.min(self.draft.screenshots.len() - 1))
        };
        self.error = None;
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Focus::Note if self.draft.screenshots.is_empty() => Focus::Note,
            Focus::Note => Focus::Screenshot(0),
            Focus::Screenshot(index) if index + 1 < self.draft.screenshots.len() => {
                Focus::Screenshot(index + 1)
            }
            Focus::Screenshot(_) => Focus::Note,
        };
    }

    fn focus_previous(&mut self) {
        self.focus = match self.focus {
            Focus::Note if self.draft.screenshots.is_empty() => Focus::Note,
            Focus::Note => Focus::Screenshot(self.draft.screenshots.len() - 1),
            Focus::Screenshot(0) => Focus::Note,
            Focus::Screenshot(index) => Focus::Screenshot(index - 1),
        };
    }

    fn select_previous_screenshot(&mut self) {
        match self.focus {
            Focus::Screenshot(index) if index > 0 => self.focus = Focus::Screenshot(index - 1),
            Focus::Screenshot(_) if !self.draft.screenshots.is_empty() => {
                self.focus = Focus::Screenshot(self.draft.screenshots.len() - 1);
            }
            Focus::Note if !self.draft.screenshots.is_empty() => {
                self.focus = Focus::Screenshot(self.draft.screenshots.len() - 1);
            }
            Focus::Screenshot(_) => self.focus = Focus::Note,
            Focus::Note => {}
        }
    }

    fn select_next_screenshot(&mut self) {
        match self.focus {
            Focus::Screenshot(index) if index + 1 < self.draft.screenshots.len() => {
                self.focus = Focus::Screenshot(index + 1);
            }
            Focus::Screenshot(_) | Focus::Note if !self.draft.screenshots.is_empty() => {
                self.focus = Focus::Screenshot(0);
            }
            Focus::Screenshot(_) => self.focus = Focus::Note,
            Focus::Note => {}
        }
    }

    fn enter_consent(&mut self) {
        self.sync_note();
        self.draft.note = self.draft.note.trim().to_string();
        self.textarea.set_text_clearing_elements(&self.draft.note);
        self.textarea.set_cursor(self.draft.note.len());
        if self.draft.note.len() > SPINE_FEEDBACK_MAX_NOTE_BYTES {
            self.error = Some(format!(
                "Feedback note is {} bytes; the limit is {SPINE_FEEDBACK_MAX_NOTE_BYTES} bytes.",
                self.draft.note.len()
            ));
            self.stage = Stage::Editing;
            self.focus = Focus::Note;
            return;
        }
        self.error = None;
        self.stage = Stage::Consent;
    }

    fn submit(&mut self) {
        self.app_event_tx.send(AppEvent::SubmitSpineFeedback {
            draft: self.draft.clone(),
        });
        self.complete = true;
    }

    fn insert_note_key(&mut self, key_event: KeyEvent) {
        self.focus = Focus::Note;
        self.textarea.input(key_event);
        self.sync_note();
        self.error = None;
    }

    fn handle_editing_key(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.complete = true;
            }
            KeyEvent {
                code: KeyCode::Char('v' | 'V'),
                modifiers,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.paste_clipboard_screenshot();
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.enter_consent(),
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.insert_note_key(key_event),
            KeyEvent {
                code: KeyCode::Tab, ..
            } => self.focus_next(),
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => self.focus_previous(),
            KeyEvent {
                code: KeyCode::Up, ..
            } if matches!(self.focus, Focus::Screenshot(_)) => self.select_previous_screenshot(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } if matches!(self.focus, Focus::Screenshot(_)) => self.select_next_screenshot(),
            KeyEvent {
                code: KeyCode::Delete | KeyCode::Backspace,
                ..
            } if matches!(self.focus, Focus::Screenshot(_)) => self.remove_selected_screenshot(),
            other => self.insert_note_key(other),
        }
    }

    fn handle_consent_key(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.stage = Stage::Editing;
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.submit(),
            _ => {}
        }
    }

    fn editing_layout(&self, content_width: u16) -> EditingLayout {
        let mut before_input = Vec::new();
        push_wrapped(
            &mut before_input,
            Line::from("Send Spine feedback".bold()),
            content_width,
        );
        push_wrapped(
            &mut before_input,
            Line::from("Describe the problem and optionally attach screenshots."),
            content_width,
        );
        push_wrapped(
            &mut before_input,
            Line::from(
                "Do not include passwords, API keys, access tokens, or other secrets.".yellow(),
            ),
            content_width,
        );
        if let Some(error) = self.error.as_deref() {
            before_input.push(Line::from(""));
            push_wrapped(
                &mut before_input,
                Line::from(vec!["Error: ".red().bold(), error.to_string().red()]),
                content_width,
            );
        }
        before_input.push(Line::from(""));
        push_wrapped(
            &mut before_input,
            Line::from("Feedback note (optional)".bold()),
            content_width,
        );

        let textarea_width = content_width.saturating_sub(2).max(1);
        let input_height = self
            .textarea
            .desired_height(textarea_width)
            .clamp(1, NOTE_INPUT_MAX_HEIGHT);

        let mut after_input = vec![Line::from("")];
        push_wrapped(
            &mut after_input,
            Line::from(format!(
                "Screenshots ({}/{SPINE_FEEDBACK_MAX_SCREENSHOTS})",
                self.draft.screenshots.len()
            ))
            .bold(),
            content_width,
        );
        if self.draft.screenshots.is_empty() {
            push_wrapped_indented(
                &mut after_input,
                Line::from(
                    "None attached. Use Ctrl/Alt+V or paste a PNG, JPEG, or static WebP path."
                        .dim(),
                ),
                Line::from("  "),
                Line::from("  "),
                content_width,
            );
        } else {
            for (index, screenshot) in self.draft.screenshots.iter().enumerate() {
                let selected = self.focus == Focus::Screenshot(index);
                let label = format!(
                    "Screenshot {} · {}×{} · {}",
                    index + 1,
                    screenshot.width,
                    screenshot.height,
                    format_bytes(screenshot.png.len())
                );
                push_wrapped_indented(
                    &mut after_input,
                    if selected {
                        Line::from(label.cyan().bold())
                    } else {
                        Line::from(label)
                    },
                    if selected {
                        Line::from("› ".cyan().bold())
                    } else {
                        Line::from("  ")
                    },
                    Line::from("  "),
                    content_width,
                );
            }
        }
        after_input.push(Line::from(""));
        push_wrapped(
            &mut after_input,
            Line::from(
                "Enter review · modified Enter newline · Ctrl/Alt+V screenshot · Tab select · Esc cancel"
                    .dim(),
            ),
            content_width,
        );

        EditingLayout {
            before_input,
            input_height,
            after_input,
        }
    }

    fn consent_lines(&self, content_width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        push_wrapped(
            &mut lines,
            Line::from("Review Spine feedback".bold()),
            content_width,
        );
        push_wrapped(
            &mut lines,
            Line::from("Sending requires your explicit confirmation."),
            content_width,
        );
        lines.push(Line::from(""));
        push_wrapped(
            &mut lines,
            Line::from("Feedback note:".bold()),
            content_width,
        );
        if self.draft.note.trim().is_empty() {
            push_wrapped(&mut lines, Line::from("  (none)".dim()), content_width);
        } else {
            for note_line in self.draft.note.lines() {
                let note_line = if note_line.is_empty() { " " } else { note_line };
                push_wrapped_indented(
                    &mut lines,
                    Line::from(note_line.to_string()),
                    Line::from("  "),
                    Line::from("  "),
                    content_width,
                );
            }
        }
        lines.push(Line::from(""));
        push_wrapped(&mut lines, Line::from("Sends:".bold()), content_width);
        for item in [
            "optional feedback note, trimmed and not redacted, when entered".to_string(),
            "redacted rollout structure for this thread and all known spawned descendants"
                .to_string(),
            format!(
                "{} screenshots, whose pixels are not redacted",
                self.draft.screenshots.len()
            ),
        ] {
            push_wrapped_indented(
                &mut lines,
                Line::from(item),
                Line::from("  • "),
                Line::from("    "),
                content_width,
            );
        }
        for (index, screenshot) in self.draft.screenshots.iter().enumerate() {
            push_wrapped_indented(
                &mut lines,
                Line::from(format!(
                    "Screenshot {} · {}×{} · {}",
                    index + 1,
                    screenshot.width,
                    screenshot.height,
                    format_bytes(screenshot.png.len())
                ))
                .dim(),
                Line::from("    "),
                Line::from("    "),
                content_width,
            );
        }
        lines.push(Line::from(""));
        push_wrapped(
            &mut lines,
            Line::from("Enter send · Esc back".dim()),
            content_width,
        );
        lines
    }
}

impl BottomPaneView for SpineFeedbackView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self.stage {
            Stage::Editing => self.handle_editing_key(key_event),
            Stage::Consent => self.handle_consent_key(key_event),
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(SPINE_FEEDBACK_VIEW_ID)
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if self.stage != Stage::Editing || pasted.is_empty() {
            return false;
        }

        let path = normalize_pasted_path(&pasted);
        let is_screenshot_path = path.as_deref().is_some_and(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "webp"
                        )
                    })
        });
        if is_screenshot_path {
            if let Some(path) = path.as_deref() {
                self.add_path_screenshot(path);
            }
        } else {
            self.focus = Focus::Note;
            self.textarea.insert_str(&pasted);
            self.sync_note();
            self.error = None;
        }
        true
    }
}

impl Renderable for SpineFeedbackView {
    fn desired_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1);
        let content_height = match self.stage {
            Stage::Editing => self.editing_layout(content_width).height(),
            Stage::Consent => {
                u16::try_from(self.consent_lines(content_width).len()).unwrap_or(u16::MAX)
            }
        };
        content_height.saturating_add(menu_surface_padding_height())
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if self.stage != Stage::Editing || self.focus != Focus::Note {
            return None;
        }
        let content = menu_surface_inset(area);
        if content.is_empty() {
            return None;
        }
        let layout = self.editing_layout(content.width.max(1));
        let input_y = content
            .y
            .saturating_add(u16::try_from(layout.before_input.len()).unwrap_or(u16::MAX));
        let input_area = Rect {
            x: content.x.saturating_add(2),
            y: input_y,
            width: content.width.saturating_sub(2),
            height: layout
                .input_height
                .min(content.bottom().saturating_sub(input_y)),
        };
        if input_area.is_empty() {
            return None;
        }
        self.textarea
            .cursor_pos_with_state(input_area, *self.textarea_state.borrow())
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let content = render_menu_surface(area, buf);
        if content.is_empty() {
            return;
        }

        match self.stage {
            Stage::Editing => {
                let layout = self.editing_layout(content.width.max(1));
                let mut y = content.y;
                y = render_lines(&layout.before_input, content, y, buf);

                let input_height = layout.input_height.min(content.bottom().saturating_sub(y));
                if input_height > 0 {
                    let selected = self.focus == Focus::Note;
                    Paragraph::new(Line::from(if selected { "› " } else { "  " }))
                        .render(Rect::new(content.x, y, content.width.min(2), 1), buf);
                    let textarea_area = Rect {
                        x: content.x.saturating_add(2),
                        y,
                        width: content.width.saturating_sub(2),
                        height: input_height,
                    };
                    if !textarea_area.is_empty() {
                        Clear.render(textarea_area, buf);
                        let mut state = self.textarea_state.borrow_mut();
                        StatefulWidgetRef::render_ref(
                            &(&self.textarea),
                            textarea_area,
                            buf,
                            &mut state,
                        );
                        if self.textarea.is_empty() {
                            Paragraph::new(Line::from("Describe what went wrong (optional)".dim()))
                                .render(textarea_area, buf);
                        }
                    }
                    y = y.saturating_add(input_height);
                }
                render_lines(&layout.after_input, content, y, buf);
            }
            Stage::Consent => {
                let lines = self.consent_lines(content.width.max(1));
                render_lines(&lines, content, content.y, buf);
            }
        }
    }
}

fn push_wrapped(lines: &mut Vec<Line<'static>>, line: Line<'static>, width: u16) {
    lines.extend(
        wrap_styled_line(&line, width)
            .into_iter()
            .map(line_to_owned),
    );
}

fn push_wrapped_indented(
    lines: &mut Vec<Line<'static>>,
    line: Line<'static>,
    initial_indent: Line<'static>,
    subsequent_indent: Line<'static>,
    width: u16,
) {
    let options = RtOptions::new(width.max(1) as usize)
        .initial_indent(initial_indent)
        .subsequent_indent(subsequent_indent);
    lines.extend(
        word_wrap_line(&line, options)
            .into_iter()
            .map(line_to_owned),
    );
}

fn line_to_owned(line: Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .into_iter()
            .map(|span| Span {
                style: span.style,
                content: Cow::Owned(span.content.into_owned()),
            })
            .collect(),
    }
}

fn render_lines(lines: &[Line<'static>], area: Rect, mut y: u16, buf: &mut Buffer) -> u16 {
    for line in lines {
        if y >= area.bottom() {
            break;
        }
        Paragraph::new(line.clone()).render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(1);
    }
    y
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event_sender::AppEventSender;

    fn make_view() -> (
        SpineFeedbackView,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            SpineFeedbackView::new(ThreadId::new(), AppEventSender::new(tx)),
            rx,
        )
    }

    fn screenshot(width: u32, height: u32, bytes: usize) -> PreparedFeedbackScreenshot {
        PreparedFeedbackScreenshot {
            png: vec![0; bytes],
            width,
            height,
        }
    }

    fn render(view: &SpineFeedbackView, width: u16) -> String {
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        (0..height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..width {
                    let symbol = buf[(x, y)].symbol();
                    if symbol.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(symbol);
                    }
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_note_enters_consent() {
        let (mut view, _rx) = make_view();
        view.enter_consent();
        assert_eq!(view.stage, Stage::Consent);
        assert_eq!(view.draft.note, "");
    }

    #[test]
    fn note_limit_is_measured_in_utf8_bytes() {
        let (mut view, _rx) = make_view();
        view.textarea
            .insert_str(&"a".repeat(SPINE_FEEDBACK_MAX_NOTE_BYTES));
        view.enter_consent();
        assert_eq!(view.stage, Stage::Consent);

        let (mut view, _rx) = make_view();
        view.textarea
            .insert_str(&"a".repeat(SPINE_FEEDBACK_MAX_NOTE_BYTES + 1));
        view.enter_consent();
        assert_eq!(view.stage, Stage::Editing);
        assert!(
            view.error
                .as_deref()
                .is_some_and(|error| error.contains("8193"))
        );

        let (mut view, _rx) = make_view();
        view.textarea.insert_str(&"界".repeat(2731));
        view.enter_consent();
        assert_eq!(view.draft.note.len(), 8193);
        assert_eq!(view.stage, Stage::Editing);
    }

    #[test]
    fn consent_reviews_the_trimmed_note_that_will_be_sent() {
        let (mut view, _rx) = make_view();
        view.textarea.insert_str("  keep this note  \n");

        view.enter_consent();

        assert_eq!(view.draft.note, "keep this note");
        assert_eq!(view.textarea.text(), "keep this note");
        assert!(render(&view, 80).contains("  keep this note"));
    }

    #[test]
    fn add_remove_and_fourth_screenshot_preserve_order() {
        let (mut view, _rx) = make_view();
        view.add_screenshot(screenshot(10, 10, 1));
        view.add_screenshot(screenshot(20, 20, 2));
        view.add_screenshot(screenshot(30, 30, 3));
        assert_eq!(
            view.draft
                .screenshots
                .iter()
                .map(|screenshot| screenshot.width)
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );

        view.focus = Focus::Screenshot(1);
        view.remove_selected_screenshot();
        assert_eq!(
            view.draft
                .screenshots
                .iter()
                .map(|screenshot| screenshot.width)
                .collect::<Vec<_>>(),
            vec![10, 30]
        );

        view.add_screenshot(screenshot(40, 40, 4));
        view.add_screenshot(screenshot(50, 50, 5));
        assert_eq!(view.draft.screenshots.len(), 3);
        assert!(
            view.error
                .as_deref()
                .is_some_and(|error| error.contains("at most 3"))
        );
    }

    #[test]
    fn consent_copy_distinguishes_unredacted_inputs_from_rollout_redaction() {
        let (mut view, _rx) = make_view();
        view.enter_consent();
        let zero = render(&view, 80);
        assert!(zero.contains("optional feedback note, trimmed and not redacted"));
        assert!(zero.contains("redacted rollout structure"));
        assert!(zero.contains("0 screenshots, whose pixels are not redacted"));

        view.stage = Stage::Editing;
        view.add_screenshot(screenshot(1920, 1080, 42));
        view.enter_consent();
        let one = render(&view, 80);
        assert!(one.contains("1 screenshots, whose pixels are not redacted"));
        assert!(!one.contains("redacted screenshots"));
    }

    #[test]
    fn escape_from_consent_preserves_draft() {
        let (mut view, _rx) = make_view();
        view.textarea.insert_str("kept note");
        view.add_screenshot(screenshot(100, 50, 4));
        view.enter_consent();

        view.handle_key_event(KeyEvent::from(KeyCode::Esc));

        assert_eq!(view.stage, Stage::Editing);
        assert_eq!(view.draft.note, "kept note");
        assert_eq!(view.draft.screenshots.len(), 1);
        assert!(!view.complete);
    }

    #[test]
    fn editing_keyboard_paths_insert_newline_delete_attachment_and_cancel() {
        let (mut view, _rx) = make_view();
        view.textarea.insert_str("first");
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        view.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(view.textarea.text(), "first\nx");

        view.add_screenshot(screenshot(100, 50, 4));
        view.focus = Focus::Screenshot(0);
        view.handle_key_event(KeyEvent::from(KeyCode::Delete));
        assert!(view.draft.screenshots.is_empty());

        view.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert!(view.complete);
    }

    #[test]
    fn nonexistent_or_non_file_image_named_paste_remains_note_text() {
        let (mut view, _rx) = make_view();
        let nonexistent =
            std::env::temp_dir().join(format!("spine-feedback-missing-{}.png", ThreadId::new()));
        assert!(!nonexistent.exists());
        let nonexistent_text = nonexistent.to_string_lossy().into_owned();

        assert!(view.handle_paste(nonexistent_text.clone()));
        assert_eq!(view.textarea.text(), nonexistent_text);

        let directory = tempfile::Builder::new()
            .suffix(".png")
            .tempdir()
            .expect("create image-named directory");
        let directory_text = directory.path().to_string_lossy().into_owned();
        assert!(view.handle_paste(directory_text.clone()));
        assert_eq!(
            view.textarea.text(),
            format!("{nonexistent_text}{directory_text}")
        );
        assert!(view.draft.screenshots.is_empty());
        assert!(view.error.is_none());
    }

    #[test]
    fn failed_upload_draft_reopens_with_actionable_error() {
        let thread_id = ThreadId::new();
        let draft = SpineFeedbackDraft {
            thread_id,
            note: "kept note".to_string(),
            screenshots: vec![screenshot(100, 50, 4)],
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let view = SpineFeedbackView::with_draft(
            draft.clone(),
            Some("request timed out; try again".to_string()),
            AppEventSender::new(tx),
        );

        assert_eq!(view.draft, draft);
        assert_eq!(view.stage, Stage::Editing);
        assert_eq!(view.view_id(), Some(SPINE_FEEDBACK_VIEW_ID));
        let rendered = render(&view, 40);
        assert!(rendered.contains("Error:"));
        assert!(rendered.contains("request timed out"));
    }

    #[test]
    fn submit_emits_complete_draft() {
        let (mut view, mut rx) = make_view();
        let thread_id = view.draft.thread_id;
        view.textarea.insert_str("optional note");
        view.add_screenshot(screenshot(12, 34, 5));
        view.enter_consent();
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));

        let event = rx.try_recv().expect("submit event");
        assert!(matches!(
            event,
            AppEvent::SubmitSpineFeedback { draft }
                if draft.thread_id == thread_id
                    && draft.note == "optional note"
                    && draft.screenshots.len() == 1
        ));
        assert!(view.complete);
    }

    #[test]
    fn editing_layout_wraps_at_wide_narrow_and_mobile_widths() {
        let (mut view, _rx) = make_view();
        view.error = Some("The screenshot could not be decoded; choose another file.".to_string());
        view.add_screenshot(screenshot(1920, 1080, 42 * 1024));
        view.error = Some("The screenshot could not be decoded; choose another file.".to_string());

        for width in [80, 40, 24] {
            let rendered = render(&view, width);
            assert!(
                rendered
                    .lines()
                    .all(|line| line.chars().count() <= usize::from(width))
            );
            assert!(rendered.contains("Send Spine feedback"));
            assert!(rendered.contains("Screenshots"));
            assert!(rendered.contains("Error:"));
        }
    }

    #[test]
    fn editing_and_consent_render_snapshots() {
        let (mut view, _rx) = make_view();
        view.textarea
            .insert_str("The subtree report missed a child.");
        view.sync_note();
        view.add_screenshot(screenshot(1280, 720, 1024));
        insta::assert_snapshot!("spine_feedback_editing_width_40", render(&view, 40));

        view.enter_consent();
        insta::assert_snapshot!("spine_feedback_consent_width_24", render(&view, 24));
    }
}
