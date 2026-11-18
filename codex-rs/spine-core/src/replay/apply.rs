use super::ReplayError;
use crate::MemorySlot;
use crate::ResolvedContextPlan;
use crate::SpineProjection;
use crate::compiler::SamplingCompileError;

pub(super) fn verify_memory_slots(
    projection: &SpineProjection,
    resolved: &ResolvedContextPlan,
) -> Result<(), ReplayError> {
    if projection_memory_slots(projection) != resolved.memory_slots {
        return Err(ReplayError::MemorySlotMismatch);
    }
    Ok(())
}

pub(super) fn projection_memory_slots(projection: &SpineProjection) -> Vec<MemorySlot> {
    projection
        .nodes
        .iter()
        .flat_map(|node| node.memory.iter().flatten())
        .cloned()
        .collect()
}

pub(super) fn map_compile_error(error: SamplingCompileError) -> ReplayError {
    match error {
        SamplingCompileError::Spine(error) => ReplayError::Compile(error),
        SamplingCompileError::Transition(error) => ReplayError::Transition(format!("{error:?}")),
    }
}
