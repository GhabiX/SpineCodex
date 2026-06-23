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
