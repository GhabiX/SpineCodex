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

pub(crate) use archive::CommittedSpineExecution;
pub(crate) use archive::SamplingStarted;
pub(crate) use bootstrap::InitError;
pub(crate) use compiler::MAX_RAW_EVENT_BYTES;
pub(crate) use compiler::MAX_SYNTHETIC_CONTEXT_BYTES;
pub(crate) use compiler::MAX_VISIBLE_CONTEXT_ITEMS;
pub(crate) use compiler::SpineCompiler;
pub(crate) use compiler::SpineError;
pub(crate) use context_char::CharParseError;
pub(crate) use context_char::SpineCharParser;
pub(crate) use context_event::ContextEventError;
pub(crate) use context_plan::ContextPlanError;
pub(crate) use context_plan::ResolvedContextPlan;
pub(crate) use context_runtime::SpineContextProjection;
pub(crate) use executed_fact::ExecutedFactError;
pub(crate) use executed_fact::ExecutedSpineFact;
pub(crate) use identity::AdmissionOrdinal;
pub(crate) use identity::BoundaryId;
pub(crate) use identity::ExecutionId;
pub(crate) use identity::ProjectionCellId;
pub(crate) use identity::SamplingAttemptId;
pub(crate) use observer::NoopSpineObserver;
pub(crate) use planner::SamplingPlanner;

pub(crate) use model::NodeSnapshot;
pub(crate) use model::RolloutEvent;
pub(crate) use sampling::FactPermit;
pub(crate) use sampling::SamplingAttempt;
pub(crate) use sampling::SamplingFactSink;
pub(crate) use sampling::SealedSampling;
pub(crate) use source_ledger::SourceCell;
pub(crate) use source_ledger::SourceCellPayload;
pub(crate) use source_ledger::SourceLedgerError;
pub(crate) use source_ledger::SourceSnapshot;
pub(crate) use status::TreeSnapshot;
pub(crate) use tools::MAX_MEMORY_BYTES;
pub(crate) use tools::MAX_SPAWN_BATCH_BYTES;

/// Host-facing Spine API used by Codex integration layers.
///
/// The implementation modules and their individual types remain internal to
/// the SDK crate. New host integrations should import from this facade instead
/// of depending on the crate root export layout.
pub mod host {
    pub use super::archive::RecordDigest;
    pub use super::archive::SamplingArchiveRecord;
    pub use super::archive::SamplingCommit;
    pub use super::artifact::closed_memory_artifacts;
    pub use super::artifact::render_memory_artifact;
    pub use super::config::ConfigError;
    pub use super::config::ConfigLoadError;
    pub use super::config::DEFAULT_CONFIG_TOML;
    pub use super::config::Feature;
    pub use super::config::MAX_MODEL_VISIBLE_ITEM_TOKENS;
    pub use super::config::MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES;
    pub use super::config::MAX_MODEL_VISIBLE_TEXT_BYTES;
    pub use super::config::SpawnPromptMode;
    pub use super::config::SpineConfig;
    pub use super::config::SpineConfigLoader;
    pub use super::context_char::CellId;
    pub use super::context_char::ParseCell;
    pub use super::context_char::ParseStack;
    pub use super::context_char::SpineChar;
    pub use super::context_char::SpineRecoveryInput;
    pub use super::context_char::SpineSignal;
    pub use super::context_event::ContextEvent;
    pub use super::context_event::ContextInsert;
    pub use super::context_event::ContextLabel;
    pub use super::context_event::SpineContextEventHandler;
    pub use super::context_plan::CONTEXT_PLAN_SCHEMA_V1;
    pub use super::context_plan::ContextCellProvenance;
    pub use super::context_plan::ContextPlanCell;
    pub use super::context_plan::ContextPlanRecipe;
    pub use super::context_plan::ContextPlanSource;
    pub use super::context_runtime::SpineContextOutput;
    pub use super::context_runtime::SpineContextRuntime;
    pub use super::context_runtime::SpineContextRuntimeError;
    pub use super::executed_fact::ExecutionOrigin;
    pub use super::executed_fact::SpineOperationFact;
    pub use super::identity::ContextEpoch;
    pub use super::identity::SamplingCommitId;
    pub use super::identity::SourceCellId;
    pub use super::identity::ThreadNamespace;
    pub use super::model::ContextItem;
    pub use super::model::MemorySlot;
    pub use super::model::Message;
    pub use super::model::MessageRole;
    pub use super::model::NativeItemRef;
    pub use super::model::NodeId;
    pub use super::model::NodeKind;
    pub use super::model::NodeStatus;
    pub use super::model::RawBoundary;
    pub use super::model::RawSpan;
    pub use super::model::SPINE_SPAWN_RESULT_SCHEMA;
    pub use super::model::SpawnOutcome;
    pub use super::model::SpawnReceipt;
    pub use super::model::SpawnResult;
    pub use super::model::SpawnTask;
    pub use super::model::SpawnValidationError;
    pub use super::model::SpineProjection;
    pub use super::observer::SpineObserverEffect;
    pub use super::observer::SpineObserverEffectHandler;
    pub use super::observer::SpineObserverEffectKind;
    pub use super::planner::PlannerError;
    pub use super::planner::PreparedSamplingCommit;
    pub use super::replay::CanonicalReplay;
    pub use super::replay::ReplayInput;
    pub use super::replay::SpineCompactBarrierV1;
    pub use super::sampling::SamplingError;
    pub use super::sampling::SamplingHandle;
    pub use super::sampling_runtime::SamplingFinish;
    pub use super::sampling_runtime::SamplingRuntime;
    pub use super::sampling_runtime::SamplingTerminal;
    pub use super::source_ledger::SourceLedger;
    pub use super::status::ContextPressureProblem;
    pub use super::status::ContextWindowSample;
    pub use super::status::NodeContextCost;
    pub use super::status::TokenUsageSample;
    pub use super::status::tree_snapshot;
    pub use super::tools::MAX_SPAWN_PROMPT_BYTES;
    pub use super::tools::MAX_SPAWN_TASKS;
    pub use super::tools::MAX_SUMMARY_BYTES;
    pub use super::tools::SPINE_NAMESPACE;
    pub use super::tools::SPINE_NAMESPACE_DESCRIPTION;
    pub use super::tools::SpineTool;
    pub use super::tools::ToolCatalog;
    pub use super::tools::ToolDefinition;
    pub use super::tools::ToolValidation;
    pub use super::tools::ToolValidationError;
    pub use super::tools::ValidatedTransition;
    pub use super::tools::success_carrier;
    pub use super::tools::validate_tool;
}

#[allow(unused_imports)]
pub(crate) use host::*;

#[cfg(test)]
#[path = "context_plan_tests.rs"]
mod context_plan_tests;

#[cfg(test)]
#[path = "source_ledger_tests.rs"]
mod source_ledger_tests;

#[cfg(test)]
mod sampling_only_tests {
    use super::Message;
    use super::MessageRole;
    use super::RawBoundary;
    use super::SourceCellPayload;
    use super::SpineChar;
    use super::SpineCharParser;

    fn message(boundary: u64, role: MessageRole) -> SpineChar {
        SpineChar::Message(Message {
            boundary: RawBoundary(boundary),
            role,
            content: format!("message-{boundary}"),
        })
    }

    #[test]
    fn native_items_are_opaque_across_message_interleaving() {
        let mut parser = SpineCharParser::default();
        for character in [
            SpineChar::Opaque {
                boundary: RawBoundary(1),
            },
            message(2, MessageRole::User),
            SpineChar::Opaque {
                boundary: RawBoundary(3),
            },
        ] {
            parser.eat(character).expect("sampling source is valid");
        }
        assert_eq!(parser.stack().len(), 3);
        assert!(parser.pending_boundaries().is_empty());
    }

    #[test]
    fn parser_has_no_native_tool_group_recovery_state() {
        let mut parser = SpineCharParser::default();
        let step = parser
            .eat(SpineChar::Opaque {
                boundary: RawBoundary(7),
            })
            .expect("opaque source is valid");
        assert!(step.pending_boundaries().is_empty());
        assert_eq!(step.stack_size(), 1);
    }

    #[test]
    fn only_sampling_alphabet_reaches_the_source_ledger() {
        let mut ledger = super::SourceLedger::new(
            super::ThreadNamespace::parse("thread").unwrap(),
            super::ContextEpoch::new(1),
        )
        .unwrap();
        ledger
            .append([SpineChar::Opaque {
                boundary: RawBoundary(1),
            }])
            .unwrap();
        assert!(matches!(
            ledger.snapshot().cells()[0].payload,
            SourceCellPayload::Opaque
        ));
    }
}
