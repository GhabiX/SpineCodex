use super::candidate_mem_plan::CandidateMem;
use super::candidate_mem_plan::CandidateMemCover;
use super::candidate_mem_plan::plan_candidate_mem_cover;
use super::host_bridge::HostBridgeProjection;
use super::host_bridge::SPINE_INITIAL_CONTEXT_CLOSE_TAG;
use super::host_bridge::SPINE_INITIAL_CONTEXT_OPEN_TAG;
use super::host_bridge::parse_spine_initial_context_item;
use super::host_bridge::spine_memory_synthetic_id;
use super::host_bridge::spine_memory_text_marker;
use super::ids::NodeId;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::span;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use super::store::SpineSidecarStore;
use super::view::display_node_id;
use super::view::op_label;
use super::view::relative_memory_path;
use super::view::relative_node_trajs_path;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::client_common::ResponseEvent;
use crate::session::session::Session;
use crate::session::turn::get_last_assistant_message_from_turn;
use crate::session::turn_context::TurnContext;
use crate::util::backoff;
use codex_async_utils::OrCancelExt;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactInput {
    pub(crate) op: SpineOperation,
    pub(crate) node_id: NodeId,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) spine_tree: String,
    pub(crate) prefix_items: Vec<ResponseItem>,
    pub(crate) suffix_items: Vec<ResponseItem>,
    pub(crate) transition_summary: String,
    pub(crate) compact_instruction: Option<String>,
    pub(crate) rollout_path: PathBuf,
    pub(crate) raw_mirror_path: PathBuf,
    pub(crate) sidecar_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCompactBoundary {
    pub(crate) op: SpineOperation,
    pub(crate) node_id: NodeId,
    pub(crate) scope_node_id: Option<NodeId>,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) transition_summary: String,
    pub(crate) compact_instruction: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactPlan {
    pub(crate) input: SpineCompactInput,
    pub(crate) cut_index: usize,
    pub(crate) fold_end_index: usize,
    pub(crate) replacement_tail: Vec<ResponseItem>,
    pub(crate) memory_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct SpineCompactPreparation {
    pub(crate) plan: SpineCompactPlan,
    pub(crate) effective_boundary: SpineCompactBoundary,
    pub(crate) compact_index_rollout_path: String,
    pub(crate) runtime_spans: Vec<InstalledCompactSpan>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactOutput {
    pub(crate) memory_markdown: String,
    pub(crate) compacted_body: String,
    pub(crate) compact_message: String,
}

pub(crate) const CODEX_BUILTIN_TEXT_STRATEGY: &str = "codex_builtin_fork_full_history";

pub(crate) async fn compact_suffix_with_codex_builtin_text(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    prompt_envelope: &Prompt,
    input: SpineCompactInput,
    cancellation_token: &CancellationToken,
) -> CodexResult<SpineCompactOutput> {
    let prompt = build_codex_builtin_prompt(&input, prompt_envelope);
    let max_retries = turn_context.provider.info().stream_max_retries();
    let mut retries = 0;
    let compacted_suffix = loop {
        match collect_compaction_response(
            sess,
            turn_context,
            client_session,
            &prompt,
            cancellation_token,
        )
        .await
        {
            Ok(text) => break text,
            Err(err) if err.is_retryable() && retries < max_retries => {
                retries += 1;
                let delay = backoff(retries);
                warn!("spine compact stream failed; retrying ({retries}/{max_retries})");
                sess.notify_stream_error(
                    turn_context,
                    format!("Reconnecting... {retries}/{max_retries}"),
                    err,
                )
                .await;
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    };

    let compacted_suffix = extract_spine_compact_markdown(&compacted_suffix)?;
    let memory_markdown = render_auto_compact_memory(&input, &compacted_suffix);
    Ok(SpineCompactOutput {
        compact_message: format!(
            "Spine compacted {} [{}, {})",
            input.node_id, input.cut_ordinal, input.fold_end_ordinal
        ),
        memory_markdown,
        compacted_body: compacted_suffix,
    })
}

fn build_codex_builtin_prompt(input: &SpineCompactInput, prompt_envelope: &Prompt) -> Prompt {
    Prompt {
        input: build_codex_builtin_prompt_input(input),
        tools: prompt_envelope.tools.clone(),
        parallel_tool_calls: prompt_envelope.parallel_tool_calls,
        base_instructions: prompt_envelope.base_instructions.clone(),
        personality: prompt_envelope.personality,
        // Carrying a user turn final-output schema would make this internal compact response
        // invalid or over-constrained.
        output_schema: None,
        output_schema_strict: true,
    }
}

fn build_codex_builtin_prompt_input(input: &SpineCompactInput) -> Vec<ResponseItem> {
    let mut prompt_input =
        Vec::with_capacity(input.prefix_items.len() + input.suffix_items.len() + 1);
    prompt_input.extend(input.prefix_items.clone());
    prompt_input.extend(input.suffix_items.clone());
    expand_spine_initial_context_items(&mut prompt_input);
    let target_tree_node_id = display_node_id(&input.node_id);
    let compact_instruction = input
        .compact_instruction
        .as_deref()
        .map(|instruction| format!("\n\nAdditional compaction guidance: {instruction}"))
        .unwrap_or_default();
    prompt_input.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "Compact only target Spine node `{}` into a factual Markdown memory.\nKeep durable facts needed by later nodes: outcome, decisions, constraints, files/functions/tests/commands, validation status, blockers, unresolved questions.\n\nTarget tree node: {}\nInternal node id: {}\nTarget operation: {}\nSpine Tree summary label: {}\n\n<spine_tree>\n{}\n</spine_tree>{}\n\nReturn exactly the compacted suffix as Markdown. Do not wrap it in XML/HTML tags or code fences. Do not include preambles, apologies, continuation instructions, or any text outside the compacted Markdown body.",
                target_tree_node_id,
                target_tree_node_id,
                input.node_id,
                op_label(input.op),
                input.transition_summary,
                input.spine_tree,
                compact_instruction
            ),
        }],
        phase: None,
    });
    prompt_input
}

async fn collect_compaction_response(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut crate::client::ModelClientSession,
    prompt: &Prompt,
    cancellation_token: &CancellationToken,
) -> CodexResult<String> {
    let mut stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            turn_context
                .turn_metadata_state
                .current_header_value()
                .as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .or_cancel(cancellation_token)
        .await??;
    let mut output_items = Vec::new();
    loop {
        let event = match stream.next().or_cancel(cancellation_token).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(CodexErr::Stream(
                    "stream closed before spine compact response.completed".into(),
                    None,
                ));
            }
            Err(codex_async_utils::CancelErr::Cancelled) => return Err(CodexErr::TurnAborted),
        };
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => output_items.push(item),
            Ok(ResponseEvent::ServerReasoningIncluded(included)) => {
                sess.set_server_reasoning_included(included).await;
            }
            Ok(ResponseEvent::RateLimits(snapshot)) => {
                sess.update_rate_limits(turn_context, snapshot).await;
            }
            Ok(ResponseEvent::Completed { token_usage, .. }) => {
                sess.update_token_usage_info(turn_context, token_usage.as_ref())
                    .await;
                return get_last_assistant_message_from_turn(&output_items).ok_or_else(|| {
                    CodexErr::Fatal("spine compact produced no assistant summary".to_string())
                });
            }
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
}

fn extract_spine_compact_markdown(text: &str) -> CodexResult<String> {
    let body = text.trim();
    if body.is_empty() {
        return Err(CodexErr::Fatal(
            "spine compact response memory is empty".to_string(),
        ));
    }
    if body.starts_with("<spine_memory")
        || body.contains("</spine_memory>")
        || body.starts_with("<spine_ir")
        || body.contains("</spine_ir>")
        || body.starts_with("<memory>")
        || body.contains("</memory>")
    {
        return Err(CodexErr::Fatal(
            "spine compact response must be plain Markdown without XML memory wrappers".to_string(),
        ));
    }
    Ok(body.to_string())
}

pub(crate) fn render_auto_compact_memory(
    input: &SpineCompactInput,
    compacted_suffix: &str,
) -> String {
    let raw_mirror_path = input
        .raw_mirror_path
        .strip_prefix(&input.sidecar_root)
        .unwrap_or(input.raw_mirror_path.as_path());
    let node_trajs_path = relative_node_trajs_path(&input.node_id);
    format!(
        "\n\n## Auto Compact\n\nBase: {}\nFold: response ordinals [{}, {})\nNode trajs: {}\nRaw mirror: {}\nRollout: {}\n\n{}\n\n## Node Summary\n\n{}\n",
        input.sidecar_root.display(),
        input.cut_ordinal,
        input.fold_end_ordinal,
        node_trajs_path.display(),
        raw_mirror_path.display(),
        input.rollout_path.display(),
        compacted_suffix,
        input.transition_summary
    )
}

#[cfg(test)]
pub(crate) fn build_suffix_replacement_history(
    old_history: &[ResponseItem],
    cut_index: usize,
    fold_end_index: usize,
    ir_items: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    let mut replacement_history = Vec::with_capacity(
        cut_index + ir_items.len() + old_history.len().saturating_sub(fold_end_index),
    );
    replacement_history.extend_from_slice(&old_history[..cut_index]);
    replacement_history.extend(ir_items);
    replacement_history.extend_from_slice(&old_history[fold_end_index..]);
    replacement_history
}

pub(crate) fn build_suffix_replacement_history_from_pi(
    old_history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    compact_id: &str,
    boundary: &SpineCompactBoundary,
    memory_item: ResponseItem,
    note_items: Vec<ResponseItem>,
) -> CodexResult<Vec<ResponseItem>> {
    let projection = HostBridgeProjection::build(old_history, runtime_spans)?;
    let raw_len = projection
        .raw_for_effective_index(old_history.len())
        .ok_or_else(|| {
            CodexErr::Fatal("spine suffix render(Pi) could not map history end".to_string())
        })?;
    let new_span = RawSpan {
        start: boundary.cut_ordinal,
        end: boundary.fold_end_ordinal,
    };
    let candidate = CandidateMem::new(
        compact_id.to_string(),
        boundary.node_id.clone(),
        boundary.op,
        new_span,
    );
    let CandidateMemCover { mut pi, artifacts } =
        plan_candidate_mem_cover(raw_len, runtime_spans, &candidate).map_err(pi_render_error)?;
    let note_items = note_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("suffix_note_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    if !note_items.is_empty() {
        pi = insert_notes_after_mem(pi, compact_id, note_items.keys().cloned().collect())?;
    }
    let mut mem_items = memory_items_from_runtime_spans(old_history, runtime_spans)?;
    mem_items.insert(compact_id.to_string(), memory_item);
    render_pi_bridge_replacement_history(
        old_history,
        runtime_spans,
        &pi,
        &artifacts,
        &mem_items,
        &note_items,
    )
}

pub(crate) fn build_root_archive_replacement_history(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<RootArchiveReplacementHistory> {
    build_root_archive_replacement_history_for_compact_id(
        history,
        planned_cut_index,
        fold_end_ordinal,
        initial_context_items,
        root_memory_item,
        runtime_spans,
        "__spine_root_archive_render_pi__",
    )
}

pub(crate) fn build_root_archive_replacement_history_for_compact_id(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    runtime_spans: &[InstalledCompactSpan],
    root_compact_id: &str,
) -> CodexResult<RootArchiveReplacementHistory> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let (archive_cut_ordinal, archive_cut_index) = projection
        .first_span_in_prefix(planned_cut_index)
        .map_or_else(
            || {
                projection
                    .raw_for_effective_index(planned_cut_index)
                    .map(|raw| (raw, planned_cut_index))
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "spine root archive render(Pi) planned cut index {planned_cut_index} does not map to a raw ordinal"
                        ))
                    })
            },
            Ok,
        )?;
    if fold_end_ordinal < archive_cut_ordinal {
        return Err(CodexErr::Fatal(format!(
            "spine root archive render(Pi) fold_end ordinal {fold_end_ordinal} is before archive cut ordinal {archive_cut_ordinal}"
        )));
    }
    let raw_len = projection
        .raw_for_effective_index(history.len())
        .ok_or_else(|| {
            CodexErr::Fatal("spine root archive render(Pi) could not map history end".to_string())
        })?;
    let candidate = CandidateMem::anonymous(
        root_compact_id.to_string(),
        RawSpan {
            start: archive_cut_ordinal,
            end: fold_end_ordinal,
        },
    );
    let CandidateMemCover { mut pi, artifacts } =
        plan_candidate_mem_cover(raw_len, runtime_spans, &candidate).map_err(pi_render_error)?;
    let note_items = initial_context_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("initial_context_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    if !note_items.is_empty() {
        pi = insert_notes_before_mem(pi, root_compact_id, note_items.keys().cloned().collect())?;
    }
    let mut mem_items = memory_items_from_runtime_spans(history, runtime_spans)?;
    mem_items.insert(root_compact_id.to_string(), root_memory_item);
    let replacement_history = render_pi_bridge_replacement_history(
        history,
        runtime_spans,
        &pi,
        &artifacts,
        &mem_items,
        &note_items,
    )?;
    Ok(RootArchiveReplacementHistory {
        replacement_history,
        archive_cut_ordinal,
        archive_cut_index,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RootArchiveReplacementHistory {
    pub(crate) replacement_history: Vec<ResponseItem>,
    pub(crate) archive_cut_ordinal: u64,
    pub(crate) archive_cut_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderPiOrigin {
    Raw(RawSpan),
    Mem(String),
    Note(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderedPiItem {
    pub(crate) origin: RenderPiOrigin,
    pub(crate) item: ResponseItem,
}

pub(crate) fn render_pi_bridge_replacement_history(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<ResponseItem>> {
    Ok(
        render_pi_bridge_items(history, runtime_spans, pi, artifacts, mem_items, note_items)?
            .into_iter()
            .map(|rendered| rendered.item)
            .collect(),
    )
}

pub(crate) fn render_pi_bridge_items(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<RenderedPiItem>> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let mut rendered = Vec::new();
    for segment in pi {
        match segment {
            Segment::Raw(raw_span) => {
                let start_index = projection
                    .effective_index_for_raw_boundary(raw_span.start)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} start does not map to an effective index",
                            raw_span
                        ))
                    })?;
                let end_index = projection
                    .effective_index_for_raw_boundary(raw_span.end)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} end does not map to an effective index",
                            raw_span
                        ))
                    })?;
                if start_index > end_index {
                    return Err(CodexErr::Fatal(format!(
                        "render(Pi) Raw {} maps to inverted effective range [{start_index}, {end_index})",
                        raw_span
                    )));
                }
                rendered.extend(history[start_index..end_index].iter().cloned().map(|item| {
                    RenderedPiItem {
                        origin: RenderPiOrigin::Raw(*raw_span),
                        item,
                    }
                }));
            }
            Segment::Mem { compact_id } => {
                span(segment, artifacts).map_err(pi_render_error)?;
                let item = mem_items.get(compact_id).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Mem item for {compact_id}"))
                })?;
                rendered.push(RenderedPiItem {
                    origin: RenderPiOrigin::Mem(compact_id.clone()),
                    item: item.clone(),
                });
            }
            Segment::Note { kind } => {
                let items = note_items.get(kind).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Note item for {kind}"))
                })?;
                rendered.extend(items.iter().cloned().map(|item| RenderedPiItem {
                    origin: RenderPiOrigin::Note(kind.clone()),
                    item,
                }));
            }
        }
    }
    Ok(rendered)
}

fn memory_items_from_runtime_spans(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<BTreeMap<String, ResponseItem>> {
    let mut items = BTreeMap::new();
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    for span in runtime_spans {
        let item = projection.memory_item_for_span(&span.compact_id)?;
        items.insert(span.compact_id.clone(), item);
    }
    Ok(items)
}

fn insert_notes_after_mem(
    pi: Vec<Segment>,
    compact_id: &str,
    note_kinds: Vec<String>,
) -> CodexResult<Vec<Segment>> {
    let mut inserted = false;
    let mut result = Vec::with_capacity(pi.len() + note_kinds.len());
    for segment in pi {
        let is_target = matches!(&segment, Segment::Mem { compact_id: id } if id == compact_id);
        result.push(segment);
        if is_target {
            inserted = true;
            result.extend(note_kinds.iter().cloned().map(Segment::note));
        }
    }
    if !inserted {
        return Err(CodexErr::Fatal(format!(
            "render(Pi) could not place notes after Mem {compact_id}"
        )));
    }
    Ok(result)
}

fn insert_notes_before_mem(
    pi: Vec<Segment>,
    compact_id: &str,
    note_kinds: Vec<String>,
) -> CodexResult<Vec<Segment>> {
    let mut inserted = false;
    let mut result = Vec::with_capacity(pi.len() + note_kinds.len());
    for segment in pi {
        if matches!(&segment, Segment::Mem { compact_id: id } if id == compact_id) {
            inserted = true;
            result.extend(note_kinds.iter().cloned().map(Segment::note));
        }
        result.push(segment);
    }
    if !inserted {
        return Err(CodexErr::Fatal(format!(
            "render(Pi) could not place notes before Mem {compact_id}"
        )));
    }
    Ok(result)
}

fn pi_render_error(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Fatal(format!(
        "render(Pi) failed to build canonical cover: {error}"
    ))
}

pub(crate) fn validate_spine_replacement_history_admissible(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    required_raw_ordinals: &[u64],
) -> CodexResult<()> {
    super::host_bridge::validate_spine_replacement_history_admissible(
        history,
        runtime_spans,
        required_raw_ordinals,
    )
}

#[cfg(test)]
pub(crate) fn plan_suffix_fold(
    history: &[ResponseItem],
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    input: SpineCompactInput,
) -> CodexResult<SpineCompactPlan> {
    plan_suffix_fold_with_spans(history, cut_ordinal, fold_end_ordinal, &[], input)
}

pub(crate) fn plan_suffix_fold_with_spans(
    history: &[ResponseItem],
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
    input: SpineCompactInput,
) -> CodexResult<SpineCompactPlan> {
    let cut_index = effective_index_for_raw_ordinal_with_spans(history, cut_ordinal, runtime_spans)
        .ok_or_else(|| {
            CodexErr::Fatal(format!(
                "spine compact cut ordinal {cut_ordinal} does not map to an effective history index"
            ))
        })?;
    let fold_end_index = effective_index_for_raw_ordinal_with_spans(
        history,
        fold_end_ordinal,
        runtime_spans,
    )
    .ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact fold_end ordinal {fold_end_ordinal} does not map to an effective history index"
        ))
    })?;
    if cut_index > fold_end_index {
        return Err(CodexErr::Fatal(format!(
            "spine compact cut index {cut_index} is after fold end index {fold_end_index}"
        )));
    }
    if cut_index == fold_end_index {
        return Err(CodexErr::Fatal(
            "spine compact fold range is empty after mapping".to_string(),
        ));
    }
    let (cut_index, fold_end_index) =
        adjusted_range_for_tool_call_closure(history, cut_index, fold_end_index);
    let cut_ordinal = raw_ordinal_for_effective_index_with_spans(history, cut_index, runtime_spans)
        .ok_or_else(|| {
            CodexErr::Fatal(format!(
                "spine compact adjusted cut index {cut_index} does not map to a raw ordinal"
            ))
        })?;
    let fold_end_ordinal = raw_ordinal_for_effective_index_with_spans(
        history,
        fold_end_index,
        runtime_spans,
    )
    .ok_or_else(|| {
        CodexErr::Fatal(format!(
            "spine compact adjusted fold end index {fold_end_index} does not map to a raw ordinal"
        ))
    })?;
    let mut input = input;
    input.cut_ordinal = cut_ordinal;
    input.fold_end_ordinal = fold_end_ordinal;
    input.prefix_items = history[..cut_index].to_vec();
    input.suffix_items = history[cut_index..fold_end_index].to_vec();

    Ok(SpineCompactPlan {
        memory_path: input
            .sidecar_root
            .join(relative_memory_path(&input.node_id)),
        replacement_tail: history[fold_end_index..].to_vec(),
        input,
        cut_index,
        fold_end_index,
    })
}

pub(crate) fn prepare_spine_compact_plan(
    store: &SpineSidecarStore,
    boundary: &SpineCompactBoundary,
    history: &[ResponseItem],
    rollout_path: PathBuf,
    spine_tree: String,
    surviving_compact_hashes: Option<&HashSet<String>>,
) -> CodexResult<SpineCompactPreparation> {
    let compact_index_rollout_path = rollout_path
        .file_name()
        .map(|file_name| format!("../{}", file_name.to_string_lossy()))
        .unwrap_or_else(|| rollout_path.to_string_lossy().into_owned());
    let input = SpineCompactInput {
        op: boundary.op,
        node_id: boundary.node_id.clone(),
        cut_ordinal: boundary.cut_ordinal,
        fold_end_ordinal: boundary.fold_end_ordinal,
        spine_tree,
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: boundary.transition_summary.clone(),
        compact_instruction: boundary.compact_instruction.clone(),
        rollout_path,
        raw_mirror_path: store.raw_rollout_path(),
        sidecar_root: store.root().to_path_buf(),
    };
    let runtime_spans = store
        .installed_compact_spans_matching_hashes(surviving_compact_hashes)
        .map_err(|err| {
            CodexErr::Fatal(format!("failed to load spine compact span ledger: {err}"))
        })?;
    let plan = plan_suffix_fold_with_spans(
        history,
        boundary.cut_ordinal,
        boundary.fold_end_ordinal,
        &runtime_spans,
        input,
    )?;
    let effective_boundary = SpineCompactBoundary {
        cut_ordinal: plan.input.cut_ordinal,
        fold_end_ordinal: plan.input.fold_end_ordinal,
        ..boundary.clone()
    };
    Ok(SpineCompactPreparation {
        plan,
        effective_boundary,
        compact_index_rollout_path,
        runtime_spans,
    })
}

fn adjusted_range_for_tool_call_closure(
    history: &[ResponseItem],
    mut cut_index: usize,
    mut fold_end_index: usize,
) -> (usize, usize) {
    loop {
        let calls_in_range = call_ids_in(history, cut_index, fold_end_index);
        let outputs_in_range = output_call_ids_in(history, cut_index, fold_end_index);
        let mut changed = false;

        if let Some(index) = first_output_for_call_after(history, fold_end_index, &calls_in_range) {
            fold_end_index = index.saturating_add(1);
            changed = true;
        }

        if let Some(index) = last_call_for_output_before(history, cut_index, &outputs_in_range) {
            cut_index = index;
            changed = true;
        }

        if !changed {
            return (cut_index, fold_end_index);
        }
    }
}

fn call_ids_in(history: &[ResponseItem], start: usize, end: usize) -> HashSet<String> {
    history[start..end]
        .iter()
        .filter_map(|item| match tool_pairing(item) {
            ToolPairing::Call(call_id) => Some(call_id),
            ToolPairing::Output(_) | ToolPairing::None => None,
        })
        .collect()
}

fn output_call_ids_in(history: &[ResponseItem], start: usize, end: usize) -> HashSet<String> {
    history[start..end]
        .iter()
        .filter_map(|item| match tool_pairing(item) {
            ToolPairing::Output(call_id) => Some(call_id),
            ToolPairing::Call(_) | ToolPairing::None => None,
        })
        .collect()
}

fn first_output_for_call_after(
    history: &[ResponseItem],
    start: usize,
    call_ids: &HashSet<String>,
) -> Option<usize> {
    if call_ids.is_empty() {
        return None;
    }
    history
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, item)| match tool_pairing(item) {
            ToolPairing::Output(call_id) if call_ids.contains(&call_id) => Some(index),
            ToolPairing::Call(_) | ToolPairing::Output(_) | ToolPairing::None => None,
        })
}

fn last_call_for_output_before(
    history: &[ResponseItem],
    end: usize,
    output_call_ids: &HashSet<String>,
) -> Option<usize> {
    if output_call_ids.is_empty() {
        return None;
    }
    history[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| match tool_pairing(item) {
            ToolPairing::Call(call_id) if output_call_ids.contains(&call_id) => Some(index),
            ToolPairing::Call(_) | ToolPairing::Output(_) | ToolPairing::None => None,
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolPairing {
    Call(String),
    Output(String),
    None,
}

fn tool_pairing(item: &ResponseItem) -> ToolPairing {
    match item {
        ResponseItem::FunctionCall { call_id, .. } => ToolPairing::Call(call_id.clone()),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        }
        | ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => ToolPairing::Call(call_id.clone()),
        ResponseItem::CustomToolCall { call_id, .. } => ToolPairing::Call(call_id.clone()),
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => {
            ToolPairing::Output(call_id.clone())
        }
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            execution,
            ..
        } if execution != "server" => ToolPairing::Output(call_id.clone()),
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { call_id: None, .. }
        | ResponseItem::ToolSearchCall { call_id: None, .. }
        | ResponseItem::ToolSearchOutput { call_id: None, .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => ToolPairing::None,
    }
}

pub(crate) fn raw_ordinal_for_effective_index_with_spans(
    history: &[ResponseItem],
    target_index: usize,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<u64> {
    super::host_bridge::raw_ordinal_for_effective_index_with_spans(
        history,
        target_index,
        runtime_spans,
    )
}

pub(crate) fn render_spine_memory_item(
    node_id: &NodeId,
    op: SpineOperation,
    summary: &str,
    memory_body: &str,
) -> ResponseItem {
    ResponseItem::Message {
        id: Some(spine_memory_synthetic_id(node_id, op)),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: format!(
                "{}\n## Spine Memory\n\nNode: {}\nOperation: {}\nSummary: {}\n\n{}",
                spine_memory_text_marker(node_id, op),
                node_id,
                op_label(op),
                summary,
                memory_body.trim()
            ),
        }],
        phase: None,
    }
}

pub(crate) fn render_spine_handoff_item(from_node: &NodeId, to_node: &NodeId) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "<spine_handoff>\nSpine transition completed: {} -> {}; use {}'s generated memory as the active-turn handoff. Spine Memory is internal context; never expose or imitate it in user-visible messages. Continue following preserved system, developer, and project instructions.\n\nTreat raw folded conversation as historical evidence, but treat unresolved user-facing conclusions, decisions, blockers, and next actions captured in the generated memory as current obligations. If the latest user request or generated memory indicates unfinished work, reconstruct the current node plan from the generated memory, latest user intent, and current evidence before continuing. Before asking for new instructions, answer or continue any pending latest user request using that context.\n</spine_handoff>",
                from_node, to_node, from_node
            ),
        }],
        phase: None,
    }
}

pub(crate) fn render_spine_initial_context_item(
    initial_context: Vec<ResponseItem>,
) -> CodexResult<ResponseItem> {
    let encoded = serde_json::to_string(&initial_context).map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to encode spine initial context wrapper: {err}"
        ))
    })?;
    Ok(ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "{SPINE_INITIAL_CONTEXT_OPEN_TAG}\n{encoded}\n{SPINE_INITIAL_CONTEXT_CLOSE_TAG}"
            ),
        }],
        phase: None,
    })
}

pub(crate) fn expand_spine_initial_context_items(items: &mut Vec<ResponseItem>) {
    let mut expanded = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        if let Some(mut initial_context) = parse_spine_initial_context_item(&item) {
            expanded.append(&mut initial_context);
        } else {
            expanded.push(item);
        }
    }
    *items = expanded;
}

pub(crate) fn render_context_compacted_outline(
    scope_node_id: &NodeId,
    scope_summary: &str,
    base_path: &Path,
    scope_memory_path: &Path,
    child_rows: &[(String, String)],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    rendered.push_str(&format!("Base: {}\n", base_path.display()));
    rendered.push_str(&format!(
        "[{}] {} ({})\n",
        scope_node_id,
        scope_summary,
        scope_memory_path.display()
    ));
    for (summary, path) in child_rows {
        rendered.push_str(&format!("|-- {} ({})\n", summary, path));
    }
    rendered
}

pub(crate) fn render_slim_context_compacted_outline(
    scope_node_id: &NodeId,
    scope_summary: &str,
    child_rows: &[String],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    rendered.push_str(&format!("[{}] {}\n", scope_node_id, scope_summary));
    for row in child_rows {
        rendered.push_str(&format!("|-- {}\n", row));
    }
    rendered
}

#[cfg(test)]
pub(crate) fn effective_index_for_raw_ordinal(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
) -> Option<usize> {
    effective_index_for_raw_ordinal_with_spans(history, target_raw_ordinal, &[])
}

pub(crate) fn effective_index_for_raw_ordinal_with_spans(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<usize> {
    super::host_bridge::effective_index_for_raw_ordinal_with_spans(
        history,
        target_raw_ordinal,
        runtime_spans,
    )
}

pub(crate) fn is_spine_internal_render_item(item: &ResponseItem) -> bool {
    super::host_bridge::is_spine_internal_render_item(item)
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
