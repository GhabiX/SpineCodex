use super::super::runtime;

pub(crate) struct HostEffects {
    pub(in crate::spine) inner: runtime::SpineHostEffects,
}

impl HostEffects {
    pub(crate) fn none() -> Self {
        Self::from_runtime(runtime::SpineHostEffects::none())
    }

    pub(in crate::spine) fn from_runtime(inner: runtime::SpineHostEffects) -> Self {
        Self { inner }
    }

    pub(crate) fn extend(&mut self, effects: Self) {
        self.inner.extend(effects.inner);
    }
}
