use crate::ContextItem;
use crate::ContextLabel;
use crate::Message;
use crate::RawBoundary;
use crate::RolloutEvent;
use crate::TokenUsageSample;
use std::fmt;

/// One character in Spine's agent-neutral context alphabet.
///
/// Every character corresponds to exactly one item in the host's live model
/// context. Zero-width observations are represented by [`SpineSignal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpineChar {
    Message(Message),
    Opaque {
        boundary: RawBoundary,
    },
    Synthetic {
        boundary: RawBoundary,
        item: ContextItem,
    },
}

/// A zero-width observation that changes Spine state without adding a context
/// cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpineSignal {
    Compact { boundary: RawBoundary },
    Usage(TokenUsageSample),
}

/// Historical input used only to recover state absent from the live context.
///
/// Live context items must be passed separately to
/// [`SpineContextRuntime::recover`](crate::SpineContextRuntime::recover).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpineRecoveryInput {
    Char(SpineChar),
    Signal(SpineSignal),
}

impl SpineChar {
    pub fn boundary(&self) -> RawBoundary {
        match self {
            Self::Message(message) => message.boundary,
            Self::Opaque { boundary } | Self::Synthetic { boundary, .. } => *boundary,
        }
    }

    pub const fn width(&self) -> usize {
        1
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseStack {
    cells: Vec<ParseCell>,
}

impl ParseStack {
    pub fn from_cells(cells: Vec<ParseCell>) -> Self {
        Self { cells }
    }

    pub fn cells(&self) -> &[ParseCell] {
        &self.cells
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseCell {
    id: CellId,
    character: SpineChar,
    labels: Vec<ContextLabel>,
}

impl ParseCell {
    pub fn new(id: CellId, character: SpineChar) -> Self {
        Self {
            id,
            character,
            labels: Vec::new(),
        }
    }

    pub fn id(&self) -> CellId {
        self.id
    }

    pub fn character(&self) -> &SpineChar {
        &self.character
    }

    pub fn labels(&self) -> &[ContextLabel] {
        &self.labels
    }

    pub(crate) fn with_labels(mut self, labels: Vec<ContextLabel>) -> Self {
        self.labels = labels;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId(u64);

impl CellId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpineCharParser {
    stack: ParseStack,
    trailing_assistant: Vec<Message>,
    last_boundary: Option<RawBoundary>,
    next_cell_id: u64,
}

impl SpineCharParser {
    pub fn stack(&self) -> &ParseStack {
        &self.stack
    }

    pub fn eat(&mut self, character: SpineChar) -> Result<CharParseStep, CharParseError> {
        let boundary = character.boundary();
        if let Some(previous) = self.last_boundary
            && boundary < previous
        {
            return Err(CharParseError::NonMonotonicBoundary {
                previous,
                next: boundary,
            });
        }

        let mut candidate = self.clone();
        let step = candidate.apply(character)?;
        *self = candidate;
        Ok(step)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn replace_stack(&mut self, stack: ParseStack) {
        self.next_cell_id = stack
            .cells
            .iter()
            .map(|cell| cell.id.value())
            .max()
            .map_or(self.next_cell_id, |id| {
                self.next_cell_id.max(id.saturating_add(1))
            });
        self.stack = stack;
    }

    pub(crate) fn synthetic_cell(&mut self, boundary: RawBoundary, item: ContextItem) -> ParseCell {
        self.new_cell(SpineChar::Synthetic { boundary, item })
    }

    fn apply(&mut self, character: SpineChar) -> Result<CharParseStep, CharParseError> {
        let mut events = Vec::new();
        self.last_boundary = Some(character.boundary());

        match character {
            SpineChar::Message(message) => {
                self.push_cell(SpineChar::Message(message.clone()));
                if message.role == crate::MessageRole::Assistant {
                    self.trailing_assistant.push(message);
                } else {
                    self.flush_trailing_assistant(&mut events);
                    events.push(RolloutEvent::Message(message));
                }
            }
            SpineChar::Opaque { boundary } => {
                self.flush_trailing_assistant(&mut events);
                self.push_cell(SpineChar::Opaque { boundary });
                events.push(RolloutEvent::Opaque { boundary });
            }
            SpineChar::Synthetic { boundary, item } => {
                self.flush_trailing_assistant(&mut events);
                self.push_cell(SpineChar::Synthetic {
                    boundary,
                    item: item.clone(),
                });
                events.push(RolloutEvent::Synthetic { boundary, item });
            }
        }

        Ok(CharParseStep {
            events,
            pending_boundaries: self.pending_boundaries(),
            stack_size: self.stack.len(),
        })
    }

    fn push_cell(&mut self, character: SpineChar) {
        let cell = self.new_cell(character);
        self.stack.cells.push(cell);
    }

    fn new_cell(&mut self, character: SpineChar) -> ParseCell {
        let id = CellId(self.next_cell_id);
        self.next_cell_id = self.next_cell_id.saturating_add(1);
        ParseCell::new(id, character)
    }

    fn flush_trailing_assistant(&mut self, events: &mut Vec<RolloutEvent>) {
        events.extend(self.trailing_assistant.drain(..).map(RolloutEvent::Message));
    }

    pub(crate) fn pending_boundaries(&self) -> Vec<RawBoundary> {
        self.trailing_assistant
            .iter()
            .map(|message| message.boundary)
            .collect()
    }

    pub(crate) fn finish_sampling(
        &mut self,
        _boundary: RawBoundary,
    ) -> Result<Vec<RolloutEvent>, CharParseError> {
        let mut events = Vec::new();
        self.flush_trailing_assistant(&mut events);
        Ok(events)
    }

    pub(crate) fn finish_epoch(
        &mut self,
        boundary: RawBoundary,
    ) -> Result<Vec<RolloutEvent>, CharParseError> {
        if let Some(previous) = self.last_boundary
            && boundary < previous
        {
            return Err(CharParseError::NonMonotonicBoundary {
                previous,
                next: boundary,
            });
        }
        let mut events = Vec::new();
        self.flush_trailing_assistant(&mut events);
        self.last_boundary = Some(boundary);
        Ok(events)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharParseStep {
    events: Vec<RolloutEvent>,
    pending_boundaries: Vec<RawBoundary>,
    stack_size: usize,
}

impl CharParseStep {
    pub(crate) fn events(&self) -> &[RolloutEvent] {
        &self.events
    }

    pub fn pending_boundaries(&self) -> &[RawBoundary] {
        &self.pending_boundaries
    }

    #[cfg(test)]
    pub fn stack_size(&self) -> usize {
        self.stack_size
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CharParseError {
    NonMonotonicBoundary {
        previous: RawBoundary,
        next: RawBoundary,
    },
}

impl fmt::Display for CharParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonMonotonicBoundary { previous, next } => write!(
                formatter,
                "Spine character boundary {} precedes {}",
                next.0, previous.0
            ),
        }
    }
}

impl std::error::Error for CharParseError {}
