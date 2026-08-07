use crate::ExecutedSpineFact;
use crate::ProjectionDelta;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SpineConfig;
use crate::SpineProjection;
use crate::TrimProjection;
use crate::bootstrap::InitError;
use crate::context_char::CompletedCalls;
use crate::reducer::SpineReducer;
use crate::reducer::TrimReducer;
use crate::reducer::TypedTransitionError;
use std::fmt;

pub const MAX_RAW_EVENT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_VISIBLE_CONTEXT_ITEMS: usize = 4096;
pub const MAX_SYNTHETIC_CONTEXT_BYTES: usize = 1024 * 1024;
pub const MAX_TREE_NODES: usize = 4096;

#[derive(Clone, Debug)]
// Compiler mutations are private and run only on caller-owned disposable candidates.
pub(crate) struct SpineCompiler {
    config: SpineConfig,
    reducer: SpineReducer,
    trim_reducer: Option<TrimReducer>,
    projection: SpineProjection,
}

impl SpineCompiler {
    pub(crate) fn new(config: SpineConfig) -> Result<Self, InitError> {
        config.validate()?;
        let reducer = SpineReducer::new();
        let trim_reducer = config
            .is_enabled(crate::Feature::Trim)
            .then(|| TrimReducer::new(config.trim_threshold_bytes()));
        let projection = reducer.projection();
        Ok(Self {
            config,
            reducer,
            trim_reducer,
            projection,
        })
    }

    pub(crate) fn eat(&mut self, event: RolloutEvent) -> Result<ProjectionDelta, SpineError> {
        validate_event(
            self.projection.last_boundary,
            event.boundary(),
            event.retained_bytes(),
        )?;
        if let Some(trim_reducer) = &mut self.trim_reducer {
            trim_reducer.apply(&event);
        }
        let delta = self.reducer.apply(event);
        validate_projection(&delta.projection)?;
        self.projection = delta.projection.clone();
        Ok(delta)
    }

    pub(crate) fn eat_source(
        &mut self,
        event: RolloutEvent,
    ) -> Result<ProjectionDelta, SamplingCompileError> {
        self.eat(event).map_err(SamplingCompileError::Spine)
    }

    pub(crate) fn observe_completed_calls(&mut self, completed: &CompletedCalls) {
        if let Some(trim_reducer) = &mut self.trim_reducer {
            trim_reducer.apply_completed_calls(completed);
        }
    }

    pub(crate) fn eat_sampling(
        &mut self,
        span: RawSpan,
        retained_bytes: usize,
        completed: &[CompletedCalls],
        facts: &[&ExecutedSpineFact],
        trims: &[(RawBoundary, &ExecutedSpineFact)],
        open_input_tokens: Option<u64>,
    ) -> Result<ProjectionDelta, SamplingCompileError> {
        let event = RolloutEvent::SourceSpan {
            span,
            retained_bytes,
        };
        validate_event(
            self.projection.last_boundary,
            span.end,
            event.retained_bytes(),
        )
        .map_err(SamplingCompileError::Spine)?;
        if let Some(trim_reducer) = &mut self.trim_reducer {
            trim_reducer
                .apply_sampling(completed, trims)
                .map_err(SamplingCompileError::Transition)?;
        }
        let settled_spawn_call_ids = completed
            .iter()
            .flat_map(|completed| completed.calls.iter())
            .filter(|call| call.name == "spine.spawn")
            .map(|call| call.call_id.clone())
            .collect::<Vec<_>>();
        let delta = self
            .reducer
            .apply_sampling(span, facts, &settled_spawn_call_ids, open_input_tokens)
            .map_err(SamplingCompileError::Transition)?;
        validate_projection(&delta.projection).map_err(SamplingCompileError::Spine)?;
        self.projection = delta.projection.clone();
        Ok(delta)
    }

    pub(crate) fn reset(&mut self) {
        self.reducer = SpineReducer::new();
        self.trim_reducer = self
            .config
            .is_enabled(crate::Feature::Trim)
            .then(|| TrimReducer::new(self.config.trim_threshold_bytes()));
        self.projection = self.reducer.projection();
    }

    pub(crate) fn projection(&self) -> &SpineProjection {
        &self.projection
    }

    pub(crate) fn node_context_costs(
        &self,
        context_window_samples: &[crate::ContextWindowSample],
    ) -> std::collections::BTreeMap<crate::NodeId, crate::NodeContextCost> {
        self.reducer.node_context_costs(context_window_samples)
    }

    pub(crate) fn trim_projection(&self) -> Option<&TrimProjection> {
        self.trim_reducer.as_ref().map(TrimReducer::projection)
    }

    pub(crate) fn set_runtime_config(&mut self, config: SpineConfig) -> Result<(), InitError> {
        config.validate()?;
        if config.is_enabled(crate::Feature::Trim) {
            self.trim_reducer
                .get_or_insert_with(|| TrimReducer::new(config.trim_threshold_bytes()));
        } else {
            self.trim_reducer = None;
        }
        self.config = config;
        Ok(())
    }

    pub(crate) fn extend_system_prompt(&self, base: &str) -> String {
        crate::prompt::extend(base.to_owned(), &self.config)
    }
}

fn validate_event(
    previous: Option<RawBoundary>,
    boundary: RawBoundary,
    retained_bytes: usize,
) -> Result<(), SpineError> {
    if retained_bytes > MAX_RAW_EVENT_BYTES {
        return Err(SpineError::ContextLimit {
            kind: "raw event bytes",
            max: MAX_RAW_EVENT_BYTES,
            actual: retained_bytes,
        });
    }
    if let Some(previous) = previous
        && boundary < previous
    {
        return Err(SpineError::NonMonotonicBoundary {
            previous,
            next: boundary,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SamplingCompileError {
    Spine(SpineError),
    Transition(TypedTransitionError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpineError {
    NonMonotonicBoundary {
        previous: RawBoundary,
        next: RawBoundary,
    },
    ContextLimit {
        kind: &'static str,
        max: usize,
        actual: usize,
    },
}

impl fmt::Display for SpineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonMonotonicBoundary { previous, next } => write!(
                formatter,
                "Spine event boundary {} precedes {}",
                next.0, previous.0
            ),
            Self::ContextLimit { kind, max, actual } => {
                write!(formatter, "Spine {kind} is {actual}; maximum is {max}")
            }
        }
    }
}

impl std::error::Error for SpineError {}

fn validate_projection(projection: &SpineProjection) -> Result<(), SpineError> {
    for (kind, actual, max) in [
        (
            "visible context items",
            projection.visible_context.len(),
            MAX_VISIBLE_CONTEXT_ITEMS,
        ),
        ("tree nodes", projection.nodes.len(), MAX_TREE_NODES),
        (
            "synthetic context bytes",
            projection
                .visible_context
                .iter()
                .map(crate::ContextItem::retained_synthetic_bytes)
                .fold(0usize, usize::saturating_add),
            MAX_SYNTHETIC_CONTEXT_BYTES,
        ),
    ] {
        if actual > max {
            return Err(SpineError::ContextLimit { kind, max, actual });
        }
    }
    Ok(())
}
