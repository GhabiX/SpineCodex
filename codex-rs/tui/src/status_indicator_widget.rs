//! A live task status row rendered above the composer while the agent is busy.
//!
//! The row owns spinner timing, the optional interrupt hint, and short inline
//! context (for example, the unified-exec background-process summary). Keeping
//! these pieces on one line avoids vertical layout churn in the bottom pane.

use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::app_event_sender::AppEventSender;
use crate::key_hint;
use crate::key_hint::ShortcutHint;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::motion::green_growth_marker;
use crate::motion::green_shimmer_text;
use crate::motion::green_then_default_shimmer_text;
use crate::motion::shimmer_text;
use crate::render::renderable::Renderable;
use crate::text_formatting::capitalize_first;
use crate::tui::FrameRequester;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

pub(crate) const STATUS_DETAILS_DEFAULT_MAX_LINES: usize = 3;
const DETAILS_PREFIX: &str = "  └ ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusDetailsCapitalization {
    CapitalizeFirst,
    Preserve,
}

/// Displays a single-line in-progress status with optional wrapped details.
pub(crate) struct StatusIndicatorWidget {
    /// Animated header text (defaults to "Working").
    header: String,
    header_is_reasoning: bool,
    organic_working_word: Option<&'static str>,
    details: Option<String>,
    details_max_lines: usize,
    /// Optional suffix rendered after the elapsed/interrupt segment.
    inline_message: Option<String>,
    show_interrupt_hint: bool,
    interrupt_binding: Option<ShortcutHint>,

    elapsed_running: Duration,
    last_resume_at: Instant,
    is_paused: bool,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    animations_enabled: bool,
}

// Format elapsed seconds into a compact human-friendly form used by the status line.
// Examples: 0s, 59s, 1m 00s, 59m 59s, 1h 00m 00s, 2h 03m 09s
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

impl StatusIndicatorWidget {
    pub(crate) fn new(
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            header: String::from("Working"),
            header_is_reasoning: false,
            organic_working_word: None,
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
            inline_message: None,
            show_interrupt_hint: true,
            interrupt_binding: Some(key_hint::plain(KeyCode::Esc).into()),
            elapsed_running: Duration::ZERO,
            last_resume_at: Instant::now(),
            is_paused: false,

            app_event_tx,
            frame_requester,
            animations_enabled,
        }
    }

    pub(crate) fn interrupt(&self) {
        self.app_event_tx.interrupt();
    }

    /// Update the animated header label (left of the brackets).
    pub(crate) fn update_header(&mut self, header: String, header_is_reasoning: bool) {
        self.header = header;
        self.header_is_reasoning = header_is_reasoning;
    }

    pub(crate) fn set_organic_working_word(&mut self, word: Option<&'static str>) {
        self.organic_working_word = word;
    }

    /// Update the details text shown below the header.
    pub(crate) fn update_details(
        &mut self,
        details: Option<String>,
        capitalization: StatusDetailsCapitalization,
        max_lines: usize,
    ) {
        self.details_max_lines = max_lines.max(1);
        self.details = details
            .filter(|details| !details.is_empty())
            .map(|details| {
                let trimmed = details.trim_start();
                match capitalization {
                    StatusDetailsCapitalization::CapitalizeFirst => capitalize_first(trimmed),
                    StatusDetailsCapitalization::Preserve => trimmed.to_string(),
                }
            });
    }

    /// Update the inline suffix text shown after the elapsed/interrupt hint.
    ///
    /// Callers should provide plain, already-contextualized text. Passing
    /// verbose status prose here can cause frequent width truncation and hide
    /// the more important elapsed/interrupt hint.
    pub(crate) fn update_inline_message(&mut self, message: Option<String>) {
        self.inline_message = message
            .map(|message| message.trim().to_string())
            .filter(|message| !message.is_empty());
    }

    pub(crate) fn header(&self) -> &str {
        &self.header
    }

    #[cfg(test)]
    pub(crate) fn organic_working_word(&self) -> Option<&str> {
        self.organic_working_word
    }

    #[cfg(test)]
    pub(crate) fn header_is_reasoning(&self) -> bool {
        self.header_is_reasoning
    }

    #[cfg(test)]
    pub(crate) fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }

    pub(crate) fn set_interrupt_hint_visible(&mut self, visible: bool) {
        self.show_interrupt_hint = visible;
    }

    pub(crate) fn set_interrupt_binding(&mut self, binding: Option<ShortcutHint>) {
        self.interrupt_binding = binding;
    }

    pub(crate) fn pause_timer(&mut self) {
        self.pause_timer_at(Instant::now());
    }

    pub(crate) fn resume_timer(&mut self) {
        self.resume_timer_at(Instant::now());
    }

    pub(crate) fn pause_timer_at(&mut self, now: Instant) {
        if self.is_paused {
            return;
        }
        self.elapsed_running += now.saturating_duration_since(self.last_resume_at);
        self.is_paused = true;
    }

    pub(crate) fn resume_timer_at(&mut self, now: Instant) {
        if !self.is_paused {
            return;
        }
        self.last_resume_at = now;
        self.is_paused = false;
        self.frame_requester.schedule_frame();
    }

    fn elapsed_duration_at(&self, now: Instant) -> Duration {
        let mut elapsed = self.elapsed_running;
        if !self.is_paused {
            elapsed += now.saturating_duration_since(self.last_resume_at);
        }
        elapsed
    }

    fn elapsed_seconds_at(&self, now: Instant) -> u64 {
        self.elapsed_duration_at(now).as_secs()
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.elapsed_seconds_at(Instant::now())
    }

    /// Wrap the details text into a fixed width and return the lines, truncating if necessary.
    fn wrapped_details_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(details) = self.details.as_deref() else {
            return Vec::new();
        };
        if width == 0 {
            return Vec::new();
        }

        let prefix_width = UnicodeWidthStr::width(DETAILS_PREFIX);
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(Line::from(DETAILS_PREFIX.dim()))
            .subsequent_indent(Line::from(Span::from(" ".repeat(prefix_width)).dim()))
            .break_words(/*break_words*/ true);

        let mut out = word_wrap_lines(details.lines().map(|line| vec![line.dim()]), opts);

        if out.len() > self.details_max_lines {
            out.truncate(self.details_max_lines);
            let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
            let max_base_len = content_width.saturating_sub(1);
            if let Some(last) = out.last_mut()
                && let Some(span) = last.spans.last_mut()
            {
                let trimmed: String = span.content.as_ref().chars().take(max_base_len).collect();
                *span = format!("{trimmed}…").dim();
            }
        }

        out
    }
}

impl Renderable for StatusIndicatorWidget {
    fn desired_height(&self, width: u16) -> u16 {
        1 + u16::try_from(self.wrapped_details_lines(width).len()).unwrap_or(0)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        if self.animations_enabled {
            // Schedule next animation frame.
            self.frame_requester
                .schedule_frame_in(Duration::from_millis(32));
        }
        let now = Instant::now();
        let elapsed_duration = self.elapsed_duration_at(now);
        let pretty_elapsed = fmt_elapsed_compact(elapsed_duration.as_secs());
        let motion_mode = MotionMode::from_animations_enabled(self.animations_enabled);

        let mut spans = Vec::with_capacity(7);
        let mut organic_activity_word = None;
        if (self.header == "Working" || self.header_is_reasoning)
            && let Some(activity_word) = self.organic_working_word
        {
            spans.push(" ".into());
            spans.push(green_growth_marker(elapsed_duration, motion_mode));
            spans.push(" ".into());
            organic_activity_word = Some(activity_word);
        } else {
            if let Some(indicator) = activity_indicator(
                Some(self.last_resume_at),
                motion_mode,
                ReducedMotionIndicator::Hidden,
            ) {
                spans.push(indicator);
                spans.push(" ".into());
            }
            spans.extend(shimmer_text(&self.header, motion_mode));
        }

        let mut suffix_spans = Vec::with_capacity(5);
        if !spans.is_empty() {
            suffix_spans.push(" ".into());
        }
        if self.show_interrupt_hint
            && let Some(interrupt_binding) = self.interrupt_binding
        {
            suffix_spans.extend(vec![
                format!("({pretty_elapsed} • ").dim(),
                interrupt_binding.into(),
                " to interrupt)".dim(),
            ]);
        } else {
            suffix_spans.push(format!("({pretty_elapsed})").dim());
        }
        if let Some(message) = &self.inline_message {
            // Keep optional context after elapsed/interrupt text so that core
            // interrupt affordances stay in a fixed visual location.
            suffix_spans.push(" · ".dim());
            suffix_spans.push(message.clone().dim());
        }

        if let Some(activity_word) = organic_activity_word {
            if self.header_is_reasoning && !self.header.is_empty() {
                let prefix_width =
                    line_width(&Line::from(spans.clone())) + UnicodeWidthStr::width(activity_word);
                let suffix_width = line_width(&Line::from(suffix_spans.clone()));
                let available_width =
                    usize::from(area.width).saturating_sub(prefix_width + suffix_width);
                if available_width >= 3 {
                    let title = truncate_line_with_ellipsis_if_overflow(
                        Line::from(self.header.clone()),
                        available_width.saturating_sub(2),
                    );
                    let mut default_text = String::from(": ");
                    for span in title.spans {
                        default_text.push_str(span.content.as_ref());
                    }
                    spans.extend(green_then_default_shimmer_text(
                        activity_word,
                        &default_text,
                        motion_mode,
                    ));
                } else {
                    spans.extend(green_shimmer_text(activity_word, motion_mode));
                }
            } else {
                spans.extend(green_shimmer_text(activity_word, motion_mode));
            }
        }
        spans.extend(suffix_spans);

        let mut lines = Vec::new();
        lines.push(truncate_line_with_ellipsis_if_overflow(
            Line::from(spans),
            usize::from(area.width),
        ));
        if area.height > 1 {
            // If there is enough space, add the details lines below the header.
            let details = self.wrapped_details_lines(area.width);
            let max_details = usize::from(area.height.saturating_sub(1));
            lines.extend(details.into_iter().take(max_details));
        }

        Paragraph::new(Text::from(lines)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;
    use std::time::Instant;
    use tokio::sync::mpsc::unbounded_channel;

    use pretty_assertions::assert_eq;

    fn render_status_line(widget: &StatusIndicatorWidget, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        terminal.backend().buffer().content()[..usize::from(width)]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 0), "0s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 1), "1s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 59), "59s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 61), "1m 01s");
        assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
        assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
        assert_eq!(fmt_elapsed_compact(/*elapsed_secs*/ 3600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3600 + 60 + 1), "1h 01m 01s");
        assert_eq!(fmt_elapsed_compact(25 * 3600 + 2 * 60 + 3), "25h 02m 03s");
    }

    #[test]
    fn renders_with_working_header() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_organic_working_header_when_enabled() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.set_organic_working_word(Some("Blooming"));
        widget.is_paused = true;
        widget.elapsed_running = Duration::ZERO;

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());

        let marker = terminal
            .backend()
            .buffer()
            .cell((1, 0))
            .expect("marker cell");
        let word = terminal
            .backend()
            .buffer()
            .cell((3, 0))
            .expect("activity word cell");
        let expected_green = crate::shimmer::motion_green_style().fg;
        assert_eq!(marker.style().fg, expected_green);
        assert_eq!(word.style().fg, expected_green);
    }

    #[test]
    fn renders_reasoning_header_with_organic_working_identity() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.set_organic_working_word(Some("Blooming"));
        widget.update_header(
            "Planning memory rollout inspection".to_string(),
            /*header_is_reasoning*/ true,
        );
        widget.is_paused = true;
        widget.elapsed_running = Duration::ZERO;

        insta::assert_snapshot!(
            render_status_line(&widget, /*width*/ 120),
            @" ϒ Blooming: Planning memory rollout inspection (0s • esc to interrupt)"
        );
    }

    #[test]
    fn operational_header_overrides_and_reasoning_header_restores_organic_identity() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.set_organic_working_word(Some("Blooming"));
        widget.is_paused = true;
        widget.elapsed_running = Duration::ZERO;

        widget.update_header(
            "Planning memory rollout inspection".to_string(),
            /*header_is_reasoning*/ true,
        );
        assert_eq!(
            render_status_line(&widget, /*width*/ 120),
            " ϒ Blooming: Planning memory rollout inspection (0s • esc to interrupt)"
        );

        widget.update_header(
            "Reconnecting... 2/5".to_string(),
            /*header_is_reasoning*/ false,
        );
        assert_eq!(
            render_status_line(&widget, /*width*/ 120),
            "Reconnecting... 2/5 (0s • esc to interrupt)"
        );

        widget.update_header(
            "Planning memory rollout inspection".to_string(),
            /*header_is_reasoning*/ true,
        );
        assert_eq!(
            render_status_line(&widget, /*width*/ 120),
            " ϒ Blooming: Planning memory rollout inspection (0s • esc to interrupt)"
        );
    }

    #[test]
    fn reasoning_title_yields_width_to_interrupt_suffix() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        widget.set_organic_working_word(Some("Blooming"));
        widget.update_header(
            "Planning memory rollout inspection".to_string(),
            /*header_is_reasoning*/ true,
        );
        widget.is_paused = true;
        widget.elapsed_running = Duration::ZERO;

        assert_eq!(
            render_status_line(&widget, /*width*/ 40),
            " ϒ Blooming: Pl… (0s • esc to interrupt)"
        );
    }

    #[test]
    fn renders_truncated() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_wrapped_details_panama_two_lines() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        w.update_details(
            Some("A man a plan a canal panama".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
        w.set_interrupt_hint_visible(/*visible*/ false);

        // Freeze time-dependent rendering (elapsed + spinner) to keep the snapshot stable.
        w.is_paused = true;
        w.elapsed_running = Duration::ZERO;

        // Prefix is 4 columns, so a width of 30 yields a content width of 26: one column
        // short of fitting the whole phrase (27 cols), forcing exactly one wrap without ellipsis.
        let mut terminal = Terminal::new(TestBackend::new(30, 3)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_without_spinner_when_animations_disabled() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        w.is_paused = true;
        w.elapsed_running = Duration::ZERO;

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        let line = terminal.backend().buffer().content()[..80]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(line.starts_with("Working (0s • esc to interrupt)"));
    }

    #[test]
    fn renders_remapped_interrupt_hint() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        w.set_interrupt_binding(Some(key_hint::plain(KeyCode::F(12)).into()));
        w.is_paused = true;
        w.elapsed_running = Duration::ZERO;

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn timer_pauses_when_requested() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );

        let baseline = Instant::now();
        widget.last_resume_at = baseline;

        let before_pause = widget.elapsed_seconds_at(baseline + Duration::from_secs(5));
        assert_eq!(before_pause, 5);

        widget.pause_timer_at(baseline + Duration::from_secs(5));
        let paused_elapsed = widget.elapsed_seconds_at(baseline + Duration::from_secs(10));
        assert_eq!(paused_elapsed, before_pause);

        widget.resume_timer_at(baseline + Duration::from_secs(10));
        let after_resume = widget.elapsed_seconds_at(baseline + Duration::from_secs(13));
        assert_eq!(after_resume, before_pause + 3);
    }

    #[test]
    fn details_overflow_adds_ellipsis() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        w.update_details(
            Some("abcd abcd abcd abcd".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );

        let lines = w.wrapped_details_lines(/*width*/ 6);
        assert_eq!(lines.len(), STATUS_DETAILS_DEFAULT_MAX_LINES);
        let last = lines.last().expect("expected last details line");
        assert!(
            last.spans[1].content.as_ref().ends_with("…"),
            "expected ellipsis in last line: {last:?}"
        );
    }

    #[test]
    fn details_args_can_disable_capitalization_and_limit_lines() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut w = StatusIndicatorWidget::new(
            tx,
            crate::tui::FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        w.update_details(
            Some("cargo test -p codex-core and then cargo test -p codex-tui".to_string()),
            StatusDetailsCapitalization::Preserve,
            /*max_lines*/ 1,
        );

        assert_eq!(
            w.details(),
            Some("cargo test -p codex-core and then cargo test -p codex-tui")
        );

        let lines = w.wrapped_details_lines(/*width*/ 24);
        assert_eq!(lines.len(), 1);
        let last = lines.last().expect("expected one details line");
        assert!(
            last.spans
                .last()
                .is_some_and(|span| span.content.as_ref().contains('…')),
            "expected one-line details to be ellipsized, got {last:?}"
        );
    }
}
