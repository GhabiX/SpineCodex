//! Prepare compact successors before persistence; install only durable checkpoints.
use super::*;

pub(crate) struct PreparedCanonicalCompact {
    runtime: SamplingRuntime,
    next_boundary: u64,
    source_items: BTreeMap<spine_core::host::SourceCellId, ResponseItemEnvelope>,
    barrier: SpineCompactBarrierV1,
    replacement: Vec<RolloutItem>,
}

impl CodexSpineCoordinator {
    pub(crate) fn prepare_compact(
        &self,
        replacement_items: &[ResponseItemEnvelope],
    ) -> Result<PreparedCanonicalCompact, CoordinatorError> {
        self.require_healthy()?;
        let epoch = self.runtime.epoch();
        let next_epoch = epoch.checked_next().ok_or_else(|| {
            CoordinatorError::Identity("Spine context epoch is exhausted".to_string())
        })?;
        let boundary = RawBoundary(self.next_boundary);
        let replacement_boundaries = (0..replacement_items.len())
            .scan(boundary.0, |next, _| {
                *next = next.saturating_add(1);
                Some(RawBoundary(*next))
            })
            .collect::<Vec<_>>();
        let barrier = SpineCompactBarrierV1::new(
            self.runtime.thread().clone(),
            epoch,
            next_epoch,
            boundary,
            replacement_boundaries.clone(),
        )
        .map_err(|error| CoordinatorError::Archive(error.to_string()))?;
        let runtime = self.runtime.prepare_compact(barrier.clone())?;
        let next_boundary = replacement_boundaries.last().map_or_else(
            || boundary.0.saturating_add(1),
            |boundary| boundary.0.saturating_add(1),
        );
        let source_items = runtime
            .source_snapshot()
            .cells()
            .iter()
            .map(|cell| cell.id.clone())
            .zip(replacement_items.iter().cloned())
            .collect();
        Ok(PreparedCanonicalCompact {
            runtime,
            next_boundary,
            source_items,
            barrier,
            replacement: replacement_items
                .iter()
                .cloned()
                .map(RolloutItem::ResponseItem)
                .collect(),
        })
    }

    pub(crate) fn install_compact(&mut self, prepared: PreparedCanonicalCompact) {
        if let Some(seed) = &mut self.replay_seed {
            seed.push(ReplaySeedItem::Compact {
                barrier: prepared.barrier,
                replacement: prepared.replacement,
            });
        }
        self.runtime = prepared.runtime;
        self.next_boundary = prepared.next_boundary;
        self.source_items = prepared.source_items;
        // User-message projections belong to the session and survive compaction.
    }
}
