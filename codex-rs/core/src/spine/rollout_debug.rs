use std::collections::BTreeMap;

use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellStatus;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::WebSearchAction;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::TokenUsage;
use serde::Serialize;
use serde::de::DeserializeSeed;
use serde::de::IgnoredAny;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde_json::Map;
use serde_json::Value;
use spine_core::SpawnTask;
use spine_core::TrimRequest;
use thiserror::Error;

use crate::tools::code_mode::is_exec_tool_name;

/// A non-replayable, content-erased observation of one native rollout record.
///
/// The distinct `record_type` tag deliberately prevents this type from being
/// accepted as a native `RolloutLine`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub(crate) enum DebugRolloutRecord {
    MalformedRedacted {
        scope: DebugPlaceholderScope,
    },
    UnknownRedacted {
        scope: DebugPlaceholderScope,
    },
    OversizedRedacted,
    SessionMeta {
        session_id: u64,
        thread_id: u64,
        forked_from_thread_id: Option<u64>,
        parent_thread_id: Option<u64>,
        context_window_id: Option<u64>,
        dynamic_tool_count: usize,
        capability_root_count: usize,
    },
    ResponseItem {
        item: DebugResponseItem,
    },
    InterAgentCommunication {
        id: Option<u64>,
        author: u64,
        recipient: u64,
        other_recipients: Vec<u64>,
        trigger_turn: bool,
        encrypted: bool,
        turn_id: Option<u64>,
    },
    InterAgentCommunicationMetadata {
        trigger_turn: bool,
    },
    Compacted {
        replacement_history: Option<Vec<DebugResponseItem>>,
        window_number: Option<u64>,
        first_window_id: Option<u64>,
        previous_window_id: Option<u64>,
        window_id: Option<u64>,
    },
    TurnContext {
        turn_id: Option<u64>,
        workspace_root_count: Option<usize>,
        realtime_active: Option<bool>,
    },
    WorldState {
        full: bool,
    },
    Event {
        event: DebugEvent,
    },
}

impl DebugRolloutRecord {
    pub(crate) fn oversized() -> Self {
        Self::OversizedRedacted
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugPlaceholderScope {
    Line,
    TopLevel,
    ResponseItem,
    Event,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DebugResponseItem {
    AdditionalTools {
        id: Option<u64>,
        tool_count: usize,
    },
    Message {
        id: Option<u64>,
        role: DebugMessageRole,
        phase: Option<DebugMessagePhase>,
        content: Vec<DebugContentKind>,
        turn_id: Option<u64>,
    },
    AgentMessage {
        id: Option<u64>,
        content: Vec<DebugAgentContentKind>,
        turn_id: Option<u64>,
    },
    Reasoning {
        id: Option<u64>,
        summary_count: usize,
        content: Option<Vec<DebugReasoningContentKind>>,
        encrypted: bool,
        turn_id: Option<u64>,
    },
    LocalShellCall {
        id: Option<u64>,
        call_id: Option<u64>,
        status: DebugShellStatus,
        command_part_count: usize,
        timeout_present: bool,
        working_directory_present: bool,
        environment_entry_count: Option<usize>,
        user_present: bool,
        turn_id: Option<u64>,
    },
    FunctionCall {
        id: Option<u64>,
        call_id: u64,
        tool: DebugToolKind,
        arguments: DebugToolArguments,
        turn_id: Option<u64>,
    },
    ToolSearchCall {
        id: Option<u64>,
        call_id: Option<u64>,
        status_present: bool,
        arguments: DebugJsonValueShape,
        turn_id: Option<u64>,
    },
    FunctionCallOutput {
        id: Option<u64>,
        call_id: u64,
        output: DebugToolOutput,
        turn_id: Option<u64>,
    },
    CustomToolCall {
        id: Option<u64>,
        call_id: u64,
        tool: DebugToolKind,
        status_present: bool,
        arguments: DebugToolArguments,
        turn_id: Option<u64>,
    },
    CustomToolCallOutput {
        id: Option<u64>,
        call_id: u64,
        output_name: DebugOutputName,
        output: DebugToolOutput,
        turn_id: Option<u64>,
    },
    ToolSearchOutput {
        id: Option<u64>,
        call_id: Option<u64>,
        tool_count: usize,
        turn_id: Option<u64>,
    },
    WebSearchCall {
        id: Option<u64>,
        status_present: bool,
        action: Option<DebugWebAction>,
        turn_id: Option<u64>,
    },
    ImageGenerationCall {
        id: Option<u64>,
        revised_prompt_present: bool,
        result_present: bool,
        turn_id: Option<u64>,
    },
    Compaction {
        id: Option<u64>,
        turn_id: Option<u64>,
    },
    CompactionTrigger,
    ContextCompaction {
        id: Option<u64>,
        encrypted: bool,
        turn_id: Option<u64>,
    },
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugMessageRole {
    User,
    Assistant,
    Developer,
    System,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugMessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugContentKind {
    InputText,
    InputImage,
    InputAudio,
    OutputText,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugAgentContentKind {
    InputText,
    EncryptedContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugReasoningContentKind {
    ReasoningText,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugShellStatus {
    Completed,
    InProgress,
    Incomplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugWebAction {
    Search { query_count: usize },
    OpenPage,
    FindInPage,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugToolKind {
    SpineOpen,
    SpineClose,
    SpineNext,
    SpineTrim,
    SpineSpawn,
    CodeMode,
    Shell,
    Patch,
    Web,
    Mcp,
    Image,
    Other,
}

impl DebugToolKind {
    fn is_spine(self) -> bool {
        matches!(
            self,
            Self::SpineOpen
                | Self::SpineClose
                | Self::SpineNext
                | Self::SpineTrim
                | Self::SpineSpawn
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugOutputName {
    Absent,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DebugToolArguments {
    Redacted {
        shape: DebugJsonValueShape,
    },
    Open {
        object: DebugObjectState,
        summary: DebugStringShape,
        unknown_fields: bool,
        valid: bool,
    },
    Close {
        object: DebugObjectState,
        memory: DebugStringShape,
        unknown_fields: bool,
        valid: bool,
    },
    Next {
        object: DebugObjectState,
        summary: DebugStringShape,
        memory: DebugStringShape,
        unknown_fields: bool,
        valid: bool,
    },
    Trim {
        object: DebugObjectState,
        trim_id: DebugMappedString,
        op: DebugTrimOpShape,
        head: DebugUnsignedShape,
        tail: DebugUnsignedShape,
        anchor: DebugStringShape,
        preceding: DebugUnsignedShape,
        following: DebugUnsignedShape,
        unknown_fields: bool,
        valid: bool,
    },
    Spawn {
        object: DebugObjectState,
        tasks: DebugCollection<DebugSpawnTaskShape>,
        unknown_fields: bool,
        valid: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugSpawnTaskShape {
    object: DebugObjectState,
    summary: DebugStringShape,
    prompt: DebugStringShape,
    unknown_fields: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugObjectState {
    Object,
    NonObject,
    MalformedJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugStringShape {
    Missing,
    Null,
    WrongType,
    Empty,
    Whitespace,
    NonEmpty,
}

impl DebugStringShape {
    fn is_non_empty(self) -> bool {
        self == Self::NonEmpty
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugMappedString {
    shape: DebugStringShape,
    local_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugTrimOpShape {
    Missing,
    Null,
    WrongType,
    Empty,
    Whitespace,
    Snip,
    Slice,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "shape", content = "value", rename_all = "snake_case")]
pub(crate) enum DebugUnsignedShape {
    Missing,
    Null,
    Unsigned(u64),
    Negative,
    OtherNumber,
    WrongType,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugCollection<T> {
    shape: DebugCollectionShape,
    items: Vec<T>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugCollectionShape {
    Missing,
    Null,
    WrongType,
    Array,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "shape", content = "count", rename_all = "snake_case")]
pub(crate) enum DebugJsonValueShape {
    Null,
    Bool,
    Number,
    String,
    Array(usize),
    Object(usize),
    MalformedJson,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DebugToolOutput {
    Redacted {
        body: DebugOutputBodyShape,
    },
    Control {
        exact_success_carrier: bool,
        body: DebugOutputBodyShape,
    },
    Spawn {
        receipt: DebugSpawnReceiptShape,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DebugOutputBodyShape {
    Text { bytes: usize },
    ContentItems { items: Vec<DebugOutputContentKind> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugOutputContentKind {
    InputText,
    InputImage,
    InputAudio,
    EncryptedContent,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugSpawnReceiptShape {
    object: DebugObjectState,
    schema: DebugSchemaShape,
    results: DebugCollection<DebugSpawnResultShape>,
    unknown_fields: bool,
    valid_for_request: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugSpawnResultShape {
    object: DebugObjectState,
    ordinal: DebugUnsignedShape,
    outcome: DebugSpawnOutcomeShape,
    memory_body: DebugStringShape,
    diagnostic: DebugStringShape,
    execution_ref: DebugMappedString,
    unknown_fields: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugSpawnOutcomeShape {
    Missing,
    Null,
    WrongType,
    Empty,
    Whitespace,
    Completed,
    Errored,
    Aborted,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugSchemaShape {
    Missing,
    Null,
    WrongType,
    Empty,
    Whitespace,
    ExactV1,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DebugEvent {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_usage: Option<DebugTokenUsageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rolled_back_turns: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DebugTokenUsageInfo {
    total: DebugTokenUsage,
    last: DebugTokenUsage,
    model_context_window: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DebugTokenUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum IdNamespace {
    Item,
    Call,
    Turn,
    Window,
    Thread,
    Session,
    Agent,
    Execution,
    Trim,
}

#[derive(Clone, Copy, Debug)]
struct SpawnRequestSignature {
    task_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct DebugCallState {
    tool: DebugToolKind,
    spawn_request: Option<SpawnRequestSignature>,
}

// These limits are deliberately above the accepted 35,612-record corpus while
// keeping package-local diagnostic state and structural work finite.
const DEFAULT_MAX_TRACKED_ID_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_TRACKED_ID_ENTRIES: usize = 131_072;
const DEFAULT_MAX_PENDING_CALLS: usize = 32_768;
const DEFAULT_MAX_JSON_NODES_PER_RECORD: usize = 65_536;
const DEFAULT_MAX_JSON_NODES_PER_PACKAGE: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct RedactorLimits {
    max_tracked_id_bytes: usize,
    max_tracked_id_entries: usize,
    max_pending_calls: usize,
    max_json_nodes_per_record: usize,
    max_json_nodes_per_package: usize,
}

impl Default for RedactorLimits {
    fn default() -> Self {
        Self {
            max_tracked_id_bytes: DEFAULT_MAX_TRACKED_ID_BYTES,
            max_tracked_id_entries: DEFAULT_MAX_TRACKED_ID_ENTRIES,
            max_pending_calls: DEFAULT_MAX_PENDING_CALLS,
            max_json_nodes_per_record: DEFAULT_MAX_JSON_NODES_PER_RECORD,
            max_json_nodes_per_package: DEFAULT_MAX_JSON_NODES_PER_PACKAGE,
        }
    }
}

struct JsonNodeBudgetSeed<'a> {
    remaining: &'a mut usize,
    exceeded: &'a mut bool,
}

impl JsonNodeBudgetSeed<'_> {
    fn consume<E: serde::de::Error>(&mut self) -> Result<(), E> {
        let Some(next) = self.remaining.checked_sub(1) else {
            *self.exceeded = true;
            return Err(E::custom("rollout debug JSON node limit exceeded"));
        };
        *self.remaining = next;
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for JsonNodeBudgetSeed<'_> {
    type Value = ();

    fn deserialize<D>(mut self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        self.consume::<D::Error>()?;
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonNodeBudgetSeed<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value within the rollout debug structural budget")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        JsonNodeBudgetSeed {
            remaining: self.remaining,
            exceeded: self.exceeded,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(JsonNodeBudgetSeed {
                remaining: &mut *self.remaining,
                exceeded: &mut *self.exceeded,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(mut self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_key::<IgnoredAny>()?.is_some() {
            self.consume::<A::Error>()?;
            map.next_value_seed(JsonNodeBudgetSeed {
                remaining: &mut *self.remaining,
                exceeded: &mut *self.exceeded,
            })?;
        }
        Ok(())
    }
}

/// The diagnostic package cannot be built without exceeding a closed safety boundary.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RolloutDebugRedactorError {
    #[error("rollout debug redactor state limit exceeded")]
    ResourceLimitExceeded,
    #[error("rollout debug call identifier is ambiguous while still outstanding")]
    AmbiguousCallId,
}

/// Stateful only for package-local identifier equality and request/output pairing.
/// Given the same record order, its output is deterministic.
#[derive(Default)]
pub struct RolloutDebugRedactor {
    ids: BTreeMap<IdNamespace, BTreeMap<String, u64>>,
    calls: BTreeMap<u64, DebugCallState>,
    tracked_id_bytes: usize,
    tracked_id_entries: usize,
    record_json_nodes: usize,
    package_json_nodes: usize,
    limits: RedactorLimits,
    failure: Option<RolloutDebugRedactorError>,
}

impl RolloutDebugRedactor {
    /// Reserve the package-local thread identifier used by the bundle manifest.
    pub fn register_thread_id(
        &mut self,
        thread_id: &str,
    ) -> Result<u64, RolloutDebugRedactorError> {
        self.ensure_within_limits()?;
        let local_id = self.local_id(IdNamespace::Thread, thread_id);
        self.ensure_within_limits()?;
        Ok(local_id)
    }

    /// Redact one native JSONL record into a non-replayable JSON value.
    pub fn redact_json_line_to_value(
        &mut self,
        line: &[u8],
    ) -> Result<Value, RolloutDebugRedactorError> {
        self.ensure_within_limits()?;
        self.begin_record();
        let syntactically_valid = self.preflight_json(line);
        self.ensure_within_limits()?;
        let record = if syntactically_valid {
            self.redact_preflighted_json_line(line)
        } else {
            DebugRolloutRecord::MalformedRedacted {
                scope: DebugPlaceholderScope::Line,
            }
        };
        self.ensure_within_limits()?;
        Ok(serde_json::to_value(record).expect("rollout debug records must serialize"))
    }

    /// Return the positional placeholder for a source line that exceeded the
    /// collector's retained-byte bound.
    pub fn oversized_value() -> Value {
        serde_json::to_value(DebugRolloutRecord::oversized())
            .expect("rollout debug placeholders must serialize")
    }

    #[cfg(test)]
    fn with_limits(
        max_tracked_id_bytes: usize,
        max_tracked_id_entries: usize,
        max_pending_calls: usize,
    ) -> Self {
        Self {
            limits: RedactorLimits {
                max_tracked_id_bytes,
                max_tracked_id_entries,
                max_pending_calls,
                ..RedactorLimits::default()
            },
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_json_node_limits(
        max_json_nodes_per_record: usize,
        max_json_nodes_per_package: usize,
    ) -> Self {
        Self {
            limits: RedactorLimits {
                max_json_nodes_per_record,
                max_json_nodes_per_package,
                ..RedactorLimits::default()
            },
            ..Self::default()
        }
    }

    fn ensure_within_limits(&self) -> Result<(), RolloutDebugRedactorError> {
        self.failure.map_or(Ok(()), Err)
    }

    fn mark_resource_limit_exceeded(&mut self) {
        self.failure
            .get_or_insert(RolloutDebugRedactorError::ResourceLimitExceeded);
    }

    fn mark_ambiguous_call_id(&mut self) {
        self.failure
            .get_or_insert(RolloutDebugRedactorError::AmbiguousCallId);
    }

    fn begin_record(&mut self) {
        self.record_json_nodes = 0;
    }

    /// Validate and count one JSON value before constructing its `Value` tree.
    ///
    /// Object keys count as nodes as well as values. Nested JSON strings used
    /// by tool arguments and carriers are preflighted separately, so a compact
    /// source line cannot expand into an unbounded diagnostic structure.
    fn preflight_json(&mut self, input: &[u8]) -> bool {
        let record_remaining = self
            .limits
            .max_json_nodes_per_record
            .saturating_sub(self.record_json_nodes);
        let package_remaining = self
            .limits
            .max_json_nodes_per_package
            .saturating_sub(self.package_json_nodes);
        let initial_remaining = record_remaining.min(package_remaining);
        let mut remaining = initial_remaining;
        let mut exceeded = false;
        let mut deserializer = serde_json::Deserializer::from_slice(input);
        let result = JsonNodeBudgetSeed {
            remaining: &mut remaining,
            exceeded: &mut exceeded,
        }
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end());
        let consumed = initial_remaining.saturating_sub(remaining);
        self.record_json_nodes = self.record_json_nodes.saturating_add(consumed);
        self.package_json_nodes = self.package_json_nodes.saturating_add(consumed);
        if exceeded {
            self.mark_resource_limit_exceeded();
        }
        result.is_ok() && !exceeded
    }

    #[cfg(test)]
    pub(crate) fn redact_json_line(&mut self, line: &[u8]) -> DebugRolloutRecord {
        self.begin_record();
        if !self.preflight_json(line) {
            return DebugRolloutRecord::MalformedRedacted {
                scope: DebugPlaceholderScope::Line,
            };
        }
        self.redact_preflighted_json_line(line)
    }

    fn redact_preflighted_json_line(&mut self, line: &[u8]) -> DebugRolloutRecord {
        let Ok(raw) = serde_json::from_slice::<Value>(line) else {
            return DebugRolloutRecord::MalformedRedacted {
                scope: DebugPlaceholderScope::Line,
            };
        };
        self.redact_value(raw)
    }

    pub(crate) fn redact_value(&mut self, raw: Value) -> DebugRolloutRecord {
        let Some(raw_object) = raw.as_object() else {
            return DebugRolloutRecord::MalformedRedacted {
                scope: DebugPlaceholderScope::TopLevel,
            };
        };
        let top_type = raw_object
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string);
        let raw_payload = raw_object.get("payload").cloned();
        if top_type
            .as_deref()
            .is_some_and(|kind| !is_known_rollout_item_type(kind))
        {
            return DebugRolloutRecord::UnknownRedacted {
                scope: DebugPlaceholderScope::TopLevel,
            };
        }
        if top_type.as_deref() == Some("response_item") {
            let response_type = raw_payload
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str);
            if response_type.is_some_and(|kind| !is_known_response_item_type(kind)) {
                return DebugRolloutRecord::UnknownRedacted {
                    scope: DebugPlaceholderScope::ResponseItem,
                };
            }
        }
        if top_type.as_deref() == Some("event_msg") {
            let event_type = raw_payload
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str);
            if event_type.is_some_and(|kind| !is_known_event_type(kind)) {
                return DebugRolloutRecord::UnknownRedacted {
                    scope: DebugPlaceholderScope::Event,
                };
            }
        }

        let Ok(line) = serde_json::from_value::<RolloutLine>(raw) else {
            let scope = match top_type.as_deref() {
                Some("response_item") => DebugPlaceholderScope::ResponseItem,
                Some("event_msg") => DebugPlaceholderScope::Event,
                _ => DebugPlaceholderScope::TopLevel,
            };
            return DebugRolloutRecord::MalformedRedacted { scope };
        };
        self.redact_rollout_item(line.item, raw_payload.as_ref())
    }

    fn redact_rollout_item(
        &mut self,
        item: RolloutItem,
        raw_payload: Option<&Value>,
    ) -> DebugRolloutRecord {
        match item {
            RolloutItem::SessionMeta(meta_line) => {
                let meta = meta_line.meta;
                DebugRolloutRecord::SessionMeta {
                    session_id: self.local_id(IdNamespace::Session, &meta.session_id.to_string()),
                    thread_id: self.local_id(IdNamespace::Thread, &meta.id.to_string()),
                    forked_from_thread_id: meta
                        .forked_from_id
                        .map(|id| self.local_id(IdNamespace::Thread, &id.to_string())),
                    parent_thread_id: meta
                        .parent_thread_id
                        .map(|id| self.local_id(IdNamespace::Thread, &id.to_string())),
                    context_window_id: meta
                        .context_window
                        .map(|window| self.local_id(IdNamespace::Window, &window.window_id)),
                    dynamic_tool_count: meta.dynamic_tools.as_ref().map_or(0, Vec::len),
                    capability_root_count: meta.selected_capability_roots.len(),
                }
            }
            RolloutItem::ResponseItem(item) => DebugRolloutRecord::ResponseItem {
                item: self.redact_response_item(item, raw_payload),
            },
            RolloutItem::InterAgentCommunication(item) => {
                DebugRolloutRecord::InterAgentCommunication {
                    id: self.optional_local_id(IdNamespace::Item, item.id.as_deref()),
                    author: self.local_id(IdNamespace::Agent, item.author.as_ref()),
                    recipient: self.local_id(IdNamespace::Agent, item.recipient.as_ref()),
                    other_recipients: item
                        .other_recipients
                        .iter()
                        .map(|recipient| self.local_id(IdNamespace::Agent, recipient.as_ref()))
                        .collect(),
                    trigger_turn: item.trigger_turn,
                    encrypted: item.encrypted_content.is_some(),
                    turn_id: self.turn_id(item.internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            RolloutItem::InterAgentCommunicationMetadata { trigger_turn } => {
                DebugRolloutRecord::InterAgentCommunicationMetadata { trigger_turn }
            }
            RolloutItem::Compacted(item) => {
                let raw_replacements = raw_payload
                    .and_then(Value::as_object)
                    .and_then(|payload| payload.get("replacement_history"))
                    .and_then(Value::as_array);
                let replacement_history = item.replacement_history.map(|items| {
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(index, item)| {
                            self.redact_response_item(
                                item,
                                raw_replacements.and_then(|items| items.get(index)),
                            )
                        })
                        .collect()
                });
                DebugRolloutRecord::Compacted {
                    replacement_history,
                    window_number: item.window_number,
                    first_window_id: self
                        .optional_local_id(IdNamespace::Window, item.first_window_id.as_deref()),
                    previous_window_id: self
                        .optional_local_id(IdNamespace::Window, item.previous_window_id.as_deref()),
                    window_id: self
                        .optional_local_id(IdNamespace::Window, item.window_id.as_deref()),
                }
            }
            RolloutItem::TurnContext(item) => DebugRolloutRecord::TurnContext {
                turn_id: self.optional_local_id(IdNamespace::Turn, item.turn_id.as_deref()),
                workspace_root_count: item.workspace_roots.as_ref().map(Vec::len),
                realtime_active: item.realtime_active,
            },
            RolloutItem::WorldState(item) => DebugRolloutRecord::WorldState { full: item.full },
            RolloutItem::EventMsg(event) => DebugRolloutRecord::Event {
                event: redact_event(event),
            },
            RolloutItem::SpineSamplingStarted(_) | RolloutItem::SpineTransition(_) => {
                DebugRolloutRecord::UnknownRedacted {
                    scope: DebugPlaceholderScope::TopLevel,
                }
            }
        }
    }

    fn redact_response_item(
        &mut self,
        item: ResponseItem,
        raw: Option<&Value>,
    ) -> DebugResponseItem {
        match item {
            ResponseItem::AdditionalTools { id, tools, .. } => DebugResponseItem::AdditionalTools {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                tool_count: tools.len(),
            },
            ResponseItem::Message {
                id,
                role,
                content,
                phase,
                internal_chat_message_metadata_passthrough,
            } => DebugResponseItem::Message {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                role: debug_message_role(&role),
                phase: phase.map(debug_message_phase),
                content: content.into_iter().map(debug_content_kind).collect(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::AgentMessage {
                id,
                content,
                internal_chat_message_metadata_passthrough,
                ..
            } => DebugResponseItem::AgentMessage {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                content: content.into_iter().map(debug_agent_content_kind).collect(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
                internal_chat_message_metadata_passthrough,
            } => DebugResponseItem::Reasoning {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                summary_count: summary.len(),
                content: content.map(|items| {
                    items
                        .into_iter()
                        .map(debug_reasoning_content_kind)
                        .collect()
                }),
                encrypted: encrypted_content.is_some(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::LocalShellCall {
                id,
                call_id,
                status,
                action,
                internal_chat_message_metadata_passthrough,
            } => {
                let LocalShellAction::Exec(action) = action;
                DebugResponseItem::LocalShellCall {
                    id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                    call_id: self.optional_local_id(IdNamespace::Call, call_id.as_deref()),
                    status: debug_shell_status(status),
                    command_part_count: action.command.len(),
                    timeout_present: action.timeout_ms.is_some(),
                    working_directory_present: action.working_directory.is_some(),
                    environment_entry_count: action
                        .env
                        .as_ref()
                        .map(std::collections::HashMap::len),
                    user_present: action.user.is_some(),
                    turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            ResponseItem::FunctionCall {
                id,
                name,
                namespace,
                arguments,
                encrypted_function_args: _,
                call_id,
                internal_chat_message_metadata_passthrough,
            } => {
                let tool = classify_tool(namespace.as_deref(), &name);
                let debug_arguments = self.redact_tool_arguments(tool, &arguments);
                let local_call_id = self.local_id(IdNamespace::Call, &call_id);
                self.remember_call(local_call_id, tool, &arguments);
                DebugResponseItem::FunctionCall {
                    id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                    call_id: local_call_id,
                    tool,
                    arguments: debug_arguments,
                    turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            ResponseItem::ToolSearchCall {
                id,
                call_id,
                status,
                arguments,
                internal_chat_message_metadata_passthrough,
                ..
            } => DebugResponseItem::ToolSearchCall {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                call_id: self.optional_local_id(IdNamespace::Call, call_id.as_deref()),
                status_present: status.is_some(),
                arguments: json_value_shape(&arguments),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::FunctionCallOutput {
                id,
                call_id,
                output,
                internal_chat_message_metadata_passthrough,
            } => {
                let local_call_id = self.local_id(IdNamespace::Call, &call_id);
                let state = self.calls.remove(&local_call_id);
                let debug_output =
                    self.redact_tool_output(state, &output.body, raw_output_value(raw));
                DebugResponseItem::FunctionCallOutput {
                    id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                    call_id: local_call_id,
                    output: debug_output,
                    turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            ResponseItem::CustomToolCall {
                id,
                status,
                call_id,
                name,
                namespace,
                input,
                internal_chat_message_metadata_passthrough,
            } => {
                let tool = classify_tool(namespace.as_deref(), &name);
                let debug_arguments = self.redact_tool_arguments(tool, &input);
                let local_call_id = self.local_id(IdNamespace::Call, &call_id);
                self.remember_call(local_call_id, tool, &input);
                DebugResponseItem::CustomToolCall {
                    id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                    call_id: local_call_id,
                    tool,
                    status_present: status.is_some(),
                    arguments: debug_arguments,
                    turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            ResponseItem::CustomToolCallOutput {
                id,
                call_id,
                name,
                output,
                internal_chat_message_metadata_passthrough,
            } => {
                let local_call_id = self.local_id(IdNamespace::Call, &call_id);
                let state = self.calls.remove(&local_call_id);
                let output_name = match name.as_deref() {
                    None => DebugOutputName::Absent,
                    Some(_) => DebugOutputName::Other,
                };
                let debug_output =
                    self.redact_tool_output(state, &output.body, raw_output_value(raw));
                DebugResponseItem::CustomToolCallOutput {
                    id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                    call_id: local_call_id,
                    output_name,
                    output: debug_output,
                    turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
                }
            }
            ResponseItem::ToolSearchOutput {
                id,
                call_id,
                tools,
                internal_chat_message_metadata_passthrough,
                ..
            } => DebugResponseItem::ToolSearchOutput {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                call_id: self.optional_local_id(IdNamespace::Call, call_id.as_deref()),
                tool_count: tools.len(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::WebSearchCall {
                id,
                status,
                action,
                internal_chat_message_metadata_passthrough,
            } => DebugResponseItem::WebSearchCall {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                status_present: status.is_some(),
                action: action.map(debug_web_action),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::ImageGenerationCall {
                id,
                revised_prompt,
                result,
                internal_chat_message_metadata_passthrough,
                ..
            } => DebugResponseItem::ImageGenerationCall {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                revised_prompt_present: revised_prompt.is_some(),
                result_present: !result.is_empty(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::Compaction {
                id,
                internal_chat_message_metadata_passthrough,
                ..
            } => DebugResponseItem::Compaction {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::CompactionTrigger {} => DebugResponseItem::CompactionTrigger,
            ResponseItem::ContextCompaction {
                id,
                encrypted_content,
                internal_chat_message_metadata_passthrough,
            } => DebugResponseItem::ContextCompaction {
                id: self.optional_local_id(IdNamespace::Item, id.as_deref()),
                encrypted: encrypted_content.is_some(),
                turn_id: self.turn_id(internal_chat_message_metadata_passthrough.as_ref()),
            },
            ResponseItem::Other => DebugResponseItem::Other,
        }
    }

    fn remember_call(&mut self, local_call_id: u64, tool: DebugToolKind, arguments: &str) {
        if self.failure.is_some() {
            return;
        }
        if self.calls.contains_key(&local_call_id) {
            self.mark_ambiguous_call_id();
            return;
        }
        let spawn_request = (tool == DebugToolKind::SpineSpawn)
            .then(|| super::spawn::parse_tasks(arguments).ok())
            .flatten()
            .map(|tasks| SpawnRequestSignature {
                task_count: tasks.len(),
            });
        if self.calls.len() >= self.limits.max_pending_calls {
            self.mark_resource_limit_exceeded();
            return;
        }
        self.calls.insert(
            local_call_id,
            DebugCallState {
                tool,
                spawn_request,
            },
        );
    }

    fn redact_tool_arguments(
        &mut self,
        tool: DebugToolKind,
        arguments: &str,
    ) -> DebugToolArguments {
        if self.failure.is_some() {
            return DebugToolArguments::Redacted {
                shape: DebugJsonValueShape::MalformedJson,
            };
        }
        let valid_json = self.preflight_json(arguments.as_bytes());
        if !tool.is_spine() {
            return DebugToolArguments::Redacted {
                shape: if valid_json {
                    json_text_shape(arguments)
                } else {
                    DebugJsonValueShape::MalformedJson
                },
            };
        }
        let parsed = valid_json
            .then(|| serde_json::from_str::<Value>(arguments))
            .transpose()
            .ok()
            .flatten();
        let object = match &parsed {
            Some(Value::Object(_)) => DebugObjectState::Object,
            Some(_) => DebugObjectState::NonObject,
            None => DebugObjectState::MalformedJson,
        };
        let fields = parsed.as_ref().and_then(Value::as_object);
        match tool {
            DebugToolKind::SpineOpen => {
                let summary = string_shape(field(fields, "summary"));
                DebugToolArguments::Open {
                    object,
                    summary,
                    unknown_fields: has_unknown_fields(fields, &["summary"]),
                    valid: object == DebugObjectState::Object
                        && summary.is_non_empty()
                        && !has_unknown_fields(fields, &["summary"]),
                }
            }
            DebugToolKind::SpineClose => {
                let memory = string_shape(field(fields, "memory"));
                DebugToolArguments::Close {
                    object,
                    memory,
                    unknown_fields: has_unknown_fields(fields, &["memory"]),
                    valid: object == DebugObjectState::Object
                        && memory.is_non_empty()
                        && !has_unknown_fields(fields, &["memory"]),
                }
            }
            DebugToolKind::SpineNext => {
                let summary = string_shape(field(fields, "summary"));
                let memory = string_shape(field(fields, "memory"));
                DebugToolArguments::Next {
                    object,
                    summary,
                    memory,
                    unknown_fields: has_unknown_fields(fields, &["summary", "memory"]),
                    valid: object == DebugObjectState::Object
                        && summary.is_non_empty()
                        && memory.is_non_empty()
                        && !has_unknown_fields(fields, &["summary", "memory"]),
                }
            }
            DebugToolKind::SpineTrim => DebugToolArguments::Trim {
                object,
                trim_id: self.mapped_string(IdNamespace::Trim, field(fields, "TRIM_ID")),
                op: trim_op_shape(field(fields, "op")),
                head: unsigned_shape(field(fields, "head")),
                tail: unsigned_shape(field(fields, "tail")),
                anchor: string_shape(field(fields, "anchor")),
                preceding: unsigned_shape(field(fields, "preceding")),
                following: unsigned_shape(field(fields, "following")),
                unknown_fields: has_unknown_fields(
                    fields,
                    &[
                        "TRIM_ID",
                        "op",
                        "head",
                        "tail",
                        "anchor",
                        "preceding",
                        "following",
                    ],
                ),
                valid: valid_json && TrimRequest::parse(arguments).is_ok(),
            },
            DebugToolKind::SpineSpawn => DebugToolArguments::Spawn {
                object,
                tasks: inspect_spawn_tasks(field(fields, "tasks")),
                unknown_fields: has_unknown_fields(fields, &["tasks"]),
                valid: valid_json && super::spawn::parse_tasks(arguments).is_ok(),
            },
            DebugToolKind::CodeMode
            | DebugToolKind::Shell
            | DebugToolKind::Patch
            | DebugToolKind::Web
            | DebugToolKind::Mcp
            | DebugToolKind::Image
            | DebugToolKind::Other => unreachable!("non-Spine tools returned above"),
        }
    }

    fn redact_tool_output(
        &mut self,
        state: Option<DebugCallState>,
        body: &FunctionCallOutputBody,
        raw_output: Option<&Value>,
    ) -> DebugToolOutput {
        match state.map(|state| state.tool) {
            Some(
                tool @ (DebugToolKind::SpineOpen
                | DebugToolKind::SpineClose
                | DebugToolKind::SpineNext
                | DebugToolKind::SpineTrim),
            ) => DebugToolOutput::Control {
                exact_success_carrier: is_exact_control_success(tool, body),
                body: debug_output_body(body),
            },
            Some(DebugToolKind::SpineSpawn) if self.failure.is_some() => {
                DebugToolOutput::Redacted {
                    body: debug_output_body(body),
                }
            }
            Some(DebugToolKind::SpineSpawn) => DebugToolOutput::Spawn {
                receipt: self
                    .inspect_spawn_receipt(raw_output, state.and_then(|state| state.spawn_request)),
            },
            Some(DebugToolKind::CodeMode)
            | Some(DebugToolKind::Shell)
            | Some(DebugToolKind::Patch)
            | Some(DebugToolKind::Web)
            | Some(DebugToolKind::Mcp)
            | Some(DebugToolKind::Image)
            | Some(DebugToolKind::Other)
            | None => DebugToolOutput::Redacted {
                body: debug_output_body(body),
            },
        }
    }

    fn inspect_spawn_receipt(
        &mut self,
        raw_output: Option<&Value>,
        request: Option<SpawnRequestSignature>,
    ) -> DebugSpawnReceiptShape {
        if self.failure.is_some() {
            return DebugSpawnReceiptShape {
                object: DebugObjectState::MalformedJson,
                schema: DebugSchemaShape::Missing,
                results: missing_collection(),
                unknown_fields: false,
                valid_for_request: false,
            };
        }
        let parsed = match raw_output {
            Some(Value::String(text)) => self
                .preflight_json(text.as_bytes())
                .then(|| serde_json::from_str::<Value>(text).ok())
                .flatten(),
            Some(value) => Some(value.clone()),
            None => None,
        };
        let object = match &parsed {
            Some(Value::Object(_)) => DebugObjectState::Object,
            Some(_) => DebugObjectState::NonObject,
            None => DebugObjectState::MalformedJson,
        };
        let fields = parsed.as_ref().and_then(Value::as_object);
        let results = self.inspect_spawn_results(field(fields, "results"));
        let valid_for_request = parsed
            .as_ref()
            .and_then(|value| {
                serde_json::from_value::<spine_core::SpawnReceipt>(value.clone()).ok()
            })
            .zip(request)
            .is_some_and(|(receipt, request)| {
                let tasks = (0..request.task_count)
                    .map(|_| SpawnTask {
                        summary: "redacted".to_string(),
                        prompt: "redacted".to_string(),
                    })
                    .collect::<Vec<_>>();
                spine_core::SpawnReceipt::validate_for(&receipt, &tasks).is_ok()
            });
        DebugSpawnReceiptShape {
            object,
            schema: schema_shape(
                field(fields, "schema"),
                spine_core::SPINE_SPAWN_RESULT_SCHEMA,
            ),
            results,
            unknown_fields: has_unknown_fields(fields, &["schema", "results"]),
            valid_for_request,
        }
    }

    fn inspect_spawn_results(
        &mut self,
        value: Option<&Value>,
    ) -> DebugCollection<DebugSpawnResultShape> {
        let Some(value) = value else {
            return missing_collection();
        };
        let Value::Array(items) = value else {
            return DebugCollection {
                shape: if value.is_null() {
                    DebugCollectionShape::Null
                } else {
                    DebugCollectionShape::WrongType
                },
                items: Vec::new(),
            };
        };
        DebugCollection {
            shape: DebugCollectionShape::Array,
            items: items
                .iter()
                .map(|item| {
                    let fields = item.as_object();
                    DebugSpawnResultShape {
                        object: if fields.is_some() {
                            DebugObjectState::Object
                        } else {
                            DebugObjectState::NonObject
                        },
                        ordinal: unsigned_shape(field(fields, "ordinal")),
                        outcome: spawn_outcome_shape(field(fields, "outcome")),
                        memory_body: string_shape(field(fields, "memory_body")),
                        diagnostic: string_shape(field(fields, "diagnostic")),
                        execution_ref: self
                            .mapped_string(IdNamespace::Execution, field(fields, "execution_ref")),
                        unknown_fields: has_unknown_fields(
                            fields,
                            &[
                                "ordinal",
                                "outcome",
                                "memory_body",
                                "diagnostic",
                                "execution_ref",
                            ],
                        ),
                    }
                })
                .collect(),
        }
    }

    fn turn_id(
        &mut self,
        metadata: Option<&InternalChatMessageMetadataPassthrough>,
    ) -> Option<u64> {
        self.optional_local_id(
            IdNamespace::Turn,
            metadata.and_then(|metadata| metadata.turn_id.as_deref()),
        )
    }

    fn mapped_string(
        &mut self,
        namespace: IdNamespace,
        value: Option<&Value>,
    ) -> DebugMappedString {
        let shape = string_shape(value);
        let local_id = value
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| self.local_id(namespace, value));
        DebugMappedString { shape, local_id }
    }

    fn optional_local_id(&mut self, namespace: IdNamespace, value: Option<&str>) -> Option<u64> {
        value.map(|value| self.local_id(namespace, value))
    }

    fn local_id(&mut self, namespace: IdNamespace, value: &str) -> u64 {
        if let Some(id) = self
            .ids
            .get(&namespace)
            .and_then(|ids| ids.get(value))
            .copied()
        {
            return id;
        }
        let Some(next_entry_count) = self.tracked_id_entries.checked_add(1) else {
            self.mark_resource_limit_exceeded();
            return 0;
        };
        let Some(next_byte_count) = self.tracked_id_bytes.checked_add(value.len()) else {
            self.mark_resource_limit_exceeded();
            return 0;
        };
        if next_entry_count > self.limits.max_tracked_id_entries
            || next_byte_count > self.limits.max_tracked_id_bytes
        {
            self.mark_resource_limit_exceeded();
            return 0;
        }
        let ids = self.ids.entry(namespace).or_default();
        let id = u64::try_from(ids.len()).unwrap_or(u64::MAX);
        ids.insert(value.to_string(), id);
        self.tracked_id_entries = next_entry_count;
        self.tracked_id_bytes = next_byte_count;
        id
    }
}

fn is_known_rollout_item_type(kind: &str) -> bool {
    matches!(
        kind,
        "session_meta"
            | "response_item"
            | "inter_agent_communication"
            | "inter_agent_communication_metadata"
            | "compacted"
            | "turn_context"
            | "world_state"
            | "event_msg"
    )
}

fn is_known_response_item_type(kind: &str) -> bool {
    matches!(
        kind,
        "additional_tools"
            | "message"
            | "agent_message"
            | "reasoning"
            | "local_shell_call"
            | "function_call"
            | "tool_search_call"
            | "function_call_output"
            | "custom_tool_call"
            | "custom_tool_call_output"
            | "tool_search_output"
            | "web_search_call"
            | "image_generation_call"
            | "compaction"
            | "compaction_summary"
            | "compaction_trigger"
            | "context_compaction"
            | "other"
    )
}

fn is_known_event_type(kind: &str) -> bool {
    matches!(
        kind,
        "error"
            | "warning"
            | "guardian_warning"
            | "realtime_conversation_started"
            | "realtime_conversation_realtime"
            | "realtime_conversation_closed"
            | "realtime_conversation_sdp"
            | "model_reroute"
            | "model_verification"
            | "turn_moderation_metadata"
            | "safety_buffering"
            | "context_compacted"
            | "thread_rolled_back"
            | "task_started"
            | "turn_started"
            | "thread_settings_applied"
            | "task_complete"
            | "turn_complete"
            | "token_count"
            | "agent_message"
            | "user_message"
            | "agent_reasoning"
            | "agent_reasoning_raw_content"
            | "agent_reasoning_section_break"
            | "session_configured"
            | "environment_connected"
            | "environment_disconnected"
            | "thread_goal_updated"
            | "mcp_startup_update"
            | "mcp_startup_complete"
            | "mcp_tool_call_begin"
            | "mcp_tool_call_end"
            | "web_search_begin"
            | "web_search_end"
            | "image_generation_begin"
            | "image_generation_end"
            | "exec_command_begin"
            | "exec_command_output_delta"
            | "terminal_interaction"
            | "exec_command_end"
            | "view_image_tool_call"
            | "exec_approval_request"
            | "request_permissions"
            | "request_user_input"
            | "dynamic_tool_call_request"
            | "dynamic_tool_call_response"
            | "elicitation_request"
            | "apply_patch_approval_request"
            | "guardian_assessment"
            | "deprecation_notice"
            | "stream_error"
            | "patch_apply_begin"
            | "patch_apply_updated"
            | "patch_apply_end"
            | "turn_diff"
            | "realtime_conversation_list_voices_response"
            | "plan_update"
            | "spine_tree_update"
            | "spine_spawn_progress"
            | "turn_aborted"
            | "shutdown_complete"
            | "entered_review_mode"
            | "exited_review_mode"
            | "raw_response_item"
            | "raw_response_completed"
            | "item_started"
            | "item_completed"
            | "hook_started"
            | "hook_completed"
            | "agent_message_content_delta"
            | "plan_delta"
            | "reasoning_content_delta"
            | "reasoning_raw_content_delta"
            | "collab_agent_spawn_begin"
            | "collab_agent_spawn_end"
            | "collab_agent_interaction_begin"
            | "collab_agent_interaction_end"
            | "collab_waiting_begin"
            | "collab_waiting_end"
            | "collab_close_begin"
            | "collab_close_end"
            | "collab_resume_begin"
            | "collab_resume_end"
            | "sub_agent_activity"
    )
}

fn classify_tool(namespace: Option<&str>, name: &str) -> DebugToolKind {
    match (namespace, name) {
        (Some("spine"), "open") | (None, "spine.open") => DebugToolKind::SpineOpen,
        (Some("spine"), "close") | (None, "spine.close") => DebugToolKind::SpineClose,
        (Some("spine"), "next") | (None, "spine.next") => DebugToolKind::SpineNext,
        (Some("spine"), "trim") | (None, "spine.trim") => DebugToolKind::SpineTrim,
        (Some("spine"), "spawn") | (None, "spine.spawn") => DebugToolKind::SpineSpawn,
        _ if is_exec_tool_name(&codex_tools::ToolName {
            namespace: namespace.map(str::to_string),
            name: name.to_string(),
        }) =>
        {
            DebugToolKind::CodeMode
        }
        (_, "shell_command" | "exec_command" | "write_stdin") => DebugToolKind::Shell,
        (_, "apply_patch") => DebugToolKind::Patch,
        (_, "web_search" | "web_search_preview") => DebugToolKind::Web,
        (_, "view_image" | "image_generation") => DebugToolKind::Image,
        (Some(_), _) => DebugToolKind::Mcp,
        (None, _) => DebugToolKind::Other,
    }
}

fn debug_message_role(role: &str) -> DebugMessageRole {
    match role {
        "user" => DebugMessageRole::User,
        "assistant" => DebugMessageRole::Assistant,
        "developer" => DebugMessageRole::Developer,
        "system" => DebugMessageRole::System,
        _ => DebugMessageRole::Other,
    }
}

fn debug_message_phase(phase: MessagePhase) -> DebugMessagePhase {
    match phase {
        MessagePhase::Commentary => DebugMessagePhase::Commentary,
        MessagePhase::FinalAnswer => DebugMessagePhase::FinalAnswer,
    }
}

fn debug_content_kind(item: ContentItem) -> DebugContentKind {
    match item {
        ContentItem::InputText { .. } => DebugContentKind::InputText,
        ContentItem::InputImage { .. } => DebugContentKind::InputImage,
        ContentItem::InputAudio { .. } => DebugContentKind::InputAudio,
        ContentItem::OutputText { .. } => DebugContentKind::OutputText,
    }
}

fn debug_agent_content_kind(item: AgentMessageInputContent) -> DebugAgentContentKind {
    match item {
        AgentMessageInputContent::InputText { .. } => DebugAgentContentKind::InputText,
        AgentMessageInputContent::EncryptedContent { .. } => {
            DebugAgentContentKind::EncryptedContent
        }
    }
}

fn debug_reasoning_content_kind(item: ReasoningItemContent) -> DebugReasoningContentKind {
    match item {
        ReasoningItemContent::ReasoningText { .. } => DebugReasoningContentKind::ReasoningText,
        ReasoningItemContent::Text { .. } => DebugReasoningContentKind::Text,
    }
}

fn debug_shell_status(status: LocalShellStatus) -> DebugShellStatus {
    match status {
        LocalShellStatus::Completed => DebugShellStatus::Completed,
        LocalShellStatus::InProgress => DebugShellStatus::InProgress,
        LocalShellStatus::Incomplete => DebugShellStatus::Incomplete,
    }
}

fn debug_web_action(action: WebSearchAction) -> DebugWebAction {
    match action {
        WebSearchAction::Search { query, queries } => DebugWebAction::Search {
            query_count: usize::from(query.is_some()) + queries.as_ref().map_or(0, Vec::len),
        },
        WebSearchAction::OpenPage { .. } => DebugWebAction::OpenPage,
        WebSearchAction::FindInPage { .. } => DebugWebAction::FindInPage,
        WebSearchAction::Other => DebugWebAction::Other,
    }
}

fn debug_output_body(body: &FunctionCallOutputBody) -> DebugOutputBodyShape {
    match body {
        FunctionCallOutputBody::Text(text) => DebugOutputBodyShape::Text { bytes: text.len() },
        FunctionCallOutputBody::ContentItems(items) => DebugOutputBodyShape::ContentItems {
            items: items
                .iter()
                .map(|item| match item {
                    FunctionCallOutputContentItem::InputText { .. } => {
                        DebugOutputContentKind::InputText
                    }
                    FunctionCallOutputContentItem::InputImage { .. } => {
                        DebugOutputContentKind::InputImage
                    }
                    FunctionCallOutputContentItem::InputAudio { .. } => {
                        DebugOutputContentKind::InputAudio
                    }
                    FunctionCallOutputContentItem::EncryptedContent { .. } => {
                        DebugOutputContentKind::EncryptedContent
                    }
                })
                .collect(),
        },
    }
}

fn is_exact_control_success(tool: DebugToolKind, body: &FunctionCallOutputBody) -> bool {
    let FunctionCallOutputBody::Text(body) = body else {
        return false;
    };
    match tool {
        DebugToolKind::SpineOpen => {
            body == super::tool_response::success_carrier(spine_core::SpineTool::Open)
        }
        DebugToolKind::SpineClose => {
            body == super::tool_response::success_carrier(spine_core::SpineTool::Close)
        }
        DebugToolKind::SpineNext => {
            body == super::tool_response::success_carrier(spine_core::SpineTool::Next)
        }
        DebugToolKind::SpineTrim => {
            body == super::tool_response::success_carrier(spine_core::SpineTool::Trim)
        }
        DebugToolKind::SpineSpawn
        | DebugToolKind::CodeMode
        | DebugToolKind::Shell
        | DebugToolKind::Patch
        | DebugToolKind::Web
        | DebugToolKind::Mcp
        | DebugToolKind::Image
        | DebugToolKind::Other => false,
    }
}

fn inspect_spawn_tasks(value: Option<&Value>) -> DebugCollection<DebugSpawnTaskShape> {
    let Some(value) = value else {
        return missing_collection();
    };
    let Value::Array(items) = value else {
        return DebugCollection {
            shape: if value.is_null() {
                DebugCollectionShape::Null
            } else {
                DebugCollectionShape::WrongType
            },
            items: Vec::new(),
        };
    };
    DebugCollection {
        shape: DebugCollectionShape::Array,
        items: items
            .iter()
            .map(|item| {
                let fields = item.as_object();
                DebugSpawnTaskShape {
                    object: if fields.is_some() {
                        DebugObjectState::Object
                    } else {
                        DebugObjectState::NonObject
                    },
                    summary: string_shape(field(fields, "summary")),
                    prompt: string_shape(field(fields, "prompt")),
                    unknown_fields: has_unknown_fields(fields, &["summary", "prompt"]),
                }
            })
            .collect(),
    }
}

fn raw_output_value(raw: Option<&Value>) -> Option<&Value> {
    raw.and_then(Value::as_object)
        .and_then(|fields| fields.get("output"))
}

fn json_text_shape(value: &str) -> DebugJsonValueShape {
    serde_json::from_str::<Value>(value)
        .as_ref()
        .map(json_value_shape)
        .unwrap_or(DebugJsonValueShape::MalformedJson)
}

fn json_value_shape(value: &Value) -> DebugJsonValueShape {
    match value {
        Value::Null => DebugJsonValueShape::Null,
        Value::Bool(_) => DebugJsonValueShape::Bool,
        Value::Number(_) => DebugJsonValueShape::Number,
        Value::String(_) => DebugJsonValueShape::String,
        Value::Array(items) => DebugJsonValueShape::Array(items.len()),
        Value::Object(fields) => DebugJsonValueShape::Object(fields.len()),
    }
}

fn field<'a>(fields: Option<&'a Map<String, Value>>, name: &str) -> Option<&'a Value> {
    fields.and_then(|fields| fields.get(name))
}

fn has_unknown_fields(fields: Option<&Map<String, Value>>, allowed: &[&str]) -> bool {
    fields.is_some_and(|fields| fields.keys().any(|name| !allowed.contains(&name.as_str())))
}

fn string_shape(value: Option<&Value>) -> DebugStringShape {
    match value {
        None => DebugStringShape::Missing,
        Some(Value::Null) => DebugStringShape::Null,
        Some(Value::String(value)) if value.is_empty() => DebugStringShape::Empty,
        Some(Value::String(value)) if value.trim().is_empty() => DebugStringShape::Whitespace,
        Some(Value::String(_)) => DebugStringShape::NonEmpty,
        Some(_) => DebugStringShape::WrongType,
    }
}

fn trim_op_shape(value: Option<&Value>) -> DebugTrimOpShape {
    match value {
        None => DebugTrimOpShape::Missing,
        Some(Value::Null) => DebugTrimOpShape::Null,
        Some(Value::String(value)) if value.is_empty() => DebugTrimOpShape::Empty,
        Some(Value::String(value)) if value.trim().is_empty() => DebugTrimOpShape::Whitespace,
        Some(Value::String(value)) if value == "snip" => DebugTrimOpShape::Snip,
        Some(Value::String(value)) if value == "slice" => DebugTrimOpShape::Slice,
        Some(Value::String(_)) => DebugTrimOpShape::Other,
        Some(_) => DebugTrimOpShape::WrongType,
    }
}

fn unsigned_shape(value: Option<&Value>) -> DebugUnsignedShape {
    match value {
        None => DebugUnsignedShape::Missing,
        Some(Value::Null) => DebugUnsignedShape::Null,
        Some(Value::Number(number)) if number.as_u64().is_some() => {
            DebugUnsignedShape::Unsigned(number.as_u64().unwrap_or_default())
        }
        Some(Value::Number(number)) if number.as_i64().is_some_and(|value| value < 0) => {
            DebugUnsignedShape::Negative
        }
        Some(Value::Number(_)) => DebugUnsignedShape::OtherNumber,
        Some(_) => DebugUnsignedShape::WrongType,
    }
}

fn schema_shape(value: Option<&Value>, expected: &str) -> DebugSchemaShape {
    match value {
        None => DebugSchemaShape::Missing,
        Some(Value::Null) => DebugSchemaShape::Null,
        Some(Value::String(value)) if value.is_empty() => DebugSchemaShape::Empty,
        Some(Value::String(value)) if value.trim().is_empty() => DebugSchemaShape::Whitespace,
        Some(Value::String(value)) if value == expected => DebugSchemaShape::ExactV1,
        Some(Value::String(_)) => DebugSchemaShape::Other,
        Some(_) => DebugSchemaShape::WrongType,
    }
}

fn spawn_outcome_shape(value: Option<&Value>) -> DebugSpawnOutcomeShape {
    match value {
        None => DebugSpawnOutcomeShape::Missing,
        Some(Value::Null) => DebugSpawnOutcomeShape::Null,
        Some(Value::String(value)) if value.is_empty() => DebugSpawnOutcomeShape::Empty,
        Some(Value::String(value)) if value.trim().is_empty() => DebugSpawnOutcomeShape::Whitespace,
        Some(Value::String(value)) if value == "completed" => DebugSpawnOutcomeShape::Completed,
        Some(Value::String(value)) if value == "errored" => DebugSpawnOutcomeShape::Errored,
        Some(Value::String(value)) if value == "aborted" => DebugSpawnOutcomeShape::Aborted,
        Some(Value::String(_)) => DebugSpawnOutcomeShape::Other,
        Some(_) => DebugSpawnOutcomeShape::WrongType,
    }
}

fn missing_collection<T>() -> DebugCollection<T> {
    DebugCollection {
        shape: DebugCollectionShape::Missing,
        items: Vec::new(),
    }
}

fn redact_event(event: EventMsg) -> DebugEvent {
    let mut debug = DebugEvent {
        kind: event_kind(&event),
        token_usage: None,
        rolled_back_turns: None,
    };
    match event {
        EventMsg::TokenCount(event) => {
            debug.token_usage = event.info.map(|info| DebugTokenUsageInfo {
                total: debug_token_usage(info.total_token_usage),
                last: debug_token_usage(info.last_token_usage),
                model_context_window: info.model_context_window,
            });
        }
        EventMsg::ThreadRolledBack(event) => {
            debug.rolled_back_turns = Some(event.num_turns);
        }
        EventMsg::Error(_)
        | EventMsg::Warning(_)
        | EventMsg::GuardianWarning(_)
        | EventMsg::RealtimeConversationStarted(_)
        | EventMsg::RealtimeConversationRealtime(_)
        | EventMsg::RealtimeConversationClosed(_)
        | EventMsg::RealtimeConversationSdp(_)
        | EventMsg::ModelReroute(_)
        | EventMsg::ModelVerification(_)
        | EventMsg::TurnModerationMetadata(_)
        | EventMsg::SafetyBuffering(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::ThreadSettingsApplied(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::AgentMessage(_)
        | EventMsg::UserMessage(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::EnvironmentConnected(_)
        | EventMsg::EnvironmentDisconnected(_)
        | EventMsg::ThreadGoalUpdated(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::WebSearchBegin(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ImageGenerationBegin(_)
        | EventMsg::ImageGenerationEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::GuardianAssessment(_)
        | EventMsg::DeprecationNotice(_)
        | EventMsg::StreamError(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::PatchApplyUpdated(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::RealtimeConversationListVoicesResponse(_)
        | EventMsg::PlanUpdate(_)
        | EventMsg::SpineTreeUpdate(_)
        | EventMsg::SpineSpawnProgress(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::ShutdownComplete
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::RawResponseCompleted(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::ItemCompleted(_)
        | EventMsg::HookStarted(_)
        | EventMsg::HookCompleted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::CollabResumeEnd(_)
        | EventMsg::SubAgentActivity(_) => {}
    }
    debug
}

fn event_kind(event: &EventMsg) -> &'static str {
    match event {
        EventMsg::Error(_) => "error",
        EventMsg::Warning(_) => "warning",
        EventMsg::GuardianWarning(_) => "guardian_warning",
        EventMsg::RealtimeConversationStarted(_) => "realtime_conversation_started",
        EventMsg::RealtimeConversationRealtime(_) => "realtime_conversation_realtime",
        EventMsg::RealtimeConversationClosed(_) => "realtime_conversation_closed",
        EventMsg::RealtimeConversationSdp(_) => "realtime_conversation_sdp",
        EventMsg::ModelReroute(_) => "model_reroute",
        EventMsg::ModelVerification(_) => "model_verification",
        EventMsg::TurnModerationMetadata(_) => "turn_moderation_metadata",
        EventMsg::SafetyBuffering(_) => "safety_buffering",
        EventMsg::ContextCompacted(_) => "context_compacted",
        EventMsg::ThreadRolledBack(_) => "thread_rolled_back",
        EventMsg::TurnStarted(_) => "task_started",
        EventMsg::ThreadSettingsApplied(_) => "thread_settings_applied",
        EventMsg::TurnComplete(_) => "task_complete",
        EventMsg::TokenCount(_) => "token_count",
        EventMsg::AgentMessage(_) => "agent_message",
        EventMsg::UserMessage(_) => "user_message",
        EventMsg::AgentReasoning(_) => "agent_reasoning",
        EventMsg::AgentReasoningRawContent(_) => "agent_reasoning_raw_content",
        EventMsg::AgentReasoningSectionBreak(_) => "agent_reasoning_section_break",
        EventMsg::SessionConfigured(_) => "session_configured",
        EventMsg::EnvironmentConnected(_) => "environment_connected",
        EventMsg::EnvironmentDisconnected(_) => "environment_disconnected",
        EventMsg::ThreadGoalUpdated(_) => "thread_goal_updated",
        EventMsg::McpStartupUpdate(_) => "mcp_startup_update",
        EventMsg::McpStartupComplete(_) => "mcp_startup_complete",
        EventMsg::McpToolCallBegin(_) => "mcp_tool_call_begin",
        EventMsg::McpToolCallEnd(_) => "mcp_tool_call_end",
        EventMsg::WebSearchBegin(_) => "web_search_begin",
        EventMsg::WebSearchEnd(_) => "web_search_end",
        EventMsg::ImageGenerationBegin(_) => "image_generation_begin",
        EventMsg::ImageGenerationEnd(_) => "image_generation_end",
        EventMsg::ExecCommandBegin(_) => "exec_command_begin",
        EventMsg::ExecCommandOutputDelta(_) => "exec_command_output_delta",
        EventMsg::TerminalInteraction(_) => "terminal_interaction",
        EventMsg::ExecCommandEnd(_) => "exec_command_end",
        EventMsg::ViewImageToolCall(_) => "view_image_tool_call",
        EventMsg::ExecApprovalRequest(_) => "exec_approval_request",
        EventMsg::RequestPermissions(_) => "request_permissions",
        EventMsg::RequestUserInput(_) => "request_user_input",
        EventMsg::DynamicToolCallRequest(_) => "dynamic_tool_call_request",
        EventMsg::DynamicToolCallResponse(_) => "dynamic_tool_call_response",
        EventMsg::ElicitationRequest(_) => "elicitation_request",
        EventMsg::ApplyPatchApprovalRequest(_) => "apply_patch_approval_request",
        EventMsg::GuardianAssessment(_) => "guardian_assessment",
        EventMsg::DeprecationNotice(_) => "deprecation_notice",
        EventMsg::StreamError(_) => "stream_error",
        EventMsg::PatchApplyBegin(_) => "patch_apply_begin",
        EventMsg::PatchApplyUpdated(_) => "patch_apply_updated",
        EventMsg::PatchApplyEnd(_) => "patch_apply_end",
        EventMsg::TurnDiff(_) => "turn_diff",
        EventMsg::RealtimeConversationListVoicesResponse(_) => {
            "realtime_conversation_list_voices_response"
        }
        EventMsg::PlanUpdate(_) => "plan_update",
        EventMsg::SpineTreeUpdate(_) => "spine_tree_update",
        EventMsg::SpineSpawnProgress(_) => "spine_spawn_progress",
        EventMsg::TurnAborted(_) => "turn_aborted",
        EventMsg::ShutdownComplete => "shutdown_complete",
        EventMsg::EnteredReviewMode(_) => "entered_review_mode",
        EventMsg::ExitedReviewMode(_) => "exited_review_mode",
        EventMsg::RawResponseItem(_) => "raw_response_item",
        EventMsg::RawResponseCompleted(_) => "raw_response_completed",
        EventMsg::ItemStarted(_) => "item_started",
        EventMsg::ItemCompleted(_) => "item_completed",
        EventMsg::HookStarted(_) => "hook_started",
        EventMsg::HookCompleted(_) => "hook_completed",
        EventMsg::AgentMessageContentDelta(_) => "agent_message_content_delta",
        EventMsg::PlanDelta(_) => "plan_delta",
        EventMsg::ReasoningContentDelta(_) => "reasoning_content_delta",
        EventMsg::ReasoningRawContentDelta(_) => "reasoning_raw_content_delta",
        EventMsg::CollabAgentSpawnBegin(_) => "collab_agent_spawn_begin",
        EventMsg::CollabAgentSpawnEnd(_) => "collab_agent_spawn_end",
        EventMsg::CollabAgentInteractionBegin(_) => "collab_agent_interaction_begin",
        EventMsg::CollabAgentInteractionEnd(_) => "collab_agent_interaction_end",
        EventMsg::CollabWaitingBegin(_) => "collab_waiting_begin",
        EventMsg::CollabWaitingEnd(_) => "collab_waiting_end",
        EventMsg::CollabCloseBegin(_) => "collab_close_begin",
        EventMsg::CollabCloseEnd(_) => "collab_close_end",
        EventMsg::CollabResumeBegin(_) => "collab_resume_begin",
        EventMsg::CollabResumeEnd(_) => "collab_resume_end",
        EventMsg::SubAgentActivity(_) => "sub_agent_activity",
    }
}

fn debug_token_usage(usage: TokenUsage) -> DebugTokenUsage {
    DebugTokenUsage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        total_tokens: usage.total_tokens,
    }
}

#[cfg(test)]
#[path = "rollout_debug_tests.rs"]
mod tests;
