use super::session::Session;
use super::spine_tree_inside::SpineTreeInsideView;
use super::spine_tree_inside::build_spine_tree_inside_view;
use super::spine_tree_inside::node_context_tokens;
use super::turn_context::TurnContext;
use codex_features::Feature;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineNodeContextUnavailableReason;

const SPINE_BOUNDARY_HINT_FIRST_TOKENS: i64 = 50_000;
const SPINE_BOUNDARY_HINT_STEP_TOKENS: i64 = 25_000;
const SPINE_CONTEXT_WARNING_RATIO_NUM: i64 = 80;
const SPINE_CONTEXT_WARNING_RATIO_DEN: i64 = 100;
const SPINE_PRESSURE_PROMPT_OVERLAY_ENABLED: bool = false;

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineStatusPromptSignal {
    cursor: String,
    node_summary: Option<String>,
    parent: Option<String>,
    live_node_context_tokens: Option<i64>,
    live_node_context_unavailable: Option<SpineNodeContextUnavailableReason>,
    context_left_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinePressurePromptSignal {
    node_id: String,
    node_summary: Option<String>,
    live_node_context_tokens: Option<i64>,
    boundary_hint_band: Option<i64>,
    context_tokens: Option<i64>,
    model_context_window: Option<i64>,
    emit_context_warning_80: bool,
    mode_allows_spine_close: bool,
}

impl Session {
    pub(crate) async fn spine_status_prompt_overlay(
        &self,
        turn_context: &TurnContext,
    ) -> Option<SpineStatusPromptOverlay> {
        if !self.features.enabled(Feature::SpineJit) {
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
            if let Err(err) = guard.ensure_valid() {
                tracing::debug!("skipping Spine status prompt signal: {err}");
                return None;
            }
            let runtime = guard.runtime()?;
            match status_prompt_signal(runtime, token_info.as_ref(), context_left_tokens) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::debug!("failed to build Spine status prompt signal: {err}");
                    return None;
                }
            }
        };

        Some(SpineStatusPromptOverlay {
            item: spine_pressure_overlay_message(format_spine_status_prompt_overlay(&signal)),
        })
    }

    pub(crate) async fn spine_pressure_prompt_overlay(
        &self,
        mode: ModeKind,
    ) -> Option<SpinePressurePromptOverlay> {
        if !SPINE_PRESSURE_PROMPT_OVERLAY_ENABLED {
            return None;
        }

        if !self.features.enabled(Feature::SpineJit) {
            return None;
        }

        let token_info = self.token_usage_info().await?;
        let spine_slot = self.spine.as_ref()?;
        let inside_view = {
            let guard = spine_slot.lock().await;
            if let Err(err) = guard.ensure_valid() {
                tracing::debug!("skipping Spine pressure prompt signal: {err}");
                return None;
            }
            let runtime = guard.runtime()?;
            match build_spine_tree_inside_view(runtime, Some(&token_info)) {
                Ok(view) => view,
                Err(err) => {
                    tracing::debug!("failed to build Spine pressure prompt signal: {err}");
                    return None;
                }
            }
        };

        let mode_allows_spine_close = mode != ModeKind::Plan;
        let mut signal = pressure_prompt_signal(&inside_view, &token_info, mode_allows_spine_close);
        let pressure_prompt_state = self.spine_pressure_prompt_state.lock().await;
        let emission = pressure_prompt_state.prepare_emission(&mut signal);
        drop(pressure_prompt_state);
        if emission.is_empty() {
            return None;
        }
        format_spine_pressure_prompt_overlay(&signal).map(|text| SpinePressurePromptOverlay {
            item: spine_pressure_overlay_message(text),
            emission,
        })
    }

    pub(crate) async fn mark_spine_pressure_prompt_overlay_sent(
        &self,
        overlay: SpinePressurePromptOverlay,
    ) {
        let mut pressure_prompt_state = self.spine_pressure_prompt_state.lock().await;
        pressure_prompt_state.mark_sent(overlay.emission);
    }
}

fn status_prompt_signal(
    runtime: &crate::spine::SpineRuntime,
    token_info: Option<&TokenUsageInfo>,
    context_left_tokens: Option<i64>,
) -> Result<SpineStatusPromptSignal, crate::spine::SpineError> {
    let snapshot = runtime.build_tree_snapshot()?;
    let open_nodes = runtime.open_node_context_projections();
    let active_node = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id);
    let parent = active_node.and_then(|node| node.parent_id.clone());
    let node_summary = active_node.and_then(|node| node.summary.clone());
    let active_open_node = open_nodes
        .iter()
        .find(|node| node.node_id.to_string() == snapshot.active_node_id);
    let (live_node_context_tokens, live_node_context_unavailable) = match active_open_node {
        Some(node) => match node_context_tokens(token_info, node.baseline_tokens) {
            Ok(tokens) => (tokens, None),
            Err(reason) => (None, Some(reason)),
        },
        None => (None, None),
    };
    Ok(SpineStatusPromptSignal {
        cursor: snapshot.active_node_id,
        node_summary,
        parent,
        live_node_context_tokens,
        live_node_context_unavailable,
        context_left_tokens,
    })
}

fn format_spine_status_prompt_overlay(signal: &SpineStatusPromptSignal) -> String {
    let live_node = signal
        .live_node_context_tokens
        .map(format_si_suffix)
        .unwrap_or_else(|| "unavailable".to_string());
    let context_left = format_context_left_status(signal.context_left_tokens)
        .unwrap_or_else(|| "unavailable".to_string());
    let summary = format_spine_status_summary(signal.node_summary.as_deref());
    let mut text = format!(
        r#"<spine_status cursor="{}" summary="{}" parent="{}" live_node="{}" context_left="{}""#,
        signal.cursor,
        summary,
        signal.parent.as_deref().unwrap_or("none"),
        live_node,
        context_left,
    );
    if signal.live_node_context_unavailable
        == Some(SpineNodeContextUnavailableReason::MissingOpenContextBaseline)
    {
        text.push_str(r#" baseline="missing""#);
    }
    text.push_str(" />");
    text
}

fn format_spine_status_summary(summary: Option<&str>) -> String {
    let Some(summary) = summary.map(str::trim).filter(|summary| !summary.is_empty()) else {
        return "none".to_string();
    };
    escape_xml_attribute(summary)
}

fn format_context_left_status(context_left_tokens: Option<i64>) -> Option<String> {
    context_left_tokens.map(format_si_suffix)
}

fn pressure_prompt_signal(
    inside_view: &SpineTreeInsideView,
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
    let live_node_context_tokens =
        active_open_node.and_then(|node| node.current_node_context_tokens);
    let boundary_hint_band = (mode_allows_spine_close && active_open_node_allows_close)
        .then(|| live_node_context_tokens.and_then(pressure_band))
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
    let emit_context_warning_80 = match (context_tokens, model_context_window) {
        (Some(context_tokens), Some(window)) => {
            context_window_at_or_above_80(context_tokens, window)
        }
        (Some(_), None) | (None, Some(_)) | (None, None) => false,
    };

    SpinePressurePromptSignal {
        node_id: inside_view.active_node_id.clone(),
        node_summary: inside_view
            .active_node_summary
            .clone()
            .or_else(|| active_open_node.and_then(|node| node.summary.clone())),
        live_node_context_tokens,
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
    let live_tokens = signal.live_node_context_tokens?;
    let mut text = format!(
        "Spine boundary hint: current live node {}{} is using ~{} context tokens",
        signal.node_id,
        format_node_summary(signal.node_summary.as_deref()),
        format_si_suffix(live_tokens),
    );
    if let Some(context_window) = format_context_window_usage(signal) {
        text.push_str("; ");
        text.push_str(&context_window);
    }
    text.push_str(
        ".\nBefore broadening the work, pause and check whether there is a completed handoff boundary.\nIf there is, close with one short sentence naming what later work should remember, then continue in a sibling if needed; only close/next compacts history and reduces future prompt context.\nIf the current thought is still unfinished, continue in this node; do not open another child unless it is a strictly narrower blocker, because opening by itself does not reduce context.",
    );
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
        text.push_str(
            "\nBefore broadening the work, pause and check whether there is a completed handoff boundary.\nIf there is, close with one short sentence naming what later work should remember, then continue in a sibling if needed; only close/next compacts history and reduces future prompt context.\nIf the current thought is still unfinished, continue in this node; do not open another child unless it is a strictly narrower blocker, because opening by itself does not reduce context.",
        );
    } else {
        text.push_str(
            "\nPrioritize summarizing the current decision before broadening the investigation.\nAvoid expanding scope while mutating Spine operations are unavailable in Plan mode.",
        );
    }
    Some(text)
}

fn format_context_window_usage(signal: &SpinePressurePromptSignal) -> Option<String> {
    Some(format!(
        "overall context window is ~{} / {}",
        format_si_suffix(signal.context_tokens?),
        format_si_suffix(signal.model_context_window?)
    ))
}

fn format_node_summary(summary: Option<&str>) -> String {
    let Some(summary) = summary.map(str::trim).filter(|summary| !summary.is_empty()) else {
        return String::new();
    };
    format!(" \"{}\"", escape_xml_attribute(summary))
}

fn escape_xml_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn spine_pressure_overlay_message(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
    }
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
    fn context_left_status_uses_absolute_tokens() {
        assert_eq!(
            format_context_left_status(Some(158_000)).as_deref(),
            Some("158K")
        );
        assert_eq!(format_context_left_status(None), None);
    }

    #[test]
    fn state_deduplicates_boundary_and_context_warning_per_node() {
        let mut state = SpinePressurePromptState::default();
        let mut first = SpinePressurePromptSignal {
            node_id: "1.1".to_string(),
            node_summary: None,
            live_node_context_tokens: Some(50_000),
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
            live_node_context_tokens: None,
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
