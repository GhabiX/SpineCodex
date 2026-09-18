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
        synthetic_sources: source
            .cells()
            .iter()
            .filter(|cell| matches!(cell.payload, SourceCellPayload::Synthetic(_)))
            .collect(),
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
            .source_at_boundary(*boundary)
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
    synthetic_sources: Vec<&'a SourceCell>,
    previous: Option<&'a ContextPlanRecipe>,
    used_source: BTreeSet<SourceCellId>,
    used_projection: BTreeSet<ProjectionCellId>,
    next_projection_ordinal: &'a mut u64,
    cells: Vec<ContextPlanCell>,
}

impl ContextPlanBuilder<'_> {
    fn push_item(&mut self, item: &ContextItem) -> Result<(), PlannerError> {
        // Compact replacements can carry any ContextItem. Preserve their
        // priority and original order without scanning ordinary source cells.
        if let Some(cell) = self.synthetic_sources.iter().find(|cell| {
            !self.used_source.contains(&cell.id)
                && matches!(&cell.payload, SourceCellPayload::Synthetic(source) if source == item)
        }).map(|cell| (*cell).clone()) {
            self.push_source(&cell, Vec::new());
            return Ok(());
        }
        match item {
            ContextItem::Message {
                message,
                user_anchor,
            } => {
                let cell = self
                    .source_at_boundary(message.boundary)
                    .filter(|cell| {
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
                    .source_at_boundary(message.boundary)
                    .filter(|cell| {
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
                    .source_at_boundary(*ordinal)
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
        // SourceLedger enforces strictly increasing raw boundaries, including
        // across namespace continuation. Both endpoints remain inclusive.
        let cells = self.source.cells();
        let start = cells.partition_point(|cell| cell.boundary.ordinal() < span.start.0);
        let source_cells = cells[start..]
            .iter()
            .take_while(|cell| cell.boundary.ordinal() <= span.end.0)
            .filter(|cell| !self.used_source.contains(&cell.id))
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

    fn source_at_boundary(&self, boundary: RawBoundary) -> Option<SourceCell> {
        let cells = self.source.cells();
        let index = cells
            .binary_search_by_key(&boundary.0, |cell| cell.boundary.ordinal())
            .ok()?;
        Some(&cells[index])
            .filter(|cell| !self.used_source.contains(&cell.id))
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
        let projection_id = reusable.unwrap_or_else(|| self.allocate_projection_id());
        self.used_projection.insert(projection_id.clone());
        if *self.next_projection_ordinal <= projection_id.ordinal() {
            *self.next_projection_ordinal = projection_id.ordinal().saturating_add(1);
        }
        self.cells.push(ContextPlanCell::Projection {
            projection_id,
            item,
        });
    }

    fn allocate_projection_id(&self) -> ProjectionCellId {
        let mut ordinal = 0u64;
        loop {
            let id = ProjectionCellId::new(
                self.source.thread().clone(),
                self.source.epoch(),
                ordinal,
            );
            if !self.used_projection.contains(&id) {
                return id;
            }
            ordinal = ordinal.saturating_add(1);
        }
    }
}

#[cfg(test)]
#[path = "context_builder_lookup_tests.rs"]
mod lookup_tests;

#[cfg(test)]
mod tests {
    use super::build_context_plan;
    use crate::ContextEpoch;
    use crate::ContextItem;
    use crate::ContextPlanCell;
    use crate::MemorySlot;
    use crate::Message;
    use crate::MessageRole;
    use crate::NodeId;
    use crate::NodeStatus;
    use crate::RawBoundary;
    use crate::RawSpan;
    use crate::SourceLedger;
    use crate::SpineChar;
    use crate::SpineProjection;
    use crate::ThreadNamespace;

    fn thread() -> ThreadNamespace {
        ThreadNamespace::parse("plan-ids").expect("thread")
    }

    #[test]
    fn closed_memory_reuses_freed_projection_ordinal() {
        let mut source = SourceLedger::new(thread(), ContextEpoch::ZERO).expect("ledger");
        source
            .append([SpineChar::Message(Message {
                boundary: RawBoundary(1),
                role: MessageRole::User,
                content: "task".to_string(),
            })])
            .expect("append");
        let snapshot = source.snapshot();
        let node = ContextItem::SyntheticNode {
            node_id: NodeId::root_epoch(1).child(1),
            summary: "child".to_string(),
            status: NodeStatus::Opened,
        };
        let mut ordinal = 0;
        let opened = build_context_plan(
            &snapshot,
            &SpineProjection {
                nodes: Vec::new(),
                cursor: NodeId::root_epoch(1).child(1),
                visible_context: vec![node],
                last_boundary: Some(RawBoundary(1)),
            },
            &[],
            None,
            &mut ordinal,
        )
        .expect("opened plan");
        assert_eq!(ordinal, 1);
        let memory = ContextItem::MemorySlot(MemorySlot::Summary {
            owner_node: NodeId::root_epoch(1).child(1),
            source: RawSpan {
                start: RawBoundary(1),
                end: RawBoundary(1),
            },
            body: "done".to_string(),
        });
        let mut next = ordinal;
        let closed = build_context_plan(
            &snapshot,
            &SpineProjection {
                nodes: Vec::new(),
                cursor: NodeId::root_epoch(1),
                visible_context: vec![memory],
                last_boundary: Some(RawBoundary(1)),
            },
            &[],
            Some(&opened),
            &mut next,
        )
        .expect("closed plan");
        let ContextPlanCell::Projection { projection_id, .. } =
            closed.cells.first().expect("memory cell")
        else {
            panic!("expected projection");
        };
        assert_eq!(projection_id.ordinal(), 0);
    }
}
