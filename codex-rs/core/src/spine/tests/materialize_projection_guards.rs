use super::*;
use crate::spine::render::render_parse_stack_to_context;

#[test]
fn end_token_is_retained_as_control_epsilon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let raw = vec![Some(item.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let mut parse_stack = runtime.parse_stack().clone();
    parse_stack
        .shift(SpineToken::End, &runtime.archive())
        .expect("shift End");

    assert!(matches!(
        parse_stack.symbols.last(),
        Some(Symbol::Control(ControlSymbol::End))
    ));
    assert_eq!(
        render_parse_stack_to_context(&parse_stack, &raw).expect("render context"),
        vec![anchored_text_item(1, "ordinary")]
    );
    let tree = parse_stack.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current"), "{tree}");
}
