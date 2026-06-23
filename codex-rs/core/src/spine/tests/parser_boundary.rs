use std::fs;
use std::path::PathBuf;

fn spine_src(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("spine")
        .join(path)
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
        observe.contains("staged_after_token")
            && observe.contains("staged_after_lexed_batch_for_observe"),
        "runtime/observe.rs should stage ordinary observations through ParserState"
    );
}

#[test]
fn parser_state_documents_spine_ownership_chain() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    assert!(
        parser.contains("hook -> lexer -> parser -> PS -> h(PS) -> host publication"),
        "parser facade must document the semantic ownership chain"
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
        replay.contains("ParserState::from_replay_events_with_forced_events")
            && replay.contains("parser.apply_replay_event"),
        "runtime replay should route replay token consumption through ParserState"
    );
}

#[test]
fn runtime_load_checkpoint_replay_routes_through_parser_state() {
    let load = fs::read_to_string(spine_src("runtime/load.rs")).expect("read runtime load source");
    assert!(
        !load.contains("parse_stack_from_events_with_forced_events"),
        "runtime/load.rs must not rebuild checkpoint ParseStack through parse_stack replay helpers"
    );
    assert!(
        load.contains("ParserState::from_replay_events_with_forced_events"),
        "checkpoint prefix replay should route through ParserState"
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
        commit.contains(".current_open_has_nodes()"),
        "runtime commit should route current-open node queries through ParserState"
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
