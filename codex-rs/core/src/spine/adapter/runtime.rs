pub(crate) type SpineHostRuntime = crate::spine::SpineSessionState;

pub(crate) fn new_spine_host_runtime(spine_jit: bool, spine_trim: bool) -> SpineHostRuntime {
    SpineHostRuntime::new_with_features(spine_jit, spine_trim)
}
