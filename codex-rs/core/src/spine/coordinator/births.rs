//! Spawn children's results waiting for the births that bind them.
use crate::spine::spawn::SpawnResult;
use spine_core::host::NodeId;
use spine_core::host::SpawnArrival;
use std::collections::BTreeMap;

/// A Spawn's children start inside the sampling that calls spine.spawn, but
/// their births exist only once that sampling commits. Results wait here,
/// by call id, until the commit names the child each one arrives as.
#[derive(Default)]
pub(crate) struct SpawnBirths {
    producers: BTreeMap<String, Vec<NodeId>>,
    results: BTreeMap<String, Vec<SpawnResult>>,
}

impl SpawnBirths {
    pub(crate) fn bear(&mut self, call_id: String, producers: Vec<NodeId>) {
        self.producers.insert(call_id, producers);
    }

    pub(crate) fn finish(&mut self, call_id: String, results: Vec<SpawnResult>) {
        self.results.entry(call_id).or_default().extend(results);
    }

    /// The arrivals whose births have committed, each handed out once.
    pub(crate) fn take_arrivals(&mut self) -> Vec<SpawnArrival> {
        let mut arrivals = Vec::new();
        for (call_id, producers) in &self.producers {
            let Some(results) = self.results.remove(call_id) else {
                continue;
            };
            arrivals.extend(results.into_iter().map(|result| {
                let producer = producers[result.ordinal as usize].clone();
                result.into_arrival(producer)
            }));
        }
        arrivals
    }
}
