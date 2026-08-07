use crate::CellId;
use crate::ContextItem;
use crate::ParseStack;
use crate::TrimEdit;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextLabel {
    UserAnchor(u64),
    ToolOutput(TrimEdit),
    SpawnOutput { succeeded: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextInsert {
    /// Reuses a cell that was present in the context before this event batch.
    ///
    /// `source_index` is always an index in the pre-batch context. A handler
    /// must resolve all existing inserts against that original snapshot rather
    /// than against a context already modified by an earlier event.
    Existing {
        cell_id: CellId,
        source_index: usize,
    },
    Synthetic {
        cell_id: CellId,
        item: ContextItem,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextEvent {
    Tag {
        index: usize,
        label: ContextLabel,
    },
    Splice {
        start: usize,
        delete: usize,
        insert: Vec<ContextInsert>,
    },
}

impl ContextEvent {
    pub fn resulting_size(
        initial_size: usize,
        events: &[Self],
    ) -> Result<usize, ContextEventError> {
        events
            .iter()
            .try_fold(initial_size, |size, event| match event {
                Self::Tag { index, .. } => {
                    if *index >= size {
                        return Err(ContextEventError::IndexOutOfBounds {
                            index: *index,
                            size,
                        });
                    }
                    Ok(size)
                }
                Self::Splice {
                    start,
                    delete,
                    insert,
                } => {
                    let end =
                        start
                            .checked_add(*delete)
                            .ok_or(ContextEventError::RangeOutOfBounds {
                                start: *start,
                                delete: *delete,
                                size,
                            })?;
                    if end > size {
                        return Err(ContextEventError::RangeOutOfBounds {
                            start: *start,
                            delete: *delete,
                            size,
                        });
                    }
                    Ok(size - *delete + insert.len())
                }
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextEventError {
    IndexOutOfBounds {
        index: usize,
        size: usize,
    },
    RangeOutOfBounds {
        start: usize,
        delete: usize,
        size: usize,
    },
}

/// Applies Spine's context events to one authoritative host context.
///
/// `prepare_context` must be side-effect free. Once it succeeds,
/// `commit_context` must be infallible. The runtime verifies event sizes before
/// preparation and checks that the committed context still matches its parse
/// stack. Events are applied in order; `Existing::source_index` values always
/// refer to the history snapshot passed to `prepare_context`, before any event
/// in the batch is applied.
pub trait SpineContextEventHandler {
    type History;
    type PreparedContext;
    type Error: std::error::Error;

    fn context_size(&self, history: &Self::History) -> usize;

    fn prepare_context(
        &self,
        history: &Self::History,
        stack: &ParseStack,
        events: &[ContextEvent],
    ) -> Result<Self::PreparedContext, Self::Error>;

    fn commit_context(&mut self, history: &mut Self::History, prepared: Self::PreparedContext);
}

#[cfg(test)]
#[path = "context_event_tests.rs"]
mod tests;
