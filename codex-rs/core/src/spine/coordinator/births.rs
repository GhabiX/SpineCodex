//! Spawn children's results waiting for the births that bind them.
use crate::spine::spawn::SpawnResult;
use spine_core::host::NodeId;
use spine_core::host::SpawnArrival;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// A Spawn's children start inside the sampling that calls spine.spawn, but
/// their births exist only once that sampling commits. Results wait here,
/// by call id, until the commit names the child each one arrives as. A Spawn
/// whose sampling ends without bearing it is orphaned: its results are
/// dropped and its runner is cancelled.
#[derive(Default)]
pub(crate) struct SpawnBirths {
    started: BTreeSet<String>,
    producers: BTreeMap<String, Vec<NodeId>>,
    results: BTreeMap<String, Vec<SpawnResult>>,
    finished: BTreeMap<String, BTreeSet<u32>>,
    orphaned: Vec<String>,
}

impl SpawnBirths {
    pub(crate) fn start(&mut self, call_id: String) {
        self.started.insert(call_id);
    }

    pub(crate) fn bear(&mut self, call_id: String, producers: Vec<NodeId>) {
        self.started.remove(&call_id);
        self.producers.insert(call_id, producers);
    }

    /// Ends the open sampling: every Spawn it started and did not bear is
    /// orphaned.
    pub(crate) fn end_sampling(&mut self) {
        for call_id in std::mem::take(&mut self.started) {
            self.results.remove(&call_id);
            self.finished.remove(&call_id);
            self.orphaned.push(call_id);
        }
    }

    pub(crate) fn take_orphaned(&mut self) -> Vec<String> {
        std::mem::take(&mut self.orphaned)
    }

    /// Records each child's result once; a result for an orphaned Spawn, or
    /// for a child that already has one, is dropped.
    pub(crate) fn finish(&mut self, call_id: String, results: Vec<SpawnResult>) {
        if !self.started.contains(&call_id) && !self.producers.contains_key(&call_id) {
            return;
        }
        let finished = self.finished.entry(call_id.clone()).or_default();
        let fresh = results
            .into_iter()
            .filter(|result| finished.insert(result.ordinal))
            .collect::<Vec<_>>();
        if !fresh.is_empty() {
            self.results.entry(call_id).or_default().extend(fresh);
        }
    }

    pub(crate) fn has_arrivals(&self) -> bool {
        self.results
            .keys()
            .any(|call_id| self.producers.contains_key(call_id))
    }

    /// The arrivals whose births have committed, each handed out once.
    pub(crate) fn take_arrivals(&mut self) -> Vec<SpawnArrival> {
        let mut arrivals = Vec::new();
        let mut complete = Vec::new();
        for (call_id, producers) in &self.producers {
            let Some(results) = self.results.remove(call_id) else {
                continue;
            };
            arrivals.extend(results.into_iter().map(|result| {
                let producer = producers[result.ordinal as usize].clone();
                result.into_arrival(producer)
            }));
            if self
                .finished
                .get(call_id)
                .is_some_and(|finished| finished.len() == producers.len())
            {
                complete.push(call_id.clone());
            }
        }
        for call_id in complete {
            self.producers.remove(&call_id);
            self.finished.remove(&call_id);
        }
        arrivals
    }
}
