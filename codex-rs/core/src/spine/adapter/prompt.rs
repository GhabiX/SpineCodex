use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::spine::SpineCurrentTrimTarget;
use crate::spine::adapter::projection::SpineTreePressureView;
use crate::spine::adapter::projection::build_spine_tree_pressure_view_from_projection;
use crate::spine::bridge::TreeSnapshotProjection;
use codex_features::Feature;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::TokenUsageInfo;

const SPINE_BOUNDARY_HINT_FIRST_TOKENS: i64 = 50_000;
const SPINE_BOUNDARY_HINT_STEP_TOKENS: i64 = 25_000;
const SPINE_CONTEXT_WARNING_RATIO_NUM: i64 = 80;
const SPINE_CONTEXT_WARNING_RATIO_DEN: i64 = 100;
const SPINE_PRESSURE_PROMPT_OVERLAY_ENABLED: bool = false;
const SPINE_TRIM_TARGET_HEAD_CHARS: usize = 80;
const SPINE_TRIM_TARGET_LIMIT: usize = 8;
const SPINE_TRIM_TAIL_GUIDANCE: &str = "At natural Spine boundaries, close/next with compact continuation memory, or open a child for a narrower blocker. For the latest tool outputs listed below, trim irrelevant noisy content now, or slice to keep only needed evidence; preserve any facts needed for continuation before trimming.";
const SPINE_CLOSE_GUIDANCE: &str = "\nBefore broadening the work, check whether the current node can be closed with useful continuation memory.\nIf it can, close it and continue in a sibling if needed; only close/next compacts history and reduces future prompt context.\nIf the current thought is still unfinished, continue in this node; do not open another child unless it is a strictly narrower blocker, because opening by itself does not reduce context.";
const SPINE_PLAN_MODE_CONTEXT_GUIDANCE: &str = "\nPrioritize summarizing the current decision before broadening the investigation.\nAvoid expanding scope while mutating Spine operations are unavailable in Plan mode.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpinePressurePromptState {
    last_boundary_hint: Option<(String, i64)>,
    context_warning_80_node: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpinePressurePromptEmission {
    boundary_hint: Option<(String, i64)>,
    context_warning_80_node: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpinePressurePromptOverlay {
    pub(crate) item: ResponseItem,
    emission: SpinePressurePromptEmission,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineStatusPromptOverlay {
    pub(crate) item: ResponseItem,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineTrimTargetsPromptOverlay {
    pub(crate) item: ResponseItem,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SpinePromptOverlays {
    pub(crate) items: Vec<ResponseItem>,
    pressure: Option<SpinePressurePromptOverlay>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineStatusPromptSignal {
    cursor: String,
    node_summary: Option<String>,
    parent: Option<String>,
    parent_summary: Option<String>,
    cursor_node_context_tokens: Option<i64>,
    context_left_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinePressurePromptSignal {
    node_id: String,
    node_summary: Option<String>,
    cursor_node_context_tokens: Option<i64>,
    boundary_hint_band: Option<i64>,
    context_tokens: Option<i64>,
    model_context_window: Option<i64>,
    emit_context_warning_80: bool,
    mode_allows_spine_close: bool,
}

impl Session {
    pub(crate) async fn spine_prompt_overlays(
        &self,
        turn_context: &TurnContext,
    ) -> SpinePromptOverlays {
        let mut overlays = SpinePromptOverlays::default();
        if let Some(overlay) = self.spine_status_prompt_overlay(turn_context).await {
            overlays.items.push(overlay.item);
        }
        if let Some(overlay) = self
            .spine_pressure_prompt_overlay(turn_context.collaboration_mode.mode)
            .await
        {
            overlays.items.push(overlay.item.clone());
            overlays.pressure = Some(overlay);
        }
        if let Some(overlay) = self.spine_trim_targets_prompt_overlay().await {
            overlays.items.push(overlay.item);
        }
        overlays
    }

    pub(crate) async fn mark_spine_prompt_overlays_sent(&self, overlays: SpinePromptOverlays) {
        if let Some(overlay) = overlays.pressure {
            self.mark_spine_pressure_prompt_overlay_sent(overlay).await;
        }
    }

    pub(crate) async fn spine_status_prompt_overlay(
        &self,
        turn_context: &TurnContext,
    ) -> Option<SpineStatusPromptOverlay> {
        if !self.features().enabled(Feature::SpineJit) {
            return None;
        }

        let token_info = self.token_usage_info().await;
        let total_usage_tokens = self.get_total_token_usage().await;
        let context_left_tokens = turn_context
            .model_info
            .auto_compact_token_limit()
            .map(|limit| limit.saturating_sub(total_usage_tokens).max(0));
        let spine_slot = self.spine.as_ref()?;
        let signal = {
            let guard = spine_slot.lock().await;
            let projection = match TreeSnapshotProjection::from_state(&guard) {
                Ok(Some(projection)) => projection,
                Ok(None) => return None,
                Err(err) => {
                    tracing::debug!("skipping Spine status prompt signal: {err}");
                    return None;
                }
            };
            match status_prompt_signal(projection, token_info.as_ref(), context_left_tokens) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::debug!("failed to build Spine status prompt signal: {err}");
                    return None;
                }
            }
        };

        Some(SpineStatusPromptOverlay {
            item: developer_prompt_overlay_item(format_spine_status_prompt_overlay(&signal)),
        })
    }

    pub(crate) async fn spine_pressure_prompt_overlay(
        &self,
        mode: ModeKind,
    ) -> Option<SpinePressurePromptOverlay> {
        if !SPINE_PRESSURE_PROMPT_OVERLAY_ENABLED {
            return None;
        }

        if !self.features().enabled(Feature::SpineJit) {
            return None;
        }

        let token_info = self.token_usage_info().await?;
        let spine_slot = self.spine.as_ref()?;
        let inside_view = {
            let guard = spine_slot.lock().await;
            let projection = match TreeSnapshotProjection::from_state(&guard) {
                Ok(Some(projection)) => projection,
                Ok(None) => return None,
                Err(err) => {
                    tracing::debug!("skipping Spine pressure prompt signal: {err}");
                    return None;
                }
            };
            build_spine_tree_pressure_view_from_projection(projection, Some(&token_info))
        };

        let mode_allows_spine_close = mode != ModeKind::Plan;
        let mut signal = pressure_prompt_signal(&inside_view, &token_info, mode_allows_spine_close);
        let pressure_prompt_state = self.spine_pressure_prompt_state_lock().await;
        let emission = pressure_prompt_state.prepare_emission(&mut signal);
        drop(pressure_prompt_state);
        if emission.is_empty() {
            return None;
        }
        format_spine_pressure_prompt_overlay(&signal).map(|text| SpinePressurePromptOverlay {
            item: developer_prompt_overlay_item(text),
            emission,
        })
    }

    pub(crate) async fn mark_spine_pressure_prompt_overlay_sent(
        &self,
        overlay: SpinePressurePromptOverlay,
    ) {
        let mut pressure_prompt_state = self.spine_pressure_prompt_state_lock().await;
        pressure_prompt_state.mark_sent(overlay.emission);
    }

    pub(crate) async fn spine_trim_targets_prompt_overlay(
        &self,
    ) -> Option<SpineTrimTargetsPromptOverlay> {
        if !self.features().enabled(Feature::SpineTrim)
            || !self.features().enabled(Feature::SpineTrimTailGuidance)
        {
            return None;
        }
        let raw_items = match self.spine_raw_items_from_rollout().await {
            Ok(raw_items) => raw_items,
            Err(err) => {
                tracing::debug!("skipping Spine trim target prompt overlay: {err}");
                return None;
            }
        };
        let targets = match self.spine_trim_targets_for_prompt(&raw_items).await {
            Ok(Some(targets)) => targets,
            Ok(None) => return None,
            Err(err) => {
                tracing::debug!("failed to build Spine trim target prompt overlay: {err}");
                return None;
            }
        };
        format_spine_trim_targets_prompt_overlay(&targets).map(|text| {
            SpineTrimTargetsPromptOverlay {
                item: developer_prompt_overlay_item(text),
            }
        })
    }
}

fn developer_prompt_overlay_item(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
    }
}

fn status_prompt_signal(
    projection: TreeSnapshotProjection,
    token_info: Option<&TokenUsageInfo>,
    context_left_tokens: Option<i64>,
) -> Result<SpineStatusPromptSignal, crate::spine::SpineError> {
    let snapshot = &projection.snapshot;
    let open_nodes = &projection.open_nodes;
    let active_node = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id);
    let parent = active_node.and_then(|node| node.parent_id.clone());
    let parent_summary = parent
        .as_deref()
        .and_then(|parent_id| snapshot.nodes.iter().find(|node| node.node_id == parent_id))
        .and_then(|node| node.summary.clone());
    let node_summary = active_node.and_then(|node| node.summary.clone());
    let active_open_node = open_nodes
        .iter()
        .find(|node| node.node_id.to_string() == snapshot.active_node_id);
    let cursor_node_context_tokens = match active_open_node {
        Some(node) if node.problem.is_some() => None,
        Some(node) => {
            let current_provider_input_tokens = token_info.and_then(|current| {
                let input_tokens = current.last_token_usage.input_tokens;
                (input_tokens > 0).then_some(input_tokens)
            });
            let (tokens, _) = node.context_state(current_provider_input_tokens);
            tokens
        }
        None => None,
    };
    Ok(SpineStatusPromptSignal {
        cursor: snapshot.active_node_id.clone(),
        node_summary,
        parent,
        parent_summary,
        cursor_node_context_tokens,
        context_left_tokens,
    })
}

fn format_optional_summary_attribute(summary: Option<&str>) -> String {
    match summary.map(str::trim).filter(|summary| !summary.is_empty()) {
        Some(summary) => escape_xml_attribute(summary),
        None => "none".to_string(),
    }
}

fn format_spine_status_prompt_overlay(signal: &SpineStatusPromptSignal) -> String {
    let cursor_node_context = signal
        .cursor_node_context_tokens
        .map(format_si_suffix)
        .unwrap_or_else(|| "unavailable".to_string());
    let context_left = signal
        .context_left_tokens
        .map(format_si_suffix)
        .unwrap_or_else(|| "unavailable".to_string());
    let summary = format_optional_summary_attribute(signal.node_summary.as_deref());
    let parent_summary = format_optional_summary_attribute(signal.parent_summary.as_deref());
    format!(
        r#"<spine_status cursor="{}" summary="{}" parent="{}" parent_summary="{}" cursor_context="{}" context_left="{}""#,
        signal.cursor,
        summary,
        signal.parent.as_deref().unwrap_or("none"),
        parent_summary,
        cursor_node_context,
        context_left,
    ) + " />"
}

fn pressure_prompt_signal(
    inside_view: &SpineTreePressureView,
    token_info: &TokenUsageInfo,
    mode_allows_spine_close: bool,
) -> SpinePressurePromptSignal {
    let active_open_node = inside_view
        .open_nodes
        .iter()
        .find(|node| node.node_id.to_string() == inside_view.active_node_id);
    let active_open_node_allows_close = active_open_node
        .and_then(|node| node.summary.as_deref())
        .is_some_and(|summary| summary.trim() != "root");
    let cursor_node_context_tokens =
        active_open_node.and_then(|node| node.current_node_context_tokens);
    let boundary_hint_band = (mode_allows_spine_close && active_open_node_allows_close)
        .then_some(cursor_node_context_tokens.and_then(pressure_band))
        .flatten();
    let context_tokens = inside_view
        .context_window
        .as_ref()
        .map(|context| context.context_tokens)
        .or_else(|| {
            let tokens = token_info.last_token_usage.tokens_in_context_window();
            (tokens > 0).then_some(tokens)
        });
    let model_context_window = inside_view
        .context_window
        .as_ref()
        .and_then(|context| context.model_context_window)
        .or(token_info.model_context_window);
    let emit_context_warning_80 = context_tokens.is_some_and(|context_tokens| {
        model_context_window
            .is_some_and(|window| context_window_at_or_above_80(context_tokens, window))
    });

    SpinePressurePromptSignal {
        node_id: inside_view.active_node_id.clone(),
        node_summary: inside_view
            .active_node_summary
            .clone()
            .or_else(|| active_open_node.and_then(|node| node.summary.clone())),
        cursor_node_context_tokens,
        boundary_hint_band,
        context_tokens,
        model_context_window,
        emit_context_warning_80,
        mode_allows_spine_close,
    }
}

fn pressure_band(tokens: i64) -> Option<i64> {
    if tokens < SPINE_BOUNDARY_HINT_FIRST_TOKENS {
        return None;
    }
    Some(
        SPINE_BOUNDARY_HINT_FIRST_TOKENS
            + ((tokens - SPINE_BOUNDARY_HINT_FIRST_TOKENS) / SPINE_BOUNDARY_HINT_STEP_TOKENS)
                * SPINE_BOUNDARY_HINT_STEP_TOKENS,
    )
}

fn context_window_at_or_above_80(context_tokens: i64, window: i64) -> bool {
    if context_tokens <= 0 || window <= 0 {
        return false;
    }
    context_tokens.saturating_mul(SPINE_CONTEXT_WARNING_RATIO_DEN)
        >= window.saturating_mul(SPINE_CONTEXT_WARNING_RATIO_NUM)
}

impl SpinePressurePromptState {
    fn prepare_emission(
        &self,
        signal: &mut SpinePressurePromptSignal,
    ) -> SpinePressurePromptEmission {
        let boundary_hint = signal
            .boundary_hint_band
            .map(|band| (signal.node_id.clone(), band));
        if boundary_hint.is_some() && self.last_boundary_hint == boundary_hint {
            signal.boundary_hint_band = None;
        }

        if signal.emit_context_warning_80
            && self.context_warning_80_node.as_deref() == Some(signal.node_id.as_str())
        {
            signal.emit_context_warning_80 = false;
        }

        SpinePressurePromptEmission {
            boundary_hint: signal
                .boundary_hint_band
                .map(|band| (signal.node_id.clone(), band)),
            context_warning_80_node: signal
                .emit_context_warning_80
                .then(|| signal.node_id.clone()),
        }
    }

    fn mark_sent(&mut self, emission: SpinePressurePromptEmission) {
        if let Some(boundary_hint) = emission.boundary_hint {
            self.last_boundary_hint = Some(boundary_hint);
        }
        if let Some(node) = emission.context_warning_80_node {
            self.context_warning_80_node = Some(node);
        }
    }
}

impl SpinePressurePromptEmission {
    fn is_empty(&self) -> bool {
        self.boundary_hint.is_none() && self.context_warning_80_node.is_none()
    }
}

fn format_spine_pressure_prompt_overlay(signal: &SpinePressurePromptSignal) -> Option<String> {
    let mut sections = Vec::new();
    if signal.boundary_hint_band.is_some()
        && signal.mode_allows_spine_close
        && let Some(section) = format_boundary_hint(signal)
    {
        sections.push(section);
    }
    if signal.emit_context_warning_80
        && let Some(section) = format_context_warning(signal)
    {
        sections.push(section);
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn format_boundary_hint(signal: &SpinePressurePromptSignal) -> Option<String> {
    let cursor_tokens = signal.cursor_node_context_tokens?;
    let node_summary = signal
        .node_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map(|summary| format!(" \"{}\"", escape_xml_attribute(summary)))
        .unwrap_or_default();
    let mut text = format!(
        "Spine node context hint: current cursor node {}{} is using ~{} context tokens",
        signal.node_id,
        node_summary,
        format_si_suffix(cursor_tokens),
    );
    if let Some(context_window) = signal.context_tokens.zip(signal.model_context_window).map(
        |(context_tokens, model_context_window)| {
            format!(
                "overall context window is ~{} / {}",
                format_si_suffix(context_tokens),
                format_si_suffix(model_context_window)
            )
        },
    ) {
        text.push_str("; ");
        text.push_str(&context_window);
    }
    text.push('.');
    text.push_str(SPINE_CLOSE_GUIDANCE);
    Some(text)
}

fn format_context_warning(signal: &SpinePressurePromptSignal) -> Option<String> {
    let context_tokens = signal.context_tokens?;
    let window = signal.model_context_window?;
    let percent = context_tokens
        .saturating_mul(100)
        .saturating_div(window)
        .clamp(0, 100);
    let mut text = format!(
        "Spine context warning: current prompt uses ~{} / {} context tokens (~{}%).",
        format_si_suffix(context_tokens),
        format_si_suffix(window),
        percent
    );
    if signal.mode_allows_spine_close {
        text.push_str(SPINE_CLOSE_GUIDANCE);
    } else {
        text.push_str(SPINE_PLAN_MODE_CONTEXT_GUIDANCE);
    }
    Some(text)
}

fn escape_xml_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn format_spine_trim_targets_prompt_overlay(targets: &[SpineCurrentTrimTarget]) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    let mut text = String::from(SPINE_TRIM_TAIL_GUIDANCE);
    text.push_str("\n<current_trim_targets>");
    for (index, target) in targets.iter().take(SPINE_TRIM_TARGET_LIMIT).enumerate() {
        let compact_head = target
            .visible_body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut head = compact_head
            .chars()
            .take(SPINE_TRIM_TARGET_HEAD_CHARS)
            .collect::<String>();
        if compact_head.chars().count() > SPINE_TRIM_TARGET_HEAD_CHARS {
            head.push_str("...");
        }
        text.push('\n');
        text.push_str(&format!(
            r#"{} id="{}" bytes="{}" head="{}""#,
            index,
            escape_xml_attribute(&target.trim_id),
            target.original_visible_size,
            escape_xml_attribute(&head),
        ));
    }
    text.push_str("\n</current_trim_targets>");
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_band_uses_50k_then_25k_steps() {
        assert_eq!(pressure_band(49_999), None);
        assert_eq!(pressure_band(50_000), Some(50_000));
        assert_eq!(pressure_band(74_999), Some(50_000));
        assert_eq!(pressure_band(75_000), Some(75_000));
    }

    #[test]
    fn context_warning_ratio_uses_multiplication_threshold() {
        assert!(!context_window_at_or_above_80(799, 1_000));
        assert!(context_window_at_or_above_80(800, 1_000));
        assert!(context_window_at_or_above_80(207_000, 258_000));
        assert!(!context_window_at_or_above_80(0, 1_000));
        assert!(!context_window_at_or_above_80(1_000, 0));
    }

    #[test]
    fn trim_targets_overlay_includes_guidance_and_escaped_current_targets() {
        let targets = vec![
            SpineCurrentTrimTarget {
                trim_id: "trim_104".to_string(),
                original_visible_size: 24_404,
                visible_body: "Exit code: 0\nOutput with <xml> & \"quotes\"".to_string(),
            },
            SpineCurrentTrimTarget {
                trim_id: "trim_105".to_string(),
                original_visible_size: 5_278,
                visible_body: "/home/ghabi/.codex path".to_string(),
            },
        ];

        assert_eq!(
            format_spine_trim_targets_prompt_overlay(&targets).as_deref(),
            Some(
                "At natural Spine boundaries, close/next with compact continuation memory, or open a child for a narrower blocker. For the latest tool outputs listed below, trim irrelevant noisy content now, or slice to keep only needed evidence; preserve any facts needed for continuation before trimming.\n<current_trim_targets>\n0 id=\"trim_104\" bytes=\"24404\" head=\"Exit code: 0 Output with &lt;xml&gt; &amp; &quot;quotes&quot;\"\n1 id=\"trim_105\" bytes=\"5278\" head=\"/home/ghabi/.codex path\"\n</current_trim_targets>"
            )
        );
    }

    #[test]
    fn state_deduplicates_boundary_and_context_warning_per_node() {
        let mut state = SpinePressurePromptState::default();
        let mut first = SpinePressurePromptSignal {
            node_id: "1.1".to_string(),
            node_summary: None,
            cursor_node_context_tokens: Some(50_000),
            boundary_hint_band: Some(50_000),
            context_tokens: Some(80_000),
            model_context_window: Some(100_000),
            emit_context_warning_80: true,
            mode_allows_spine_close: true,
        };
        let first_emission = state.prepare_emission(&mut first);
        assert!(!first_emission.is_empty());
        assert_eq!(first.boundary_hint_band, Some(50_000));
        assert!(first.emit_context_warning_80);
        state.mark_sent(first_emission);

        let mut duplicate = first.clone();
        assert!(state.prepare_emission(&mut duplicate).is_empty());
        assert_eq!(duplicate.boundary_hint_band, None);
        assert!(!duplicate.emit_context_warning_80);

        let mut next_band = first.clone();
        next_band.boundary_hint_band = Some(75_000);
        next_band.emit_context_warning_80 = true;
        assert!(!state.prepare_emission(&mut next_band).is_empty());
        assert_eq!(next_band.boundary_hint_band, Some(75_000));
        assert!(!next_band.emit_context_warning_80);
    }

    #[test]
    fn plan_mode_context_warning_does_not_instruct_close() {
        let signal = SpinePressurePromptSignal {
            node_id: "1.1".to_string(),
            node_summary: None,
            cursor_node_context_tokens: None,
            boundary_hint_band: None,
            context_tokens: Some(80_000),
            model_context_window: Some(100_000),
            emit_context_warning_80: true,
            mode_allows_spine_close: false,
        };
        let text = format_spine_pressure_prompt_overlay(&signal).expect("warning");
        assert!(text.contains("Spine context warning"));
        assert!(!text.contains("spine.close"));
    }
}
