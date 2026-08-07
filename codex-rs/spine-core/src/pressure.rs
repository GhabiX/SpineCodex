use crate::SpineOperationFact;

/// Tracks provider-confirmed input pressure at Spine node boundaries.
///
/// A host supplies the input tokens reported for each completed sampling. Open
/// checkpoints that value; Close restores it after the node suffix is replaced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputPressureState {
    current: Option<u64>,
    open_checkpoints: Vec<Option<u64>>,
}

impl InputPressureState {
    pub(crate) fn apply_sampling<'a>(
        &mut self,
        input_tokens: Option<u64>,
        operations: impl IntoIterator<Item = &'a SpineOperationFact>,
    ) {
        self.current = input_tokens;
        for operation in operations {
            match operation {
                SpineOperationFact::Open { .. } => {
                    self.open_checkpoints.push(self.current);
                }
                SpineOperationFact::Close { .. } => {
                    if let Some(checkpoint) = self.open_checkpoints.pop() {
                        self.current = checkpoint;
                    }
                }
                SpineOperationFact::Next { .. } => {
                    let checkpoint = self.open_checkpoints.pop().unwrap_or(self.current);
                    self.current = checkpoint;
                    self.open_checkpoints.push(self.current);
                }
                SpineOperationFact::Spawn { .. } | SpineOperationFact::Trim { .. } => {}
            }
        }
    }

    pub(crate) fn compact(&mut self) {
        self.current = None;
        self.open_checkpoints.clear();
    }

    pub(crate) fn current_input_tokens(&self) -> Option<u64> {
        self.current
    }
}

#[cfg(test)]
#[path = "pressure_tests.rs"]
mod tests;
