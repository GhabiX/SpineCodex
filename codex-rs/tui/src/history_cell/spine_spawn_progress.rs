use super::spine_spawn_completion::CompletionFrame;
use super::spine_spawn_completion::SettledTaskVisual;
use super::spine_spawn_completion::completion_deadline;
use super::spine_spawn_completion::completion_frame;
use super::spine_spawn_completion::next_frame_in;
use crate::color::blend;
use crate::motion::MotionMode;
use crate::motion::ORGANIC_ACTIVITY_WORDS;
use crate::motion::spine_brand_shimmer_text;
use crate::multi_agents::AgentActivityPreview;
use crate::multi_agents::AgentActivityTracker;
use crate::product_brand::SPINE_BRAND_COLOR;
use crate::render::line_utils::push_owned_lines;
use crate::style::muted_text_style;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SpineSpawnOutcome;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use codex_app_server_protocol::SpineSpawnTaskProgress;
use codex_app_server_protocol::ThreadStatus;
use rand::Rng;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

const ACTIVITY_PREVIEW_LINES: usize = 4;
const ACTIVITY_INDENT: &str = "   ";
const COMPLETION_RGB: (u8, u8, u8) = (86, 191, 128);
const FALLBACK_FOREGROUND_RGB: (u8, u8, u8) = (160, 160, 160);

#[derive(Debug, Clone)]
struct TaskVisual {
    activity: Option<AgentActivityTracker>,
    activity_word: String,
    completed_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct SpineSpawnOverlay {
    notification: SpineSpawnProgressUpdatedNotification,
    visuals: HashMap<String, TaskVisual>,
    started_at: Instant,
}

impl SpineSpawnOverlay {
    pub(crate) fn new(notification: SpineSpawnProgressUpdatedNotification) -> Self {
        let started_at = Instant::now();
        let mut visuals = HashMap::new();
        sync_task_visuals(&notification.tasks, &mut visuals, started_at);
        Self {
            notification,
            visuals,
            started_at,
        }
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.notification.call_id
    }

    pub(crate) fn thread_id(&self) -> &str {
        &self.notification.thread_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.notification.turn_id
    }

    pub(crate) fn task_signature(&self) -> Vec<(u32, String)> {
        self.notification
            .tasks
            .iter()
            .map(|task| (task.ordinal, task.thread_id.clone()))
            .collect()
    }

    pub(crate) fn replace_notification(
        &mut self,
        mut notification: SpineSpawnProgressUpdatedNotification,
    ) {
        let now = Instant::now();
        for task in &mut notification.tasks {
            if let Some(current) = self
                .notification
                .tasks
                .iter()
                .find(|current| current.thread_id == task.thread_id)
            {
                task.status = merged_status(&current.status, task.status.clone());
            }
        }
        self.notification = notification;
        sync_task_visuals(&self.notification.tasks, &mut self.visuals, now);
    }

    pub(crate) fn seed_activity(
        &mut self,
        thread_id: &str,
        notifications: impl Iterator<Item = ServerNotification>,
    ) -> bool {
        let Some(task_index) = self
            .notification
            .tasks
            .iter()
            .position(|task| task.thread_id == thread_id)
        else {
            return false;
        };
        let now = Instant::now();
        let mut changed = false;
        for notification in notifications {
            changed |= {
                let tracker = self
                    .visuals
                    .get_mut(thread_id)
                    .expect("known spawn task")
                    .activity
                    .get_or_insert_default();
                apply_notification(
                    &mut self.notification.tasks[task_index],
                    tracker,
                    &notification,
                    spine_spawn_status(&notification),
                )
            };
        }
        sync_task_visuals(&self.notification.tasks, &mut self.visuals, now);
        changed
    }

    pub(crate) fn update_activity(
        &mut self,
        thread_id: &str,
        notification: &ServerNotification,
        status: Option<CollabAgentStatus>,
    ) -> bool {
        let Some(task_index) = self
            .notification
            .tasks
            .iter()
            .position(|task| task.thread_id == thread_id)
        else {
            return false;
        };
        let tracker = self
            .visuals
            .get_mut(thread_id)
            .expect("known spawn task")
            .activity
            .get_or_insert_default();
        let changed = apply_notification(
            &mut self.notification.tasks[task_index],
            tracker,
            notification,
            status,
        );
        sync_task_visuals(&self.notification.tasks, &mut self.visuals, Instant::now());
        changed
    }

    pub(crate) fn has_child_thread(&self, thread_id: &str) -> bool {
        self.notification
            .tasks
            .iter()
            .any(|task| task.thread_id == thread_id)
    }

    pub(crate) fn summary_for_child_thread(&self, thread_id: &str) -> Option<&str> {
        self.notification
            .tasks
            .iter()
            .find(|task| task.thread_id == thread_id)
            .map(|task| task.summary.trim())
            .filter(|summary| !summary.is_empty())
    }

    pub(crate) fn child_thread_ids(&self) -> impl Iterator<Item = &str> {
        self.notification
            .tasks
            .iter()
            .map(|task| task.thread_id.as_str())
    }

    pub(crate) fn has_activity(&self, thread_id: &str) -> bool {
        self.visuals
            .get(thread_id)
            .is_some_and(|visual| visual.activity.is_some())
    }

    pub(crate) fn update_status(&mut self, thread_id: &str, status: CollabAgentStatus) -> bool {
        let Some(task_index) = self
            .notification
            .tasks
            .iter()
            .position(|task| task.thread_id == thread_id)
        else {
            return false;
        };
        let changed = apply_status(&mut self.notification.tasks[task_index].status, status);
        sync_task_visuals(&self.notification.tasks, &mut self.visuals, Instant::now());
        changed
    }

    pub(crate) fn display_lines(
        &self,
        prefix: &str,
        is_last: bool,
        width: u16,
        animations_enabled: bool,
    ) -> Vec<Line<'static>> {
        self.display_lines_at(prefix, is_last, width, animations_enabled, Instant::now())
    }

    pub(crate) fn display_lines_at(
        &self,
        prefix: &str,
        is_last: bool,
        width: u16,
        animations_enabled: bool,
        now: Instant,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (index, task) in self.notification.tasks.iter().enumerate() {
            let task_is_last = is_last && index + 1 == self.notification.tasks.len();
            let visual = &self.visuals[task.thread_id.as_str()];
            let activity_word = visual.activity_word.as_str();
            let completion = (task.status == CollabAgentStatus::Completed)
                .then_some(visual.completed_at)
                .flatten();
            let completion_frame = completion.map(|completed_at| {
                let render_at = if animations_enabled {
                    now
                } else {
                    completion_deadline(completed_at)
                };
                completion_frame(activity_word, completed_at, render_at)
            });
            let mut label_spans =
                vec![Span::from(format!("{prefix}{}", branch(task_is_last))).dim()];
            if let Some(frame) = completion_frame.as_ref() {
                label_spans.extend(completion_activity_spans(frame));
            } else {
                label_spans.extend(status_and_activity_word_spans(
                    &task.status,
                    activity_word,
                    animations_enabled,
                ));
            }
            label_spans.push(" ".into());
            label_spans.push(task.summary.trim().to_string().into());
            let label_line = Line::from(label_spans);
            let continuation = Span::from(format!("{prefix}{}  ", child_prefix(task_is_last)))
                .dim()
                .into();
            push_wrapped_line(label_line, continuation, width, &mut lines);

            let preview = visual.activity.as_ref().map(AgentActivityTracker::preview);
            let (empty_state, alphas) = match completion_frame.as_ref() {
                Some(frame) if !frame.body_alphas.is_empty() => (
                    "Waiting for activity...",
                    Some(frame.body_alphas.as_slice()),
                ),
                Some(_) => continue,
                None => match task.status {
                    CollabAgentStatus::PendingInit => ("Waiting to start...", None),
                    CollabAgentStatus::Running => ("Waiting for activity...", None),
                    _ => continue,
                },
            };
            append_activity_body(
                &mut lines,
                preview.as_ref(),
                empty_state,
                prefix,
                task_is_last,
                width,
                alphas,
            );
        }
        lines
    }

    pub(crate) fn animation_start(&self) -> Instant {
        self.started_at
    }

    #[cfg(test)]
    pub(crate) fn completion_deadline(&self, thread_id: &str) -> Option<Instant> {
        self.visuals
            .get(thread_id)?
            .completed_at
            .map(completion_deadline)
    }

    pub(crate) fn next_completion_frame_in(&self, now: Instant) -> Option<Duration> {
        self.visuals
            .values()
            .filter_map(|visual| next_frame_in(visual.completed_at?, now))
            .min()
    }

    pub(crate) fn settled_task_visuals(&self) -> Option<Vec<SettledTaskVisual>> {
        self.notification
            .tasks
            .iter()
            .enumerate()
            .map(|(ordinal, task)| {
                (task.ordinal == ordinal as u32).then_some(())?;
                let outcome = match &task.status {
                    CollabAgentStatus::Completed => SpineSpawnOutcome::Completed,
                    CollabAgentStatus::Errored | CollabAgentStatus::NotFound => {
                        SpineSpawnOutcome::Errored
                    }
                    CollabAgentStatus::Interrupted | CollabAgentStatus::Shutdown => {
                        SpineSpawnOutcome::Aborted
                    }
                    CollabAgentStatus::PendingInit | CollabAgentStatus::Running => return None,
                };
                Some(SettledTaskVisual {
                    outcome,
                    completion_deadline: match outcome {
                        SpineSpawnOutcome::Completed => Some(completion_deadline(
                            self.visuals.get(&task.thread_id)?.completed_at?,
                        )),
                        SpineSpawnOutcome::Errored | SpineSpawnOutcome::Aborted => None,
                    },
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn activity_word(&self, thread_id: &str) -> Option<&str> {
        self.visuals
            .get(thread_id)
            .map(|visual| visual.activity_word.as_str())
    }

    #[cfg(test)]
    pub(crate) fn set_activity_word_for_test(&mut self, thread_id: &str, word: &str) -> bool {
        let Some(visual) = self.visuals.get_mut(thread_id) else {
            return false;
        };
        visual.activity_word = word.to_string();
        true
    }
}

fn apply_notification(
    task: &mut SpineSpawnTaskProgress,
    tracker: &mut AgentActivityTracker,
    notification: &ServerNotification,
    status: Option<CollabAgentStatus>,
) -> bool {
    // Parent settlement and child notifications are separate app-server streams. A terminal
    // progress update can therefore arrive before the child's final item delta. The TUI keeps the
    // child stream open until its own terminal barrier, so activity remains admissible here even
    // after the parent has reported a terminal task status.
    let activity_changed = tracker.apply(notification);
    let status_changed = status.is_some_and(|status| apply_status(&mut task.status, status));
    let inferred_running = activity_changed
        && task.status == CollabAgentStatus::PendingInit
        && apply_status(&mut task.status, CollabAgentStatus::Running);
    activity_changed || status_changed || inferred_running
}

fn apply_status(current: &mut CollabAgentStatus, incoming: CollabAgentStatus) -> bool {
    let next = merged_status(current, incoming);
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn merged_status(current: &CollabAgentStatus, incoming: CollabAgentStatus) -> CollabAgentStatus {
    let is_failure = |status: &CollabAgentStatus| {
        matches!(
            status,
            CollabAgentStatus::Interrupted
                | CollabAgentStatus::Errored
                | CollabAgentStatus::Shutdown
                | CollabAgentStatus::NotFound
        )
    };
    if is_failure(current)
        || (*current == CollabAgentStatus::Completed && !is_failure(&incoming))
        || (*current != CollabAgentStatus::PendingInit
            && incoming == CollabAgentStatus::PendingInit)
    {
        return current.clone();
    }
    incoming
}

pub(crate) fn spine_spawn_status(notification: &ServerNotification) -> Option<CollabAgentStatus> {
    match notification {
        ServerNotification::TurnStarted(_) => Some(CollabAgentStatus::Running),
        ServerNotification::ThreadStatusChanged(notification)
            if matches!(notification.status, ThreadStatus::Active { .. }) =>
        {
            Some(CollabAgentStatus::Running)
        }
        _ => None,
    }
}

fn sync_task_visuals(
    tasks: &[SpineSpawnTaskProgress],
    visuals: &mut HashMap<String, TaskVisual>,
    now: Instant,
) {
    visuals.retain(|thread_id, _| tasks.iter().any(|task| task.thread_id == *thread_id));
    let mut available = ORGANIC_ACTIVITY_WORDS
        .iter()
        .copied()
        .filter(|word| !visuals.values().any(|visual| visual.activity_word == *word))
        .collect::<Vec<_>>();
    let mut rng = rand::rng();
    for task in tasks {
        if !visuals.contains_key(&task.thread_id) {
            let word = if !available.is_empty() {
                let index = rng.random_range(0..available.len());
                available.swap_remove(index).to_string()
            } else {
                let base =
                    ORGANIC_ACTIVITY_WORDS[rng.random_range(0..ORGANIC_ACTIVITY_WORDS.len())];
                let mut label = format!("Further {base}");
                while visuals.values().any(|visual| visual.activity_word == label) {
                    label.insert_str(0, "Further ");
                }
                label
            };
            visuals.insert(
                task.thread_id.clone(),
                TaskVisual {
                    activity: None,
                    activity_word: word,
                    completed_at: None,
                },
            );
        }
        let visual = visuals
            .get_mut(task.thread_id.as_str())
            .expect("known spawn task");
        visual.completed_at = match task.status {
            CollabAgentStatus::Completed => Some(visual.completed_at.unwrap_or(now)),
            CollabAgentStatus::Interrupted
            | CollabAgentStatus::Errored
            | CollabAgentStatus::Shutdown
            | CollabAgentStatus::NotFound => None,
            CollabAgentStatus::PendingInit | CollabAgentStatus::Running => visual.completed_at,
        };
    }
}

fn status_and_activity_word_spans(
    status: &CollabAgentStatus,
    activity_word: &str,
    animations_enabled: bool,
) -> Vec<Span<'static>> {
    if *status == CollabAgentStatus::Running {
        return spine_brand_shimmer_text(
            activity_word,
            MotionMode::from_animations_enabled(animations_enabled),
        );
    }

    vec![
        status_span(status),
        " ".into(),
        Span::from(activity_word.to_string()).fg(SPINE_BRAND_COLOR),
    ]
}

fn completion_activity_spans(frame: &CompletionFrame) -> Vec<Span<'static>> {
    let background = default_bg();
    let check_width = frame.activity_slots.first().map_or(1, |slot| slot.width);
    frame
        .activity_slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let check_wins =
                index == 0 && frame.check_alpha > 0.0 && frame.check_alpha >= slot.alpha;
            if !check_wins && slot.alpha <= f32::EPSILON {
                return None;
            }
            let (content, alpha) = if check_wins {
                (
                    format!("✓{}", " ".repeat(check_width.saturating_sub(1))),
                    frame.check_alpha,
                )
            } else {
                let mut content = slot.grapheme.clone();
                content.push_str(&" ".repeat(slot.width.saturating_sub(slot.grapheme.width())));
                (content, slot.alpha)
            };
            let style = completion_style(
                Style::default().fg(SPINE_BRAND_COLOR),
                COMPLETION_RGB,
                alpha,
                background,
            );
            Some(Span::styled(content, style))
        })
        .collect()
}

fn append_activity_body(
    out: &mut Vec<Line<'static>>,
    preview: Option<&AgentActivityPreview>,
    empty_state: &str,
    task_prefix: &str,
    task_is_last: bool,
    width: u16,
    alphas: Option<&[f32]>,
) {
    let activity_prefix = format!(
        "{task_prefix}{}{ACTIVITY_INDENT}",
        child_prefix(task_is_last)
    );
    let activity_width = width
        .saturating_sub(activity_prefix.chars().count() as u16)
        .max(1);
    let mut lines = preview
        .map(|preview| preview.lines_with_limit(activity_width, ACTIVITY_PREVIEW_LINES))
        .unwrap_or_default();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            empty_state.to_string(),
            muted_text_style().italic(),
        )));
    }
    lines.resize(ACTIVITY_PREVIEW_LINES, Line::default());
    for line in &mut lines {
        line.spans
            .insert(0, Span::from(activity_prefix.clone()).dim());
    }
    lines.push(activity_separator(task_prefix, task_is_last));
    if let Some(alphas) = alphas {
        lines.truncate(alphas.len());
        let background = default_bg();
        let foreground = default_fg().unwrap_or(FALLBACK_FOREGROUND_RGB);
        for (line, alpha) in lines.iter_mut().zip(alphas.iter().copied()) {
            for span in &mut line.spans {
                span.style = completion_style(span.style, foreground, alpha, background);
            }
        }
    }
    out.extend(lines);
}

fn completion_style(
    base: Style,
    foreground: (u8, u8, u8),
    alpha: f32,
    background: Option<(u8, u8, u8)>,
) -> Style {
    let alpha = if alpha < 0.01 { 0.0 } else { alpha };
    if alpha >= 1.0 - f32::EPSILON {
        return base;
    }
    let style = base.fg(best_color(background.map_or(foreground, |background| {
        blend(foreground, background, alpha)
    })));
    if background.is_none() && alpha < 0.65 {
        return style.add_modifier(Modifier::DIM);
    }
    style
}

fn activity_separator(prefix: &str, task_is_last: bool) -> Line<'static> {
    if !task_is_last {
        Span::from(format!("{prefix}│")).dim().into()
    } else {
        Default::default()
    }
}

fn push_wrapped_line(
    line: Line<'static>,
    continuation: Line<'static>,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.max(1) as usize).subsequent_indent(continuation),
    );
    push_owned_lines(&wrapped, out);
}

fn branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn child_prefix(is_last: bool) -> &'static str {
    if is_last { "  " } else { "│ " }
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
