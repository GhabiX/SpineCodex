pub(crate) type SpineHostRuntime = crate::spine::SpineSessionState;
pub(crate) type SpineReplayPlan = crate::spine::bridge::ReplayRuntime;

pub(crate) fn new_spine_host_runtime(
    spine_jit: bool,
    spine_trim: bool,
    spinetree_memory_projection: Option<crate::spine::SpinetreeMemoryProjectionConfig>,
) -> SpineHostRuntime {
    SpineHostRuntime::new_with_features_and_spinetree_projection(
        spine_jit,
        spine_trim,
        spinetree_memory_projection,
    )
}
