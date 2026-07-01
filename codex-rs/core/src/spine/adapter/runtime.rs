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

pub(crate) async fn read_spine_host_runtime<T>(
    runtime: &tokio::sync::Mutex<SpineHostRuntime>,
    read: impl FnOnce(&SpineHostRuntime) -> T,
) -> T {
    let guard = runtime.lock().await;
    read(&guard)
}

pub(crate) async fn update_spine_host_runtime<T>(
    runtime: &tokio::sync::Mutex<SpineHostRuntime>,
    update: impl FnOnce(&mut SpineHostRuntime) -> T,
) -> T {
    let mut guard = runtime.lock().await;
    update(&mut guard)
}
