use super::context_handler::apply_label;
use super::materialize_context;
use super::memory_projection::SpinetreeUserMessageProjectionEntry;
use super::message_from_response_item;
use crate::context_manager::truncate_function_output_payload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use spine_core::ContextCellProvenance;
use spine_core::ContextLabel;
use spine_core::ContextPlanRecipe;
use spine_core::ContextPlanSource;
use spine_core::NodeContextCost;
use spine_core::NodeId;
use spine_core::SourceCellId;
use std::collections::BTreeMap;
use thiserror::Error;

const MAX_MODEL_VISIBLE_ITEM_TOKENS: usize = 9_999;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedCodexContextPlan {
    pub(crate) items: Vec<ResponseItem>,
    pub(crate) user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
}

pub(crate) fn prepare_codex_context_plan<S>(
    plan: &ContextPlanRecipe,
    source: &S,
    source_items: &BTreeMap<SourceCellId, ResponseItem>,
    node_context_costs: &BTreeMap<NodeId, NodeContextCost>,
    node_prompt: &str,
) -> Result<PreparedCodexContextPlan, CodexContextPlanError>
where
    S: ContextPlanSource,
{
    let resolved = plan
        .resolve(source)
        .map_err(|error| CodexContextPlanError(error.to_string()))?;
    let mut items = Vec::with_capacity(resolved.cells.len());
    let mut user_messages = Vec::new();
    for cell in resolved.cells {
        let mut item = match cell.provenance {
            ContextCellProvenance::Source(source_id) => {
                source_items.get(&source_id).cloned().ok_or_else(|| {
                    CodexContextPlanError(format!(
                        "missing Codex source item for stable identity {source_id:?}"
                    ))
                })?
            }
            ContextCellProvenance::Projection(_) => materialize_context(
                std::slice::from_ref(&cell.item),
                &[],
                None,
                None,
                node_context_costs,
                node_prompt,
            )
            .map_err(CodexContextPlanError)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                CodexContextPlanError(
                    "projected Spine context item rendered no Codex item".to_string(),
                )
            })?,
        };
        for label in &cell.labels {
            if let ContextLabel::UserAnchor(anchor) = label {
                user_messages.push(SpinetreeUserMessageProjectionEntry {
                    anchor: *anchor,
                    body: message_from_response_item(/*raw_index*/ 0, &item).content,
                });
            }
            apply_label(&mut item, label);
        }
        let mut estimated_tokens = projected_item_tokens(&item)?;
        if estimated_tokens > MAX_MODEL_VISIBLE_ITEM_TOKENS
            && let Some((bounded_item, bounded_tokens)) = bounded_projected_tool_output(&item)?
        {
            item = bounded_item;
            estimated_tokens = bounded_tokens;
        }
        if estimated_tokens > MAX_MODEL_VISIBLE_ITEM_TOKENS {
            return Err(CodexContextPlanError(format!(
                "projected Codex item is {estimated_tokens} tokens; maximum is \
                 {MAX_MODEL_VISIBLE_ITEM_TOKENS}"
            )));
        }
        items.push(item);
    }
    Ok(PreparedCodexContextPlan {
        items,
        user_messages,
    })
}

fn projected_item_tokens(item: &ResponseItem) -> Result<usize, CodexContextPlanError> {
    serde_json::to_string(item)
        .map(|encoded| approx_token_count(&encoded))
        .map_err(|error| {
            CodexContextPlanError(format!("failed to size projected Codex item: {error}"))
        })
}

fn bounded_projected_tool_output(
    item: &ResponseItem,
) -> Result<Option<(ResponseItem, usize)>, CodexContextPlanError> {
    let original_output = match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output,
        _ => return Ok(None),
    };
    let mut minimum_budget = 0;
    let mut maximum_budget = MAX_MODEL_VISIBLE_ITEM_TOKENS;
    let mut best = None;
    while minimum_budget <= maximum_budget {
        let budget = minimum_budget + (maximum_budget - minimum_budget) / 2;
        let mut candidate = item.clone();
        match &mut candidate {
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                *output = truncate_function_output_payload(
                    original_output,
                    TruncationPolicy::Tokens(budget),
                );
            }
            _ => unreachable!("tool output variant checked above"),
        }
        let tokens = projected_item_tokens(&candidate)?;
        if tokens <= MAX_MODEL_VISIBLE_ITEM_TOKENS {
            best = Some((candidate, tokens));
            minimum_budget = budget + 1;
        } else if budget == 0 {
            break;
        } else {
            maximum_budget = budget - 1;
        }
    }
    Ok(best)
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub(crate) struct CodexContextPlanError(pub(super) String);
