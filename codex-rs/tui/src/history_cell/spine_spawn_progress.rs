use crate::multi_agents::AgentActivityPreview;
use crate::render::line_utils::push_owned_lines;
use crate::wrapping::{RtOptions, adaptive_wrap_line};
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct SpineSpawnOverlay {
    notification: SpineSpawnProgressUpdatedNotification,
    activity: HashMap<String, AgentActivityPreview>,
}

impl SpineSpawnOverlay {
    pub(crate) fn new(notification: SpineSpawnProgressUpdatedNotification) -> Self {
        Self {
            notification,
            activity: HashMap::new(),
        }
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.notification.call_id
    }

    pub(crate) fn replace_notification(
        &mut self,
        notification: SpineSpawnProgressUpdatedNotification,
    ) {
        self.notification = notification;
        self.activity.retain(|agent_path, _| {
            self.notification
                .tasks
                .iter()
                .any(|task| task.agent_path.as_deref() == Some(agent_path.as_str()))
        });
    }

    pub(crate) fn update_activity(
        &mut self,
        agent_path: &str,
        preview: AgentActivityPreview,
        status: Option<CollabAgentStatus>,
    ) -> bool {
        let Some(task) = self
            .notification
            .tasks
            .iter_mut()
            .find(|task| task.agent_path.as_deref() == Some(agent_path))
        else {
            return false;
        };
        if let Some(status) = status {
            task.status = status;
        }
        self.activity.insert(agent_path.to_string(), preview);
        true
    }

    pub(crate) fn display_lines(
        &self,
        prefix: &str,
        is_last: bool,
        width: u16,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let counts = task_counts(&self.notification);
        let aggregate = aggregate_label(counts);
        let aggregate_line = Line::from(vec![
            Span::from(format!("{prefix}{}", dotted_branch(is_last))).dim(),
            aggregate_status_span(counts),
            " ".into(),
            aggregate.magenta().bold(),
        ]);
        push_wrapped_line(
            aggregate_line,
            format!("{prefix}{}  ", child_prefix(is_last)),
            width,
            &mut lines,
        );

        let task_prefix = format!("{prefix}{}", child_prefix(is_last));
        let task_count = self.notification.tasks.len();
        for (index, task) in self.notification.tasks.iter().enumerate() {
            let task_is_last = index + 1 == task_count;
            let label = format!("[{}] {}", task.ordinal, task.summary.trim());
            let label_line = Line::from(vec![
                Span::from(format!("{task_prefix}{}", dotted_branch(task_is_last))).dim(),
                status_span(&task.status),
                " ".into(),
                label.into(),
            ]);
            let continuation = format!("{task_prefix}{}  ", child_prefix(task_is_last));
            push_wrapped_line(label_line, continuation, width, &mut lines);

            if !matches!(
                task.status,
                CollabAgentStatus::PendingInit | CollabAgentStatus::Running
            ) {
                continue;
            }
            let activity_prefix = format!("{task_prefix}{}", child_prefix(task_is_last));
            let activity_width = width
                .saturating_sub(activity_prefix.chars().count() as u16)
                .max(1);
            let preview = task
                .agent_path
                .as_deref()
                .and_then(|path| self.activity.get(path));
            let mut preview_lines = preview
                .map(|preview| preview.lines(activity_width))
                .unwrap_or_default();
            if preview_lines.is_empty() {
                preview_lines.push("Waiting for activity...".dim().italic().into());
            }
            while preview_lines.len() < 3 {
                preview_lines.push(Line::default());
            }
            lines.extend(preview_lines.into_iter().take(3).map(|mut line| {
                line.spans.insert(0, Span::from(activity_prefix.clone()));
                line
            }));
        }
        lines
    }
}

fn push_wrapped_line(
    line: Line<'static>,
    continuation: String,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.max(1) as usize).subsequent_indent(continuation.into()),
    );
    push_owned_lines(&wrapped, out);
}

#[derive(Default, Clone, Copy)]
struct TaskCounts {
    running: usize,
    complete: usize,
    interrupted: usize,
    failed: usize,
    stopped: usize,
}

fn task_counts(notification: &SpineSpawnProgressUpdatedNotification) -> TaskCounts {
    notification
        .tasks
        .iter()
        .fold(TaskCounts::default(), |mut counts, task| {
            match task.status {
                CollabAgentStatus::PendingInit | CollabAgentStatus::Running => counts.running += 1,
                CollabAgentStatus::Completed => counts.complete += 1,
                CollabAgentStatus::Interrupted => counts.interrupted += 1,
                CollabAgentStatus::Errored | CollabAgentStatus::NotFound => counts.failed += 1,
                CollabAgentStatus::Shutdown => counts.stopped += 1,
            }
            counts
        })
}

fn aggregate_label(counts: TaskCounts) -> String {
    let mut parts = vec![
        format!("{} running", counts.running),
        format!("{} complete", counts.complete),
    ];
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    if counts.interrupted > 0 {
        parts.push(format!("{} interrupted", counts.interrupted));
    }
    if counts.stopped > 0 {
        parts.push(format!("{} stopped", counts.stopped));
    }
    format!("spine.spawn {}", parts.join(" · "))
}

fn aggregate_status_span(counts: TaskCounts) -> Span<'static> {
    if counts.failed > 0 {
        "×".red()
    } else if counts.running > 0 {
        "◐".cyan().bold()
    } else if counts.interrupted > 0 || counts.stopped > 0 {
        "!".yellow()
    } else {
        "✓".green()
    }
}

fn dotted_branch(is_last: bool) -> &'static str {
    if is_last { "└┈ " } else { "├┈ " }
}

fn child_prefix(is_last: bool) -> &'static str {
    if is_last { "   " } else { "│  " }
}

fn status_span(status: &CollabAgentStatus) -> Span<'static> {
    match status {
        CollabAgentStatus::PendingInit => "◌".cyan(),
        CollabAgentStatus::Running => "◐".cyan().bold(),
        CollabAgentStatus::Completed => "✓".green(),
        CollabAgentStatus::Interrupted => "!".yellow(),
        CollabAgentStatus::Errored | CollabAgentStatus::NotFound => "×".red(),
        CollabAgentStatus::Shutdown => "×".dim(),
    }
}
