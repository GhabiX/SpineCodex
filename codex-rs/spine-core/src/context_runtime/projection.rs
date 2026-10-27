use super::SpineContextRuntimeError;
use crate::CellId;
use crate::ContextEvent;
use crate::ContextInsert;
use crate::ContextItem;
use crate::ContextLabel;
use crate::MemorySlot;
use crate::NativeItemRef;
use crate::ParseCell;
use crate::ParseStack;
use crate::RawBoundary;
use crate::SpineChar;
use crate::SpineCharParser;
use crate::SpineProjection;
use std::collections::BTreeSet;

pub(super) fn project_jit_stack<E>(
    parser: &mut SpineCharParser,
    projection: &SpineProjection,
    pending_boundaries: &[RawBoundary],
) -> Result<ParseStack, SpineContextRuntimeError<E>>
where
    E: std::error::Error,
{
    let source = parser.stack().cells().to_vec();
    let mut used = BTreeSet::new();
    let mut cells = Vec::new();
    for item in &projection.visible_context {
        project_context_item::<E>(
            parser,
            &source,
            &mut used,
            &mut cells,
            item,
            projection.last_boundary.unwrap_or(RawBoundary(0)),
        )?;
    }
    for boundary in pending_boundaries {
        let cell = take_source_cell(&source, &mut used, |cell| {
            cell.character().boundary() == *boundary
        })
        .ok_or(SpineContextRuntimeError::MissingCell {
            boundary: *boundary,
        })?;
        cells.push(cell.with_labels(Vec::new()));
    }
    Ok(ParseStack::from_cells(cells))
}

fn project_context_item<E>(
    parser: &mut SpineCharParser,
    source: &[ParseCell],
    used: &mut BTreeSet<CellId>,
    target: &mut Vec<ParseCell>,
    item: &ContextItem,
    synthetic_boundary: RawBoundary,
) -> Result<(), SpineContextRuntimeError<E>>
where
    E: std::error::Error,
{
    if let Some(cell) = take_source_cell(source, used, |cell| {
        matches!(
            cell.character(),
            SpineChar::Synthetic { item: source, .. } if source == item
        )
    }) {
        target.push(cell.with_labels(Vec::new()));
        return Ok(());
    }
    match item {
        ContextItem::Message {
            message,
            user_anchor,
        } => {
            let cell = take_source_cell(source, used, |cell| {
                matches!(
                    cell.character(),
                    SpineChar::Message(source)
                        if source.boundary == message.boundary
                )
            })
            .ok_or(SpineContextRuntimeError::MissingCell {
                boundary: message.boundary,
            })?;
            let labels = user_anchor
                .iter()
                .copied()
                .map(ContextLabel::UserAnchor)
                .collect();
            target.push(cell.with_labels(labels));
        }
        ContextItem::SourceSpan { span } => {
            let span_cells = source
                .iter()
                .filter(|cell| {
                    !used.contains(&cell.id())
                        && span.start <= cell.character().boundary()
                        && cell.character().boundary() <= span.end
                })
                .cloned()
                .collect::<Vec<_>>();
            if span_cells.is_empty() {
                return Err(SpineContextRuntimeError::MissingCell {
                    boundary: span.start,
                });
            }
            for cell in span_cells {
                used.insert(cell.id());
                target.push(cell.with_labels(Vec::new()));
            }
        }
        ContextItem::MemorySlot(MemorySlot::User {
            message, anchor, ..
        }) => {
            let cell = take_source_cell(source, used, |cell| {
                matches!(
                    cell.character(),
                    SpineChar::Message(source)
                        if source.boundary == message.boundary
                )
            })
            .ok_or(SpineContextRuntimeError::MissingCell {
                boundary: message.boundary,
            })?;
            target.push(cell.with_labels(vec![ContextLabel::UserAnchor(*anchor)]));
        }
        ContextItem::Native {
            source: NativeItemRef::Rollout { ordinal },
        } => {
            let cell =
                take_source_cell(source, used, |cell| cell.character().boundary() == *ordinal)
                    .ok_or(SpineContextRuntimeError::MissingCell { boundary: *ordinal })?;
            target.push(cell.with_labels(Vec::new()));
        }
        ContextItem::Native {
            source: NativeItemRef::CompactReplacement { .. },
        } => return Err(SpineContextRuntimeError::ArchivedSourceInLiveContext),
        ContextItem::SyntheticNode { .. }
        | ContextItem::MemorySlot(MemorySlot::Summary { .. })
        | ContextItem::MemorySlot(MemorySlot::SpawnEvidence { .. }) => {
            if let Some(cell) = take_source_cell(source, used, |cell| {
                matches!(
                    cell.character(),
                    SpineChar::Synthetic { item: source, .. } if source == item
                )
            }) {
                target.push(cell.with_labels(Vec::new()));
            } else {
                target.push(
                    parser
                        .synthetic_cell(synthetic_boundary, item.clone())
                        .with_labels(Vec::new()),
                );
            }
        }
    }
    Ok(())
}

fn take_source_cell(
    source: &[ParseCell],
    used: &mut BTreeSet<CellId>,
    predicate: impl Fn(&ParseCell) -> bool,
) -> Option<ParseCell> {
    let cell = source
        .iter()
        .find(|cell| !used.contains(&cell.id()) && predicate(cell))?
        .clone();
    used.insert(cell.id());
    Some(cell)
}

pub(super) fn context_events_between<E>(
    before: &ParseStack,
    after: &ParseStack,
) -> Result<Vec<ContextEvent>, SpineContextRuntimeError<E>>
where
    E: std::error::Error,
{
    let common_prefix = before
        .cells()
        .iter()
        .zip(after.cells())
        .take_while(|(left, right)| same_cell_source(left, right))
        .count();
    let max_suffix = before
        .len()
        .saturating_sub(common_prefix)
        .min(after.len().saturating_sub(common_prefix));
    let common_suffix = before
        .cells()
        .iter()
        .rev()
        .zip(after.cells().iter().rev())
        .take(max_suffix)
        .take_while(|(left, right)| same_cell_source(left, right))
        .count();

    let before_middle_end = before.len().saturating_sub(common_suffix);
    let after_middle_end = after.len().saturating_sub(common_suffix);
    let structural_change = common_prefix != before_middle_end || common_prefix != after_middle_end;
    let mut events = Vec::new();
    if structural_change {
        let insert = after.cells()[common_prefix..after_middle_end]
            .iter()
            .map(|cell| {
                if let Some(source_index) = before
                    .cells()
                    .iter()
                    .position(|source| source.id() == cell.id())
                {
                    Ok(ContextInsert::Existing {
                        cell_id: cell.id(),
                        source_index,
                    })
                } else if let SpineChar::Synthetic { item, .. } = cell.character() {
                    Ok(ContextInsert::Synthetic {
                        cell_id: cell.id(),
                        item: item.clone(),
                    })
                } else {
                    Err(SpineContextRuntimeError::MissingCell {
                        boundary: cell.character().boundary(),
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        events.push(ContextEvent::Splice {
            start: common_prefix,
            delete: before_middle_end.saturating_sub(common_prefix),
            insert,
        });
    }

    for (index, target) in after.cells().iter().enumerate() {
        let preserved = index < common_prefix || index >= after_middle_end;
        let source_index = before
            .cells()
            .iter()
            .position(|source| source.id() == target.id());
        let current_labels = source_index
            .filter(|_| preserved)
            .map_or(&[][..], |source_index| {
                before.cells()[source_index].labels()
            });
        if current_labels == target.labels() {
            continue;
        }
        if let Some(source_index) = source_index
            && !current_labels.is_empty()
        {
            events.push(ContextEvent::Splice {
                start: index,
                delete: 1,
                insert: vec![ContextInsert::Existing {
                    cell_id: target.id(),
                    source_index,
                }],
            });
        }
        events.extend(
            target
                .labels()
                .iter()
                .cloned()
                .map(|label| ContextEvent::Tag { index, label }),
        );
    }
    Ok(events)
}

fn same_cell_source(left: &ParseCell, right: &ParseCell) -> bool {
    left.id() == right.id() && left.character() == right.character()
}
