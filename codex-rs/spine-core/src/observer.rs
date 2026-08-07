use crate::SpineContextProjection;

/// Identifies the committed state transition that produced an observer effect.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpineObserverEffectKind {
    ContextCommitted,
    UsageUpdated,
}

/// A borrowed view of a committed Spine state change.
#[derive(Clone, Copy, Debug)]
pub struct SpineObserverEffect<'a> {
    kind: SpineObserverEffectKind,
    projection: &'a SpineContextProjection,
}

impl<'a> SpineObserverEffect<'a> {
    pub(crate) fn new(
        kind: SpineObserverEffectKind,
        projection: &'a SpineContextProjection,
    ) -> Self {
        Self { kind, projection }
    }

    pub fn kind(self) -> SpineObserverEffectKind {
        self.kind
    }

    pub fn projection(self) -> &'a SpineContextProjection {
        self.projection
    }
}

/// Receives committed Spine state changes.
///
/// Implementations are called synchronously after the context handler commits.
/// They should enqueue host work and return promptly rather than performing
/// blocking I/O in the callback.
pub trait SpineObserverEffectHandler<C> {
    fn handle(&mut self, effect: SpineObserverEffect<'_>, context_handler: &C);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopSpineObserver;

impl<C> SpineObserverEffectHandler<C> for NoopSpineObserver {
    fn handle(&mut self, _effect: SpineObserverEffect<'_>, _context_handler: &C) {}
}
