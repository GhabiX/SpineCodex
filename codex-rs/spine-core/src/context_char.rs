use crate::ContextItem;
use crate::ContextLabel;
use crate::Message;
use crate::RawBoundary;
use crate::RolloutEvent;
use crate::TokenUsageSample;
use crate::ToolOutcome;
use crate::ToolUse;
use std::fmt;

/// One character in Spine's agent-neutral context alphabet.
///
/// Every character corresponds to exactly one item in the host's live model
/// context. Zero-width observations are represented by [`SpineSignal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpineChar {
    Message(Message),
    TurnAborted(Message),
    ToolRequest(ToolRequestChar),
    ToolResponse(ToolResponseChar),
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
            Self::Message(message) | Self::TurnAborted(message) => message.boundary,
            Self::ToolRequest(request) => request.boundary,
            Self::ToolResponse(response) => response.boundary,
            Self::Opaque { boundary } | Self::Synthetic { boundary, .. } => *boundary,
        }
    }

    pub const fn width(&self) -> usize {
        1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRequestChar {
    pub boundary: RawBoundary,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResponseChar {
    pub boundary: RawBoundary,
    pub call_id: String,
    pub outcome: ToolOutcome,
    pub output: String,
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
    pending_calls: Option<PendingCalls>,
    last_boundary: Option<RawBoundary>,
    next_cell_id: u64,
}

impl SpineCharParser {
    pub fn stack(&self) -> &ParseStack {
        &self.stack
    }

    pub fn eat(&mut self, character: SpineChar) -> Result<CharParseStep, CharParseError> {
        if let Some(previous) = self.last_boundary
            && character.boundary() < previous
        {
            return Err(CharParseError::NonMonotonicBoundary {
                previous,
                next: character.boundary(),
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

    pub fn install_stack(&mut self, stack: ParseStack) {
        self.last_boundary = stack.cells.last().map(|cell| cell.character.boundary());
        self.next_cell_id = stack
            .cells
            .iter()
            .map(|cell| cell.id.value())
            .max()
            .map_or(0, |id| id.saturating_add(1));
        self.stack = stack;
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
        let mut completed_calls = Vec::new();
        self.last_boundary = Some(character.boundary());

        match character {
            SpineChar::Message(message) => {
                self.require_no_pending_calls(message.boundary)?;
                self.push_cell(SpineChar::Message(message.clone()));
                if message.role == crate::MessageRole::Assistant {
                    self.trailing_assistant.push(message);
                } else {
                    self.flush_trailing_assistant(&mut events);
                    events.push(RolloutEvent::Message(message));
                }
            }
            SpineChar::TurnAborted(message) => {
                if self.pending_calls.take().is_none() {
                    self.flush_trailing_assistant(&mut events);
                }
                self.push_cell(SpineChar::TurnAborted(message.clone()));
                events.push(RolloutEvent::Message(message));
            }
            SpineChar::ToolRequest(request) => {
                self.push_cell(SpineChar::ToolRequest(request.clone()));
                let group = self.pending_calls.get_or_insert_with(|| {
                    let leading_assistant_messages = std::mem::take(&mut self.trailing_assistant);
                    let start = leading_assistant_messages
                        .first()
                        .map_or(request.boundary, |message| message.boundary);
                    PendingCalls {
                        start,
                        leading_assistant_messages,
                        calls: Vec::new(),
                        boundaries: Vec::new(),
                    }
                });
                group.boundaries.push(request.boundary);
                group.calls.push(ToolUse {
                    call_id: request.call_id,
                    name: request.name,
                    arguments: request.arguments,
                    call_ordinal: None,
                    outcome: None,
                    output: None,
                    output_boundary: None,
                });
            }
            SpineChar::ToolResponse(response) => {
                self.push_cell(SpineChar::ToolResponse(response.clone()));
                let group = self.pending_calls.as_mut().ok_or_else(|| {
                    CharParseError::UnmatchedToolResponse {
                        call_id: response.call_id.clone(),
                    }
                })?;
                group.boundaries.push(response.boundary);
                let call = group
                    .calls
                    .iter_mut()
                    .find(|call| call.call_id == response.call_id)
                    .ok_or_else(|| CharParseError::UnmatchedToolResponse {
                        call_id: response.call_id.clone(),
                    })?;
                if call.output.is_some() {
                    return Err(CharParseError::DuplicateToolResponse {
                        call_id: response.call_id,
                    });
                }
                call.outcome = Some(response.outcome);
                call.output = Some(response.output);
                call.output_boundary = Some(response.boundary);
                if group.calls.iter().all(|call| call.output.is_some()) {
                    let group = self.pending_calls.take().ok_or_else(|| {
                        CharParseError::UnmatchedToolResponse {
                            call_id: response.call_id.clone(),
                        }
                    })?;
                    let completed = CompletedCalls::new(group, response.boundary);
                    events.push(RolloutEvent::SourceSpan {
                        span: completed.span,
                        retained_bytes: completed.retained_bytes,
                    });
                    completed_calls.push(completed);
                }
            }
            SpineChar::Opaque { boundary } => {
                self.require_no_pending_calls(boundary)?;
                self.flush_trailing_assistant(&mut events);
                self.push_cell(SpineChar::Opaque { boundary });
                events.push(RolloutEvent::Opaque { boundary });
            }
            SpineChar::Synthetic { boundary, item } => {
                self.require_no_pending_calls(boundary)?;
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
            completed_calls,
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

    fn require_no_pending_calls(&self, boundary: RawBoundary) -> Result<(), CharParseError> {
        if self.pending_calls.is_some() {
            return Err(CharParseError::IncompleteToolGroup { boundary });
        }
        Ok(())
    }

    fn flush_trailing_assistant(&mut self, events: &mut Vec<RolloutEvent>) {
        events.extend(self.trailing_assistant.drain(..).map(RolloutEvent::Message));
    }

    pub(crate) fn pending_boundaries(&self) -> Vec<RawBoundary> {
        self.trailing_assistant
            .iter()
            .map(|message| message.boundary)
            .chain(self.pending_calls.iter().flat_map(PendingCalls::boundaries))
            .collect()
    }

    pub(crate) fn finish_sampling(
        &mut self,
        boundary: RawBoundary,
    ) -> Result<Vec<RolloutEvent>, CharParseError> {
        self.require_no_pending_calls(boundary)?;
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
        self.require_no_pending_calls(boundary)?;
        let mut events = Vec::new();
        self.flush_trailing_assistant(&mut events);
        self.last_boundary = Some(boundary);
        Ok(events)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingCalls {
    start: RawBoundary,
    leading_assistant_messages: Vec<Message>,
    calls: Vec<ToolUse>,
    boundaries: Vec<RawBoundary>,
}

impl PendingCalls {
    fn boundaries(&self) -> impl Iterator<Item = RawBoundary> + '_ {
        self.leading_assistant_messages
            .iter()
            .map(|message| message.boundary)
            .chain(self.boundaries.iter().copied())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletedCalls {
    pub(crate) span: crate::RawSpan,
    pub(crate) calls: Vec<ToolUse>,
    retained_bytes: usize,
}

impl CompletedCalls {
    #[cfg(test)]
    pub(crate) fn from_test(span: crate::RawSpan, calls: Vec<ToolUse>) -> Self {
        Self {
            span,
            retained_bytes: 0,
            calls,
        }
    }

    fn new(pending: PendingCalls, end: RawBoundary) -> Self {
        let retained_bytes = pending
            .leading_assistant_messages
            .iter()
            .map(|message| message.content.len())
            .chain(pending.calls.iter().map(|call| {
                call.call_id
                    .len()
                    .saturating_add(call.name.len())
                    .saturating_add(call.arguments.len())
                    .saturating_add(call.output.as_ref().map_or(0, String::len))
            }))
            .fold(0usize, usize::saturating_add);
        Self {
            span: crate::RawSpan {
                start: pending.start,
                end,
            },
            calls: pending.calls,
            retained_bytes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharParseStep {
    events: Vec<RolloutEvent>,
    completed_calls: Vec<CompletedCalls>,
    pending_boundaries: Vec<RawBoundary>,
    stack_size: usize,
}

impl CharParseStep {
    pub(crate) fn events(&self) -> &[RolloutEvent] {
        &self.events
    }

    pub(crate) fn completed_calls(&self) -> &[CompletedCalls] {
        &self.completed_calls
    }

    pub fn pending_boundaries(&self) -> &[RawBoundary] {
        &self.pending_boundaries
    }

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
    IncompleteToolGroup {
        boundary: RawBoundary,
    },
    UnmatchedToolResponse {
        call_id: String,
    },
    DuplicateToolResponse {
        call_id: String,
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
            Self::IncompleteToolGroup { boundary } => write!(
                formatter,
                "Spine character at boundary {} interrupts an incomplete tool group",
                boundary.0
            ),
            Self::UnmatchedToolResponse { call_id } => {
                write!(formatter, "Spine tool response `{call_id}` has no request")
            }
            Self::DuplicateToolResponse { call_id } => {
                write!(formatter, "Spine tool response `{call_id}` is duplicated")
            }
        }
    }
}

impl std::error::Error for CharParseError {}

#[cfg(test)]
#[path = "context_char_tests.rs"]
mod tests;
