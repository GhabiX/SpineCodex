use super::*;

#[test]
fn materialize_history_requires_visible_msg_raw_item() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let err = runtime
        .materialize_history(&[None])
        .expect_err("h(PS) must render visible Msg from ParseStack, not raw gaps");
    assert!(
        err.to_string()
            .contains("missing raw item for visible Msg raw ordinal 0"),
        "unexpected materialization error: {err}"
    );
}
