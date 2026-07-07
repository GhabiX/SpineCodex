use std::fs;
use std::path::PathBuf;

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

use crate::spine::SpineRuntime;
use crate::spine::runtime::SpineHistoryUpdate;
use crate::spine::runtime::tests::completed_toolcall;
use crate::spine::runtime::tests::function_output;
use crate::spine::runtime::tests::function_output_text;
use crate::spine::runtime::tests::ordinary_call;
use crate::spine::runtime::tests::rollout_path;
use crate::spine::runtime::tests::tool_req;
use crate::spine::runtime::tests::tool_resp;

fn spine_src(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("spine")
        .join(path)
}

fn core_src(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(path)
}

fn parser_state_src() -> String {
    fs::read_to_string(spine_src("parser/state.rs")).expect("read parser state source")
}

fn source_without_line_comments(path: PathBuf) -> String {
    fs::read_to_string(path)
        .expect("read source")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_session_and_task_sources_do_not_expose_forbidden_spine_bridge_symbols() {
    let native_sources = [
        "session/mod.rs",
        "session/session.rs",
        "session/spine_bridge.rs",
        "session/turn.rs",
        "tasks/mod.rs",
    ];
    let forbidden = [
        "RawObservationRuntime",
        "ReplayRuntime",
        "TreeSnapshotProjection",
        "TrimRuntime",
        "ToolcallRuntime",
        "SpineSessionState",
        "spine_slot.lock",
        "ToolCallEvidence::",
        "SPINE_TOOL_OPEN",
        "SPINE_TOOL_CLOSE",
        "SPINE_TOOL_NEXT",
        "SpineControlOverlay",
        "DeferredSpineToolGroup",
    ];

    for path in native_sources {
        let source = source_without_line_comments(core_src(path));
        for symbol in forbidden {
            assert!(
                !source.contains(symbol),
                "{path} must not expose forbidden Spine bridge symbol {symbol}"
            );
        }
    }
}

fn observe_ordinary_toolcall(
    runtime: &mut SpineRuntime,
    raw: &[Option<ResponseItem>],
    request: &ResponseItem,
    output: &ResponseItem,
    call_id: &str,
) {
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall(call_id, vec![tool_req(0, 0), tool_resp(1, 1)]),
            raw,
        )
        .expect("observe completed toolcall");
}

#[test]
fn observe_runtime_routes_token_shifts_through_parser_state() {
    let observe =
        fs::read_to_string(spine_src("runtime/observe.rs")).expect("read observe runtime source");
    assert!(
        !observe.contains(".shift("),
        "runtime/observe.rs must not directly shift parser tokens"
    );
    assert!(
        !observe.contains("self.parse_stack"),
        "runtime/observe.rs must not directly access live ParseStack"
    );
    assert!(
        !observe.contains("replace_parse_stack_for_runtime_transition"),
        "runtime/observe.rs must not use the generic parser replacement escape hatch"
    );
    assert!(
        !observe.contains("staged_after_token"),
        "runtime/observe.rs must stage observations as lexed batches, not raw tokens"
    );
    assert!(
        !observe.contains("append_and_shift_msg") && !observe.contains("append_and_shift_toolcall"),
        "runtime/observe.rs should not name observed batch installs as runtime token shifts"
    );
    assert!(
        !observe.contains("use crate::spine::model::SpineToken"),
        "runtime/observe.rs must not import raw SpineToken"
    );
    assert!(
        !observe.contains("LexedTokenKind") && !observe.contains("plan_control_toolcall"),
        "runtime/observe.rs must not restate ordinary toolcall token order"
    );
    assert!(
        observe.contains("consume_lexed_batch") && observe.contains("install_prepared_observe"),
        "runtime/observe.rs should consume lexed batches and install observations through parser-owned install handles"
    );
    assert!(
        observe.contains(".restore_from_checkpoint(checkpoint)")
            && !observe.contains("ParserState::from_parse_stack")
            && !observe.contains("self.parser ="),
        "runtime/observe.rs should request ParserState checkpoint restore instead of constructing or assigning live parser state directly"
    );
    assert!(
        !observe.contains("install_staged("),
        "runtime/observe.rs should not install generic staged parser state"
    );
}

#[test]
fn parser_state_documents_spine_ownership_chain() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    let parser_state = parser_state_src();
    assert!(
        parser.contains("hook -> lexer -> parser -> PS -> h(PS) -> variable host publication")
            && !parser.contains("hook -> lexer -> parser -> PS -> h(PS) -> host publication"),
        "parser facade must document the semantic ownership chain"
    );
    assert!(
        parser
            .contains("Fixed prompt prefix (`Q`) is outside `PS` and outside parser-owned `h(PS)`")
            && parser.contains("ContextManager.items")
            && parser.contains("must not treat fixed prefix as parser state"),
        "parser facade must document that fixed prefix is outside parser-owned variable context"
    );
    assert!(
        parser.contains("mod state;")
            && parser.contains("pub(in crate::spine) use state::ParserState;")
            && !parser.contains("struct ParserState")
            && parser_state.contains("struct ParserState"),
        "parser facade should wire modules and re-export ParserState, while parser/state.rs owns the state implementation"
    );
}

#[test]
fn parser_state_mutable_parse_stack_handle_is_test_only() {
    let parser_state = parser_state_src();
    assert!(
        parser_state.contains("#[cfg(test)]\n    pub(in crate::spine) fn parse_stack_mut_for_test")
            && !parser_state.contains("fn parse_stack_mut_for_runtime_transition"),
        "mutable ParserState ParseStack handle must remain test-only and not be exposed as a runtime transition API"
    );
}

#[test]
fn runtime_test_visible_context_refs_route_through_parser_state() {
    let runtime = fs::read_to_string(spine_src("runtime.rs")).expect("read runtime source");
    let parser_state = parser_state_src();
    let visible_refs = runtime
        .split("fn visible_response_context_refs_for_test")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn last_visible_response_context_index_for_test")
                .next()
        })
        .expect("runtime visible refs test helper");
    assert!(
        parser_state.contains("fn visible_response_context_refs_for_test")
            && visible_refs.contains("self.parser.visible_response_context_refs_for_test()")
            && !visible_refs.contains("self.parse_stack()"),
        "runtime test facade should read visible response refs through ParserState, not raw ParseStack"
    );
}

#[test]
fn parser_state_does_not_expose_single_token_staging_api() {
    let parser_state = parser_state_src();
    let transaction =
        fs::read_to_string(spine_src("parser/transaction.rs")).expect("read parser transaction");
    assert!(
        !parser_state.contains("fn staged_after_token("),
        "ParserState should expose batch staging, not a single-token staging API"
    );
    assert!(
        !parser_state.contains("pub(super) fn into_parse_stack(self)")
            && !transaction.contains("pub(super) fn into_parse_stack(self)")
            && transaction.contains("fn into_parse_stack_for_install(self)"),
        "parser prepared state must not expose a generic raw ParseStack escape hatch; installs should use an install-scoped consumer"
    );
}

#[test]
fn parser_state_routes_live_batches_through_one_batch_helper() {
    let parser_state = parser_state_src();
    let reducer =
        fs::read_to_string(spine_src("parser/reducer.rs")).expect("read parser reducer source");
    assert!(
        parser_state.contains("fn stage_lexed_batches")
            && reducer.contains("fn apply_lexed_batches_to_parse_stack"),
        "ParserState should keep live token-batch staging behind one parser-owned helper"
    );
    let open_install = parser_state
        .split("fn prepare_open_install(")
        .nth(1)
        .and_then(|tail| tail.split("fn close_reduced_next_child_id").next())
        .expect("prepare_open_install section");
    assert!(
        open_install.contains("stage_lexed_batches")
            && !open_install.contains("single_lexed_token")
            && !open_install.contains(".shift("),
        "open parser transactions should consume lexed batches through the shared parser helper"
    );
    let close_family = parser_state
        .split("fn prepare_close_family_install(")
        .nth(1)
        .and_then(|tail| tail.split("fn prepare_root_compact_txn").next())
        .expect("close-family parser section");
    assert!(
        close_family.contains("apply_lexed_batches_to_parse_stack")
            && !close_family.contains("single_lexed_token")
            && !close_family.contains(".shift("),
        "close/next parser transactions should consume final lexed batches through the shared parser helper"
    );
    let observe = parser_state
        .split("fn consume_lexed_batch(")
        .nth(1)
        .and_then(|tail| tail.split("fn materialize_variable_context").next())
        .expect("observe parser section");
    assert!(
        observe.contains("stage_lexed_batches") && !observe.contains("tokens.iter().cloned()"),
        "observe parser transactions should stage the whole lexed batch instead of unpacking raw tokens at the callsite"
    );
    let root_compact_probe = parser_state
        .split("fn root_compact_next_open_index_or_probe(")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("root compact probe parser section");
    assert!(
        root_compact_probe.contains("lex_compact_batch")
            && root_compact_probe.contains("stage_lexed_batches")
            && !root_compact_probe.contains("probe_parse_stack")
            && !root_compact_probe.contains(".shift("),
        "root compact parser probe should stage a lexer batch instead of shifting a raw compact token"
    );
}

#[test]
fn runtime_replay_routes_token_consumption_through_parser_state() {
    let replay =
        fs::read_to_string(spine_src("runtime/replay.rs")).expect("read runtime replay source");
    assert!(
        !replay.contains("apply_replay_event_to_parse_stack"),
        "runtime/replay.rs must not directly apply replay events to ParseStack"
    );
    assert!(
        !replay.contains("parse_stack_from_events_with_forced_events"),
        "runtime/replay.rs must not rebuild ParseStack through parse_stack replay helpers"
    );
    assert!(
        replay.contains("ParserState::from_replay_events_with_initial_and_forced_events")
            && !replay.contains("parser.apply_replay_event"),
        "runtime replay should route replay token consumption and initial-state continuation through ParserState"
    );
    assert!(
        replay.contains(") -> Result<ParserState, SpineError>")
            && !replay.contains(") -> Result<ParseStack, SpineError>"),
        "runtime replay should return parser-owned state instead of exposing raw ParseStack"
    );
}

#[test]
fn parse_stack_replay_is_not_a_token_consumer() {
    let replay =
        fs::read_to_string(spine_src("parse_stack/replay.rs")).expect("read parse_stack replay");
    let parse_stack = fs::read_to_string(spine_src("parse_stack.rs")).expect("read parse_stack");
    assert!(
        !replay.contains(".shift("),
        "parse_stack/replay.rs should adapt replay events to parser inputs, not consume tokens"
    );
    assert!(
        !replay.contains("fn apply_replay_event_to_parse_stack")
            && !replay.contains("fn parse_stack_from_events_with_forced_events"),
        "replay event loops belong in ParserState, not parse_stack replay helpers"
    );
    assert!(
        !parse_stack.contains("pub(in crate::spine) use replay::event_to_token")
            && !parse_stack.contains("pub(in crate::spine) use replay::apply_metadata_event")
            && !parse_stack.contains("mod replay;"),
        "parse_stack must not export replay token adapters; parser owns replay event adaptation"
    );
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser");
    let parser_replay =
        fs::read_to_string(spine_src("parser/replay.rs")).expect("read parser replay");
    let parser_state = fs::read_to_string(spine_src("parser/state.rs")).expect("read parser state");
    assert!(
        parser.contains("mod replay;")
            && !parser.contains("fn apply_replay_event(")
            && parser_replay.contains("fn replay_event_to_lexed_batch(")
            && parser_replay.contains("fn apply_replay_metadata_event(")
            && parser_replay.contains("impl ParserState")
            && parser_replay.contains(
                "pub(in crate::spine) fn from_replay_events_with_initial_and_forced_events("
            )
            && parser_replay.contains("fn from_replay_events_with_forced_events(")
            && parser_replay.contains("fn apply_replay_event(")
            && !parser_replay
                .contains("pub(in crate::spine) fn from_replay_events_with_forced_events(")
            && !parser_replay.contains("pub(in crate::spine) fn apply_replay_event("),
        "parser replay module should expose only the unified replay facade while keeping replay event helpers private"
    );
    assert!(
        parser_state.contains("pub(in crate::spine::parser) fn from_parse_stack(")
            && !parser_state.contains("pub(in crate::spine) fn from_parse_stack(")
            && parser_replay.contains("Self::from_parse_stack(initial.clone())"),
        "ParserState::from_parse_stack should stay parser-private for replay continuation and must not be callable from runtime/session"
    );
    let replay_apply = parser_replay
        .split("fn apply_replay_event(")
        .nth(1)
        .and_then(|tail| tail.split("fn replay_event_to_lexed_batch").next())
        .expect("parser replay apply section");
    assert!(
        replay_apply.contains("replay_event_to_lexed_batch")
            && replay_apply.contains("stage_lexed_batches")
            && !replay_apply.contains(".shift("),
        "parser replay should stage lexer batches instead of shifting raw replay tokens"
    );
    assert!(
        !parser_replay.contains(".shift("),
        "parser replay adapter must return lexed batches or metadata effects without directly shifting parser state"
    );
}

#[test]
fn parse_stack_mutation_helpers_stay_parser_scoped() {
    let parse_stack = fs::read_to_string(spine_src("parse_stack.rs")).expect("read parse_stack");
    let parse_stack_context =
        fs::read_to_string(spine_src("parse_stack/context.rs")).expect("read parse stack context");
    let parse_stack_cursor =
        fs::read_to_string(spine_src("parse_stack/cursor.rs")).expect("read parse stack cursor");
    for helper in ["shift", "shift_pending_close", "shift_pending_compact"] {
        assert!(
            parse_stack.contains(&format!("pub(super) fn {helper}(")),
            "ParseStack::{helper} should stay scoped to spine parser/reducer modules"
        );
        for forbidden_visibility in [
            format!("pub(crate) fn {helper}("),
            format!("pub(in crate::spine) fn {helper}("),
        ] {
            assert!(
                !parse_stack.contains(&forbidden_visibility),
                "ParseStack::{helper} must not be widened to a runtime-callable API"
            );
        }
    }

    let reducer = fs::read_to_string(spine_src("parser/reducer.rs")).expect("read parser reducer");
    assert!(
        reducer.contains("parse_stack.shift(token, archive)?"),
        "parser reducer should remain the live token-batch consumer for ParseStack::shift"
    );
    assert!(
        parse_stack.contains("mod context;")
            && parse_stack.contains("context::validate_shifted_symbol_context_indices")
            && !parse_stack.contains("fn collect_symbol_response_context_refs")
            && parse_stack_context.contains("fn collect_symbol_response_context_refs")
            && parse_stack_context
                .contains("pub(super) fn validate_shifted_symbol_context_indices"),
        "ParseStack visible context-index helpers should stay split into parse_stack/context.rs as parser reducer internals"
    );
    assert!(
        parse_stack.contains("mod cursor;")
            && !parse_stack.contains("fn current_cursor_id(")
            && parse_stack_cursor.contains("fn current_cursor_id(")
            && parse_stack_cursor.contains("fn set_live_open_context_baseline("),
        "ParseStack cursor/open query helpers should stay split into parse_stack/cursor.rs as parser reducer internals"
    );
    let parse_stack_task_tree = fs::read_to_string(spine_src("parse_stack/task_tree.rs"))
        .expect("read parse stack task tree");
    assert!(
        parse_stack.contains("mod task_tree;")
            && !parse_stack.contains("fn reduce_task_tree(")
            && parse_stack_task_tree.contains("fn reduce_task_tree(")
            && parse_stack_task_tree.contains("fn shift_pending_close("),
        "ParseStack close/task-tree helpers should stay split into parse_stack/task_tree.rs as parser reducer internals"
    );

    for path in [
        "runtime/observe.rs",
        "runtime/commit.rs",
        "runtime/root_compact.rs",
        "runtime/load.rs",
        "runtime/pending.rs",
        "runtime/session_state.rs",
        "runtime/session_state/root_compact_session.rs",
        "runtime/session_state/completed_toolcall_session.rs",
    ] {
        let source = source_without_line_comments(spine_src(path));
        for forbidden in [
            ".shift(",
            "shift_pending_close(",
            "shift_pending_compact(",
            "apply_prevalidated",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} must not bypass ParserState by calling ParseStack mutation helper {forbidden}"
            );
        }
    }
}

#[test]
fn parse_stack_stays_out_of_host_publication_boundary() {
    for path in [
        "parse_stack.rs",
        "parse_stack/context.rs",
        "parse_stack/cursor.rs",
        "parse_stack/task_tree.rs",
        "parse_stack/tree.rs",
    ] {
        let source = fs::read_to_string(spine_src(path)).expect("read parse stack source");
        for forbidden in [
            "codex_protocol::models::ResponseItem",
            "ParserPublication",
            "ContextManager",
            "SpineHistoryUpdate",
            "SpineHostEffect",
            "build_checkpoint",
            "materialize_variable_context",
            "materialize_history",
            "replacement_history",
            "host publication",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} should remain a reducer/tree-read layer and must not depend on host publication or h(PS) materialization through {forbidden}"
            );
        }
    }
}

#[test]
fn hook_and_session_bridge_stay_out_of_parser_token_boundary() {
    for (label, source) in [
        (
            "session/spine_bridge.rs",
            source_without_line_comments(core_src("session/spine_bridge.rs")),
        ),
        (
            "spine/hooks.rs",
            source_without_line_comments(spine_src("hooks.rs")),
        ),
    ] {
        for forbidden in [
            "use crate::spine::lexer",
            "use crate::spine::model::SpineToken",
            "LexedTokenBatch",
            "ControlIntent",
            "RootCompactPlan",
            "SpineLedgerEvent",
            "ParserState",
            "ParserPublication",
            "ParseStack",
            "shift_pending_",
            "apply_prevalidated",
            "replace_parse_stack",
            "render_parse_stack",
            "materialize_parse_stack",
            "parse_stack_mut",
            ".parse_stack(",
            ".shift(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} must stay on the host bridge/hook side and must not consume parser tokens or mutate parser state through {forbidden}"
            );
        }
    }
}

#[test]
fn lexer_and_token_model_stay_out_of_parser_publication_boundary() {
    for (label, source) in [
        (
            "spine/lexer.rs",
            source_without_line_comments(spine_src("lexer.rs")),
        ),
        (
            "spine/model/token.rs",
            source_without_line_comments(spine_src("model/token.rs")),
        ),
    ] {
        for forbidden in [
            "use crate::spine::parser",
            "use crate::spine::parse_stack",
            "use crate::spine::render",
            "use crate::spine::runtime",
            "use crate::session",
            "ParserState",
            "ParserPublication",
            "ParseStack",
            "ContextManager",
            "SpineHostEffect",
            "SpineHostEffects",
            "HistoryPublication",
            "SpinePrepared",
            "render_parse_stack",
            "materialize_parse_stack",
            "materialize_variable_context",
            "build_checkpoint",
            "compact_checkpoint",
            "replace_parse_stack",
            "shift_pending_",
            "apply_prevalidated",
            ".shift(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} must remain a lexer/token vocabulary layer and must not depend on parser state, h(PS), host publication, or runtime/session boundaries through {forbidden}"
            );
        }
    }
}

#[test]
fn archive_and_store_stay_out_of_parser_publication_boundary() {
    for (label, source) in [
        (
            "spine/archive.rs",
            source_without_line_comments(spine_src("archive.rs")),
        ),
        (
            "spine/store.rs",
            source_without_line_comments(spine_src("store.rs")),
        ),
    ] {
        for forbidden in [
            "use crate::spine::parser",
            "use crate::spine::parse_stack",
            "use crate::spine::runtime",
            "use crate::session",
            "ParserState",
            "ParserPublication",
            "ParseStack",
            "ContextManager",
            "SpineHostEffect",
            "SpineHostEffects",
            "HistoryPublication",
            "SpinePrepared",
            "build_checkpoint",
            "compact_checkpoint",
            "materialize_variable_context",
            "materialize_history",
            "replace_parse_stack",
            "shift_pending_",
            "apply_prevalidated",
            "session_bridge",
            ".shift(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} must remain an archive/store layer and must not depend on parser state, host publication, or runtime/session boundaries through {forbidden}"
            );
        }
    }
}

#[test]
fn render_stays_out_of_host_publication_boundary() {
    for (label, source) in [(
        "spine/render.rs",
        source_without_line_comments(spine_src("render.rs")),
    )] {
        for forbidden in [
            "use crate::spine::runtime",
            "use crate::session",
            "ContextManager",
            "SpineHostEffect",
            "SpineHostEffects",
            "SpineHistoryUpdate",
            "HistoryPublication",
            "ParserPublication",
            "SpinePrepared",
            "replace_history",
            "install_prepared",
            "session_bridge",
            "host publication",
            "runtime::",
            "session::",
            ".shift(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} must remain a parser-side projection/checkpoint layer and must not depend on host publication or runtime/session boundaries through {forbidden}"
            );
        }
    }
}

#[test]
fn runtime_load_checkpoint_replay_routes_through_parser_state() {
    let load = fs::read_to_string(spine_src("runtime/load.rs")).expect("read runtime load source");
    assert!(
        !load.contains("lex_init_event_token")
            && !load.contains("lex_open_event_token")
            && !load.contains("_init_token")
            && !load.contains("_open_token")
            && !load.contains(".into_single("),
        "runtime/load.rs bootstrap should append ledger events only and must not consume lexer tokens"
    );
    assert!(
        load.contains("lex_init_event(") && load.contains("lex_open_event("),
        "runtime/load.rs bootstrap should use event-only lexer APIs"
    );
    assert!(
        !load.contains("parse_stack_from_events_with_forced_events"),
        "runtime/load.rs must not rebuild checkpoint ParseStack through parse_stack replay helpers"
    );
    assert!(
        load.contains("ParserState::from_replay_events_with_initial_and_forced_events")
            && !load.contains("ParserState::from_replay_events_with_forced_events"),
        "checkpoint prefix replay should route through the unified ParserState replay facade"
    );
    assert!(
        !load.contains("let parse_stack = replay_from_events(")
            && !load.contains("ParserState::from_parse_stack(parse_stack)")
            && !load.contains(".into_parse_stack()"),
        "runtime/load.rs should keep replay output as ParserState, not unwrap and rewrap ParseStack"
    );
    assert!(
        load.contains(".validate_checkpoint_parse_stack(checkpoint)"),
        "checkpoint ParseStack equivalence should be checked behind ParserState"
    );
}

#[test]
fn runtime_accounting_routes_open_baseline_mutation_through_parser_state() {
    let accounting = fs::read_to_string(spine_src("runtime/accounting.rs"))
        .expect("read runtime accounting source");
    assert!(
        !accounting.contains("parse_stack_mut_for_runtime_transition"),
        "runtime/accounting.rs must not take a mutable ParseStack handle"
    );
    assert!(
        !accounting.contains(".parse_stack()"),
        "runtime/accounting.rs must not read ParseStack through the raw parser handle"
    );
    assert!(
        accounting.contains("self.parser")
            && accounting.contains(".set_live_open_context_baseline("),
        "runtime accounting should route live open baseline updates through ParserState"
    );
    assert!(
        !accounting.contains(".render_tree()")
            && !accounting.contains(".render_tree_with_context_annotations(")
            && !accounting.contains(".tree_snapshot_nodes(")
            && !accounting.contains(".current_cursor_id()"),
        "runtime/accounting.rs must not build tree render/snapshot output from raw ParseStack"
    );
    assert!(
        accounting.contains(".render_tree_with_context_annotations_and_memory_context_accounting(")
            && accounting.contains(".build_tree_snapshot_with_memory_context_accounting("),
        "runtime accounting tree publication should route parser tree reads through ParserState"
    );
}

#[test]
fn runtime_source_plan_routes_parse_stack_reads_through_parser_state() {
    let source_plan = fs::read_to_string(spine_src("runtime/source_plan.rs"))
        .expect("read runtime source_plan source");
    assert!(
        !source_plan.contains(".parse_stack()"),
        "runtime/source_plan.rs must not read ParseStack through the raw parser handle"
    );
    assert!(
        !source_plan.contains("use crate::spine::model::Symbol"),
        "runtime/source_plan.rs must not inspect parser symbols directly"
    );
    assert!(
        source_plan.contains(".current_open_suffix_nodes_cloned()")
            && source_plan.contains(".current_open_has_nodes()"),
        "runtime source-plan construction should route current-open queries through ParserState"
    );
}

#[test]
fn runtime_commit_routes_current_open_queries_through_parser_state() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains(".parse_stack().current_open_has_nodes()"),
        "runtime/commit.rs must not query current-open node state through the raw parser handle"
    );
    assert!(
        !commit.contains(".parse_stack()"),
        "runtime/commit.rs must not read ParseStack through the raw parser handle"
    );
    assert!(
        commit.contains(".current_open_has_nodes()"),
        "runtime commit should route current-open node queries through ParserState"
    );
}

#[test]
fn runtime_routes_open_cursor_reads_through_parser_state() {
    let runtime = fs::read_to_string(spine_src("runtime.rs")).expect("read runtime source");
    let current_open_index = runtime
        .split("#[cfg(test)]\n    pub(crate) fn current_open_index")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("test-only current_open_index section");
    assert!(
        current_open_index.contains("self.parser.current_open_index()")
            && !current_open_index.contains(".parse_stack()"),
        "test-only runtime current_open_index should delegate parser cursor reads to ParserState"
    );
    let parser_state = parser_state_src();
    assert!(
        parser_state.contains("#[cfg(test)]\n    pub(in crate::spine) fn current_open_index"),
        "ParserState current_open_index should stay test-only; production publication checks should use prepared proofs"
    );
    let current_close_open_meta = runtime
        .split("fn current_close_open_meta")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("current_close_open_meta section");
    assert!(
        current_close_open_meta.contains("self.parser.current_close_open_meta()")
            && !current_close_open_meta.contains(".parse_stack()"),
        "runtime close-open metadata checks should delegate parser cursor reads to ParserState"
    );
}

#[test]
fn parser_state_owns_visible_response_context_index_reads() {
    let parser_state = parser_state_src();
    let parser_visible_index = parser_state
        .split("fn last_visible_response_context_index(")
        .nth(1)
        .and_then(|tail| tail.split("fn current_open_suffix_nodes_cloned").next())
        .expect("ParserState visible response context index section");
    assert!(
        parser_visible_index.contains("self.parse_stack.last_visible_response_context_index()"),
        "ParserState should be the facade for visible response context index reads"
    );

    for path in [
        "runtime.rs",
        "runtime/observe.rs",
        "runtime/commit.rs",
        "runtime/root_compact.rs",
        "runtime/session_state.rs",
    ] {
        let source = fs::read_to_string(spine_src(path)).expect("read spine runtime source");
        assert!(
            !source.contains(".parse_stack().last_visible_response_context_index()")
                && !source.contains("parse_stack.last_visible_response_context_index()"),
            "{path} must route visible response context index reads through ParserState"
        );
    }
}

#[test]
fn runtime_commit_routes_open_token_staging_through_parser_state() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains("self.parser.parse_stack().next_child_id()"),
        "runtime/commit.rs must not query open child ids through the raw parser handle"
    );
    assert!(
        !commit.contains("use crate::spine::model::SpineToken"),
        "runtime/commit.rs must not import raw SpineToken"
    );
    assert!(
        !commit.contains("LexedTokenKind") && !commit.contains("plan_control_toolcall"),
        "runtime/commit.rs must not restate open/close/next parser-visible token order"
    );
    assert!(
        !commit.contains("completed_toolcall_parts("),
        "runtime/commit.rs must not unwrap completed toolcalls into raw event/token pairs"
    );
    assert!(
        !commit.contains("lex_open_token("),
        "runtime/commit.rs must not request raw open tokens from lexer"
    );
    assert!(
        !commit.contains("lex_close_event_token("),
        "runtime/commit.rs must not request raw close event/token pairs from lexer"
    );
    assert!(
        !commit.contains("staged_parse_stack.shift("),
        "runtime/commit.rs open staging must not directly shift parser tokens"
    );
    assert!(
        commit.contains("self.parser.next_child_id()")
            && commit.contains(".prepare_open_install(&open_lexed"),
        "runtime open commit should route child id and token staging through ParserState"
    );
    let open_with_toolcall_install = commit
        .split(".prepare_open_install(\n                &open_lexed,\n                Some(&toolcall_lexed)")
        .nth(1)
        .and_then(|tail| {
            tail.split("self.append_trim_candidates_for_completed_toolcall")
                .next()
        })
        .expect("open-with-toolcall install section");
    assert!(
        open_with_toolcall_install.contains("SpinePreparedCommit::open_with_toolcall(")
            && open_with_toolcall_install.contains("parser_install.into_commit_install()")
            && !open_with_toolcall_install.contains("replace_parse_stack_for_runtime_transition"),
        "runtime open-with-toolcall should return a parser-owned prepared commit install handle"
    );
    let open_without_toolcall_install = commit
        .split(".prepare_open_install(&open_lexed, None")
        .nth(1)
        .and_then(|tail| tail.split("Ok(SpinePreparedCommit").next())
        .expect("open-without-toolcall install section");
    assert!(
        open_without_toolcall_install.contains(".install_prepared_open(parser_install)")
            && !open_without_toolcall_install
                .contains("replace_parse_stack_for_runtime_transition"),
        "runtime open-without-toolcall should install parser-owned open handle through ParserState"
    );
}

#[test]
fn runtime_pending_does_not_depend_on_lexer_token_plans() {
    let pending =
        fs::read_to_string(spine_src("runtime/pending.rs")).expect("read runtime pending source");
    assert!(
        !pending.contains("plan_control_toolcall")
            && !pending.contains("LexedTokenKind")
            && !pending.contains("ControlToolCallPlan"),
        "runtime/pending.rs must not query lexer token-plan APIs to classify pending transitions"
    );
    assert!(
        pending.contains("fn is_close_like(&self) -> bool"),
        "runtime pending transition classification should stay local to PendingTransition"
    );
}

#[test]
fn runtime_commit_routes_close_family_staging_through_parser_state() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains("shift_pending_close"),
        "runtime/commit.rs must not directly stage spine.close parser tokens"
    );
    assert!(
        !commit.contains("apply_prevalidated_task_tree_reduction"),
        "runtime/commit.rs must not directly apply close task-tree reductions"
    );
    assert!(
        !commit.contains("final_parse_stack.shift("),
        "runtime/commit.rs must not directly shift close-family final parser tokens"
    );
    assert!(
        commit.contains(".prepare_close_family_install(")
            && commit.contains(".close_reduced_next_child_id(")
            && commit.contains(".prepare_current_task_tree_reduction("),
        "runtime close/next commit should route staged parser reductions through ParserState"
    );
    assert!(
        !commit.contains(".close_family_staged_parse_stacks("),
        "runtime close/next commit should not depend on parser APIs named after raw staged ParseStacks"
    );
    let parser_state = parser_state_src();
    assert!(
        !parser_state.contains("fn close_family_staged_parse_stacks("),
        "parser close/next API should expose prepared install semantics, not raw staged ParseStack semantics"
    );
}

#[test]
fn runtime_commit_routes_close_installs_through_named_parser_methods() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains("replace_parse_stack_for_runtime_transition"),
        "runtime/commit.rs must not use the generic parser replacement escape hatch"
    );
    assert!(
        commit.contains(".install_pending_close_after_side_effect_failure(")
            && commit.contains(".install_prepared_commit("),
        "runtime close/next commit should install pending/final parser states through named ParserState methods"
    );
    assert!(
        !commit.contains("pending_close_parse_stack"),
        "runtime close/next commit should not name or hold pending raw parser state"
    );
    let parser_state = parser_state_src();
    let transaction = fs::read_to_string(spine_src("parser/transaction.rs"))
        .expect("read parser transaction source");
    assert!(
        transaction.contains("ParserCommitPendingInstall")
            && transaction.contains("ParserCommitPreparedInstall")
            && parser_state.contains("fn install_pending_close_after_side_effect_failure")
            && transaction.contains("ParserCommitInstall"),
        "parser should expose parser-owned close/next prepared, pending, and final install handles"
    );
    let parser_commit_prepared_install_carrier = transaction
        .split("struct ParserCommitPreparedInstall")
        .nth(1)
        .and_then(|tail| tail.split("struct ParserCommitPendingInstall").next())
        .expect("ParserCommitPreparedInstall carrier");
    assert!(
        !transaction.contains("struct ParserPreparedInstallPair<PendingInstall, FinalInstall>")
            && parser_commit_prepared_install_carrier
                .contains("pending_install: ParserCommitPendingInstall")
            && parser_commit_prepared_install_carrier
                .contains("final_install: ParserCommitInstall")
            && !parser_commit_prepared_install_carrier.contains("install_pair:"),
        "parser close/next prepared install should directly hold parser-owned pending/final install handles without the obsolete shared pair wrapper"
    );
    assert!(
        commit.contains(".install_pending_close_after_side_effect_failure(")
            && commit.contains("&parser_install")
            && commit.contains("parser_install,\n            completed_toolcall")
            && !commit.contains(".pending_install()")
            && !commit.contains(".into_final_install()")
            && !commit.contains("let (pending_parser_install, parser_install)"),
        "runtime close/next commit should pass parser prepared installs to parser-owned install methods, not inspect pending/final internals"
    );
    assert!(
        transaction.contains("final_state: ParserPreparedState")
            && transaction.contains("pending_state: ParserPreparedState")
            && !transaction.contains("final_parse_stack: ParserPreparedState")
            && !transaction.contains("pending_parse_stack: ParserPreparedState"),
        "parser install handles should name prepared parser state, not raw parse stack fields"
    );
    assert!(
        parser_state.contains("fn install_prepared_state(&mut self, state: ParserPreparedState)")
            && !parser_state.contains("fn replace_parse_stack_for_runtime_transition"),
        "parser live state replacement should be a parser-owned install operation, not a runtime transition escape hatch"
    );
    let parser_commit_pending_install = transaction
        .split("impl ParserCommitPendingInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserCommitPreparedInstall").next())
        .expect("ParserCommitPendingInstall impl block");
    assert!(
        !transaction.contains("fn into_final_parse_stack(")
            && !transaction.contains("fn into_pending_parse_stack(")
            && !parser_commit_pending_install.contains("fn into_pending_state("),
        "parser install handles should not expose raw-state consumers"
    );
    assert!(
        parser_commit_pending_install.contains("fn pending_state(&self) -> &ParserPreparedState"),
        "close pending install should expose parser prepared state only to parser-owned install helpers"
    );
    assert!(
        transaction.contains("pub(in crate::spine::parser) struct ParserCommitPendingInstall")
            && !transaction.contains("pub(in crate::spine) struct ParserCommitPendingInstall")
            && transaction.contains("pub(in crate::spine) struct ParserObserveInstall")
            && transaction.contains("pub(in crate::spine) struct ParserOpenInstall"),
        "parser-only pending install helpers should not be visible outside the parser module while observe/open remain opaque runtime handles"
    );
    let parser_commit_prepared_install = transaction
        .split("impl ParserCommitPreparedInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserOpenInstall").next())
        .expect("ParserCommitPreparedInstall impl block");
    assert!(
        parser_commit_prepared_install.contains("fn pending_state(&self) -> &ParserPreparedState")
            && parser_commit_prepared_install
                .contains("fn into_final_state(self) -> ParserPreparedState")
            && !parser_commit_prepared_install.contains("fn pending_install(&self)")
            && !parser_commit_prepared_install.contains("fn into_final_install(self)"),
        "close prepared install should expose parser prepared states to ParserState without leaking pending/final install handles"
    );
    let parser_install_methods = parser_state
        .split("fn install_prepared_state(&mut self, state: ParserPreparedState)")
        .nth(1)
        .and_then(|tail| tail.split("fn stage_lexed_batches").next())
        .expect("parser install methods section");
    let restore_from_checkpoint = parser_state
        .split("fn restore_from_checkpoint(&mut self, checkpoint: &SpineCheckpoint)")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("parser restore_from_checkpoint section");
    assert!(
        parser_install_methods.matches("self.parse_stack =").count() == 1
            && parser_install_methods
                .contains("self.install_prepared_state(install.pending_state().clone());"),
        "all parser pending/final installs should route live ParseStack assignment through install_prepared_state"
    );
    assert!(
        restore_from_checkpoint.contains("self.install_prepared_state(")
            && !restore_from_checkpoint.contains("self.parse_stack ="),
        "checkpoint restore should also route live ParseStack assignment through install_prepared_state"
    );
    assert!(
        !commit.contains(".install_prepared_commit_final_parse_stack("),
        "runtime close/next final install should use the parser-owned commit install handle"
    );
}

#[test]
fn runtime_commit_routes_open_with_toolcall_publication_through_prepared_commit() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        commit.contains("SpinePreparedCommit::open_with_toolcall("),
        "open-with-toolcall should be represented as a prepared parser commit"
    );
    assert!(
        !commit.contains("self.parser.install_prepared_open(parser_install);\n            self.append_trim_candidates_for_completed_toolcall"),
        "open-with-toolcall must not install parser state before publication side effects"
    );
    let publication_parts = commit
        .split("fn commit_host_history_update")
        .nth(1)
        .and_then(|tail| tail.split("fn prepare_close_commit").next())
        .expect("commit publication history update function");
    assert!(
        publication_parts
            .contains("prepared_commit.apply_full_variable_context_publication_update("),
        "open-with-toolcall publication should ask the prepared install carrier to build host-history updates from variable h(PS)"
    );
    assert!(
        !publication_parts.contains("parser_install.full_context_publication_update("),
        "prepared commit publication should not use full-context naming that could include fixed prefix"
    );
    assert!(
        !publication_parts.contains("ParserPublicationUpdate::new("),
        "runtime commit publication should not construct full-context parser publication updates directly"
    );
    assert!(
        !publication_parts.contains("SpinePreparedCommitInstall::parser_install")
            && !publication_parts.contains("commit.parser_install")
            && !publication_parts.contains("SpinePreparedCommit::parser_install"),
        "runtime commit publication should not borrow parser install out of the prepared install carrier"
    );
}

#[test]
fn parser_commit_install_materializes_publication_through_prepared_state() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    let transaction = fs::read_to_string(spine_src("parser/transaction.rs"))
        .expect("read parser transaction source");
    let publication = fs::read_to_string(spine_src("parser/publication.rs"))
        .expect("read parser publication source");
    let parser_commit_install = transaction
        .split("impl ParserCommitInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserCommitPendingInstall").next())
        .expect("ParserCommitInstall impl block");
    let full_variable_context_publication_update = parser_commit_install
        .split("fn full_variable_context_publication_update")
        .nth(1)
        .expect("full variable context publication update method");
    assert!(
        full_variable_context_publication_update
            .contains("self.final_state.full_variable_context_publication_update("),
        "prepared commit publication should delegate full h(PS) publication through ParserPreparedState"
    );
    assert!(
        !full_variable_context_publication_update
            .contains("render_parse_stack_to_context_with_trim_projection("),
        "prepared commit publication must not bypass the parser-owned variable context helper"
    );
    assert!(
        !parser_commit_install.contains("fn full_context_publication_update("),
        "prepared commit publication API should name variable context explicitly"
    );
    assert!(
        !parser.contains("fn variable_context_from_state(")
            && publication.contains("fn variable_context_from_state(")
            && publication
                .contains("render_parse_stack_to_context_with_trim_projection(parse_stack"),
        "parser publication module should keep the internal helper for PS -> h(PS) variable context projection"
    );
    assert!(
        publication.contains("fn full_variable_context_publication_update_from_state")
            && publication
                .matches("fn full_variable_context_publication_update_from_state")
                .count()
                == 1
            && !publication
                .contains("fn full_variable_context_host_history_update_from_parse_stack")
            && publication.contains("Ok(full_variable_context_publication_update("),
        "full h(PS) publication update construction should be centralized behind one parser-private helper"
    );
    let full_variable_context_publication_update = publication
        .split("fn full_variable_context_publication_update(")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn full_variable_context_publication_update_from_state")
                .next()
        })
        .expect("full variable context publication helper");
    assert!(
        full_variable_context_publication_update.contains("variable_context: Vec<ResponseItem>")
            && !full_variable_context_publication_update
                .contains("materialized: Vec<ResponseItem>")
            && !full_variable_context_publication_update.contains("if materialized.as_slice()"),
        "parser full h(PS) publication helper should name its payload variable_context"
    );
}

#[test]
fn runtime_commit_does_not_structurally_project_ordinary_already_recorded_toolcall() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    let publication_parts = commit
        .split("fn commit_host_history_update")
        .nth(1)
        .and_then(|tail| tail.split("fn prepare_close_commit").next())
        .expect("commit publication history update function");
    assert!(
        !publication_parts.contains("self.materialize_variable_context_for_test("),
        "runtime/commit.rs must not materialize h(PS) directly while preparing toolcall projection publication"
    );
    assert!(
        !publication_parts.contains("ordinary_already_recorded_toolcall_host_update(")
            && !publication_parts.contains(".ordinary_body_projection_publication_update(")
            && !publication_parts.contains("self.parser.full_variable_context_publication_update("),
        "ordinary already-recorded trim publication must not use parser h(PS) publication; trim body changes are local host patches"
    );
    assert!(
        publication_parts.contains("let Some(prepared_commit) = prepared_commit else")
            && publication_parts.contains("return Ok(None);"),
        "ordinary already-recorded toolcalls without a prepared structural commit should return no parser publication"
    );
}

#[test]
fn ordinary_already_recorded_toolcall_publication_returns_no_host_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let history_items = vec![
        ordinary_call("shell_command", "ordinary"),
        function_output("ordinary"),
    ];
    let tool_resp_item = history_items[1].clone();
    let raw = vec![
        Some(history_items[0].clone()),
        Some(history_items[1].clone()),
    ];
    observe_ordinary_toolcall(
        &mut runtime,
        &raw,
        &history_items[0],
        &history_items[1],
        "ordinary",
    );

    let publication = runtime
        .prepare_commit_publication(
            "ordinary",
            None,
            &tool_resp_item,
            true,
            &raw,
            &history_items,
            |_call_id, _operation, _replacement_start, _old, _new| {
                panic!("ordinary already-recorded toolcall must not build host update")
            },
        )
        .expect("prepare ordinary publication");

    assert!(
        !runtime.install_commit_publication(publication),
        "ordinary already-recorded toolcall publication should have no prepared install"
    );
}

#[test]
fn ordinary_already_recorded_toolcall_allows_coordinate_preserving_body_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "ordinary-trim");
    let output = function_output_text("ordinary-trim", &trim_candidate_text("abcdefg "));
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    observe_ordinary_toolcall(&mut runtime, &raw, &request, &output, "ordinary-trim");
    runtime
        .slice_tool_response_head("trim_0", 7, &raw)
        .expect("slice output");
    let history_items = vec![request.clone(), output.clone()];
    let tool_resp_item = output.clone();

    let mut publication = runtime
        .prepare_commit_publication(
            "ordinary-trim",
            None,
            &tool_resp_item,
            true,
            &raw,
            &history_items,
            |call_id, operation, suffix_start, expected_history, replacement| SpineHistoryUpdate {
                call_id: call_id.to_string(),
                operation,
                suffix_start,
                expected_history,
                replacement,
                reference_context_item: None,
            },
        )
        .expect("prepare ordinary trim local patch publication");

    assert!(
        publication.take_pre_apply_host_history_update().is_none(),
        "ordinary trim body changes must be delivered by TrimBodyUpdate, not parser publication"
    );
}

#[test]
fn ordinary_body_projection_with_fixed_prefix_returns_no_host_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let fixed_prefix = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fixed developer prefix".to_string(),
        }],
        phase: None,
    };
    let request = ordinary_call("shell_command", "ordinary-fixed-prefix-trim");
    let output = function_output_text(
        "ordinary-fixed-prefix-trim",
        &trim_candidate_text("abcdefg "),
    );
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    observe_ordinary_toolcall(
        &mut runtime,
        &raw,
        &request,
        &output,
        "ordinary-fixed-prefix-trim",
    );
    runtime
        .slice_tool_response_head("trim_0", 7, &raw)
        .expect("slice output");
    let history_items = vec![fixed_prefix.clone(), request.clone(), output.clone()];

    let mut publication = runtime
        .prepare_commit_publication(
            "ordinary-fixed-prefix-trim",
            None,
            &output,
            true,
            &raw,
            &history_items,
            |call_id, operation, suffix_start, expected_history, replacement| SpineHistoryUpdate {
                call_id: call_id.to_string(),
                operation,
                suffix_start,
                expected_history,
                replacement,
                reference_context_item: None,
            },
        )
        .expect("prepare ordinary trim local patch publication with fixed prefix");

    assert!(
        publication.take_pre_apply_host_history_update().is_none(),
        "fixed-prefix ordinary trim body changes must still be local patches, not full-history projection"
    );
}

#[test]
fn ordinary_already_recorded_toolcall_with_extra_history_returns_no_host_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "ordinary-hole");
    let output = function_output("ordinary-hole");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    observe_ordinary_toolcall(&mut runtime, &raw, &request, &output, "ordinary-hole");
    let history_items = vec![request.clone(), output.clone(), function_output("extra")];
    let tool_resp_item = output.clone();

    let mut publication = runtime
        .prepare_commit_publication(
            "ordinary-hole",
            None,
            &tool_resp_item,
            true,
            &raw,
            &history_items,
            |_, _, _, _, _| (),
        )
        .expect("ordinary already-recorded trim local patch publication");

    assert!(
        publication.take_pre_apply_host_history_update().is_none(),
        "ordinary already-recorded toolcalls without prepared structure must not attempt parser projection even when host history has extra items"
    );
}

#[test]
fn runtime_commit_delegates_parser_publication_plan_application_to_prepared_carrier() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    let publication_parts = commit
        .split("fn commit_host_history_update")
        .nth(1)
        .and_then(|tail| tail.split("fn prepare_close_commit").next())
        .expect("commit publication history update function");
    assert!(
        publication_parts.contains(".apply_variable_context_publication_update(")
            && !publication_parts.contains(".apply_publication_history_update("),
        "runtime commit publication should delegate parser publication plan application to the prepared parser carrier"
    );
    assert!(
        !publication_parts.contains("plan.history_update(")
            && !publication_parts.contains("history_update_with_host_boundaries(")
            && !publication_parts.contains("prepared.publication_plan")
            && !publication_parts.contains("publication_plan.as_ref()")
            && !publication_parts.contains(".has_publication_plan("),
        "runtime commit publication must not borrow, query, or apply parser publication plans directly"
    );
    assert!(
        !publication_parts.contains("SpinePreparedPublicationUpdate")
            && publication_parts.contains("let mut build_update = Some(build_update)")
            && publication_parts.contains("return Ok(Some(update));"),
        "runtime commit should not branch on a separate prepared-publication enum"
    );
    assert!(
        !publication_parts.contains("update.into_history_update("),
        "runtime commit fallback should not convert parser publication updates directly"
    );
    assert!(
        !publication_parts.contains("plan.replacement_prefix")
            && !publication_parts.contains("plan.preserve_host_history_from")
            && !publication_parts.contains("plan.append_current_tool_response_if_missing")
            && !publication_parts.contains("plan.atomic_mutable_context_segments()"),
        "runtime commit publication must not interpret parser publication plan internals"
    );
    assert!(
        !commit.contains("use crate::spine::parser::ParserPublicationUpdate")
            && !publication_parts.contains("Result<Option<ParserPublicationUpdate>"),
        "runtime commit should not name the parser publication update carrier"
    );
    assert!(
        commit.contains("fn commit_host_history_update")
            && !commit.contains("fn parser_commit_publication_history_update"),
        "runtime publication helper should be named for host history update, not parser publication internals"
    );
    assert!(
        !commit.contains("fn commit_publication_history_update")
            && !commit.contains("pub(crate) fn commit_install_publication_history_update")
            && commit.contains("fn commit_install_host_history_update"),
        "runtime should expose only prepare_commit_publication as its publication entrypoint and keep host-history helpers private"
    );
}

#[test]
fn parser_publication_update_constructor_is_parser_private() {
    let publication = fs::read_to_string(spine_src("parser/publication.rs"))
        .expect("read parser publication source");
    let publication_update_impl = publication
        .split("impl ParserPublicationUpdate")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserPublicationPlan").next())
        .expect("ParserPublicationUpdate impl block");
    assert!(
        publication_update_impl.contains("fn new("),
        "ParserPublicationUpdate should still have a parser-local constructor"
    );
    assert!(
        !publication_update_impl.contains("pub(crate) fn new(")
            && !publication_update_impl.contains("pub(super) fn new(")
            && !publication_update_impl.contains("pub(in crate::spine) fn new("),
        "ParserPublicationUpdate construction must stay inside parser publication module"
    );
    assert!(
        publication_update_impl.contains("fn into_host_history_update<T>(")
            && !publication_update_impl.contains("fn into_history_update<T>("),
        "ParserPublicationUpdate should name the host-history conversion boundary explicitly"
    );
}

#[test]
fn runtime_commit_does_not_interpret_close_family_plan_fields() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    let close_family_commit = commit
        .split("fn commit_close_family_pending(")
        .nth(1)
        .and_then(|tail| tail.split("fn close_family_plan(").next())
        .expect("close-family commit section");
    assert!(
        !close_family_commit.contains("plan.operation,")
            && !close_family_commit.contains("plan.marker_kind,")
            && !close_family_commit.contains("plan.kind,")
            && !close_family_commit.contains("plan.toolcall_context_index {")
            && !close_family_commit.contains("plan.toolcall_context_index,")
            && !close_family_commit.contains("plan.open.as_ref"),
        "runtime close-family commit must consume CloseFamilyPlan through named methods"
    );
    assert!(
        close_family_commit.contains("plan.append_open_events(")
            && close_family_commit.contains("plan.require_completed_toolcall(")
            && close_family_commit.contains("plan.toolcall_context_index(")
            && close_family_commit.contains("plan.open_lexed()")
            && close_family_commit.contains("plan.marker_kind()")
            && close_family_commit.contains("plan.kind()")
            && close_family_commit.contains("plan.operation()"),
        "runtime close-family commit should keep close/next plan interpretation inside CloseFamilyPlan"
    );
}

#[test]
fn runtime_commit_does_not_construct_parser_publication_plans() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains("use crate::spine::parser::ParserPublicationPlan"),
        "runtime/commit.rs must not import parser publication plans just to construct their fields"
    );
    assert!(
        !commit.contains("use crate::spine::parser::ParserPublicationToolcallSegment")
            && !commit.contains("ParserPublicationToolcallSegment {"),
        "runtime/commit.rs must not import or construct parser publication toolcall segments"
    );
    assert!(
        !commit
            .lines()
            .any(|line| line.contains("ParserPublicationPlan {")
                && !line.contains("NoParserPublicationPlan {")),
        "runtime/commit.rs must not construct parser publication plans field-by-field"
    );
    assert!(
        !commit.contains("(operation, suffix_start, expected_history, replacement)"),
        "runtime/commit.rs must not reconstruct parser publication updates as untyped tuples"
    );
    assert!(
        commit.contains(".close_family_publication_plan("),
        "close/next publication plan construction should route through ParserState"
    );
}

#[test]
fn parser_publication_plan_fields_are_parser_private() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    let publication = fs::read_to_string(spine_src("parser/publication.rs"))
        .expect("read parser publication source");
    let publication_plan = publication
        .split("struct ParserPublicationPlan")
        .nth(1)
        .and_then(|tail| tail.split("struct ParserPublicationUpdate").next())
        .expect("ParserPublicationPlan definition");
    assert!(
        parser.contains("pub(in crate::spine) use publication::ParserPublicationPlan")
            && publication.contains("pub(in crate::spine) struct ParserPublicationPlan")
            && publication.contains("pub(in crate::spine) struct ParserPublicationUpdate")
            && !publication.contains("pub(crate) struct ParserPublicationPlan")
            && !publication.contains("pub(crate) struct ParserPublicationUpdate"),
        "parser publication carriers should be visible only inside the spine module, not crate-wide"
    );
    assert!(
        !publication_plan.contains("pub(super) operation")
            && !publication_plan.contains("pub(super) suffix_start")
            && !publication_plan.contains("pub(super) replacement_prefix")
            && !publication_plan.contains("pub(super) preserve_host_history_from")
            && !publication_plan.contains("pub(super) append_current_tool_response_if_missing"),
        "ParserPublicationPlan fields must stay parser-private so runtime cannot interpret publication internals"
    );
    assert!(
        publication.contains("fn full_variable_context_publication_update("),
        "parser should centralize full h(PS) publication update construction in one helper"
    );
    assert!(
        publication.contains("fn full_variable_context_publication_update")
            && !publication.contains("fn full_variable_context_host_history_update<T>"),
        "parser should expose a full variable-context publication facade, not a host-history helper"
    );
    assert!(
        publication.contains("fn validate_host_boundaries_do_not_split_toolcall")
            && publication.contains("fn publication_update_with_host_lens(")
            && publication.contains("fn publication_update_with_host_boundaries(")
            && !publication.contains("fn history_update_with_host_boundaries(")
            && publication.contains("self.atomic_mutable_context_segments")
            && !publication.contains("pub(super) fn atomic_mutable_context_segments"),
        "parser publication plan should own completed-toolcall atomic boundary validation"
    );
    let prepared =
        fs::read_to_string(spine_src("runtime/prepared.rs")).expect("read runtime prepared source");
    assert!(
        prepared.contains(".publication_update_with_host_lens(")
            && !prepared.contains(".publication_update_with_host_boundaries(")
            && !prepared.contains(".suffix_start()")
            && !prepared.contains(".preserve_host_history_from()"),
        "runtime prepared carriers must provide host lens evidence without interpreting parser publication plan boundaries"
    );
    assert_eq!(
        publication.matches("ParserPublicationUpdate::new(").count(),
        2,
        "ParserPublicationUpdate construction should stay centralized in parser plan and full-context helpers"
    );
    let full_publication_helper = publication
        .split("fn full_variable_context_publication_update(")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("full variable context publication helper");
    assert!(
        full_publication_helper.contains("ParserPublicationUpdate::new("),
        "full h(PS) publication updates should be constructed by the parser helper"
    );
}

#[test]
fn runtime_prepared_carriers_hold_parser_prepared_state() {
    let prepared =
        fs::read_to_string(spine_src("runtime/prepared.rs")).expect("read runtime prepared source");
    assert!(
        !prepared.contains("use crate::spine::parse_stack::ParseStack"),
        "runtime prepared carriers must not import raw ParseStack"
    );
    assert!(
        prepared.contains("use crate::spine::parser::ParserCommitInstall")
            && prepared.contains("use crate::spine::parser::ParserCommitPreparedInstall")
            && prepared.contains("enum SpinePreparedParserInstall"),
        "runtime prepared carriers should wrap parser-owned install handles"
    );
    assert!(
        prepared.contains("parser_install: Option<SpinePreparedParserInstall>")
            && prepared.contains("Final(ParserCommitInstall)")
            && prepared.contains("Prepared(ParserCommitPreparedInstall)")
            && prepared.contains("SpinePreparedParserInstall::Prepared(parser_install)"),
        "runtime prepared carrier should distinguish open final installs from close/next prepared installs without exposing parser state directly"
    );
    assert!(
        !prepared.contains("final_parse_stack: Option<ParserPreparedState>"),
        "runtime close prepared carrier must not expose final parser state directly"
    );
    assert!(
        !prepared.contains("pub(super) final_parse_stack: ParserPreparedState"),
        "runtime root compact prepared carrier must not expose final parser state directly"
    );
    assert!(
        prepared.contains("parser_install: ParserRootCompactPreparedCommitInstall"),
        "runtime root compact prepared carrier should hold a parser-owned prepared commit install handle"
    );
    assert!(
        prepared.contains("publication: SpineRootCompactResult")
            && !prepared.contains("result: SpineRootCompactResult")
            && !prepared.contains("pub(super) result: SpineRootCompactResult")
            && !prepared
                .contains("pub(super) parser_install: ParserRootCompactPreparedCommitInstall"),
        "runtime root compact prepared carrier should name parser h(PS) output as publication and keep fields private"
    );
    assert!(
        !prepared.contains("ParserPublicationToolcallSegment")
            && !prepared.contains("ToolCallSegmentKind")
            && !prepared.contains("validate_publication_boundaries_do_not_split_toolcall")
            && !prepared.contains("plan.atomic_mutable_context_segments()"),
        "runtime prepared carriers must not inspect parser publication toolcall boundary internals"
    );
    assert!(
        prepared.contains("fn new(\n        publication: SpineRootCompactResult,\n        parser_install: ParserRootCompactPreparedCommitInstall,")
            && prepared.contains("fn consume_parser_install(")
            && prepared.contains("consume: impl FnOnce(ParserRootCompactPreparedCommitInstall)")
            && !prepared.contains("fn install_parser_state(self, install: impl FnOnce(ParserRootCompactPreparedCommitInstall))")
            && !prepared.contains("fn into_parser_install("),
        "runtime root compact prepared carrier should expose a constructor and scoped parser install consumer without naming itself as parser-state installer"
    );
    assert!(
        !prepared.contains("fn result(&self)")
            && prepared.contains("fn variable_context(&self) -> &[ResponseItem]")
            && prepared.contains(
                "let publication_variable_context_len = self.variable_context().len();"
            )
            && prepared.contains("#[cfg(test)]\n    pub(crate) fn clone_variable_context_publication_for_test(&self) -> SpineRootCompactResult")
            && !prepared.contains("fn publication_result(&self) -> &SpineRootCompactResult"),
        "runtime root compact prepared carrier should expose variable-context publication intent and avoid parser materialization wording"
    );
    assert!(
        prepared.contains("fn consume_for_direct_publication(")
            && prepared.contains("consume: impl FnOnce(ParserRootCompactPreparedCommitInstall),")
            && !prepared.contains("fn install_for_direct_result(")
            && !prepared.contains("fn install_for_direct_publication(")
            && !prepared.contains("fn into_publication_result_and_parser_install(")
            && !prepared
                .contains("(SpineRootCompactResult, ParserRootCompactPreparedCommitInstall)"),
        "runtime root compact prepared carrier should scope direct-publication parser install instead of exposing result/install tuples"
    );
    assert!(
        !prepared.contains("SpinePreparedRootCompactInstall"),
        "runtime root compact should not add an extra install wrapper around the parser-owned install handle"
    );
    assert!(
        !prepared.contains("struct HistoryPublicationPlan"),
        "runtime prepared carriers must not define parser publication plans"
    );
    assert!(
        prepared.contains("use crate::spine::parser::ParserPublicationPlan")
            && prepared.contains("publication_plan: Option<ParserPublicationPlan>"),
        "runtime prepared carriers should hold parser-owned publication plans"
    );
    assert!(
        !prepared.contains("pub(super) publication_plan"),
        "runtime prepared carrier must expose parser publication plans only through an accessor"
    );
    assert!(
        !prepared.contains("fn publication_plan(&self)")
            && !prepared.contains("pub(crate) fn has_publication_plan(&self)")
            && prepared.contains("pub(crate) fn apply_variable_context_publication_update")
            && !prepared.contains("pub(crate) fn apply_publication_history_update")
            && !prepared.contains("enum SpinePreparedPublicationUpdate"),
        "runtime prepared carrier should apply parser publication plans directly without exposing borrowed plan internals or plan-presence probes"
    );
    assert!(
        prepared.contains("struct SpinePreparedCommitInstall")
            && prepared.contains("install: Option<SpinePreparedCommitInstall>")
            && !prepared.contains("SpinePreparedCommitApplication")
            && !prepared.contains("application: Option"),
        "runtime commit publication should name the parser install carrier directly, not as an application wrapper"
    );
    let has_generic_history_update_field = prepared
        .lines()
        .any(|line| line.trim_start().starts_with("history_update: Option<T>"));
    assert!(
        prepared.contains("fn take_pre_apply_host_history_update(&mut self)")
            && !prepared.contains("fn take_pre_apply_history_update(&mut self)")
            && prepared.contains("pre_apply_host_history_update: Option<T>")
            && !prepared.contains("pre_apply_history_update: Option<T>")
            && !has_generic_history_update_field
            && !prepared.contains("fn take_history_update(&mut self)"),
        "SpineCommitPublication should expose only the host-history named pre-apply accessor"
    );
    let commit_publication_impl = prepared
        .split("impl<T> SpineCommitPublication<T> {")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("SpineCommitPublication impl block");
    assert!(
        commit_publication_impl.contains("fn into_install(self)")
            && commit_publication_impl.contains("fn apply_install_side_effects")
            && !commit_publication_impl.contains("fn install(&self)"),
        "SpineCommitPublication should consume parser installs through into_install and keep side-effect access behind a scoped callback"
    );
    assert!(
        prepared.contains("fn apply_variable_context_publication_update<T, F>")
            && !prepared.contains("fn apply_publication_history_update<T, F>(")
            && prepared.contains("fn apply_full_variable_context_publication_update")
            && !prepared.contains("fn full_variable_context_host_history_update")
            && !prepared.contains("fn parser_install(&self) -> Option<&ParserCommitInstall>")
            && prepared.contains("fn trim_candidate_inputs(")
            && prepared.contains("fn mem_for_accounting(&self)")
            && prepared.contains("fn consume_parser_install(")
            && prepared.contains("consume: impl FnOnce(SpinePreparedParserInstall)")
            && !prepared.contains("fn install_parser_state(")
            && !prepared.contains("fn into_install_parts(")
            && !prepared.contains("(Option<ParserCommitInstall>, Option<CompletedToolCall>)")
            && !prepared.contains("fn as_prepared_commit(&self)")
            && !prepared.contains("fn into_prepared_commit(self)"),
        "SpinePreparedCommitInstall should expose named install/publication accessors without naming itself as parser-state installer"
    );
    let prepared_commit_impl = prepared
        .split("impl SpinePreparedCommit {")
        .nth(1)
        .and_then(|tail| tail.split("impl SpinePreparedCommitInstall").next())
        .expect("SpinePreparedCommit impl block");
    assert!(
        !prepared_commit_impl.contains("fn apply_publication_history_update")
            && !prepared_commit_impl.contains("fn validate_against_host_history")
            && !prepared_commit_impl.contains("fn kind(&self)")
            && !prepared_commit_impl.contains("fn parser_install(&self)")
            && !prepared_commit_impl.contains("fn trim_candidate_inputs(")
            && !prepared_commit_impl.contains("fn mem_for_accounting(&self)")
            && !prepared_commit_impl.contains("fn into_install_parts("),
        "SpinePreparedCommit should construct prepared commits; publication and install access should live on SpinePreparedCommitInstall"
    );
    assert!(
        prepared_commit_impl.contains("fn into_kind_and_install(self)")
            && prepared_commit_impl.contains("fn into_kind_and_install_for_test(")
            && !prepared_commit_impl.contains("fn into_install_for_test("),
        "SpinePreparedCommit should expose commit kind only while consuming the prepared carrier into an install"
    );
    let prepared_commit_install_impl = prepared
        .split("impl SpinePreparedCommitInstall {")
        .nth(1)
        .and_then(|tail| tail.split("impl<T> SpineCommitPublication<T>").next())
        .expect("SpinePreparedCommitInstall impl block");
    assert!(
        prepared_commit_install_impl.contains("fn validate_against_host_history")
            && prepared_commit_install_impl
                .contains("fn apply_variable_context_publication_update")
            && !prepared_commit_install_impl.contains("fn apply_publication_history_update")
            && prepared_commit_install_impl
                .contains("fn apply_full_variable_context_publication_update")
            && !prepared_commit_install_impl
                .contains("fn full_variable_context_host_history_update")
            && prepared_commit_install_impl.contains("self.prepared.publication_plan.as_ref()")
            && prepared_commit_install_impl.contains("self.prepared.parser_install.as_ref()")
            && prepared_commit_install_impl.contains("self.prepared.mem_for_accounting.as_ref()"),
        "SpinePreparedCommitInstall should own host-publication validation and install-side-effect access"
    );
    let completed_toolcall_session = fs::read_to_string(spine_src(
        "runtime/session_state/completed_toolcall_session.rs",
    ))
    .expect("read completed toolcall session source");
    assert!(
        completed_toolcall_session.contains(".take_pre_apply_host_history_update()")
            && !completed_toolcall_session.contains(".take_pre_apply_history_update()")
            && !completed_toolcall_session.contains(".take_history_update()"),
        "session toolcall commit should consume publication through the named pre-apply host-history API"
    );
    let prepared_commit = prepared
        .split("struct SpinePreparedCommit {")
        .nth(1)
        .and_then(|tail| tail.split("struct SpinePreparedCommitInstall").next())
        .expect("SpinePreparedCommit definition");
    for field in [
        "kind",
        "parser_install",
        "completed_toolcall",
        "toolcall_seq",
        "raw_items",
        "mem_for_accounting",
        "publication_plan",
    ] {
        assert!(
            !prepared_commit.contains(&format!("pub(super) {field}"))
                && !prepared_commit.contains(&format!("pub(crate) {field}"))
                && !prepared_commit.contains(&format!("pub(in crate::spine) {field}")),
            "SpinePreparedCommit field {field} should stay private behind named accessors"
        );
    }
    assert!(
        prepared.contains("fn trim_candidate_inputs(")
            && prepared.contains("fn mem_for_accounting(&self)")
            && prepared.contains("fn consume_parser_install(")
            && !prepared.contains("fn install_parser_state(")
            && !prepared.contains("fn into_install_parts("),
        "SpinePreparedCommit should expose named side-effect/install accessors instead of public fields"
    );
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    let runtime = fs::read_to_string(spine_src("runtime.rs")).expect("read runtime source");
    let spine_mod = fs::read_to_string(spine_src("mod.rs")).expect("read spine mod source");
    assert!(
        commit.contains("SpinePreparedCommitInstall")
            && !commit.contains("SpinePreparedCommitApplication")
            && !commit.contains("commit_application_publication_history_update")
            && !commit.contains(".application()")
            && !commit.contains(".into_application()")
            && !commit.contains(".as_prepared_commit()")
            && !commit.contains(".into_prepared_commit()"),
        "runtime commit should consume parser-install intent directly instead of extracting prepared carrier internals"
    );
    assert!(
        !runtime.contains("pub(crate) use prepared::SpinePreparedCommit")
            && !spine_mod.contains("pub(crate) use runtime::SpinePreparedCommit"),
        "SpinePreparedCommit should remain runtime/prepared.rs construction detail, not a re-exported parser publication surface"
    );
    assert!(
        !runtime.contains("pub(crate) use prepared::SpinePreparedRootCompact")
            && !spine_mod.contains("pub(crate) use runtime::SpinePreparedRootCompact"),
        "SpinePreparedRootCompact should remain a runtime/prepared.rs detail, not an outer parser publication surface"
    );
    for direct_field_access in [
        "prepared.parser_install",
        "prepared.completed_toolcall",
        "prepared.toolcall_seq",
        "prepared.raw_items",
        "prepared.mem_for_accounting.as_ref()",
        "prepared.publication_plan",
    ] {
        assert!(
            !commit.contains(direct_field_access),
            "runtime commit should not read SpinePreparedCommit internals through {direct_field_access}"
        );
    }
    assert!(
        commit.contains("install.trim_candidate_inputs()")
            && commit.contains("install.mem_for_accounting()")
            && commit.contains("install.consume_parser_install(")
            && !commit.contains("install.install_parser_state(")
            && !commit.contains("install.into_install_parts()")
            && commit.contains("persist_prepared_commit_install_side_effects")
            && commit.contains("install_prepared_commit_install"),
        "runtime commit should use prepared install carrier accessors for side effects and install"
    );
}

#[test]
fn hooks_expose_variable_context_after_batch_publication() {
    let host_effects =
        fs::read_to_string(spine_src("bridge/host_effects.rs")).expect("read bridge host effects");
    assert!(
        host_effects.contains("fn apply_after_batch_variable_context_request")
            && host_effects.contains(".apply_after_batch_variable_context_request(")
            && !host_effects.contains("fn apply_after_batch_materialized_history_request"),
        "Spine hooks should expose variable-context after-batch publication without a materialized-history compatibility wrapper"
    );
}

#[test]
fn runtime_checkpoint_routes_parser_reads_through_parser_state() {
    let checkpoint = fs::read_to_string(spine_src("runtime/checkpoint.rs"))
        .expect("read runtime checkpoint source");
    assert!(
        !checkpoint.contains(".parse_stack()"),
        "runtime/checkpoint.rs must not read ParseStack through the raw parser handle"
    );
    assert!(
        !checkpoint.contains("use crate::spine::checkpoint::build_checkpoint"),
        "runtime/checkpoint.rs must not import checkpoint construction outside ParserState"
    );
    assert!(
        checkpoint.contains("self.parser.build_checkpoint("),
        "runtime checkpoint construction should route PS and h(PS) reads through ParserState"
    );
}

#[test]
fn checkpoint_proof_names_h_ps_as_variable_context() {
    let checkpoint =
        fs::read_to_string(spine_src("checkpoint.rs")).expect("read checkpoint source");
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    let validate_checkpoint = checkpoint
        .split("fn validate_checkpoint(")
        .nth(1)
        .and_then(|tail| tail.split("fn checkpoint_raw_ordinal_usize").next())
        .expect("validate_checkpoint body");
    assert!(
        validate_checkpoint.contains("let variable_context =")
            && validate_checkpoint.contains("hash_response_items(&variable_context)")
            && !validate_checkpoint.contains("let materialized =")
            && !validate_checkpoint.contains("hash_response_items(&materialized)"),
        "checkpoint h(PS) proof should use variable_context naming, not materialized-history terminology"
    );
    assert!(
        validate_checkpoint.contains("ParserState::checkpoint_variable_context(")
            && !validate_checkpoint
                .contains("ParserState::from_parse_stack(checkpoint.parse_stack.clone())")
            && !validate_checkpoint.contains(".materialize_variable_context(")
            && !checkpoint.contains("use crate::spine::parser::checkpoint_variable_context"),
        "checkpoint validation should route h(PS) proof through a ParserState checkpoint helper, not construct state or re-export parser publication helpers"
    );
    assert!(
        !parser.contains("pub(in crate::spine) use publication::checkpoint_variable_context"),
        "parser facade must not re-export checkpoint publication helpers outside the parser module"
    );
}

#[test]
fn runtime_root_compact_routes_probe_reads_through_parser_state() {
    let root_compact = fs::read_to_string(spine_src("runtime/root_compact.rs"))
        .expect("read runtime root_compact source");
    assert!(
        !root_compact.contains(".parse_stack().current_root_epoch_id()"),
        "runtime/root_compact.rs must not query root epoch ids through the raw parser handle"
    );
    assert!(
        !root_compact.contains(".pending_compact_next_open_index("),
        "runtime/root_compact.rs must not compute compact next-open probe state outside ParserState"
    );
    assert!(
        !root_compact.contains("probe_parse_stack"),
        "runtime/root_compact.rs must not clone ParseStack for compact probe materialization"
    );
    assert!(
        root_compact.contains(".current_root_epoch_id()")
            && root_compact.contains(".root_compact_next_open_index_or_probe("),
        "runtime root compact should route root id and next-open probe reads through ParserState"
    );
}

#[test]
fn runtime_root_compact_routes_reductions_through_parser_state() {
    let root_compact = fs::read_to_string(spine_src("runtime/root_compact.rs"))
        .expect("read runtime root_compact source");
    let prepared =
        fs::read_to_string(spine_src("runtime/prepared.rs")).expect("read runtime prepared source");
    assert!(
        !root_compact.contains("shift_pending_compact"),
        "runtime/root_compact.rs must not directly stage root compact parser tokens"
    );
    assert!(
        !root_compact.contains("apply_prevalidated_root_epoch_reduction"),
        "runtime/root_compact.rs must not directly apply root epoch reductions"
    );
    assert!(
        !root_compact.contains("prepare_root_epoch_reduction("),
        "runtime/root_compact.rs must not directly prepare root epoch reductions"
    );
    assert!(
        root_compact.contains(".prepare_root_compact_txn(")
            && !root_compact.contains(".root_compact_staged_parse_stacks("),
        "runtime root compact should prepare root compact parser transaction once through ParserState"
    );
    assert!(
        !root_compact.contains("lex_event_token")
            && !root_compact.contains("lex_root_compact_event_token")
            && !root_compact.contains("let (root_compact_event, _token)"),
        "runtime/root_compact.rs must not request or discard raw root compact parser tokens"
    );
    assert!(
        !root_compact.contains(".prepare_root_compact_reduction("),
        "runtime root compact should not name the parser transaction as a raw reduction"
    );
    assert!(
        !root_compact.contains("final_parse_stack.parse_stack()"),
        "runtime root compact must not read prepared parser state for compact checkpoint construction"
    );
    assert!(
        root_compact.contains(".build_compact_checkpoint("),
        "runtime root compact checkpoint construction should route through parser prepared txn"
    );
    assert!(
        !root_compact.contains("prepared_reduction.current_open_index")
            && !root_compact.contains("prepared_reduction.materialized.len()")
            && !root_compact.contains("prepared_reduction.materialized()")
            && !root_compact.contains("prepared_reduction.root_epoch_reduction"),
        "runtime root compact must not inspect parser prepared transaction internals"
    );
    assert!(
        !root_compact.contains("let prepared_reduction"),
        "runtime root compact should not name the parser transaction as a prepared reduction"
    );
    assert!(
        root_compact.contains(".validate_current_open_matches_variable_context_len()")
            && root_compact.contains("SpinePreparedRootCompact::from_parser_prepared_txn(")
            && !root_compact.contains(".into_publication_parts()")
            && !root_compact.contains(".validate_current_open_matches_materialized_len()")
            && !root_compact.contains(".into_publication_history_and_install()")
            && !root_compact.contains(".into_materialized_and_install()"),
        "runtime root compact should pass parser prepared txn through the prepared-root-compact constructor"
    );
    assert!(
        !root_compact.contains(".into_publication_materialized_and_install()"),
        "runtime root compact should not expose parser materialization wording at the publication/install boundary"
    );
    assert!(
        root_compact.contains("SpinePreparedRootCompact::from_parser_prepared_txn(")
            && root_compact.contains("prepared_txn,")
            && !root_compact.contains("let publication_parts = prepared_txn")
            && !root_compact.contains(".into_publication_parts()")
            && !root_compact.contains("SpinePreparedRootCompact::from_parser_publication_parts(")
            && !root_compact.contains("let (variable_context, parser_install) = prepared_txn")
            && !root_compact.contains(".into_variable_context_and_install();")
            && !root_compact.contains("let parser_txn")
            && !root_compact.contains("let parser_install_parts")
            && !root_compact.contains("parser_install_parts.pending_install")
            && !root_compact.contains("parser_install_parts.final_install")
            && !root_compact.contains("parser_publication_install.into_pending_and_final_install(")
            && !root_compact.contains("parser_publication_install.variable_context().to_vec()")
            && !root_compact.contains("variable_context: parser_txn.variable_context,")
            && !root_compact.contains("pending_parser_install: parser_txn.pending_install,")
            && !root_compact.contains("parser_install: parser_txn.final_install,")
            && !root_compact
                .contains("let (variable_context, pending_parser_install, parser_install)"),
        "runtime root compact should pass the parser prepared txn through named prepared-root-compact methods instead of tuple unpacking"
    );
    let publication = fs::read_to_string(spine_src("parser/publication.rs"))
        .expect("read parser publication source");
    let transaction = fs::read_to_string(spine_src("parser/transaction.rs"))
        .expect("read parser transaction source");
    let parser_state = parser_state_src();
    assert!(
        transaction.contains("struct ParserRootCompactPreparedTxn")
            && !transaction.contains("struct ParserRootCompactPreparedReduction"),
        "parser root compact transaction carrier should not be named as a raw reduction"
    );
    assert!(
        transaction.contains("struct ParserRootCompactPreparedInstall")
            && !transaction.contains("struct ParserRootCompactPublicationParts")
            && !transaction.contains("struct ParserRootCompactPublicationInstall")
            && transaction.contains("struct ParserRootCompactPreparedCommitInstall")
            && !transaction.contains("struct ParserRootCompactTxnParts")
            && transaction.contains("prepared_install: ParserRootCompactPreparedInstall")
            && !transaction.contains(
                "ParserPreparedInstallPair<ParserRootCompactPendingInstall, ParserRootCompactInstall>"
            ),
        "parser root compact prepared txn should hold a named prepared install carrier without the obsolete shared pending/final pair wrapper"
    );
    let root_compact_prepared_txn = transaction
        .split("struct ParserRootCompactPreparedTxn")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserRootCompactPreparedTxn").next())
        .expect("root compact prepared txn body");
    assert!(
        root_compact_prepared_txn.contains("publication: ParserRootCompactPublication")
            && !root_compact_prepared_txn.contains("pub(super) publication")
            && root_compact_prepared_txn
                .contains("prepared_install: ParserRootCompactPreparedInstall")
            && !root_compact_prepared_txn.contains("pub(super) prepared_install"),
        "ParserRootCompactPreparedTxn fields should stay private behind parser transaction methods"
    );
    let root_compact_prepared_txn_impl = transaction
        .split("impl ParserRootCompactPreparedTxn")
        .nth(1)
        .and_then(|tail| {
            tail.split("impl ParserRootCompactPreparedCommitInstall")
                .next()
        })
        .expect("root compact prepared txn impl");
    assert!(
        root_compact_prepared_txn_impl.contains("fn new(\n        publication: ParserRootCompactPublication,\n        prepared_install: ParserRootCompactPreparedInstall,"),
        "ParserRootCompactPreparedTxn construction should be centralized behind a parser transaction constructor"
    );
    assert!(
        publication.contains("struct ParserRootCompactCheckpointProof")
            && publication.contains("fn root_compact_checkpoint_publication_proof")
            && root_compact_prepared_txn_impl.contains("fn checkpoint_publication_proof(")
            && root_compact_prepared_txn_impl
                .contains("let proof = self.checkpoint_publication_proof();")
            && root_compact_prepared_txn_impl.contains("proof.parse_stack()")
            && root_compact_prepared_txn_impl
                .matches("proof.variable_context()")
                .count()
                == 2,
        "root compact checkpoint construction should bind final PS and h(PS) variable context through a named parser-owned proof"
    );
    let root_compact_prepared_txn_impl = transaction
        .split("impl ParserRootCompactPreparedTxn")
        .nth(1)
        .and_then(|tail| {
            tail.split("impl ParserRootCompactPreparedCommitInstall")
                .next()
        })
        .expect("root compact prepared txn impl");
    let root_compact_prepared_install_impl = transaction
        .split("impl ParserRootCompactPreparedInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserRootCompactPendingInstall").next())
        .expect("root compact prepared install impl");
    let root_compact_prepared_install_carrier = transaction
        .split("struct ParserRootCompactPreparedInstall")
        .nth(1)
        .and_then(|tail| tail.split("struct ParserRootCompactPendingInstall").next())
        .expect("root compact prepared install carrier");
    assert!(
        !transaction.contains("struct ParserPreparedInstallPair<PendingInstall, FinalInstall>")
            && !transaction.contains("struct ParserRootCompactPublicationParts")
            && !transaction.contains("struct ParserRootCompactPublicationInstall")
            && root_compact_prepared_txn_impl.contains("self.prepared_install")
            && root_compact_prepared_txn_impl.contains("into_prepared_commit_install()")
            && root_compact_prepared_txn_impl
                .contains("self.publication.into_variable_context()")
            && transaction.contains("fn consume_variable_context_and_install")
            && transaction.contains("consume(\n            self.publication.into_variable_context(),\n            self.prepared_install.into_prepared_commit_install(),\n        )")
            && !transaction.contains("fn into_variable_context_and_install(\n        self,\n    ) -> (Vec<ResponseItem>, ParserRootCompactPreparedCommitInstall)")
            && root_compact_prepared_install_impl.contains("fn into_prepared_commit_install")
            && root_compact_prepared_install_carrier
                .contains("pending_install: ParserRootCompactPendingInstall")
            && root_compact_prepared_install_carrier
                .contains("final_install: ParserRootCompactInstall")
            && !root_compact_prepared_install_carrier.contains("install_pair:")
            && root_compact_prepared_install_impl
                .contains("ParserRootCompactPreparedCommitInstall::new(self.pending_install, self.final_install)")
            && !transaction.contains("struct ParserPreparedInstallParts")
            && !transaction.contains("fn into_parts("),
        "parser prepared install carriers should consume named pending/final handles directly without install-parts carriers, positional tuples, or obsolete pair wrappers"
    );
    let root_compact_prepared_commit_install = transaction
        .split("struct ParserRootCompactPreparedCommitInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserRootCompactPreparedTxn").next())
        .expect("root compact prepared commit install carrier");
    assert!(
        root_compact_prepared_commit_install
            .contains("pending_install: ParserRootCompactPendingInstall")
            && root_compact_prepared_commit_install
                .contains("final_install: ParserRootCompactInstall"),
        "parser should keep root compact pending/final install pair behind one parser-owned commit install carrier"
    );
    let root_compact_prepared_commit_install_impl = transaction
        .split("impl ParserRootCompactPreparedCommitInstall")
        .nth(1)
        .and_then(|tail| tail.split("impl ParserObserveInstall").next())
        .expect("root compact prepared commit install impl");
    assert!(
        root_compact_prepared_commit_install_impl.contains("fn pending_state(&self)")
            && root_compact_prepared_commit_install_impl.contains("fn into_final_state(self)")
            && !root_compact_prepared_commit_install_impl.contains("fn pending_install(&self)")
            && !root_compact_prepared_commit_install_impl.contains("fn into_final_install(self)"),
        "root compact prepared commit install should expose parser prepared states, not pending/final install internals"
    );
    assert!(
        prepared.contains("parser_install: ParserRootCompactPreparedCommitInstall")
            && prepared.contains("fn consume_parser_install(")
            && prepared.contains("fn parser_install_for_side_effect_failure(")
            && !root_compact.contains("ParserRootCompactPendingInstall")
            && !root_compact.contains("ParserRootCompactInstall")
            && !root_compact.contains("ParserRootCompactPreparedCommitInstall")
            && !root_compact.contains("prepared.parser_install")
            && !root_compact.contains("pending_parser_install:"),
        "runtime root compact should consume one parser-owned committed install carrier through scoped prepared-carrier methods, not parser pending/final install internals"
    );
    assert!(
        !transaction.contains("struct ParserRootCompactInstallParts")
            && !transaction.contains("pub(super) pending_install: ParserRootCompactPendingInstall")
            && !transaction.contains("pub(super) final_install: ParserRootCompactInstall")
            && transaction
                .contains("pub(in crate::spine::parser) struct ParserRootCompactPendingInstall")
            && transaction.contains("pub(in crate::spine::parser) struct ParserRootCompactInstall")
            && transaction
                .contains("pub(in crate::spine::parser) struct ParserRootCompactPreparedInstall")
            && !transaction.contains("pub(in crate::spine) struct ParserRootCompactPendingInstall")
            && !transaction.contains("pub(in crate::spine) struct ParserRootCompactInstall")
            && !transaction
                .contains("pub(in crate::spine) struct ParserRootCompactPreparedInstall"),
        "parser should not expose a root-compact-specific install-parts carrier or parser-only root compact install helpers outside the parser module"
    );
    assert!(
        !transaction.contains("struct ParserRootCompactPublicationInstall")
            && !transaction.contains("struct ParserRootCompactPublicationParts")
            && root_compact_prepared_txn_impl.contains("fn consume_variable_context_and_install")
            && root_compact_prepared_txn_impl.contains("self.publication.into_variable_context()")
            && root_compact_prepared_txn_impl
                .contains("self.prepared_install.into_prepared_commit_install()")
            && !root_compact_prepared_txn_impl.contains("pub(super) variable_context")
            && !root_compact_prepared_txn_impl.contains("pub(super) prepared_commit_install")
            && !root_compact_prepared_txn_impl.contains("pub(super) pending_install")
            && !root_compact_prepared_txn_impl.contains("pub(super) final_install"),
        "parser root compact prepared txn should consume variable context and install through a closure without an intermediate publication-parts carrier"
    );
    let root_compact_publication = publication
        .split("struct ParserRootCompactPublication")
        .nth(1)
        .and_then(|tail| tail.split("struct ParserPublicationUpdate").next())
        .expect("root compact publication carrier section");
    assert!(
        publication.contains("struct ParserRootCompactPublication")
            && transaction.contains("publication: ParserRootCompactPublication")
            && publication.contains("fn root_compact_publication_from_state(")
            && parser_state.contains("root_compact_publication_from_state(")
            && !transaction.contains("struct ParserRootCompactPublication {")
            && root_compact_publication.contains("variable_context: Vec<ResponseItem>")
            && !root_compact_publication.contains("materialized: Vec<ResponseItem>")
            && !transaction.contains("materialized: Vec<ResponseItem>,\n    current_open_index: usize,\n    prepared_install: ParserRootCompactPreparedInstall"),
        "parser root compact prepared txn should hold a named variable-context publication carrier instead of parallel publication fields"
    );
    let prepare_root_compact_txn = parser_state
        .split("fn prepare_root_compact_txn(")
        .nth(1)
        .and_then(|tail| tail.split("fn consume_lexed_batch(").next())
        .expect("prepare_root_compact_txn section");
    assert!(
        !prepare_root_compact_txn
            .contains("render_parse_stack_to_context_with_memory_body_and_trim_projection(")
            && !prepare_root_compact_txn.contains("current_open_meta()?.index"),
        "ParserState root compact transaction should delegate PS -> h(PS) publication construction to parser/publication.rs"
    );
}

#[test]
fn runtime_root_compact_routes_source_context_len_through_parser_state() {
    let root_compact = fs::read_to_string(spine_src("runtime/root_compact.rs"))
        .expect("read runtime root_compact source");
    let prepare_commit = root_compact
        .split("fn prepare_root_compact_commit(")
        .nth(1)
        .expect("root compact prepare function");
    assert!(
        !prepare_commit.contains("self.materialize_variable_context_for_test("),
        "runtime/root_compact.rs must not materialize h(PS) directly while preparing root compact source bounds"
    );
    assert!(
        prepare_commit.contains("variable_context_len(")
            && !prepare_commit.contains("materialized_variable_context_len("),
        "root compact source context length should route through ParserState variable context API"
    );
}

#[test]
fn lifecycle_fork_derives_suffix_indices_from_raw_mutable_projection() {
    let lifecycle = fs::read_to_string(spine_src("runtime/session_state/lifecycle_session.rs"))
        .expect("read lifecycle session source");
    let fork_install = lifecycle
        .split("fn install_cloned_sidecar_for_fork(")
        .nth(1)
        .expect("fork clone install function");
    assert!(
        !fork_install.contains("materialize_variable_context_for_test(raw_items)?.len()"),
        "fork clone append context index calculation must not materialize h(PS) directly"
    );
    assert!(
        !fork_install.contains("variable_context_len(raw_items)?")
            && !fork_install.contains("materialized_history_len(raw_items)?"),
        "fork clone suffix item indices must not be derived from h(PS) length"
    );
    assert!(
        lifecycle.contains("fn mutable_context_index_for_raw_item(")
            && fork_install.contains("mutable_context_index_for_raw_item(raw_items, raw_ordinal)?"),
        "fork clone must map raw suffix items into mutable context index space explicitly"
    );
}

#[test]
fn session_state_materialization_uses_variable_context_api() {
    for path in [
        "runtime/session_state/lifecycle_session.rs",
        "runtime/session_state/trim_session.rs",
    ] {
        let source = fs::read_to_string(spine_src(path)).expect("read session state source");
        assert!(
            !source.contains(".materialize_variable_context_for_test("),
            "{path} must not call the runtime materialize_variable_context_for_test facade"
        );
        assert!(
            source.contains(".materialize_variable_context("),
            "{path} should name parser-owned variable context materialization explicitly"
        );
    }
    let trim_session = fs::read_to_string(spine_src("runtime/session_state/trim_session.rs"))
        .expect("read trim session source");
    assert!(
        trim_session.contains("fn variable_context_if_no_pending_tool_request(")
            && !trim_session.contains("fn materialize_history_if_no_pending_tool_request("),
        "session state should name conditional h(PS) reads as variable-context reads, not materialized-history reads"
    );

    let runtime = fs::read_to_string(spine_src("runtime.rs")).expect("read runtime source");
    let materialize_variable_context = runtime
        .split("fn materialize_variable_context(")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("runtime materialize_variable_context section");
    assert!(
        materialize_variable_context.contains("Spine variable context materialization")
            && !materialize_variable_context.contains("Spine history materialization")
            && !materialize_variable_context.contains("materialized history"),
        "production runtime variable-context materialization should not use materialized-history naming"
    );
    let marker = "#[cfg(test)]\n    pub(crate) fn materialize_variable_context_for_test";
    assert!(
        runtime.contains(marker),
        "runtime materialize_variable_context_for_test facade should remain test-only"
    );
    assert!(
        !runtime.contains("fn materialize_history_for_test")
            && !runtime.contains("materialize_history_for_test("),
        "runtime should not keep the legacy materialize_history_for_test alias"
    );
    let spine_test_sources = [
        "tests/materialize_variable_context_for_test.rs",
        "tests/materialize_projection_memory.rs",
        "tests/materialize_projection_visible_msg_guard.rs",
    ];
    for path in spine_test_sources {
        let source = fs::read_to_string(spine_src(path)).expect("read spine test source");
        assert!(
            !source.contains("materialize_history_for_test("),
            "{path} should call the variable-context test helper, not the legacy alias"
        );
    }
    assert!(
        !runtime.contains("use crate::spine::render"),
        "runtime must not import render helpers; h(PS) materialization should route through ParserState"
    );
    assert!(
        !runtime.contains("fn materialized_history_len("),
        "runtime should not keep a materialized-history length compatibility wrapper after production callers move to variable_context_len"
    );
}

#[test]
fn runtime_root_compact_routes_installs_through_named_parser_methods() {
    let root_compact = fs::read_to_string(spine_src("runtime/root_compact.rs"))
        .expect("read runtime root_compact source");
    assert!(
        !root_compact.contains("replace_parse_stack_for_runtime_transition"),
        "runtime/root_compact.rs must not use the generic parser replacement escape hatch"
    );
    assert!(
        !root_compact.contains("ParserPreparedState"),
        "runtime/root_compact.rs must not hold raw parser prepared states; use parser-owned install handles"
    );
    assert!(
        root_compact.contains(".install_pending_root_compact_after_side_effect_failure(")
            && root_compact.contains(".install_prepared_root_compact("),
        "runtime root compact should install pending/final parser states through named ParserState methods"
    );
    let prepared =
        fs::read_to_string(spine_src("runtime/prepared.rs")).expect("read runtime prepared source");
    assert!(
        prepared.contains("parser_install: ParserRootCompactPreparedCommitInstall")
            && root_compact.contains("SpinePreparedRootCompact::from_parser_prepared_txn(")
            && !root_compact.contains("ParserRootCompactPreparedCommitInstall")
            && !root_compact.contains("ParserRootCompactPendingInstall")
            && !root_compact.contains("ParserRootCompactInstall"),
        "runtime prepared root compact should hold one parser-owned prepared commit install handle while root_compact.rs avoids pending/final internals"
    );
    assert!(
        !root_compact.contains(".install_prepared_root_compact_final_parse_stack("),
        "runtime root compact final install should use the parser-owned root compact install handle"
    );
    assert!(
        prepared.contains("fn new(\n        publication: SpineRootCompactResult,\n        parser_install: ParserRootCompactPreparedCommitInstall,")
            && prepared.contains("fn from_parser_prepared_txn(")
            && prepared.contains("prepared_txn.consume_variable_context_and_install(")
            && prepared.contains("Self::new(\n                SpineRootCompactResult {")
            && root_compact.contains("SpinePreparedRootCompact::from_parser_prepared_txn("),
        "runtime prepared root compact should construct prepared root compact through named constructors"
    );
    assert!(
        prepared.contains("publication: SpineRootCompactResult")
            && !prepared.contains("result: SpineRootCompactResult"),
        "runtime root compact prepared commit should name parser h(PS) payload as publication, not generic result"
    );
    assert!(
        !root_compact.contains("clone_publication_result"),
        "runtime root compact production paths should consume prepared result/install without cloning publication results"
    );
    assert!(
        !root_compact.contains(".into_publication_result_and_parser_install()")
            && root_compact.contains("fn install_prepared_root_compact_for_direct_publication(")
            && !root_compact.contains("fn install_prepared_root_compact_for_direct_result(")
            && root_compact.contains(".consume_for_direct_publication(|parser_install|")
            && !root_compact.contains(".install_for_direct_publication(|parser_install|")
            && !root_compact.contains(".install_for_direct_result(|parser_install|"),
        "runtime root compact should centralize direct-publication parser install without exposing result/install tuples"
    );
    let install_prepared_root_compact = root_compact
        .split("pub(crate) fn install_prepared_root_compact(")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn commit_root_compact_prepared_side_effects")
                .next()
        })
        .expect("install_prepared_root_compact section");
    assert!(
        install_prepared_root_compact.contains(".consume_parser_install(|parser_install|")
            && !install_prepared_root_compact.contains(".install_parser_state(|parser_install|")
            && !install_prepared_root_compact.contains(".parser_install"),
        "runtime root compact should construct and consume prepared root compact through scoped parser install methods"
    );
    assert!(
        !root_compact.contains("prepare_root_compact_install_with_checkpoint")
            && !root_compact.contains("install_prepared_root_compact_install"),
        "runtime root compact should not reintroduce transitional root compact install wrapper APIs"
    );
    let session_state =
        fs::read_to_string(spine_src("runtime/session_state.rs")).expect("read session_state");
    let state_types = fs::read_to_string(spine_src("runtime/session_state/state_types.rs"))
        .expect("read session state types");
    assert!(
        !session_state.contains("PreparedSpineRootCompactCommit")
            && !state_types.contains("PreparedSpineRootCompactCommit"),
        "session root compact host install should directly carry the prepared root compact commit"
    );
    assert!(
        state_types.contains("struct SpineRootCompactHostInstall")
            && state_types.contains("prepared: SpinePreparedRootCompact"),
        "root compact host install should keep only the host-publication boundary wrapper"
    );
    assert!(
        state_types.contains("fn variable_context(")
            && state_types.contains("fn variable_context_len(")
            && !state_types.contains("fn materialized("),
        "root compact host install should expose variable-context publication accessors, not parser materialization internals"
    );
    assert!(
        state_types.contains("self.prepared.variable_context()")
            && state_types.contains("self.prepared.clone_variable_context_publication_for_test()")
            && !state_types.contains("self.prepared.publication_result()")
            && !state_types.contains("self.prepared.result().materialized"),
        "root compact host install should publish through prepared variable-context accessors, not parser result internals"
    );
    let runtime_types =
        fs::read_to_string(spine_src("runtime/types.rs")).expect("read runtime types source");
    assert!(
        runtime_types.contains("struct SpineRootCompactResult")
            && runtime_types.contains("variable_context: Vec<ResponseItem>")
            && runtime_types.contains("fn variable_context(&self)")
            && !runtime_types.contains("materialized: Vec<ResponseItem>")
            && !runtime_types.contains("self.materialized"),
        "SpineRootCompactResult should carry parser h(PS) as variable_context, not materialized history"
    );
    let root_compact_session =
        fs::read_to_string(spine_src("runtime/session_state/root_compact_session.rs"))
            .expect("read root compact session source");
    assert!(
        root_compact_session.contains(".variable_context().to_vec()")
            && !root_compact_session.contains(".materialized().to_vec()"),
        "root compact session should publish through the host-publication wrapper variable-context accessor"
    );
    assert!(
        root_compact_session
            .contains("let variable_context = install.variable_context().to_vec();")
            && !root_compact_session
                .contains("let materialized = install.publication_history().to_vec();"),
        "root compact host publication locals should keep variable-context naming instead of parser materialization naming"
    );
    let host_effect =
        fs::read_to_string(spine_src("runtime/host_effect.rs")).expect("read host effect source");
    let root_compact_host_publish = host_effect
        .split("struct SpineRootCompactHostPublish")
        .nth(1)
        .and_then(|tail| tail.split("impl SpineHostEffects").next())
        .expect("root compact host publish carrier");
    assert!(
        root_compact_host_publish.contains("variable_context: Vec<ResponseItem>")
            && !root_compact_host_publish.contains("materialized: Vec<ResponseItem>"),
        "root compact host publish carrier should name the payload as variable context"
    );
    let message_session = fs::read_to_string(spine_src("runtime/session_state/message_session.rs"))
        .expect("read message session source");
    let spine_bridge = fs::read_to_string(core_src("session/spine_bridge.rs"))
        .expect("read session spine bridge source");
    let session_mod = fs::read_to_string(core_src("session/mod.rs")).expect("read session source");
    let tasks_mod = fs::read_to_string(core_src("tasks/mod.rs")).expect("read tasks source");
    let host_effects =
        fs::read_to_string(spine_src("bridge/host_effects.rs")).expect("read host effects source");
    let toolcall_lifecycle = fs::read_to_string(spine_src("bridge/toolcall_lifecycle.rs"))
        .expect("read toolcall lifecycle facade source");
    let bridge_mod = fs::read_to_string(spine_src("bridge.rs")).expect("read bridge mod source");
    let toolcall_host_commit = fs::read_to_string(spine_src("bridge/toolcall_host_commit.rs"))
        .expect("read toolcall host commit source");
    assert!(
        message_session
            .contains("pub(crate) fn variable_context_host_effects_if_no_pending_tool_request(")
            && message_session.contains(".variable_context_if_no_pending_tool_request(")
            && !message_session.contains(".materialize_history_if_no_pending_tool_request(")
            && !message_session.contains(
                "pub(crate) fn materialized_history_host_effects_if_no_pending_tool_request("
            ),
        "message session should expose only the variable-context named host-effect API"
    );
    assert!(
        spine_bridge.contains("non_toolcall_msg_effects\n            .apply_after_batch_variable_context_request_from_state(")
            && !spine_bridge.contains("MessageRuntime::")
            && !spine_bridge
                .contains("fn variable_context_host_effects_if_no_pending_tool_request(")
            && !spine_bridge.contains(".variable_context_host_effects_if_no_pending_tool_request(")
            && !spine_bridge.contains(".apply_after_batch_materialized_history_request(")
            && !spine_bridge
                .contains(".materialized_history_host_effects_if_no_pending_tool_request(")
            && !spine_bridge.contains(".into_materialized()"),
        "session bridge should delegate variable-context host publication through HostEffects instead of owning runtime internals"
    );
    assert!(
        !host_effects.contains("struct MessageRuntime")
            && host_effects.contains(
                "pub(crate) async fn apply_after_batch_variable_context_request_from_state"
            )
            && host_effects.contains(".apply_after_batch_variable_context_request(")
            && host_effects.contains(".variable_context_host_effects_if_no_pending_tool_request(")
            && !host_effects.contains("apply_after_batch_materialized_history_request")
            && !host_effects
                .contains("materialized_history_host_effects_if_no_pending_tool_request"),
        "HostEffects should own deferred variable-context publication without materialized-history compatibility wrappers"
    );
    assert!(
        spine_bridge.contains(".apply_toolcall_host_commit(")
            && !spine_bridge.contains("ToolcallRuntime::apply_host_commit("),
        "session bridge should apply toolcall host commit effects through HostEffects, not a separate ToolcallRuntime pass-through"
    );
    assert!(
        !spine_bridge.contains("ToolcallRuntime")
            && !toolcall_lifecycle.contains("struct ToolcallRuntime")
            && !bridge_mod.contains("ToolcallRuntime")
            && spine_bridge.contains("prepare_completed_toolcall_for_commit(")
            && toolcall_lifecycle
                .contains("pub(crate) async fn prepare_completed_toolcall_for_commit"),
        "session bridge should not import a ToolcallRuntime facade; toolcall preparation should remain in named bridge functions"
    );
    assert!(
        !toolcall_lifecycle.contains("pub(crate) async fn apply_host_commit")
            && !toolcall_lifecycle.contains(".apply_toolcall_host_commit(")
            && toolcall_host_commit.contains("pub(crate) async fn apply_toolcall_host_commit"),
        "toolcall host commit effect application should live on HostEffects without a ToolcallRuntime pass-through"
    );
    let replay_facade = fs::read_to_string(spine_src("bridge/replay.rs"))
        .expect("read bridge replay facade source");
    let state_types = fs::read_to_string(spine_src("runtime/session_state/state_types.rs"))
        .expect("read session state types");
    assert!(
        state_types.contains("fn into_variable_context(")
            && replay_facade.contains("fn into_variable_context(")
            && !replay_facade.contains("fn into_materialized(")
            && !state_types.contains("fn into_materialized("),
        "replay runtime carriers should expose variable-context publication names from the replay facade without materialized-history compatibility wrappers"
    );
    assert!(
        host_effect.contains("SpineRootCompactHostPublish { variable_context }")
            && host_effect.contains("fn root_compact_variable_context_publication(")
            && !host_effect.contains("fn root_compact_variable_history_publication(")
            && host_effect.contains("RootCompactVariableContextPublication")
            && !host_effect.contains("RootCompactHistoryPublication")
            && host_effect.contains("apply_root_compact_variable_context_publication")
            && !host_effect.contains("apply_root_compact_history_publication")
            && host_effect
                .contains("multiple Spine root compact variable-context publications in one hook")
            && !host_effect
                .contains("multiple Spine root compact history publications in one hook")
            && host_effect.contains("host_publish.variable_context.len()")
            && host_effect.contains("published.extend_from_slice(&self.variable_context)")
            && host_effect.contains(".published_host_history_from_variable_context(")
            && !host_effect.contains(".published_variable_history_from_native_items(")
            && !host_effect.contains(".published_history_from_native_items(")
            && !host_effect.contains("host_publish.materialized.len()")
            && !host_effect.contains("published.extend_from_slice(&self.materialized)"),
        "root compact host publication should not expose parser materialization wording in host-effect internals"
    );
    assert!(
        host_effect.contains("PublishVariableContextAfterBatch")
            && host_effect.contains("fn publish_variable_context_after_batch(")
            && host_effect.contains("apply_after_batch_variable_context_request")
            && !host_effect.contains("PublishVariableHistoryAfterBatch")
            && !host_effect.contains("fn publish_variable_history_after_batch(")
            && !host_effect.contains("fn apply_after_batch_variable_history_request(")
            && !host_effect.contains("PublishMaterializedHistoryAfterBatch"),
        "after-batch host publication effect should name variable h(PS), not parser materialization"
    );
    assert!(
        host_effects.contains("RootCompactVariableContextPublication")
            && !host_effects.contains("RootCompactHistoryPublication")
            && !host_effects.contains("struct NativeCompactRuntime")
            && host_effects.contains("pub(crate) async fn apply_history_publication")
            && host_effects.contains(".apply_root_compact_variable_context_publication(")
            && !host_effects.contains(".apply_root_compact_history_publication("),
        "bridge host effects should carry root compact parser publication as variable context, while outer native compact code may still publish host history"
    );
    assert!(
        spine_bridge.contains("effects\n            .apply_history_publication(")
            && !spine_bridge.contains("NativeCompactRuntime::"),
        "session bridge should apply native compact host publication through HostEffects, not a separate NativeCompactRuntime facade"
    );
    assert!(
        !tasks_mod.contains("spine")
            && !tasks_mod.contains("Spine")
            && !tasks_mod.contains("SPINE")
            && tasks_mod.contains(".abort_pending_turn_commit_after_turn_abort()")
            && tasks_mod.contains(".close_pending_turn_commit_as_aborted_toolcall(")
            && spine_bridge.contains("pub(crate) async fn abort_pending_turn_commit_after_turn_abort(")
            && spine_bridge
                .contains("pub(crate) async fn close_pending_turn_commit_as_aborted_toolcall(")
            && !tasks_mod.contains("abort_stale_spine_pending")
            && !tasks_mod.contains("close_stale_spine_pending_as_aborted_toolcall"),
        "task abort should call generic session lifecycle hooks without direct Spine pending-control cleanup knowledge"
    );
    let record_token_usage = session_mod
        .split("pub(crate) async fn record_token_usage_info(")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) async fn recompute_token_usage(").next())
        .expect("record token usage source section");
    assert!(
        record_token_usage.contains(".observe_provider_input_tokens_for_projection(")
            && !record_token_usage.contains("observe_provider_token_usage")
            && !record_token_usage.contains("spine_slot")
            && spine_bridge
                .contains("pub(crate) async fn observe_provider_input_tokens_for_projection(")
            && spine_bridge.contains(".observe_provider_token_usage(input_tokens)"),
        "session token accounting should report provider usage through a bridge hook instead of locking Spine state directly"
    );
    let apply_after_publish = root_compact_session
        .split("pub(crate) fn apply_root_compact_after_history_publish(")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn take_pending_root_compact_after_history_publish")
                .next()
        })
        .expect("apply root compact after publish section");
    assert!(
        apply_after_publish.contains(
            "prepared.validate_published_variable_context_len(published_variable_context_len)?"
        ) && !apply_after_publish.contains("runtime.current_open_index()"),
        "session must validate the prepared root compact publication length before installing live PS"
    );
}
