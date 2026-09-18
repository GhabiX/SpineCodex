use super::PlannerError;
use crate::ContextItem;
use crate::ContextLabel;
use crate::ContextPlanCell;
use crate::ContextPlanRecipe;
use crate::ContextPlanSource;
use crate::MemorySlot;
use crate::NativeItemRef;
use crate::ProjectionCellId;
use crate::RawBoundary;
use crate::SourceCell;
use crate::SourceCellId;
use crate::SourceCellPayload;
use crate::SourceSnapshot;
use crate::SpineProjection;
use crate::context_plan::CONTEXT_PLAN_SCHEMA_V1;
use std::collections::BTreeSet;

pub(crate) fn build_context_plan(
    source: &SourceSnapshot,
    projection: &SpineProjection,
    pending_boundaries: &[RawBoundary],
    previous: Option<&ContextPlanRecipe>,
    next_projection_ordinal: &mut u64,
) -> Result<ContextPlanRecipe, PlannerError> {
    let mut builder = ContextPlanBuilder {
        source,
        previous,
        used_source: BTreeSet::new(),
        used_projection: BTreeSet::new(),
        next_projection_ordinal,
        cells: Vec::new(),
    };
    for item in &projection.visible_context {
        builder.push_item(item)?;
    }
    for boundary in pending_boundaries {
        let cell = builder
            .take_source(|cell| cell.boundary.ordinal() == boundary.0)
            .ok_or(PlannerError::MissingSourceBoundary(*boundary))?;
        builder.push_source(&cell, Vec::new());
    }
    let memory_slots = projection
        .nodes
        .iter()
        .flat_map(|node| node.memory.iter().flatten())
        .cloned()
        .collect();
    ContextPlanRecipe {
        schema: CONTEXT_PLAN_SCHEMA_V1.to_string(),
        thread: source.thread().clone(),
        epoch: source.epoch(),
        source_snapshot_digest: None,
        cells: builder.cells,
        memory_slots,
        plan_digest: None,
    }
    .finalize()
    .map_err(PlannerError::ContextPlan)
}

struct ContextPlanBuilder<'a> {
    source: &'a SourceSnapshot,
    previous: Option<&'a ContextPlanRecipe>,
    used_source: BTreeSet<SourceCellId>,
    used_projection: BTreeSet<ProjectionCellId>,
    next_projection_ordinal: &'a mut u64,
    cells: Vec<ContextPlanCell>,
}

impl ContextPlanBuilder<'_> {
    fn push_item(&mut self, item: &ContextItem) -> Result<(), PlannerError> {
        if let Some(cell) = self.take_source(
            |cell| matches!(&cell.payload, SourceCellPayload::Synthetic(source) if source == item),
        ) {
            self.push_source(&cell, Vec::new());
            return Ok(());
        }
        match item {
            ContextItem::Message {
                message,
                user_anchor,
            } => {
                let cell = self
                    .take_source(|cell| {
                        matches!(
                            &cell.payload,
                            SourceCellPayload::Message(source)
                                if source == message
                        )
                    })
                    .ok_or(PlannerError::MissingSourceBoundary(message.boundary))?;
                let labels = user_anchor
                    .iter()
                    .copied()
                    .map(ContextLabel::UserAnchor)
                    .collect();
                self.push_source(&cell, labels);
            }
            ContextItem::SourceSpan { span } => self.push_span(*span)?,
            ContextItem::MemorySlot(MemorySlot::User {
                message, anchor, ..
            }) => {
                let cell = self
                    .take_source(|cell| {
                        matches!(
                            &cell.payload,
                            SourceCellPayload::Message(source)
                                if source == message
                        )
                    })
                    .ok_or(PlannerError::MissingSourceBoundary(message.boundary))?;
                self.push_source(&cell, vec![ContextLabel::UserAnchor(*anchor)]);
            }
            ContextItem::Native {
                source: NativeItemRef::Rollout { ordinal },
            } => {
                let cell = self
                    .take_source(|cell| cell.boundary.ordinal() == ordinal.0)
                    .ok_or(PlannerError::MissingSourceBoundary(*ordinal))?;
                self.push_source(&cell, Vec::new());
            }
            ContextItem::Native {
                source: NativeItemRef::CompactReplacement { .. },
            } => {
                self.push_projection(item.clone());
            }
            ContextItem::SyntheticNode { .. }
            | ContextItem::MemorySlot(MemorySlot::Summary { .. })
            | ContextItem::MemorySlot(MemorySlot::SpawnEvidence { .. }) => {
                self.push_projection(item.clone());
            }
        }
        Ok(())
    }

    fn push_span(&mut self, span: crate::RawSpan) -> Result<(), PlannerError> {
        let source_cells = self
            .source
            .cells()
            .iter()
            .filter(|cell| {
                !self.used_source.contains(&cell.id)
                    && span.start.0 <= cell.boundary.ordinal()
                    && cell.boundary.ordinal() <= span.end.0
            })
            .cloned()
            .collect::<Vec<_>>();
        if source_cells.is_empty() {
            return Err(PlannerError::MissingSourceBoundary(span.start));
        }
        for cell in source_cells {
            self.push_source(&cell, Vec::new());
        }
        Ok(())
    }

    fn take_source(&self, predicate: impl Fn(&SourceCell) -> bool) -> Option<SourceCell> {
        self.source
            .cells()
            .iter()
            .find(|cell| !self.used_source.contains(&cell.id) && predicate(cell))
            .cloned()
    }

    fn push_source(&mut self, cell: &SourceCell, labels: Vec<ContextLabel>) {
        self.used_source.insert(cell.id.clone());
        self.cells.push(ContextPlanCell::Source {
            source_id: cell.id.clone(),
            labels,
        });
    }

    fn push_projection(&mut self, item: ContextItem) {
        let reusable = self.previous.and_then(|previous| {
            previous.cells.iter().find_map(|cell| match cell {
                ContextPlanCell::Projection {
                    projection_id,
                    item: previous_item,
                } if projection_id.thread() == self.source.thread()
                    && projection_id.epoch() == self.source.epoch()
                    && previous_item == &item
                    && !self.used_projection.contains(projection_id) =>
                {
                    Some(projection_id.clone())
                }
                ContextPlanCell::Source { .. } | ContextPlanCell::Projection { .. } => None,
            })
        });
        let projection_id = reusable.unwrap_or_else(|| {
            let id = ProjectionCellId::new(
                self.source.thread().clone(),
                self.source.epoch(),
                *self.next_projection_ordinal,
            );
            *self.next_projection_ordinal = self.next_projection_ordinal.saturating_add(1);
            id
        });
        self.used_projection.insert(projection_id.clone());
        self.cells.push(ContextPlanCell::Projection {
            projection_id,
            item,
        });
    }
}
