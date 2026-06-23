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
    let observe = fs::read_to_string(spine_src("runtime/observe.rs"))
        .expect("read observe runtime source");
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
