use super::*;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;

#[derive(Debug)]
pub(crate) struct SpineSpawnProgressCell {
    notification: SpineSpawnProgressUpdatedNotification,
}

impl SpineSpawnProgressCell {
    pub(crate) fn new(notification: SpineSpawnProgressUpdatedNotification) -> Self {
        Self { notification }
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.notification.turn_id
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.notification.call_id
    }
}

impl HistoryCell for SpineSpawnProgressCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec!["spine.spawn".magenta().bold().into()];
        for task in &self.notification.tasks {
            let status = status_span(&task.status);
            let label = format!("[{}] {}", task.ordinal, task.summary.trim());
            let path = task
                .agent_path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(|path| format!("  `{path}`"))
                .unwrap_or_default();
            let prefix_width = 7usize;
            let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
            let text = format!("{label}{path}");
            let wrapped = textwrap::wrap(&text, content_width);
            for (line_index, line) in wrapped.into_iter().enumerate() {
                if line_index == 0 {
                    lines.push(
                        vec![
                            "  ".into(),
                            status.clone(),
                            " ".into(),
                            line.into_owned().into(),
                        ]
                        .into(),
                    );
                } else {
                    lines.push(vec!["      ".into(), line.into_owned().into()].into());
                }
            }
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

fn status_span(status: &CollabAgentStatus) -> Span<'static> {
    match status {
        CollabAgentStatus::PendingInit => "◌".cyan(),
        CollabAgentStatus::Running => "◌".cyan().bold(),
        CollabAgentStatus::Completed => "✓".green(),
        CollabAgentStatus::Interrupted => "!".yellow(),
        CollabAgentStatus::Errored | CollabAgentStatus::NotFound => "×".red(),
        CollabAgentStatus::Shutdown => "×".dim(),
    }
}

#[cfg(test)]
#[path = "spine_spawn_progress_tests.rs"]
mod tests;
