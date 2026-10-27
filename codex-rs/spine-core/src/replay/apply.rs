use super::ReplayError;
use crate::compiler::SamplingCompileError;

pub(super) fn map_compile_error(error: SamplingCompileError) -> ReplayError {
    match error {
        SamplingCompileError::Spine(error) => ReplayError::Compile(error),
        SamplingCompileError::Transition(error) => ReplayError::Transition(format!("{error:?}")),
    }
}
