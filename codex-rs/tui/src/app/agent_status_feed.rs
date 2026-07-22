//! Bounded, best-effort previews for the v2 `/agent` status output.

use super::ThreadEventStore;
use crate::history_cell::HistoryCell;
use crate::history_cell::plain_lines;
use crate::multi_agents::AgentActivityPathDisplay;
use crate::multi_agents::AgentActivityPreview;
use ratatui::style::Stylize;
use ratatui::text::Line;

const AGENT_STATUS_PREVIEW_INDENT: u16 = 4;

#[derive(Debug)]
pub(super) struct AgentStatusHistoryCell {
    entries: Vec<AgentStatusThreadPreview>,
}

impl AgentStatusHistoryCell {
    pub(super) fn new(entries: Vec<AgentStatusThreadPreview>) -> Self {
        Self { entries }
    }
}

impl HistoryCell for AgentStatusHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = vec![
            "/agent".magenta().into(),
            "Sub-agents running".bold().into(),
            "".into(),
        ];

        if self.entries.is_empty() {
            lines.push("  • No sub-agents running.".italic().into());
            return lines;
        }

        for entry in &self.entries {
            lines.push(entry.title_line());
            let preview_width = width.saturating_sub(AGENT_STATUS_PREVIEW_INDENT).max(1);
            let preview_lines = entry.preview_lines(preview_width);
            if preview_lines.is_empty() {
                lines.push(vec!["    ".into(), "No recent activity yet.".dim().italic()].into());
            } else {
                lines.extend(preview_lines.into_iter().map(indent_preview_line));
            }
            lines.push("".into());
        }
        let _ = lines.pop();
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

#[derive(Debug)]
pub(super) struct AgentStatusThreadPreview {
    agent_path: String,
    activity: AgentActivityPreview,
}

impl AgentStatusThreadPreview {
    pub(super) fn from_store(agent_path: String, store: &ThreadEventStore) -> Self {
        Self {
            agent_path,
            activity: store.agent_activity_preview(AgentActivityPathDisplay::Show),
        }
    }

    pub(super) fn empty(agent_path: String) -> Self {
        Self {
            agent_path,
            activity: AgentActivityPreview::default(),
        }
    }

    fn title_line(&self) -> Line<'static> {
        vec!["  • ".dim(), format!("`{}`", self.agent_path).cyan()].into()
    }

    fn preview_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.activity.lines(width)
    }
}

fn indent_preview_line(mut line: Line<'static>) -> Line<'static> {
    line.spans.insert(0, "    ".into());
    line
}

#[cfg(test)]
#[path = "agent_status_feed_tests.rs"]
mod tests;
