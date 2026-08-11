//! Helpers for rendering and navigating multi-agent state in the TUI.
//!
//! This module owns the shared presentation contracts for multi-agent history rows, `/agent` picker
//! entries, and the fast-switch keyboard shortcuts. Higher-level coordination, such as deciding
//! which thread becomes active or when a thread closes, stays in [`crate::app::App`].

use crate::history_cell::PlainHistoryCell;
use crate::render::line_utils::prefix_lines;
use crate::style::muted_text_style;
use crate::text_formatting::truncate_text;
use codex_app_server_protocol::CollabAgentState;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
#[cfg(target_os = "macos")]
use crossterm::event::KeyEventKind;
#[cfg(target_os = "macos")]
use crossterm::event::KeyModifiers;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashSet;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

const COLLAB_PROMPT_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES: usize = 240;
const AGENT_ACTIVITY_PREVIEW_LINES: usize = 3;
const AGENT_ACTIVITY_PREVIEW_ITEMS: usize = 6;
const AGENT_ACTIVITY_PREVIEW_GRAPHEMES: usize = 240;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentActivityPreview {
    activity: Vec<String>,
}

impl AgentActivityPreview {
    fn from_summaries<'a>(summaries: impl Iterator<Item = &'a str>) -> Self {
        let mut activity = summaries
            .filter_map(bounded_agent_activity_summary)
            .collect::<Vec<_>>();
        if activity.len() > AGENT_ACTIVITY_PREVIEW_ITEMS {
            activity.drain(..activity.len() - AGENT_ACTIVITY_PREVIEW_ITEMS);
        }
        Self { activity }
    }

    pub(crate) fn lines(&self, width: u16) -> Vec<Line<'static>> {
        self.lines_with_limit(width, AGENT_ACTIVITY_PREVIEW_LINES)
    }

    pub(crate) fn lines_with_limit(&self, width: u16, max_lines: usize) -> Vec<Line<'static>> {
        if max_lines == 0 {
            return Vec::new();
        }
        let style = muted_text_style();
        let width = usize::from(width.max(1));
        let mut remaining = max_lines;
        let mut newest_first = Vec::new();
        for activity in self.activity.iter().rev() {
            let wrapped = textwrap::wrap(activity, width)
                .into_iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| Line::from(Span::styled(line.into_owned(), style)))
                .collect::<Vec<_>>();
            if wrapped.is_empty() {
                continue;
            }
            if wrapped.len() <= remaining {
                remaining -= wrapped.len();
                newest_first.push(wrapped);
                if remaining == 0 {
                    break;
                }
                continue;
            }
            if newest_first.is_empty() {
                if max_lines == 1 {
                    let inline_width = width.saturating_sub(1);
                    let preview = if inline_width == 0 {
                        String::new()
                    } else {
                        textwrap::wrap(activity, inline_width)
                            .first()
                            .map(|line| line.trim_end().to_string())
                            .unwrap_or_default()
                    };
                    newest_first.push(vec![Line::from(Span::styled(format!("{preview}…"), style))]);
                    break;
                }
                let mut clipped = wrapped
                    .into_iter()
                    .take(max_lines.saturating_sub(1))
                    .collect::<Vec<_>>();
                clipped.push(Line::from(Span::styled("…", style)));
                newest_first.push(clipped);
            } else if remaining > 0 {
                newest_first.push(vec![Line::from(Span::styled("…", style))]);
            }
            break;
        }
        newest_first.into_iter().rev().flatten().collect()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentActivityTracker {
    entries: VecDeque<AgentActivityEntry>,
}

#[derive(Debug, Clone)]
struct AgentActivityEntry {
    item_id: String,
    summary_index: Option<i64>,
    summary: String,
}

impl AgentActivityTracker {
    pub(crate) fn apply(&mut self, notification: &ServerNotification) -> bool {
        match notification {
            ServerNotification::ItemStarted(notification) => {
                self.touch_started_item(&notification.item)
            }
            ServerNotification::ItemCompleted(notification) => {
                let item_id = notification.item.id().to_string();
                let removed = self.remove_item(&item_id);
                let inserted =
                    self.push_summary(item_id, None, agent_activity_summary(&notification.item));
                removed || inserted
            }
            ServerNotification::AgentMessageDelta(notification) => {
                self.append_delta(&notification.item_id, None, &notification.delta)
            }
            ServerNotification::PlanDelta(notification) => {
                self.append_delta(&notification.item_id, None, &notification.delta)
            }
            ServerNotification::ReasoningSummaryTextDelta(notification) => self.append_delta(
                &notification.item_id,
                Some(notification.summary_index),
                &notification.delta,
            ),
            _ => false,
        }
    }

    pub(crate) fn preview(&self) -> AgentActivityPreview {
        AgentActivityPreview::from_summaries(
            self.entries.iter().map(|entry| entry.summary.as_str()),
        )
    }

    fn touch_started_item(&mut self, item: &ThreadItem) -> bool {
        let item_id = item.id().to_string();
        let summary = agent_activity_summary(item);
        let Some(summary) = summary.and_then(|summary| bounded_agent_activity_summary(&summary))
        else {
            return false;
        };
        if let Some(position) = self.position(&item_id, None) {
            let entry = self.entries.remove(position).expect("position must exist");
            self.entries.push_back(entry);
        } else {
            self.entries.push_back(AgentActivityEntry {
                item_id,
                summary_index: None,
                summary,
            });
            self.trim();
        }
        true
    }

    fn push_summary(
        &mut self,
        item_id: String,
        summary_index: Option<i64>,
        summary: Option<String>,
    ) -> bool {
        let Some(summary) = summary.and_then(|summary| bounded_agent_activity_summary(&summary))
        else {
            return false;
        };
        self.entries.push_back(AgentActivityEntry {
            item_id,
            summary_index,
            summary,
        });
        self.trim();
        true
    }

    fn append_delta(&mut self, item_id: &str, summary_index: Option<i64>, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let mut entry = self
            .position(item_id, summary_index)
            .and_then(|position| self.entries.remove(position))
            .unwrap_or_else(|| AgentActivityEntry {
                item_id: item_id.to_string(),
                summary_index,
                summary: String::new(),
            });
        entry.summary.push_str(delta);
        entry.summary = truncate_activity_summary(&entry.summary);
        self.entries.push_back(entry);
        self.trim();
        true
    }

    fn remove_item(&mut self, item_id: &str) -> bool {
        let old_len = self.entries.len();
        self.entries.retain(|entry| entry.item_id != item_id);
        old_len != self.entries.len()
    }

    fn position(&self, item_id: &str, summary_index: Option<i64>) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.item_id == item_id && entry.summary_index == summary_index)
    }

    fn trim(&mut self) {
        while self.entries.len() > AGENT_ACTIVITY_PREVIEW_ITEMS {
            self.entries.pop_front();
        }
    }
}

fn agent_activity_summary(item: &ThreadItem) -> Option<String> {
    let summary = match item {
        ThreadItem::AgentMessage { text, .. } | ThreadItem::Plan { text, .. } => text,
        ThreadItem::Reasoning { summary, .. } => summary.last()?,
        ThreadItem::CommandExecution { command, .. } => {
            let command = truncate_text(
                command,
                AGENT_ACTIVITY_PREVIEW_GRAPHEMES.saturating_sub("$ ".len()),
            );
            return bounded_agent_activity_summary(&format!("$ {command}"));
        }
        ThreadItem::FileChange { changes, .. } => {
            return bounded_agent_activity_summary(&format!("Updated {} file(s)", changes.len()));
        }
        ThreadItem::McpToolCall { server, tool, .. } => {
            return bounded_agent_activity_summary(&format!("MCP {server}/{tool}"));
        }
        ThreadItem::DynamicToolCall {
            namespace, tool, ..
        } => {
            let tool = namespace
                .as_ref()
                .map(|namespace| format!("{namespace}/{tool}"))
                .unwrap_or_else(|| tool.clone());
            return bounded_agent_activity_summary(&format!("Tool {tool}"));
        }
        ThreadItem::CollabAgentToolCall { tool, .. } => {
            let action = match tool {
                CollabAgentTool::SpawnAgent => "Spawned an agent",
                CollabAgentTool::SendInput => "Sent input to an agent",
                CollabAgentTool::ResumeAgent => "Resumed an agent",
                CollabAgentTool::Wait => "Waited for an agent",
                CollabAgentTool::CloseAgent => "Closed an agent",
            };
            return Some(action.to_string());
        }
        ThreadItem::SubAgentActivity { kind, .. } => {
            let action = match kind {
                SubAgentActivityKind::Started => "Started",
                SubAgentActivityKind::Interacted => "Contacted",
                SubAgentActivityKind::Interrupted => "Interrupted",
            };
            return Some(format!("{action} sub-agent"));
        }
        ThreadItem::WebSearch(item) => {
            return bounded_agent_activity_summary(&format!("Web search: {}", item.query));
        }
        ThreadItem::ImageView { path, .. } => {
            return bounded_agent_activity_summary(&format!("Viewed {}", path.render_for_ui()));
        }
        ThreadItem::ImageGeneration(_) => return Some("Generated an image".to_string()),
        ThreadItem::EnteredReviewMode { .. } => return Some("Entered review mode".to_string()),
        ThreadItem::ExitedReviewMode { .. } => return Some("Exited review mode".to_string()),
        ThreadItem::ContextCompaction { .. } => return Some("Compacted context".to_string()),
        ThreadItem::UserMessage { .. }
        | ThreadItem::HookPrompt { .. }
        | ThreadItem::Sleep { .. } => return None,
    };
    bounded_agent_activity_summary(summary)
}

fn bounded_agent_activity_summary(summary: &str) -> Option<String> {
    let summary = truncate_text(summary, AGENT_ACTIVITY_PREVIEW_GRAPHEMES);
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    (!summary.is_empty()).then_some(summary)
}

fn truncate_activity_summary(summary: &str) -> String {
    let grapheme_count = summary.graphemes(true).count();
    if grapheme_count <= AGENT_ACTIVITY_PREVIEW_GRAPHEMES {
        summary.to_string()
    } else {
        let suffix = summary
            .graphemes(true)
            .skip(grapheme_count - AGENT_ACTIVITY_PREVIEW_GRAPHEMES + 1)
            .collect::<String>();
        format!("…{suffix}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentPickerThreadEntry {
    /// Human-friendly nickname shown in picker rows and footer labels.
    pub(crate) agent_nickname: Option<String>,
    /// Agent type shown in brackets when present, for example `worker`.
    pub(crate) agent_role: Option<String>,
    /// Canonical v2 agent path, when the thread was observed through v2 activity.
    pub(crate) agent_path: Option<String>,
    /// Whether the latest liveness refresh says the agent thread is actively working.
    pub(crate) is_running: bool,
    /// Whether the thread has emitted a close event and should render dimmed.
    pub(crate) is_closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubAgentActivityDisplay {
    pub(crate) thread_id: ThreadId,
    pub(crate) agent_path: String,
    pub(crate) is_running_hint: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentMetadata {
    /// Human-friendly nickname shown in rendered tool-call rows.
    pub(crate) agent_nickname: Option<String>,
    /// Agent type shown in brackets when present, for example `worker`.
    pub(crate) agent_role: Option<String>,
}

#[derive(Clone, Copy)]
struct AgentLabel<'a> {
    thread_id: Option<ThreadId>,
    nickname: Option<&'a str>,
    role: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnRequestSummary {
    pub(crate) model: String,
    pub(crate) reasoning_effort: ReasoningEffortConfig,
}

pub(crate) fn agent_picker_status_dot_spans(is_closed: bool) -> Vec<Span<'static>> {
    let dot = if is_closed {
        "•".into()
    } else {
        "•".green()
    };
    vec![dot, " ".into()]
}

pub(crate) fn format_agent_picker_item_name(
    agent_nickname: Option<&str>,
    agent_role: Option<&str>,
    is_primary: bool,
) -> String {
    if is_primary {
        return "Main [default]".to_string();
    }

    let agent_nickname = agent_nickname
        .map(str::trim)
        .filter(|nickname| !nickname.is_empty());
    let agent_role = agent_role.map(str::trim).filter(|role| !role.is_empty());
    match (agent_nickname, agent_role) {
        (Some(agent_nickname), Some(agent_role)) => format!("{agent_nickname} [{agent_role}]"),
        (Some(agent_nickname), None) => agent_nickname.to_string(),
        (None, Some(agent_role)) => format!("[{agent_role}]"),
        (None, None) => "Agent".to_string(),
    }
}

pub(crate) fn previous_agent_shortcut() -> crate::key_hint::KeyBinding {
    crate::key_hint::alt(KeyCode::Left)
}

pub(crate) fn next_agent_shortcut() -> crate::key_hint::KeyBinding {
    crate::key_hint::alt(KeyCode::Right)
}

/// Matches the canonical "previous agent" binding plus platform-specific fallbacks that keep agent
/// navigation working when enhanced key reporting is unavailable.
pub(crate) fn previous_agent_shortcut_matches(
    key_event: KeyEvent,
    allow_word_motion_fallback: bool,
) -> bool {
    previous_agent_shortcut().is_press(key_event)
        || previous_agent_word_motion_fallback(key_event, allow_word_motion_fallback)
}

/// Matches the canonical "next agent" binding plus platform-specific fallbacks that keep agent
/// navigation working when enhanced key reporting is unavailable.
pub(crate) fn next_agent_shortcut_matches(
    key_event: KeyEvent,
    allow_word_motion_fallback: bool,
) -> bool {
    next_agent_shortcut().is_press(key_event)
        || next_agent_word_motion_fallback(key_event, allow_word_motion_fallback)
}

#[cfg(target_os = "macos")]
fn previous_agent_word_motion_fallback(
    key_event: KeyEvent,
    allow_word_motion_fallback: bool,
) -> bool {
    // Some terminals, especially on macOS, send Option+b/f as word-motion keys instead of
    // Option+arrow events unless enhanced keyboard reporting is enabled. Callers should only
    // enable this fallback when the composer is empty so draft editing retains the expected
    // word-wise motion behavior.
    allow_word_motion_fallback
        && matches!(
            key_event,
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::ALT,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            }
        )
}

#[cfg(not(target_os = "macos"))]
fn previous_agent_word_motion_fallback(
    _key_event: KeyEvent,
    _allow_word_motion_fallback: bool,
) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn next_agent_word_motion_fallback(key_event: KeyEvent, allow_word_motion_fallback: bool) -> bool {
    // Some terminals, especially on macOS, send Option+b/f as word-motion keys instead of
    // Option+arrow events unless enhanced keyboard reporting is enabled. Callers should only
    // enable this fallback when the composer is empty so draft editing retains the expected
    // word-wise motion behavior.
    allow_word_motion_fallback
        && matches!(
            key_event,
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::ALT,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            }
        )
}

#[cfg(not(target_os = "macos"))]
fn next_agent_word_motion_fallback(
    _key_event: KeyEvent,
    _allow_word_motion_fallback: bool,
) -> bool {
    false
}

pub(crate) fn spawn_request_summary(item: &ThreadItem) -> Option<SpawnRequestSummary> {
    match item {
        ThreadItem::CollabAgentToolCall {
            tool: CollabAgentTool::SpawnAgent,
            model: Some(model),
            reasoning_effort: Some(reasoning_effort),
            ..
        } => Some(SpawnRequestSummary {
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn tool_call_history_cell(
    item: &ThreadItem,
    cached_spawn_request: Option<&SpawnRequestSummary>,
    mut agent_metadata: impl FnMut(ThreadId) -> AgentMetadata,
) -> Option<PlainHistoryCell> {
    let ThreadItem::CollabAgentToolCall {
        tool,
        status,
        receiver_thread_ids,
        prompt,
        agents_states,
        ..
    } = item
    else {
        return None;
    };

    let first_receiver = receiver_thread_ids
        .first()
        .and_then(|id| parse_thread_id(id));
    let prompt = prompt.as_deref().unwrap_or_default();

    match tool {
        CollabAgentTool::SpawnAgent => {
            if matches!(status, CollabAgentToolCallStatus::InProgress) {
                return None;
            }
            let fallback_spawn_request = spawn_request_summary(item);
            let spawn_request = cached_spawn_request.or(fallback_spawn_request.as_ref());
            Some(spawn_end(
                first_receiver,
                prompt,
                spawn_request,
                &mut agent_metadata,
            ))
        }
        CollabAgentTool::SendInput => {
            if matches!(status, CollabAgentToolCallStatus::InProgress) {
                return None;
            }
            first_receiver.map(|receiver_thread_id| {
                interaction_end(receiver_thread_id, prompt, &mut agent_metadata)
            })
        }
        CollabAgentTool::ResumeAgent => first_receiver.map(|receiver_thread_id| {
            if matches!(status, CollabAgentToolCallStatus::InProgress) {
                resume_begin(receiver_thread_id, &mut agent_metadata)
            } else {
                let state = first_agent_state(receiver_thread_ids, agents_states);
                resume_end(
                    receiver_thread_id,
                    state,
                    "Agent resume failed",
                    &mut agent_metadata,
                )
            }
        }),
        CollabAgentTool::Wait => {
            if matches!(status, CollabAgentToolCallStatus::InProgress) {
                Some(waiting_begin(receiver_thread_ids, &mut agent_metadata))
            } else {
                Some(waiting_end(
                    receiver_thread_ids,
                    agents_states,
                    &mut agent_metadata,
                ))
            }
        }
        CollabAgentTool::CloseAgent => {
            if matches!(status, CollabAgentToolCallStatus::InProgress) {
                return None;
            }
            first_receiver
                .map(|receiver_thread_id| close_end(receiver_thread_id, &mut agent_metadata))
        }
    }
}

pub(crate) fn sub_agent_activity_display(item: &ThreadItem) -> Option<SubAgentActivityDisplay> {
    let ThreadItem::SubAgentActivity {
        kind,
        agent_thread_id,
        agent_path,
        ..
    } = item
    else {
        return None;
    };
    Some(SubAgentActivityDisplay {
        thread_id: parse_thread_id(agent_thread_id)?,
        agent_path: agent_path.clone(),
        is_running_hint: !matches!(kind, SubAgentActivityKind::Interrupted),
    })
}

pub(crate) fn sub_agent_activity_history_cell(item: &ThreadItem) -> Option<PlainHistoryCell> {
    let ThreadItem::SubAgentActivity {
        kind, agent_path, ..
    } = item
    else {
        return None;
    };
    Some(collab_event(
        sub_agent_activity_title(*kind, agent_path),
        Vec::new(),
    ))
}

pub(crate) fn sub_agent_activity_summary(kind: SubAgentActivityKind, agent_path: &str) -> String {
    match kind {
        SubAgentActivityKind::Started => format!("Started `{agent_path}`"),
        SubAgentActivityKind::Interacted => format!("Interacted with `{agent_path}`"),
        SubAgentActivityKind::Interrupted => format!("Interrupted `{agent_path}`"),
    }
}

fn sub_agent_activity_title(kind: SubAgentActivityKind, agent_path: &str) -> Line<'static> {
    let (prefix, path) = match kind {
        SubAgentActivityKind::Started => ("Started ", agent_path),
        SubAgentActivityKind::Interacted => ("Interacted with ", agent_path),
        SubAgentActivityKind::Interrupted => ("Interrupted ", agent_path),
    };
    title_spans_line(vec![
        Span::from(prefix).bold(),
        Span::from(format!("`{path}`")).cyan(),
    ])
}

fn spawn_end(
    new_thread_id: Option<ThreadId>,
    prompt: &str,
    spawn_request: Option<&SpawnRequestSummary>,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    let title = match new_thread_id {
        Some(thread_id) => title_with_agent(
            "Spawned",
            agent_label(thread_id, &agent_metadata(thread_id)),
            spawn_request,
        ),
        None => title_text("Agent spawn failed"),
    };

    let mut details = Vec::new();
    if let Some(line) = prompt_line(prompt) {
        details.push(line);
    }
    collab_event(title, details)
}

fn interaction_end(
    receiver_thread_id: ThreadId,
    prompt: &str,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    let title = title_with_agent(
        "Sent input to",
        agent_label(receiver_thread_id, &agent_metadata(receiver_thread_id)),
        /*spawn_request*/ None,
    );

    let mut details = Vec::new();
    if let Some(line) = prompt_line(prompt) {
        details.push(line);
    }
    collab_event(title, details)
}

fn waiting_begin(
    receiver_thread_ids: &[String],
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    let receiver_agents = receiver_thread_ids
        .iter()
        .filter_map(|thread_id| parse_thread_id(thread_id))
        .map(|thread_id| (thread_id, agent_metadata(thread_id)))
        .collect::<Vec<_>>();

    let title = match receiver_agents.as_slice() {
        [(thread_id, metadata)] => title_with_agent(
            "Waiting for",
            agent_label(*thread_id, metadata),
            /*spawn_request*/ None,
        ),
        [] => title_text("Waiting for agents"),
        _ => title_text(format!("Waiting for {} agents", receiver_agents.len())),
    };

    let details = if receiver_agents.len() > 1 {
        receiver_agents
            .iter()
            .map(|(thread_id, metadata)| agent_label_line(agent_label(*thread_id, metadata)))
            .collect()
    } else {
        Vec::new()
    };

    collab_event(title, details)
}

fn waiting_end(
    receiver_thread_ids: &[String],
    agents_states: &std::collections::HashMap<String, CollabAgentState>,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    let details = wait_complete_lines(receiver_thread_ids, agents_states, agent_metadata);
    collab_event(title_text("Finished waiting"), details)
}

fn close_end(
    receiver_thread_id: ThreadId,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    collab_event(
        title_with_agent(
            "Closed",
            agent_label(receiver_thread_id, &agent_metadata(receiver_thread_id)),
            /*spawn_request*/ None,
        ),
        Vec::new(),
    )
}

fn resume_begin(
    receiver_thread_id: ThreadId,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    collab_event(
        title_with_agent(
            "Resuming",
            agent_label(receiver_thread_id, &agent_metadata(receiver_thread_id)),
            /*spawn_request*/ None,
        ),
        Vec::new(),
    )
}

fn resume_end(
    receiver_thread_id: ThreadId,
    status: Option<&CollabAgentState>,
    fallback_error: &str,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> PlainHistoryCell {
    collab_event(
        title_with_agent(
            "Resumed",
            agent_label(receiver_thread_id, &agent_metadata(receiver_thread_id)),
            /*spawn_request*/ None,
        ),
        vec![status_summary_line(status, fallback_error)],
    )
}

fn collab_event(title: Line<'static>, details: Vec<Line<'static>>) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = vec![title];
    if !details.is_empty() {
        lines.extend(prefix_lines(details, "  └ ".dim(), "    ".into()));
    }
    PlainHistoryCell::new(lines)
}

fn title_text(title: impl Into<String>) -> Line<'static> {
    title_spans_line(vec![Span::from(title.into()).bold()])
}

fn title_with_agent(
    prefix: &str,
    agent: AgentLabel<'_>,
    spawn_request: Option<&SpawnRequestSummary>,
) -> Line<'static> {
    let mut spans = vec![Span::from(format!("{prefix} ")).bold()];
    spans.extend(agent_label_spans(agent));
    spans.extend(spawn_request_spans(spawn_request));
    title_spans_line(spans)
}

fn title_spans_line(mut spans: Vec<Span<'static>>) -> Line<'static> {
    let mut title = Vec::with_capacity(spans.len() + 1);
    title.push(Span::from("• ").dim());
    title.append(&mut spans);
    title.into()
}

fn parse_thread_id(thread_id: &str) -> Option<ThreadId> {
    ThreadId::from_string(thread_id).ok()
}

fn agent_label(thread_id: ThreadId, metadata: &AgentMetadata) -> AgentLabel<'_> {
    AgentLabel {
        thread_id: Some(thread_id),
        nickname: metadata.agent_nickname.as_deref(),
        role: metadata.agent_role.as_deref(),
    }
}

fn agent_label_line(agent: AgentLabel<'_>) -> Line<'static> {
    agent_label_spans(agent).into()
}

fn agent_label_spans(agent: AgentLabel<'_>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let nickname = agent
        .nickname
        .map(str::trim)
        .filter(|nickname| !nickname.is_empty());
    let role = agent.role.map(str::trim).filter(|role| !role.is_empty());

    if let Some(nickname) = nickname {
        spans.push(Span::from(nickname.to_string()).cyan().bold());
    } else if let Some(thread_id) = agent.thread_id {
        spans.push(Span::from(thread_id.to_string()).cyan());
    } else {
        spans.push(Span::from("agent").cyan());
    }

    if let Some(role) = role {
        spans.push(Span::from(" ").dim());
        spans.push(Span::from(format!("[{role}]")));
    }

    spans
}

fn spawn_request_spans(spawn_request: Option<&SpawnRequestSummary>) -> Vec<Span<'static>> {
    let Some(spawn_request) = spawn_request else {
        return Vec::new();
    };

    let model = spawn_request.model.trim();
    if model.is_empty() && spawn_request.reasoning_effort == ReasoningEffortConfig::default() {
        return Vec::new();
    }

    let details = if model.is_empty() {
        format!("({})", spawn_request.reasoning_effort)
    } else {
        format!("({model} {})", spawn_request.reasoning_effort)
    };

    vec![Span::from(" ").dim(), Span::from(details).magenta()]
}

fn prompt_line(prompt: &str) -> Option<Line<'static>> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Line::from(Span::from(truncate_text(
            trimmed,
            COLLAB_PROMPT_PREVIEW_GRAPHEMES,
        ))))
    }
}

fn wait_complete_lines(
    receiver_thread_ids: &[String],
    agents_states: &std::collections::HashMap<String, CollabAgentState>,
    agent_metadata: &mut impl FnMut(ThreadId) -> AgentMetadata,
) -> Vec<Line<'static>> {
    let mut seen = HashSet::new();
    let mut entries = receiver_thread_ids
        .iter()
        .filter_map(|thread_id| {
            let parsed_thread_id = parse_thread_id(thread_id)?;
            let status = agents_states.get(thread_id)?;
            seen.insert(parsed_thread_id);
            Some((parsed_thread_id, agent_metadata(parsed_thread_id), status))
        })
        .collect::<Vec<_>>();

    let mut extras = agents_states
        .iter()
        .filter_map(|(thread_id, status)| {
            let parsed_thread_id = parse_thread_id(thread_id)?;
            (!seen.contains(&parsed_thread_id))
                .then(|| (parsed_thread_id, agent_metadata(parsed_thread_id), status))
        })
        .collect::<Vec<_>>();
    extras.sort_by_key(|entry| entry.0.to_string());
    entries.extend(extras);

    if entries.is_empty() {
        vec![Line::from(Span::from("No agents completed yet"))]
    } else {
        entries
            .into_iter()
            .map(|(thread_id, metadata, status)| {
                let mut spans = agent_label_spans(agent_label(thread_id, &metadata));
                spans.push(Span::from(": ").dim());
                spans.extend(status_summary_spans(status));
                spans.into()
            })
            .collect()
    }
}

fn first_agent_state<'a>(
    receiver_thread_ids: &[String],
    agents_states: &'a std::collections::HashMap<String, CollabAgentState>,
) -> Option<&'a CollabAgentState> {
    receiver_thread_ids
        .iter()
        .find_map(|thread_id| agents_states.get(thread_id))
        .or_else(|| {
            agents_states
                .iter()
                .min_by(|left, right| left.0.cmp(right.0))
                .map(|(_, status)| status)
        })
}

fn status_summary_line(status: Option<&CollabAgentState>, fallback_error: &str) -> Line<'static> {
    match status {
        Some(status) => status_summary_spans(status).into(),
        None => error_summary_spans(fallback_error).into(),
    }
}

fn status_summary_spans(status: &CollabAgentState) -> Vec<Span<'static>> {
    match status.status {
        CollabAgentStatus::PendingInit => vec![Span::from("Pending init").cyan()],
        CollabAgentStatus::Running => vec![Span::from("Running").cyan().bold()],
        // Allow `.yellow()`
        #[allow(clippy::disallowed_methods)]
        CollabAgentStatus::Interrupted => vec![Span::from("Interrupted").yellow()],
        CollabAgentStatus::Completed => {
            let mut spans = vec![Span::from("Completed").green()];
            if let Some(message) = status.message.as_ref() {
                let message_preview = truncate_text(
                    &message.split_whitespace().collect::<Vec<_>>().join(" "),
                    COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES,
                );
                if !message_preview.is_empty() {
                    spans.push(Span::from(" - ").dim());
                    spans.push(Span::from(message_preview));
                }
            }
            spans
        }
        CollabAgentStatus::Errored => {
            error_summary_spans(status.message.as_deref().unwrap_or("Agent errored"))
        }
        CollabAgentStatus::Shutdown => vec![Span::from("Shutdown")],
        CollabAgentStatus::NotFound => vec![Span::from("Not found").red()],
    }
}

fn error_summary_spans(error: &str) -> Vec<Span<'static>> {
    let mut spans = vec![Span::from("Error").red()];
    let error_preview = truncate_text(
        &error.split_whitespace().collect::<Vec<_>>().join(" "),
        COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES,
    );
    if !error_preview.is_empty() {
        spans.push(Span::from(" - ").dim());
        spans.push(Span::from(error_preview));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::HistoryCell;
    #[cfg(target_os = "macos")]
    use crossterm::event::KeyEvent;
    #[cfg(target_os = "macos")]
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ratatui::style::Modifier;
    use std::collections::HashMap;

    #[test]
    fn interacted_sub_agent_activity_refreshes_running_metadata() {
        let thread_id = ThreadId::new();
        let item = ThreadItem::SubAgentActivity {
            id: "activity-1".to_string(),
            kind: SubAgentActivityKind::Interacted,
            agent_thread_id: thread_id.to_string(),
            agent_path: "/root/child".to_string(),
        };

        assert_eq!(
            sub_agent_activity_display(&item),
            Some(SubAgentActivityDisplay {
                thread_id,
                agent_path: "/root/child".to_string(),
                is_running_hint: true,
            })
        );
    }

    #[test]
    fn collab_events_snapshot() {
        let sender_thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000001")
            .expect("valid sender thread id");
        let robie_id = ThreadId::from_string("00000000-0000-0000-0000-000000000002")
            .expect("valid robie thread id");
        let bob_id = ThreadId::from_string("00000000-0000-0000-0000-000000000003")
            .expect("valid bob thread id");

        let spawn = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-spawn".to_string(),
                tool: CollabAgentTool::SpawnAgent,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: Some("Compute 11! and reply with just the integer result.".to_string()),
                model: Some("gpt-5".to_string()),
                reasoning_effort: Some(ReasoningEffortConfig::High),
                agents_states: HashMap::from([(
                    robie_id.to_string(),
                    agent_state(CollabAgentStatus::PendingInit, /*message*/ None),
                )]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, bob_id),
        )
        .expect("spawn item renders");

        let send = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-send".to_string(),
                tool: CollabAgentTool::SendInput,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: Some("Please continue and return the answer only.".to_string()),
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::from([(
                    robie_id.to_string(),
                    agent_state(CollabAgentStatus::Running, /*message*/ None),
                )]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, bob_id),
        )
        .expect("send-input item renders");

        let waiting = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-wait".to_string(),
                tool: CollabAgentTool::Wait,
                status: CollabAgentToolCallStatus::InProgress,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::new(),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, bob_id),
        )
        .expect("wait begin item renders");

        let finished = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-wait".to_string(),
                tool: CollabAgentTool::Wait,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string(), bob_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::from([
                    (
                        robie_id.to_string(),
                        agent_state(CollabAgentStatus::Completed, Some("39916800")),
                    ),
                    (
                        bob_id.to_string(),
                        agent_state(CollabAgentStatus::Errored, Some("tool timeout")),
                    ),
                ]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, bob_id),
        )
        .expect("wait end item renders");

        let close = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-close".to_string(),
                tool: CollabAgentTool::CloseAgent,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::from([(
                    robie_id.to_string(),
                    agent_state(CollabAgentStatus::Completed, Some("39916800")),
                )]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, bob_id),
        )
        .expect("close item renders");

        let snapshot = [spawn, send, waiting, finished, close]
            .iter()
            .map(cell_to_text)
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_snapshot!("collab_agent_transcript", snapshot);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn agent_shortcut_matches_option_arrow_word_motion_fallbacks_only_when_allowed() {
        assert!(previous_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ false,
        ));
        assert!(next_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ false,
        ));
        assert!(previous_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ true,
        ));
        assert!(next_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ true,
        ));
        assert!(!previous_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ false,
        ));
        assert!(!next_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
            /*allow_word_motion_fallback*/ false,
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn agent_shortcut_matches_option_arrows_only() {
        assert!(previous_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Left, crossterm::event::KeyModifiers::ALT,),
            /*allow_word_motion_fallback*/ false
        ));
        assert!(next_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Right, crossterm::event::KeyModifiers::ALT,),
            /*allow_word_motion_fallback*/ false
        ));
        assert!(!previous_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('b'), crossterm::event::KeyModifiers::ALT,),
            /*allow_word_motion_fallback*/ false
        ));
        assert!(!next_agent_shortcut_matches(
            KeyEvent::new(KeyCode::Char('f'), crossterm::event::KeyModifiers::ALT,),
            /*allow_word_motion_fallback*/ false
        ));
    }

    #[test]
    fn title_styles_nickname_and_role() {
        let sender_thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000001")
            .expect("valid sender thread id");
        let robie_id = ThreadId::from_string("00000000-0000-0000-0000-000000000002")
            .expect("valid robie thread id");
        let cell = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-spawn".to_string(),
                tool: CollabAgentTool::SpawnAgent,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: Some(String::new()),
                model: Some("gpt-5".to_string()),
                reasoning_effort: Some(ReasoningEffortConfig::High),
                agents_states: HashMap::from([(
                    robie_id.to_string(),
                    agent_state(CollabAgentStatus::PendingInit, /*message*/ None),
                )]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, ThreadId::new()),
        )
        .expect("spawn item renders");

        let lines = cell.display_lines(/*width*/ 200);
        let title = &lines[0];
        assert_eq!(title.spans[2].content.as_ref(), "Robie");
        assert_eq!(title.spans[2].style.fg, Some(Color::Cyan));
        assert!(title.spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title.spans[4].content.as_ref(), "[explorer]");
        assert_eq!(title.spans[4].style.fg, None);
        assert!(!title.spans[4].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(title.spans[6].content.as_ref(), "(gpt-5 high)");
        assert_eq!(title.spans[6].style.fg, Some(Color::Magenta));
    }

    #[test]
    fn activity_preview_drops_whole_older_entries_at_line_limit() {
        let preview = AgentActivityPreview::from_summaries(
            ["older one two three four five six", "newest complete"].into_iter(),
        );

        let rendered = preview
            .lines_with_limit(/*width*/ 8, /*max_lines*/ 4)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["…", "newest", "complete"]);
    }

    #[test]
    fn activity_preview_marks_a_truncated_newest_entry() {
        let preview =
            AgentActivityPreview::from_summaries(["alpha beta gamma delta epsilon"].into_iter());

        let rendered = preview
            .lines_with_limit(/*width*/ 7, /*max_lines*/ 3)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["alpha", "beta", "…"]);
    }

    #[test]
    fn activity_preview_preserves_text_with_a_single_line_limit() {
        let preview = AgentActivityPreview::from_summaries(["alpha beta gamma delta"].into_iter());

        let rendered = preview
            .lines_with_limit(/*width*/ 7, /*max_lines*/ 1)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["alpha…"]);
    }

    #[test]
    fn completed_invisible_item_still_reports_removed_preview() {
        let mut tracker = AgentActivityTracker::default();
        assert!(tracker.append_delta("item-1", None, "partial response"));
        let notification = ServerNotification::ItemCompleted(
            codex_app_server_protocol::ItemCompletedNotification {
                thread_id: ThreadId::new().to_string(),
                turn_id: "turn-1".to_string(),
                item: ThreadItem::UserMessage {
                    id: "item-1".to_string(),
                    client_id: None,
                    content: vec![],
                },
                completed_at_ms: 1,
            },
        );

        assert!(tracker.apply(&notification));
        assert_eq!(tracker.preview(), AgentActivityPreview::default());
    }

    #[test]
    fn spine_spawn_activity_tracker_bounds_entries_and_text() {
        let mut tracker = AgentActivityTracker::default();
        for index in 0..=AGENT_ACTIVITY_PREVIEW_ITEMS {
            assert!(tracker.append_delta(
                &format!("message-{index}"),
                None,
                &format!("activity {index}"),
            ));
        }
        assert_eq!(tracker.entries.len(), AGENT_ACTIVITY_PREVIEW_ITEMS);
        assert_eq!(tracker.entries.front().unwrap().item_id, "message-1");

        assert!(tracker.append_delta(
            "message-long",
            None,
            &"界".repeat(AGENT_ACTIVITY_PREVIEW_GRAPHEMES + 40),
        ));
        let summary = &tracker.entries.back().unwrap().summary;
        assert_eq!(
            summary.graphemes(true).count(),
            AGENT_ACTIVITY_PREVIEW_GRAPHEMES
        );
        assert!(summary.starts_with('…'));
    }

    #[test]
    fn collab_resume_interrupted_snapshot() {
        let sender_thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000001")
            .expect("valid sender thread id");
        let robie_id = ThreadId::from_string("00000000-0000-0000-0000-000000000002")
            .expect("valid robie thread id");

        let cell = tool_call_history_cell(
            &ThreadItem::CollabAgentToolCall {
                id: "call-resume".to_string(),
                tool: CollabAgentTool::ResumeAgent,
                status: CollabAgentToolCallStatus::Completed,
                sender_thread_id: sender_thread_id.to_string(),
                receiver_thread_ids: vec![robie_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::from([(
                    robie_id.to_string(),
                    agent_state(CollabAgentStatus::Interrupted, /*message*/ None),
                )]),
            },
            /*cached_spawn_request*/ None,
            |thread_id| metadata_for(thread_id, robie_id, ThreadId::new()),
        )
        .expect("resume item renders");

        assert_snapshot!("collab_resume_interrupted", cell_to_text(&cell));
    }

    fn agent_state(status: CollabAgentStatus, message: Option<&str>) -> CollabAgentState {
        CollabAgentState {
            status,
            message: message.map(str::to_string),
        }
    }

    fn metadata_for(thread_id: ThreadId, robie_id: ThreadId, bob_id: ThreadId) -> AgentMetadata {
        if thread_id == robie_id {
            AgentMetadata {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
            }
        } else if thread_id == bob_id {
            AgentMetadata {
                agent_nickname: Some("Bob".to_string()),
                agent_role: Some("worker".to_string()),
            }
        } else {
            AgentMetadata::default()
        }
    }

    fn cell_to_text(cell: &PlainHistoryCell) -> String {
        cell.display_lines(/*width*/ 200)
            .iter()
            .map(line_to_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_to_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }
}
