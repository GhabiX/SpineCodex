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
        !observe.contains("replace_parse_stack_for_runtime_transition"),
        "runtime/observe.rs must not use the generic parser replacement escape hatch"
    );
    assert!(
        !observe.contains("staged_after_token"),
        "runtime/observe.rs must stage observations as lexed batches, not raw tokens"
    );
    assert!(
        !observe.contains("use crate::spine::model::SpineToken"),
        "runtime/observe.rs must not import raw SpineToken"
    );
    assert!(
        observe.contains("staged_after_lexed_batch_for_observe"),
        "runtime/observe.rs should stage ordinary observations through ParserState batch API"
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
fn parser_state_mutable_runtime_transition_handle_is_test_only() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    let marker = "#[cfg(test)]\n    pub(super) fn parse_stack_mut_for_runtime_transition";
    assert!(
        parser.contains(marker),
        "mutable ParserState runtime transition handle must remain test-only"
    );
}

#[test]
fn parser_state_does_not_expose_single_token_staging_api() {
    let parser = fs::read_to_string(spine_src("parser.rs")).expect("read parser source");
    assert!(
        !parser.contains("fn staged_after_token("),
        "ParserState should expose batch staging, not a single-token staging API"
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
fn runtime_commit_routes_open_token_staging_through_parser_state() {
    let commit =
        fs::read_to_string(spine_src("runtime/commit.rs")).expect("read runtime commit source");
    assert!(
        !commit.contains("self.parser.parse_stack().next_child_id()"),
        "runtime/commit.rs must not query open child ids through the raw parser handle"
    );
    assert!(
        !commit.contains("staged_parse_stack.shift("),
        "runtime/commit.rs open staging must not directly shift parser tokens"
    );
    assert!(
        commit.contains("self.parser.next_child_id()")
            && commit.contains(".staged_after_tokens([open_token"),
        "runtime open commit should route child id and token staging through ParserState"
    );
    let open_with_toolcall_install = commit
        .split(".staged_after_tokens([open_token, token]")
        .nth(1)
        .and_then(|tail| {
            tail.split("self.append_trim_candidates_for_completed_toolcall")
                .next()
        })
        .expect("open-with-toolcall install section");
    assert!(
        open_with_toolcall_install.contains(".install_staged(staged_parse_stack)")
            && !open_with_toolcall_install.contains("replace_parse_stack_for_runtime_transition"),
        "runtime open-with-toolcall should install staged parser state through ParserState"
    );
    let open_without_toolcall_install = commit
        .split(".staged_after_tokens([open_token]")
        .nth(1)
        .and_then(|tail| tail.split("Ok(SpinePreparedCommit").next())
        .expect("open-without-toolcall install section");
    assert!(
        open_without_toolcall_install.contains(".install_staged(staged_parse_stack)")
            && !open_without_toolcall_install
                .contains("replace_parse_stack_for_runtime_transition"),
        "runtime open-without-toolcall should install staged parser state through ParserState"
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
        commit.contains(".close_family_staged_parse_stacks(")
            && commit.contains(".close_reduced_next_child_id("),
        "runtime close/next commit should route staged parser reductions through ParserState"
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
            && commit.contains(".install_prepared_commit_final_parse_stack("),
        "runtime close/next commit should install pending/final parser states through named ParserState methods"
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
        root_compact.contains(".prepare_root_compact_reduction(")
            && root_compact.contains(".root_compact_staged_parse_stacks("),
        "runtime root compact should route staged parser reductions through ParserState"
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
        root_compact.contains(".install_pending_root_compact_after_side_effect_failure(")
            && root_compact.contains(".install_prepared_root_compact_final_parse_stack("),
        "runtime root compact should install pending/final parser states through named ParserState methods"
    );
}
