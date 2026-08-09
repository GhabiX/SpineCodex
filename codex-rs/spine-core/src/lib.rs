mod archive;
mod artifact;
mod bootstrap;
mod compiler;
mod config;
mod context_char;
mod context_event;
mod context_plan;
mod context_runtime;
mod executed_fact;
mod identity;
mod model;
mod observer;
mod planner;
mod pressure;
mod prompt;
mod reducer;
mod replay;
mod sampling;
mod sampling_delta;
mod sampling_runtime;
mod source_ledger;
mod status;
mod tools;

pub use archive::CommittedSpineExecution;
pub use archive::RecordDigest;
pub use archive::SamplingArchiveRecord;
pub use archive::SamplingCommit;
pub use archive::SamplingStarted;
pub use archive::SourceSpan;
pub use bootstrap::InitError;
pub use compiler::MAX_RAW_EVENT_BYTES;
pub use compiler::MAX_SYNTHETIC_CONTEXT_BYTES;
pub use compiler::MAX_TREE_NODES;
pub use compiler::MAX_VISIBLE_CONTEXT_ITEMS;
pub(crate) use compiler::SpineCompiler;
pub use compiler::SpineError;
pub use config::ConfigError;
pub use config::ConfigLoadError;
pub use config::DEFAULT_CONFIG_TOML;
pub use config::Feature;
pub use config::MAX_MODEL_VISIBLE_ITEM_TOKENS;
pub use config::MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES;
pub use config::MAX_MODEL_VISIBLE_TEXT_BYTES;
pub use config::MAX_PROVIDER_ADDED_FRAME_TOKENS;
pub use config::SpawnPromptMode;
pub use config::SpineConfig;
pub use config::SpineConfigLoader;
pub use context_char::CellId;
pub use context_char::CharParseError;
pub use context_char::CharParseStep;
pub use context_char::ParseCell;
pub use context_char::ParseStack;
pub use context_char::SpineChar;
pub use context_char::SpineCharParser;
pub use context_char::SpineRecoveryInput;
pub use context_char::SpineSignal;
pub use context_char::ToolRequestChar;
pub use context_char::ToolResponseChar;
pub use context_event::ContextEvent;
pub use context_event::ContextEventError;
pub use context_event::ContextInsert;
pub use context_event::ContextLabel;
pub use context_event::SpineContextEventHandler;
pub use context_plan::CONTEXT_PLAN_SCHEMA_V1;
pub use context_plan::ContextCellProvenance;
pub use context_plan::ContextPlanCell;
pub(crate) use context_plan::ContextPlanError;
pub use context_plan::ContextPlanRecipe;
pub use context_plan::ContextPlanSource;
pub(crate) use context_plan::ResolvedContextPlan;
pub use context_runtime::ContextSizePhase;
pub use context_runtime::SpineContextOutput;
pub use context_runtime::SpineContextProjection;
pub use context_runtime::SpineContextRuntime;
pub use context_runtime::SpineContextRuntimeError;
pub(crate) use executed_fact::ExecutedFactError;
pub use executed_fact::ExecutedSpineFact;
pub use executed_fact::ExecutionOrigin;
pub use executed_fact::SpineOperationFact;
pub(crate) use executed_fact::StableToolOutputId;
pub use identity::AdmissionOrdinal;
pub(crate) use identity::BoundaryId;
pub use identity::ContextEpoch;
pub use identity::ExecutionId;
pub(crate) use identity::ProjectionCellId;
pub use identity::SamplingAttemptId;
pub use identity::SamplingCommitId;
pub use identity::SourceCellId;
pub use identity::ThreadNamespace;
pub(crate) use identity::TrimTicket;
pub use observer::NoopSpineObserver;
pub use observer::SpineObserverEffect;
pub use observer::SpineObserverEffectHandler;
pub use observer::SpineObserverEffectKind;
pub use planner::PlannerError;
pub use planner::PreparedSamplingCommit;
pub(crate) use planner::SamplingPlanner;

pub use artifact::MemoryArtifact;
pub use artifact::TRIM_SNIPPED_BODY;
pub use artifact::UserMessageArtifact;
pub use artifact::closed_memory_artifacts;
pub use artifact::render_memory_artifact;
pub use model::ContextEdit;
pub use model::ContextItem;
pub use model::MemorySlot;
pub use model::Message;
pub use model::MessageRole;
pub use model::NativeItemRef;
pub use model::NodeId;
pub use model::NodeKind;
pub use model::NodeSnapshot;
pub use model::NodeStatus;
pub use model::ProjectionDelta;
pub use model::RawBoundary;
pub use model::RawSpan;
pub(crate) use model::RolloutEvent;
pub use model::SPINE_SPAWN_RESULT_SCHEMA;
pub use model::SpawnOutcome;
pub use model::SpawnReceipt;
pub use model::SpawnResult;
pub use model::SpawnTask;
pub use model::SpawnValidationError;
pub use model::SpineProjection;
pub use model::ToolOutcome;
pub use model::ToolUse;
pub use model::TrimEdit;
pub use model::TrimOperation;
pub use model::TrimProjection;
pub use model::TrimRequest;
pub use model::TrimSlice;
pub use reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
pub use replay::CanonicalReplay;
pub use replay::ReplayInput;
pub use replay::SpineCompactBarrierV1;
pub(crate) use sampling::FactPermit;
pub(crate) use sampling::SamplingAttempt;
pub use sampling::SamplingError;
pub(crate) use sampling::SamplingFactSink;
pub use sampling::SamplingHandle;
pub(crate) use sampling::SealedSampling;
#[cfg(test)]
pub(crate) use sampling::SpineFactKind;
#[cfg(test)]
pub(crate) use sampling::SpineFactReservation;
pub use sampling_runtime::SamplingFinish;
pub use sampling_runtime::SamplingRuntime;
pub use sampling_runtime::SamplingTerminal;
pub(crate) use source_ledger::SourceCell;
pub(crate) use source_ledger::SourceCellPayload;
pub use source_ledger::SourceLedger;
pub(crate) use source_ledger::SourceLedgerError;
pub(crate) use source_ledger::SourceSnapshot;
pub use status::ContextPressure;
pub use status::ContextPressureProblem;
pub use status::ContextWindowSample;
pub use status::NodeContextCost;
pub use status::StatusSignal;
pub use status::TokenUsageSample;
pub use status::TreeNode;
pub use status::TreeSnapshot;
pub use status::context_pressures;
pub use status::status_signal;
pub use status::tree_snapshot;
pub use tools::MAX_MEMORY_BYTES;
pub use tools::MAX_SPAWN_BATCH_BYTES;
pub use tools::MAX_SPAWN_PROMPT_BYTES;
pub use tools::MAX_SPAWN_TASKS;
pub use tools::MAX_SUMMARY_BYTES;
pub use tools::SPINE_NAMESPACE;
pub use tools::SPINE_NAMESPACE_DESCRIPTION;
pub use tools::SpineTool;
pub use tools::ToolCatalog;
pub use tools::ToolDefinition;
pub use tools::ToolValidation;
pub use tools::ToolValidationError;
pub use tools::ValidatedTransition;
pub use tools::success_carrier;
pub use tools::validate_tool;

#[cfg(test)]
#[path = "archive_tests.rs"]
mod archive_tests;

#[cfg(test)]
#[path = "executed_fact_tests.rs"]
mod executed_fact_tests;

#[cfg(test)]
#[path = "context_plan_tests.rs"]
mod context_plan_tests;

#[cfg(test)]
#[path = "sampling_tests.rs"]
mod sampling_tests;

#[cfg(test)]
#[path = "sampling_runtime_tests.rs"]
mod sampling_runtime_tests;

#[cfg(test)]
#[path = "source_ledger_tests.rs"]
mod source_ledger_tests;

#[cfg(test)]
#[path = "planner_tests.rs"]
mod planner_tests;

#[cfg(test)]
#[path = "replay_tests.rs"]
mod replay_tests;

#[cfg(test)]
mod tests;
