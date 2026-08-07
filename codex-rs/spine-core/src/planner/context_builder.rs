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
use crate::RecordDigest;
use crate::SourceCell;
use crate::SourceCellId;
use crate::SourceCellPayload;
use crate::SourceSnapshot;
use crate::SpineProjection;
use crate::TrimProjection;
use crate::context_plan::CONTEXT_PLAN_SCHEMA_V1;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_context_plan(
    source: &SourceSnapshot,
    projection: &SpineProjection,
    trim: Option<&TrimProjection>,
    pending_boundaries: &[RawBoundary],
    previous: Option<&ContextPlanRecipe>,
    next_projection_ordinal: &mut u64,
    spawn_enabled: bool,
) -> Result<ContextPlanRecipe, PlannerError> {
    let mut builder = ContextPlanBuilder {
        source,
        trim,
        previous,
        used_source: BTreeSet::new(),
        used_projection: BTreeSet::new(),
        next_projection_ordinal,
        cells: Vec::new(),
        spawn_enabled,
        spawn_call_outcomes: spawn_call_outcomes(source, projection),
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
        source_snapshot_digest: source.digest().clone(),
        cells: builder.cells,
        memory_slots,
        plan_digest: RecordDigest::parse("0".repeat(64)).map_err(PlannerError::Archive)?,
    }
    .finalize_digest()
    .map_err(PlannerError::ContextPlan)
}

struct ContextPlanBuilder<'a> {
    source: &'a SourceSnapshot,
    trim: Option<&'a TrimProjection>,
    previous: Option<&'a ContextPlanRecipe>,
    used_source: BTreeSet<SourceCellId>,
    used_projection: BTreeSet<ProjectionCellId>,
    next_projection_ordinal: &'a mut u64,
    cells: Vec<ContextPlanCell>,
    spawn_enabled: bool,
    spawn_call_outcomes: BTreeMap<RawBoundary, bool>,
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
                                | SourceCellPayload::TurnAborted(source)
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
                                | SourceCellPayload::TurnAborted(source)
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
            let labels = source_span_labels(
                &cell,
                self.trim,
                self.spawn_enabled,
                &self.spawn_call_outcomes,
            );
            self.push_source(&cell, labels);
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

fn source_span_labels(
    cell: &SourceCell,
    trim: Option<&TrimProjection>,
    spawn_enabled: bool,
    spawn_call_outcomes: &BTreeMap<RawBoundary, bool>,
) -> Vec<ContextLabel> {
    let SourceCellPayload::ToolResponse { call_id, .. } = &cell.payload else {
        return Vec::new();
    };
    let boundary = RawBoundary(cell.boundary.ordinal());
    let mut labels = trim
        .and_then(|projection| projection.edit(boundary, call_id))
        .cloned()
        .map(ContextLabel::ToolOutput)
        .into_iter()
        .collect::<Vec<_>>();
    if spawn_enabled && let Some(succeeded) = spawn_call_outcomes.get(&boundary) {
        labels.push(ContextLabel::SpawnOutput {
            succeeded: *succeeded,
        });
    }
    labels
}

fn spawn_call_outcomes(
    source: &SourceSnapshot,
    projection: &SpineProjection,
) -> BTreeMap<RawBoundary, bool> {
    projection
        .visible_context
        .iter()
        .filter_map(|item| match item {
            ContextItem::SourceSpan { span } => Some(*span),
            ContextItem::Message { .. }
            | ContextItem::SyntheticNode { .. }
            | ContextItem::MemorySlot(_)
            | ContextItem::Native { .. } => None,
        })
        .flat_map(|span| {
            let in_span = move |cell: &SourceCell| {
                span.start.0 <= cell.boundary.ordinal() && cell.boundary.ordinal() <= span.end.0
            };
            let requests = source
                .cells()
                .iter()
                .filter(move |cell| in_span(cell))
                .filter_map(|cell| match &cell.payload {
                    SourceCellPayload::ToolRequest { call_id, name, .. }
                        if name == "spine.spawn" =>
                    {
                        Some(call_id.clone())
                    }
                    SourceCellPayload::Message(_)
                    | SourceCellPayload::TurnAborted(_)
                    | SourceCellPayload::ToolRequest { .. }
                    | SourceCellPayload::ToolResponse { .. }
                    | SourceCellPayload::Opaque
                    | SourceCellPayload::Synthetic(_) => None,
                })
                .collect::<BTreeSet<_>>();
            let conflicting = source.cells().iter().any(|cell| {
                in_span(cell)
                    && matches!(
                        &cell.payload,
                        SourceCellPayload::ToolRequest { name, .. }
                            if matches!(name.as_str(), "spine.open" | "spine.close" | "spine.next")
                    )
            });
            source
                .cells()
                .iter()
                .filter(move |cell| in_span(cell))
                .filter_map(move |cell| match &cell.payload {
                    SourceCellPayload::ToolResponse {
                        call_id, outcome, ..
                    } if requests.contains(call_id) => Some((
                        RawBoundary(cell.boundary.ordinal()),
                        *outcome == crate::ToolOutcome::Succeeded && !conflicting,
                    )),
                    SourceCellPayload::Message(_)
                    | SourceCellPayload::TurnAborted(_)
                    | SourceCellPayload::ToolRequest { .. }
                    | SourceCellPayload::ToolResponse { .. }
                    | SourceCellPayload::Opaque
                    | SourceCellPayload::Synthetic(_) => None,
                })
        })
        .collect()
}
