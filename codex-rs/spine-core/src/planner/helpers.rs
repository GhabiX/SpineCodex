use super::PlannerError;
use super::PlannerTransitionError;
use crate::compiler::SamplingCompileError;
use crate::reducer::TypedTransitionError;

pub(super) fn map_compile_error(error: SamplingCompileError) -> PlannerError {
    match error {
        SamplingCompileError::Spine(error) => PlannerError::CompileSpine(error),
        SamplingCompileError::Transition(error) => PlannerError::InvalidTransition(match error {
            TypedTransitionError::MultipleStructuralFacts => {
                PlannerTransitionError::MultipleStructuralFacts
            }
            TypedTransitionError::TaskCursorRequired(operation) => {
                PlannerTransitionError::TaskCursorRequired(operation)
            }
            TypedTransitionError::NonTrimFactInTrimSet => {
                PlannerTransitionError::NonTrimFactInTrimSet
            }
            TypedTransitionError::InactiveTrimTarget(boundary) => {
                PlannerTransitionError::InactiveTrimTarget(boundary)
            }
            TypedTransitionError::TrimTargetMismatch => PlannerTransitionError::TrimTargetMismatch,
        }),
    }
}
