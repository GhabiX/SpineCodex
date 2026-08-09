use super::context_handler::apply_label;
use super::materialize_context;
use super::memory_projection::SpinetreeUserMessageProjectionEntry;
use super::message_from_response_item;
use crate::context::ContextualUserFragment;
use crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES;
use crate::context::SpineUserAnchor;
use crate::context::spine_model_item_wire_bytes;
use crate::context::validate_spine_model_item;
use crate::context_manager::truncate_function_output_payload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TruncationPolicy;
use spine_core::ContextCellProvenance;
use spine_core::ContextLabel;
use spine_core::ContextPlanRecipe;
use spine_core::ContextPlanSource;
use spine_core::NodeContextCost;
use spine_core::NodeId;
use spine_core::SourceCellId;
use std::collections::BTreeMap;
use thiserror::Error;

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
        let mut spine_owned = matches!(&cell.provenance, ContextCellProvenance::Projection(_));
        let mut anchor_items = Vec::new();
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
                let mut anchored = item.clone();
                SpineUserAnchor::new(*anchor).prepend_to(&mut anchored);
                if validate_spine_model_item(&anchored).is_ok() {
                    item = anchored;
                } else {
                    let anchor_item: ResponseItem =
                        ContextualUserFragment::into(SpineUserAnchor::new(*anchor));
                    validate_spine_model_item(&anchor_item).map_err(CodexContextPlanError)?;
                    anchor_items.push(anchor_item);
                }
                continue;
            }
            if matches!(
                label,
                ContextLabel::ToolOutput(_) | ContextLabel::SpawnOutput { .. }
            ) {
                spine_owned = true;
            }
            apply_label(&mut item, label);
        }
        if spine_owned {
            let mut wire_bytes = projected_item_wire_bytes(&item)?;
            if wire_bytes > MAX_SPINE_MODEL_ITEM_WIRE_BYTES
                && let Some((bounded_item, bounded_wire_bytes)) =
                    bounded_projected_tool_output(&item)?
            {
                item = bounded_item;
                wire_bytes = bounded_wire_bytes;
            }
            if wire_bytes > MAX_SPINE_MODEL_ITEM_WIRE_BYTES {
                return Err(CodexContextPlanError(format!(
                    "Spine-owned model item is {wire_bytes} serialized bytes; maximum is \
                     {MAX_SPINE_MODEL_ITEM_WIRE_BYTES}"
                )));
            }
        }
        items.extend(anchor_items);
        items.push(item);
    }
    Ok(PreparedCodexContextPlan {
        items,
        user_messages,
    })
}

fn projected_item_wire_bytes(item: &ResponseItem) -> Result<usize, CodexContextPlanError> {
    spine_model_item_wire_bytes(item).map_err(CodexContextPlanError)
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
    let mut maximum_budget = MAX_SPINE_MODEL_ITEM_WIRE_BYTES;
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
        let wire_bytes = projected_item_wire_bytes(&candidate)?;
        if wire_bytes <= MAX_SPINE_MODEL_ITEM_WIRE_BYTES {
            best = Some((candidate, wire_bytes));
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
