//! Data model for grouped exec-call history cells in the TUI transcript.
//!
//! An `ExecCell` can represent either a single command or an "exploring" group of related read/
//! list/search commands. The chat widget relies on stable `call_id` matching to route progress and
//! end events into the right cell, and it treats "call id not found" as a real signal (for
//! example, an orphan end that should render as a separate history entry).

use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

use codex_app_server_protocol::CommandExecutionSource as ExecCommandSource;
use codex_protocol::parse_command::ParsedCommand;

#[derive(Clone, Debug, Default)]
pub(crate) struct CommandOutput {
    pub(crate) exit_code: i32,
    /// The aggregated stderr + stdout interleaved.
    pub(crate) aggregated_output: String,
    /// The formatted output of the command, as seen by the model.
    pub(crate) formatted_output: String,
    preview: OutputPreview,
}

impl CommandOutput {
    pub(crate) fn new(
        exit_code: i32,
        aggregated_output: impl Into<String>,
        formatted_output: impl Into<String>,
    ) -> Self {
        let aggregated_output = aggregated_output.into();
        let formatted_output = formatted_output.into();
        Self {
            exit_code,
            preview: OutputPreview::from_aggregated_output(&aggregated_output),
            aggregated_output,
            formatted_output,
        }
    }

    pub(crate) fn preview(&self) -> &OutputPreview {
        &self.preview
    }

    pub(crate) fn append_aggregated_output(&mut self, chunk: &str) {
        self.aggregated_output.push_str(chunk);
        self.preview.append(chunk);
    }
}

pub(crate) const OUTPUT_PREVIEW_LINES_PER_EDGE: usize = 100;
const OUTPUT_PREVIEW_MAX_BYTES_PER_LINE: usize = 2 * 1024;
const OUTPUT_PREVIEW_MAX_VISIBLE_CHARS_PER_LINE: usize = 2 * 1024;

/// Bounded projection used by the main exec history cell.
///
/// Keypress, resize, and output-delta redraws call `ExecCell::display_lines`.
/// Rebuilding the folded preview from full `aggregated_output` made input
/// latency grow with command output size; very long JSON/SVG lines then made
/// wrapping especially expensive. Keep full output for transcript/model/log
/// use, but render the main cell from this head/tail buffer instead. This is a
/// local perf fix for the class of issues discussed upstream around exec output
/// folding/streaming: openai/codex#4550, #4751, #3675, and #5005.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutputPreview {
    finalized_line_count: usize,
    head: Vec<RetainedPreviewLine>,
    tail: VecDeque<RetainedPreviewLine>,
    pending_line: OutputPreviewLine,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputPreviewEntry {
    Line(OutputPreviewLine),
    Omitted(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputPreviewLine {
    text: String,
    truncated: bool,
}

#[derive(Clone, Debug)]
struct RetainedPreviewLine {
    index: usize,
    line: OutputPreviewLine,
}

impl OutputPreview {
    fn from_aggregated_output(output: &str) -> Self {
        let mut preview = Self::default();
        preview.append(output);
        preview
    }

    fn append(&mut self, chunk: &str) {
        for segment in chunk.split_inclusive('\n') {
            if let Some(line_segment) = segment.strip_suffix('\n') {
                let line_segment = line_segment.strip_suffix('\r').unwrap_or(line_segment);
                self.pending_line.push_str(line_segment);
                self.pending_line.strip_trailing_cr();
                self.finalize_pending_line();
            } else {
                self.pending_line.push_str(segment);
            }
        }
    }

    pub(crate) fn display_entries(&self, line_limit: usize) -> Vec<OutputPreviewEntry> {
        let total = self.total_line_count();
        if total == 0 {
            return Vec::new();
        }

        let line_limit = line_limit.min(OUTPUT_PREVIEW_LINES_PER_EDGE);
        if line_limit == 0 {
            return vec![OutputPreviewEntry::Omitted(total)];
        }

        let show_ellipsis = total > 2 * line_limit;
        let mut retained = self.retained_lines();
        retained.sort_by_key(|line| line.index);
        retained.dedup_by_key(|line| line.index);

        if !show_ellipsis {
            return retained
                .into_iter()
                .map(|line| OutputPreviewEntry::Line(line.line))
                .collect();
        }

        let tail_start = total.saturating_sub(line_limit);
        let mut entries = Vec::with_capacity((2 * line_limit).saturating_add(1));
        entries.extend(
            retained
                .iter()
                .filter(|line| line.index < line_limit)
                .cloned()
                .map(|line| OutputPreviewEntry::Line(line.line)),
        );
        entries.push(OutputPreviewEntry::Omitted(total - 2 * line_limit));
        entries.extend(
            retained
                .into_iter()
                .filter(|line| line.index >= tail_start)
                .map(|line| OutputPreviewEntry::Line(line.line)),
        );
        entries
    }

    fn total_line_count(&self) -> usize {
        self.finalized_line_count + usize::from(!self.pending_line.is_empty())
    }

    fn retained_lines(&self) -> Vec<RetainedPreviewLine> {
        let mut lines = Vec::with_capacity(
            self.head
                .len()
                .saturating_add(self.tail.len())
                .saturating_add(usize::from(!self.pending_line.is_empty())),
        );
        lines.extend(self.head.iter().cloned());
        lines.extend(self.tail.iter().cloned());
        if !self.pending_line.is_empty() {
            lines.push(RetainedPreviewLine {
                index: self.finalized_line_count,
                line: self.pending_line.clone(),
            });
        }
        lines
    }

    fn finalize_pending_line(&mut self) {
        let line = std::mem::take(&mut self.pending_line);
        let retained = RetainedPreviewLine {
            index: self.finalized_line_count,
            line,
        };
        self.finalized_line_count = self.finalized_line_count.saturating_add(1);
        if self.head.len() < OUTPUT_PREVIEW_LINES_PER_EDGE {
            self.head.push(retained.clone());
        }
        self.tail.push_back(retained);
        while self.tail.len() > OUTPUT_PREVIEW_LINES_PER_EDGE {
            self.tail.pop_front();
        }
    }
}

impl OutputPreviewLine {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.truncated
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty() && !self.truncated
    }

    fn push_str(&mut self, segment: &str) {
        if self.truncated || segment.is_empty() {
            return;
        }

        if self.text.len().saturating_add(segment.len()) <= OUTPUT_PREVIEW_MAX_BYTES_PER_LINE {
            self.text.push_str(segment);
            return;
        }

        let remaining = OUTPUT_PREVIEW_MAX_BYTES_PER_LINE.saturating_sub(self.text.len());
        let prefix = utf8_prefix_by_bytes(segment, remaining);
        let mut raw = String::with_capacity(self.text.len().saturating_add(prefix.len()));
        raw.push_str(&self.text);
        raw.push_str(prefix);
        self.text = visible_prefix_without_ansi(&raw, OUTPUT_PREVIEW_MAX_VISIBLE_CHARS_PER_LINE);
        self.truncated = true;
    }

    fn strip_trailing_cr(&mut self) {
        if self.truncated {
            return;
        }
        if self.text.ends_with('\r') {
            self.text.pop();
        }
    }
}

fn utf8_prefix_by_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }

    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn visible_prefix_without_ansi(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut index = 0usize;
    let mut visible = 0usize;
    while index < text.len() && visible < max_chars {
        let rest = &text[index..];
        if rest.starts_with('\u{1b}') {
            index = index.saturating_add(ansi_escape_sequence_len(rest));
            continue;
        }

        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
        visible += 1;
    }
    out
}

fn ansi_escape_sequence_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.first(), Some(&0x1b));
    if bytes.len() < 2 {
        return 1;
    }

    match bytes[1] {
        b'[' => {
            for (idx, byte) in bytes.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(byte) {
                    return idx + 1;
                }
            }
            bytes.len()
        }
        b']' => {
            let mut idx = 2usize;
            while idx < bytes.len() {
                if bytes[idx] == 0x07 {
                    return idx + 1;
                }
                if bytes[idx] == 0x1b && bytes.get(idx + 1) == Some(&b'\\') {
                    return idx + 2;
                }
                idx += 1;
            }
            bytes.len()
        }
        b'(' | b')' | b'*' | b'+' => bytes.len().min(3),
        _ => 2,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExecCall {
    pub(crate) call_id: String,
    pub(crate) command: Vec<String>,
    pub(crate) parsed: Vec<ParsedCommand>,
    pub(crate) output: Option<CommandOutput>,
    pub(crate) source: ExecCommandSource,
    pub(crate) start_time: Option<Instant>,
    pub(crate) duration: Option<Duration>,
    pub(crate) interaction_input: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ExecCell {
    pub(crate) calls: Vec<ExecCall>,
    animations_enabled: bool,
}

impl ExecCell {
    pub(crate) fn new(call: ExecCall, animations_enabled: bool) -> Self {
        Self {
            calls: vec![call],
            animations_enabled,
        }
    }

    pub(crate) fn with_added_call(
        &self,
        call_id: String,
        command: Vec<String>,
        parsed: Vec<ParsedCommand>,
        source: ExecCommandSource,
        interaction_input: Option<String>,
    ) -> Option<Self> {
        let call = ExecCall {
            call_id,
            command,
            parsed,
            output: None,
            source,
            start_time: Some(Instant::now()),
            duration: None,
            interaction_input,
        };
        if self.is_exploring_cell() && Self::is_exploring_call(&call) {
            Some(Self {
                calls: [self.calls.clone(), vec![call]].concat(),
                animations_enabled: self.animations_enabled,
            })
        } else {
            None
        }
    }

    /// Marks the most recently matching call as finished and returns whether a call was found.
    ///
    /// Callers should treat `false` as a routing mismatch rather than silently ignoring it. The
    /// chat widget uses that signal to avoid attaching an orphan `exec_end` event to an unrelated
    /// active exploring cell, which would incorrectly collapse two transcript entries together.
    pub(crate) fn complete_call(
        &mut self,
        call_id: &str,
        output: CommandOutput,
        duration: Duration,
    ) -> bool {
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        call.output = Some(output);
        call.duration = Some(duration);
        call.start_time = None;
        true
    }

    pub(crate) fn should_flush(&self) -> bool {
        !self.is_exploring_cell() && self.calls.iter().all(|c| c.output.is_some())
    }

    pub(crate) fn mark_failed(&mut self) {
        for call in self.calls.iter_mut() {
            if call.output.is_none() {
                let elapsed = call
                    .start_time
                    .map(|st| st.elapsed())
                    .unwrap_or_else(|| Duration::from_millis(0));
                call.start_time = None;
                call.duration = Some(elapsed);
                call.output = Some(CommandOutput::new(1, String::new(), String::new()));
            }
        }
    }

    pub(crate) fn is_exploring_cell(&self) -> bool {
        self.calls.iter().all(Self::is_exploring_call)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.calls.iter().any(|c| c.output.is_none())
    }

    pub(crate) fn active_start_time(&self) -> Option<Instant> {
        self.calls
            .iter()
            .find(|c| c.output.is_none())
            .and_then(|c| c.start_time)
    }

    pub(crate) fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub(crate) fn iter_calls(&self) -> impl Iterator<Item = &ExecCall> {
        self.calls.iter()
    }

    pub(crate) fn append_output(&mut self, call_id: &str, chunk: &str) -> bool {
        if chunk.is_empty() {
            return false;
        }
        let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) else {
            return false;
        };
        let output = call.output.get_or_insert_with(CommandOutput::default);
        output.append_aggregated_output(chunk);
        true
    }

    pub(super) fn is_exploring_call(call: &ExecCall) -> bool {
        !matches!(call.source, ExecCommandSource::UserShell)
            && !call.parsed.is_empty()
            && call.parsed.iter().all(|p| {
                matches!(
                    p,
                    ParsedCommand::Read { .. }
                        | ParsedCommand::ListFiles { .. }
                        | ParsedCommand::Search { .. }
                )
            })
    }
}

impl ExecCall {
    pub(crate) fn is_user_shell_command(&self) -> bool {
        matches!(self.source, ExecCommandSource::UserShell)
    }

    pub(crate) fn is_unified_exec_interaction(&self) -> bool {
        matches!(self.source, ExecCommandSource::UnifiedExecInteraction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_texts(preview: &OutputPreview, line_limit: usize) -> Vec<String> {
        preview
            .display_entries(line_limit)
            .into_iter()
            .map(|entry| match entry {
                OutputPreviewEntry::Line(line) => {
                    if line.truncated {
                        format!("{}…", line.text)
                    } else {
                        line.text
                    }
                }
                OutputPreviewEntry::Omitted(count) => format!("OMITTED:{count}"),
            })
            .collect()
    }

    #[test]
    fn output_preview_matches_str_lines_semantics() {
        for (source, expected) in [
            ("", Vec::<&str>::new()),
            ("one", vec!["one"]),
            ("one\n", vec!["one"]),
            ("\n", vec![""]),
            ("one\n\nthree", vec!["one", "", "three"]),
        ] {
            let preview = OutputPreview::from_aggregated_output(source);
            assert_eq!(
                entry_texts(&preview, 100),
                expected.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "source={source:?}",
            );
        }
    }

    #[test]
    fn output_preview_handles_chunk_boundaries_and_split_crlf() {
        let mut output = CommandOutput::default();
        for chunk in ["o", "ne\n", "two\r", "\nthree"] {
            output.append_aggregated_output(chunk);
        }

        assert_eq!(output.aggregated_output, "one\ntwo\r\nthree");
        assert_eq!(
            entry_texts(output.preview(), 100),
            vec!["one", "two", "three"]
        );
    }

    #[test]
    fn output_preview_keeps_bounded_head_and_tail() {
        let output = CommandOutput::new(
            0,
            (1..=250)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            String::new(),
        );

        let rendered = entry_texts(output.preview(), 5);
        assert_eq!(
            rendered,
            vec![
                "1",
                "2",
                "3",
                "4",
                "5",
                "OMITTED:240",
                "246",
                "247",
                "248",
                "249",
                "250",
            ]
        );
    }

    #[test]
    fn output_preview_truncates_retained_long_lines() {
        let output = CommandOutput::new(
            0,
            format!("\u{1b}[31m{}\n", "x".repeat(16 * 1024)),
            String::new(),
        );
        let entries = output.preview().display_entries(5);
        let [OutputPreviewEntry::Line(line)] = entries.as_slice() else {
            panic!("expected a single retained line");
        };

        assert!(line.truncated);
        assert!(!line.text.contains('\u{1b}'));
        assert!(
            line.text.len() <= OUTPUT_PREVIEW_MAX_VISIBLE_CHARS_PER_LINE,
            "preview line should be capped, got {} bytes",
            line.text.len()
        );
    }
}
