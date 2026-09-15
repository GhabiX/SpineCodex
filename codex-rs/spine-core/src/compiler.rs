use crate::ExecutedSpineFact;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SpineConfig;
use crate::SpineProjection;
use crate::bootstrap::InitError;
use crate::reducer::SpineReducer;
use crate::reducer::TypedTransitionError;
use std::fmt;

// Fail-closed safety valves for one compiled projection, not append-only source history.
// Synthetic Node Memory / spawn evidence is bounded by the token window and the
// context-plan JSON recipe cap, not a 1MiB text valve.
pub const MAX_RAW_EVENT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_VISIBLE_CONTEXT_ITEMS: usize = 4096;
pub const MAX_TREE_NODES: usize = 4096;

#[derive(Clone, Debug)]
// Compiler mutations are private and run only on caller-owned disposable candidates.
pub(crate) struct SpineCompiler {
    config: SpineConfig,
    reducer: SpineReducer,
    projection: SpineProjection,
}

impl SpineCompiler {
    pub(crate) fn new(config: SpineConfig) -> Result<Self, InitError> {
        config.validate()?;
        let reducer = SpineReducer::new();
        let projection = reducer.projection();
        Ok(Self {
            config,
            reducer,
            projection,
        })
    }

    pub(crate) fn eat(&mut self, event: RolloutEvent) -> Result<(), SpineError> {
        validate_event(
            self.projection.last_boundary,
            event.boundary(),
            event.retained_bytes(),
        )?;
        let projection = self.reducer.apply(event);
        validate_projection(&projection)?;
        self.projection = projection;
        Ok(())
    }

    pub(crate) fn eat_source(&mut self, event: RolloutEvent) -> Result<(), SamplingCompileError> {
        self.eat(event).map_err(SamplingCompileError::Spine)
    }

    pub(crate) fn eat_sampling(
        &mut self,
        span: RawSpan,
        retained_bytes: usize,
        facts: &[&ExecutedSpineFact],
        open_input_tokens: Option<u64>,
    ) -> Result<(), SamplingCompileError> {
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
        let projection = self
            .reducer
            .apply_sampling(span, facts, open_input_tokens)
            .map_err(SamplingCompileError::Transition)?;
        validate_projection(&projection).map_err(SamplingCompileError::Spine)?;
        self.projection = projection;
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.reducer = SpineReducer::new();
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

    pub(crate) fn set_runtime_config(&mut self, config: SpineConfig) -> Result<(), InitError> {
        config.validate()?;
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
    ] {
        if actual > max {
            return Err(SpineError::ContextLimit { kind, max, actual });
        }
    }
    Ok(())
}
