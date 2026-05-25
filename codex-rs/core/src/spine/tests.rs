use super::*;
use crate::spine::CHECKPOINT_VERSION;
use crate::spine::io::hash_response_items;
use codex_protocol::models::ContentItem;
use std::path::PathBuf;

fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn logged_events(runtime: &SpineRuntime) -> Vec<LoggedKEvent> {
    runtime.store.events_for_test().expect("events")
}

fn event_log(runtime: &SpineRuntime) -> Vec<KEvent> {
    logged_events(runtime)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

fn event_log_debug(runtime: &SpineRuntime) -> Vec<String> {
    event_log(runtime)
        .into_iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

fn spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text("ok".to_string()),
    }
}

fn compact_body_with_context_range(
    node_id: &str,
    source_context_range: Range<usize>,
) -> SpineCloseCompact {
    SpineCloseCompact {
        body: format!("# Spine Memory {node_id}\n\nreal compact body for {node_id}\n"),
        source_context_range,
    }
}

fn open_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
) {
    let request = spine_call(SPINE_TOOL_OPEN, call_id);
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(
            request_ordinal,
            usize::try_from(request_ordinal).expect("context index fits usize"),
            &request,
        )
        .expect("observe open request");
    runtime
        .stage_open(call_id.to_string(), summary.to_string())
        .expect("stage open");

    let output = function_output(call_id);
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(output.clone()));
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(
            output_ordinal,
            usize::try_from(output_ordinal).expect("context index fits usize"),
            &output,
        )
        .expect("observe open output");
    runtime
        .maybe_commit_output(call_id, None)
        .expect("commit open");
}

fn append_msg(runtime: &mut SpineRuntime, raw: &mut Vec<Option<ResponseItem>>, text: &str) {
    let item = text_item(text);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record msg");
    runtime
        .observe_context_item(
            raw_ordinal,
            usize::try_from(raw_ordinal).expect("context index fits usize"),
            &item,
        )
        .expect("observe msg");
}

fn close_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
) {
    let request = spine_call(SPINE_TOOL_CLOSE, call_id);
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(
            request_ordinal,
            usize::try_from(request_ordinal).expect("context index fits usize"),
            &request,
        )
        .expect("observe close request");
    runtime
        .stage_close(call_id.to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit(call_id)
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };

    let output = function_output(call_id);
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(output.clone()));
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(
            output_ordinal,
            usize::try_from(output_ordinal).expect("context index fits usize"),
            &output,
        )
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            call_id,
            Some(compact_body_with_context_range(
                node_id,
                suffix_start..raw.len(),
            )),
        )
        .expect("commit close");
}

#[test]
fn ordinary_response_item_shifts_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            KEvent::Init { raw_start: 0 },
            KEvent::Open { summary, .. },
            KEvent::Msg {
                raw_ordinal: 0,
                context_index: 0,
                from_user: true,
            }
        ] if summary == "root"
    ));
    assert_eq!(
        runtime.parse_stack().symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
            }]),
        ]
    );
}

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
        vec![item]
    );
    let tree = parse_stack.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current root"), "{tree}");
}

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

#[test]
fn spine_open_request_and_output_are_control_carriers_not_persistent_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    let output = function_output("open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let events = event_log(&runtime);
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], KEvent::Init { raw_start: 0 }));
    assert!(matches!(
        &events[1],
        KEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "root"
    ));
    assert!(matches!(
        &events[2],
        KEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "child"
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::Control(ControlSymbol::Open(child)),
        ] if meta.summary == "root"
            && meta.id == NodeId::root_epoch(1).child(1)
            && child.summary == "child"
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 0
    ));
    assert_eq!(
        runtime
            .materialize_history(&[Some(request), Some(output)])
            .expect("materialize history"),
        Vec::<ResponseItem>::new()
    );
}

#[test]
fn duplicate_open_call_id_does_not_create_second_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "dup-open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe first open request");

    runtime
        .observe_raw_items(1)
        .expect("record duplicate request");
    let err = runtime
        .observe_context_item(1, 1, &request)
        .expect_err("duplicate open request anchor must fail fast");
    assert!(
        err.to_string()
            .contains("duplicate spine.open request anchor for dup-open"),
        "unexpected duplicate error: {err}"
    );

    runtime
        .stage_open("dup-open".to_string(), "only child".to_string())
        .expect("stage open");
    let output = function_output("dup-open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("dup-open", None)
        .expect("commit open");
    let events_after_first_commit = event_log(&runtime);
    let event_debug_after_first_commit = event_log_debug(&runtime);
    assert_eq!(
        events_after_first_commit
            .iter()
            .filter(
                |event| matches!(event, KEvent::Open { summary, .. } if summary == "only child")
            )
            .count(),
        1
    );
    assert_eq!(
        runtime
            .maybe_commit_output("dup-open", None)
            .expect("duplicate output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), event_debug_after_first_commit);
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("- [1.1] Open root"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current only child"), "{tree}");
}

#[test]
fn clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&KEvent::Init { raw_start: 0 })
        .expect("append init");
    let mem = MemRecord {
        compact_id: "mem-missing".to_string(),
        kind: MemKind::Suffix,
        node: NodeId::root_epoch(1).child(1),
        raw_start: 0,
        raw_end: 1,
        context_start: 0,
        context_end: 1,
        raw_live_hash: None,
        body_path: "bodies/mem-missing.md".to_string(),
        body_hash: sha1_hex(b"missing body"),
    };
    source.append_mem(&mem).expect("append missing mem ref");

    let err =
        SpineStore::clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true])
            .expect_err("missing visible memory body must fail closed");
    assert!(
        err.to_string().contains("No such file") || err.to_string().contains("os error 2"),
        "unexpected clone error: {err}"
    );
}

#[test]
fn spine_close_output_does_not_shift_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..5)),
        )
        .expect("commit close");

    let events = event_log(&runtime);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                KEvent::Msg { raw_ordinal, .. } => Some(*raw_ordinal),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![2],
        "only the real child suffix item should shift as Msg"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            KEvent::Msg {
                raw_ordinal: 3 | 4,
                ..
            }
        )),
        "close request/output carriers must not shift as Msg"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, KEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(events.last(), Some(KEvent::Close { .. })));
    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("close should reduce task tree into a tree node inside Nodes")
    };
    assert_eq!(nodes.len(), 1);
    let SpineTreeNode::SpineTree {
        meta,
        children,
        memory_path,
        trajs_path,
        ..
    } = &nodes[0]
    else {
        panic!("close should reduce to SpineTree")
    };
    assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
    assert_eq!(meta.index, 0);
    assert_eq!(meta.summary, "child");
    assert!(matches!(
        children.as_slice(),
        [SpineTreeNode::MsgAsLeafNode {
            msg: SegRef::ResponseItem {
                raw_ordinal: 2,
                context_index: 2,
            },
            ..
        }]
    ));
    assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
    assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));

    let memory_archive =
        std::fs::read_to_string(runtime.store.root.join(memory_path)).expect("memory archive");
    assert!(memory_archive.contains("compact_id: mem-1-1-1-0-5"));
    assert!(memory_archive.contains("source_context_range: [0..5)"));
    assert!(memory_archive.contains("# Spine Memory 1.1.1"));
    let trajs_archive =
        std::fs::read_to_string(runtime.store.root.join(trajs_path)).expect("trajs archive");
    assert!(trajs_archive.contains("raw raw_ordinal=2 context_index=2"));
}

#[test]
fn empty_task_tree_reduce_fails_without_archive_side_effects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let archive = runtime.archive();
    let node_id = NodeId::root_epoch(1).child(1);
    let open = Symbol::Control(ControlSymbol::Open(
        tree_meta(&archive, node_id.clone(), 0, "empty".to_string()).expect("meta"),
    ));
    let memory = memory_ref(
        &archive,
        "empty-memory".to_string(),
        node_id,
        sha1_hex(b"empty"),
        0..0,
        0..0,
        0..0,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![open, Symbol::Control(ControlSymbol::Close(memory))],
    };

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("open close without Nodes must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected empty task close error: {err}"
    );
    assert!(
        !runtime.store.root.join("nodes/1/1").exists(),
        "empty close must not archive a TaskTree"
    );
}

#[test]
fn empty_spine_close_does_not_commit_memory_or_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "empty child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(3, 3, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let err = runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", 0..4)),
        )
        .expect_err("empty live suffix must not close");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected empty close error: {err}"
    );
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
    assert!(
        runtime.store.mems().expect("read mems").is_empty(),
        "empty close must not append a memory record"
    );
}

#[test]
fn duplicate_close_call_id_does_not_create_second_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "dup-close"))
        .expect("observe close request");
    runtime
        .stage_close("dup-close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("dup-close")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("dup-close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "dup-close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..5)),
        )
        .expect("commit close");

    let events_after_first_commit = event_log_debug(&runtime);
    let mems_after_first_commit = runtime.store.mems().expect("read mems");
    assert_eq!(mems_after_first_commit.len(), 1);
    assert_eq!(
        runtime
            .maybe_commit_output(
                "dup-close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..5)),
            )
            .expect("duplicate close output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), events_after_first_commit);
    assert_eq!(
        runtime
            .store
            .mems()
            .expect("read mems after duplicate")
            .len(),
        1
    );
}

#[test]
fn close_failure_does_not_mutate_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let err = runtime
        .maybe_commit_output("close", None)
        .expect_err("close without compact output must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires a completed suffix compact"),
        "unexpected close failure: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before
    );
    assert_eq!(event_log_debug(&runtime), events_before);
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn close_persistence_failure_does_not_mutate_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    std::fs::create_dir(runtime.store.mem_path()).expect("poison mem ledger path");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..5)),
        )
        .expect_err("close mem persistence failure must fail");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("os error 21")
            || err.to_string().contains("Permission denied"),
        "unexpected close persistence failure: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before
    );
    assert_eq!(event_log_debug(&runtime), events_before);
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn nested_close_reduces_inner_tree_into_parent_nodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(1)
        .expect("record outer open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "outer"))
        .expect("observe outer open request");
    runtime
        .stage_open("outer".to_string(), "outer".to_string())
        .expect("stage outer open");
    runtime.observe_raw_items(1).expect("record outer output");
    runtime
        .observe_context_item(1, 1, &function_output("outer"))
        .expect("observe outer output");
    runtime
        .maybe_commit_output("outer", None)
        .expect("commit outer");

    runtime
        .observe_raw_items(1)
        .expect("record inner open request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_OPEN, "inner"))
        .expect("observe inner open request");
    runtime
        .stage_open("inner".to_string(), "inner".to_string())
        .expect("stage inner open");
    runtime.observe_raw_items(1).expect("record inner output");
    runtime
        .observe_context_item(3, 3, &function_output("inner"))
        .expect("observe inner output");
    runtime
        .maybe_commit_output("inner", None)
        .expect("commit inner");

    runtime.observe_raw_items(1).expect("record inner raw");
    runtime
        .observe_context_item(4, 4, &text_item("inner body"))
        .expect("observe inner raw");
    runtime
        .observe_raw_items(1)
        .expect("record inner close request");
    runtime
        .stage_close("close-inner".to_string(), None)
        .expect("stage inner close");
    let inner_suffix_start = match runtime
        .pending_commit("close-inner")
        .expect("pending inner close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending inner close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record inner close output");
    runtime
        .observe_context_item(6, 6, &function_output("close-inner"))
        .expect("observe inner close output");
    runtime
        .maybe_commit_output(
            "close-inner",
            Some(compact_body_with_context_range(
                "1.1.1.1",
                inner_suffix_start..7,
            )),
        )
        .expect("commit inner close");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::Control(ControlSymbol::Open(outer)),
            Symbol::SpineTreeNodes(nodes),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && outer.id == NodeId::root_epoch(1).child(1).child(1)
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::SpineTree { meta, .. }]
                    if meta.id == NodeId::root_epoch(1).child(1).child(1).child(1)
                        && meta.summary == "inner"
            )
    ));

    runtime
        .observe_raw_items(1)
        .expect("record outer close request");
    runtime
        .stage_close("close-outer".to_string(), None)
        .expect("stage outer close");
    let outer_suffix_start = match runtime
        .pending_commit("close-outer")
        .expect("pending outer close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending outer close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record outer close output");
    runtime
        .observe_context_item(8, 8, &function_output("close-outer"))
        .expect("observe outer close output");
    runtime
        .maybe_commit_output(
            "close-outer",
            Some(compact_body_with_context_range(
                "1.1.1",
                outer_suffix_start..9,
            )),
        )
        .expect("commit outer close");

    let Some(Symbol::SpineTreeNodes(root_nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("outer close should reduce to root Nodes")
    };
    assert!(matches!(
        root_nodes.as_slice(),
        [
            SpineTreeNode::SpineTree {
                meta,
                children,
                trajs_path,
                ..
            }
        ] if meta.id == NodeId::root_epoch(1).child(1).child(1)
            && meta.summary == "outer"
            && matches!(
                children.as_slice(),
                [SpineTreeNode::SpineTree { meta: inner, .. }]
                    if inner.summary == "inner"
            )
            && trajs_path == &PathBuf::from("nodes/1/1/1/Trajs.md")
    ));
    let outer_trajs = std::fs::read_to_string(runtime.store.root.join("nodes/1/1/1/Trajs.md"))
        .expect("outer trajs");
    assert!(outer_trajs.contains("compact_id=mem-1-1-1-1-2-7"));
    assert!(outer_trajs.contains("node_id=1.1.1.1"));
    assert!(outer_trajs.contains("body_path="));
    assert!(outer_trajs.contains("memory_path=nodes/1/1/1/1/Memory.md"));
    assert!(outer_trajs.contains("trajs_path=nodes/1/1/1/1/Trajs.md"));
    assert!(!outer_trajs.contains("body_hash:"));
    assert!(!outer_trajs.contains("body:"));
    assert!(!outer_trajs.contains("Spine Memory 1.1.1.1"));
    assert!(!outer_trajs.contains("inner assistant traj"));
}

#[test]
fn layer_1_2_4_example_trace_replays_shift_reduce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root work");
    open_task(&mut runtime, &mut raw, "open-1-1", "task 1.1");
    append_msg(&mut runtime, &mut raw, "1.1 work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    open_task(&mut runtime, &mut raw, "open-1-2", "task 1.2");
    append_msg(&mut runtime, &mut raw, "1.2 work");
    open_task(&mut runtime, &mut raw, "open-1-2-1", "task 1.2.1");
    append_msg(&mut runtime, &mut raw, "1.2.1 work");
    close_task(&mut runtime, &mut raw, "close-1-2-1", "1.2.1");
    open_task(&mut runtime, &mut raw, "open-1-2-2", "task 1.2.2");
    append_msg(&mut runtime, &mut raw, "1.2.2 work");
    close_task(&mut runtime, &mut raw, "close-1-2-2", "1.2.2");
    close_task(&mut runtime, &mut raw, "close-1-2", "1.2");
    append_msg(&mut runtime, &mut raw, "1.3 work");
    runtime
        .root_compact("root epoch 1 memory".to_string(), raw.len())
        .expect("root compact");
    append_msg(&mut runtime, &mut raw, "2.1 work");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
            Symbol::SpineTreeNodes(nodes),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == raw.len() - 1
            && matches!(
                nodes.as_slice(),
                [
                    SpineTreeNode::MsgAsLeafNode {
                        msg: SegRef::ResponseItem {
                            raw_ordinal,
                            context_index,
                        },
                        ..
                    }
                ] if *raw_ordinal == u64::try_from(raw.len() - 1).expect("ordinal")
                    && *context_index == raw.len() - 1
            )
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );

    let tree = replayed.parse_stack().render_tree().expect("render tree");
    assert!(tree.contains("[1] Done root"), "{tree}");
    assert!(tree.contains("[2.1] Current root"), "{tree}");
    assert!(
        !tree.contains("[1.2.1]") && !tree.contains("[1.2.2]"),
        "closed descendants of a previous root epoch must stay folded: {tree}"
    );

    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 2);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root epoch 1 memory")
            )
    ));
    assert_eq!(materialized[1], text_item("2.1 work"));
}

#[test]
fn fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent_rollout = dir.path().join("parent.jsonl");
    let child_rollout = dir.path().join("child.jsonl");
    let mut raw = Vec::new();
    let mut parent = SpineRuntime::load_or_create(&parent_rollout, 0).expect("create parent");

    append_msg(&mut parent, &mut raw, "parent root before child");
    open_task(&mut parent, &mut raw, "open-child", "child task");
    append_msg(&mut parent, &mut raw, "child work");
    close_task(&mut parent, &mut raw, "close-child", "1.1.1");
    append_msg(&mut parent, &mut raw, "parent after child");

    let parent_materialized = parent.materialize_history(&raw).expect("parent h(PS)");
    let parent_stack_before_child_work = parent.parse_stack().clone();
    let parent_tree_events_before_child_work = event_log_debug(&parent);
    let parent_root = parent.store.root.clone();

    let raw_live = vec![true; raw.len()];
    SpineStore::clone_for_rollout_with_raw_live(&parent_rollout, &child_rollout, &raw_live)
        .expect("clone sidecar");
    let child = SpineRuntime::load_for_rollout_items(&child_rollout, &raw, &[])
        .expect("load child")
        .expect("child sidecar exists");
    let child_root = child.store.root.clone();

    assert_ne!(child_root, parent_root);
    assert_eq!(
        child.materialize_history(&raw).expect("child h(PS)"),
        parent_materialized,
        "fork child h(PS) must match parent at fork boundary"
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = child.parse_stack().symbols.last() else {
        panic!("fork child should replay parent root nodes");
    };
    let child_meta_dir = match nodes.as_slice() {
        [
            SpineTreeNode::MsgAsLeafNode { .. },
            SpineTreeNode::SpineTree {
                meta,
                memory_path,
                trajs_path,
                ..
            },
            SpineTreeNode::MsgAsLeafNode { .. },
        ] => {
            assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
            assert!(meta.node_dir.starts_with(&child_root));
            assert!(!meta.node_dir.starts_with(&parent_root));
            assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
            assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));
            meta.node_dir.clone()
        }
        other => panic!("unexpected fork child nodes: {other:?}"),
    };
    let child_memory_archive =
        std::fs::read_to_string(child_meta_dir.join("Memory.md")).expect("child Memory.md");
    let child_trajs_archive =
        std::fs::read_to_string(child_meta_dir.join("Trajs.md")).expect("child Trajs.md");
    assert!(child_memory_archive.contains("Spine Memory 1.1.1"));
    assert!(child_trajs_archive.contains("raw raw_ordinal=3"));
    assert!(child_trajs_archive.contains("context_index=3"));
    assert!(child_meta_dir.join("Memory.md").exists());
    assert!(child_meta_dir.join("Trajs.md").exists());

    let mut child = child;
    open_task(&mut child, &mut raw, "child-open-only", "child-only task");
    append_msg(&mut child, &mut raw, "child-only work");
    close_task(&mut child, &mut raw, "child-close-only", "1.1.2");

    let reloaded_parent = SpineRuntime::load_for_rollout(&parent_rollout, parent.raw_len)
        .expect("reload parent")
        .expect("parent sidecar exists");
    assert_eq!(
        reloaded_parent.parse_stack(),
        &parent_stack_before_child_work
    );
    assert_eq!(
        event_log_debug(&reloaded_parent),
        parent_tree_events_before_child_work
    );
}

#[test]
fn open_close_replay_materializes_closed_child_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("[1.1] Current"));
    assert!(tree.contains("[1.1.1] Done child task"));

    let materialized = replayed
        .materialize_history(&raw)
        .expect("materialize history");
    assert_eq!(materialized.len(), 2);
    assert_eq!(materialized[0], text_item("before"));
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
        )
    ));
}

#[test]
fn tree_renders_from_parse_stack_without_mutating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let before = replayed.parse_stack().clone();
    let tree = replayed.render_tree().expect("render tree");
    assert_eq!(replayed.parse_stack(), &before);
    assert_eq!(
        tree,
        replayed.parse_stack().render_tree().expect("render ps")
    );
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("[1.1] Current root"), "{tree}");
    assert!(tree.contains("[1.1.1] Done child task"), "{tree}");
    assert!(
        tree.contains("memory=nodes/1/1/1/Memory.md")
            && tree.contains("trajs=nodes/1/1/1/Trajs.md"),
        "{tree}"
    );
}

#[test]
fn materialize_history_renders_from_parse_stack_memory_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
        )
        .expect("commit close");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.compact_id, "mem-1-1-1-1-6");
    assert_eq!(memory.source_context_range, 1..6);
    assert_eq!(memory.source_raw_range, 1..6);
    let memory_seg = SegRef::from_memory_ref(memory);
    assert!(matches!(
        &memory_seg,
        SegRef::Memory {
            memory_id,
            body_path,
        } if memory_id == "mem-1-1-1-1-6"
            && body_path.ends_with("memory/mem-1-1-1-1-6.md")
    ));
    let memory_only = ParseStack {
        symbols: vec![Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
            msg: memory_seg,
            from_user: true,
        }])],
    };
    let rendered_memory =
        render_parse_stack_to_context(&memory_only, &[]).expect("render SegRef::Memory");
    assert!(matches!(
        rendered_memory.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 2);
    assert_eq!(materialized[0], text_item("before"));
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
}

#[test]
fn materialization_skips_rolled_back_raw_items_without_shifting_ordinals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept item");
    runtime
        .observe_context_item(2, 2, &text_item("after rollback"))
        .expect("observe surviving item");
    let materialized = runtime.materialize_history(&raw).expect("materialize");

    assert_eq!(
        materialized,
        vec![text_item("kept"), text_item("after rollback")]
    );
}

#[test]
fn rollback_keeps_open_when_request_item_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(text_item("open request")),
        None,
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open root"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current child task"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![text_item("before")]
    );
}

#[test]
fn rollback_skips_open_when_request_item_is_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        None,
        Some(text_item("open output")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current root"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![text_item("before")]
    );
}

#[test]
fn rollback_hole_rejects_suffix_memory_span() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(text_item("open request")),
        Some(function_output("open")),
        None,
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime
        .observe_raw_items(1)
        .expect("record rolled-back child raw");
    runtime
        .observe_context_item(3, 3, &text_item("rolled back child"))
        .expect("observe rolled-back child raw");
    runtime
        .stage_close("close".to_string(), None)
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(compact_body_with_context_range("1.1.1", suffix_start..4)),
        )
        .expect("commit close");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("suffix memory spanning a rollback hole must fail closed");
    assert!(
        err.to_string()
            .contains("memory mem-1-1-1-1-5 does not cover live raw evidence"),
        "unexpected materialization error: {err}"
    );
}

#[test]
fn native_compact_shifts_compact_and_new_root_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before compact"))
        .expect("observe first context item");
    runtime
        .observe_context_item(1, 1, &text_item("more context"))
        .expect("observe second context item");

    runtime
        .root_compact("root summary".to_string(), 1)
        .expect("compact root");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            KEvent::Init { .. },
            KEvent::Open { summary, .. },
            KEvent::Msg { raw_ordinal: 0, .. },
            KEvent::Msg { raw_ordinal: 1, .. },
            KEvent::RootCompact {
                boundary: 2,
                next_open_index: 1,
                ..
            },
        ] if summary == "root"
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.compact_id == "root-1-2"
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == 1
            && next_root.summary == "root"
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
}

#[test]
fn native_compact_failure_leaves_parse_stack_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before failed compact"))
        .expect("observe context item");
    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();

    let err = runtime
        .root_compact("   \n\t".to_string(), 1)
        .expect_err("empty native compact body must fail closed");
    assert!(
        err.to_string()
            .contains("spine root compact memory body must not be empty"),
        "unexpected empty compact error: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before
    );
    assert_eq!(event_log_debug(&runtime), events_before);
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
}

#[test]
fn root_compact_survives_rollback_without_new_raw_items() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime.raw_live = vec![true, false];
    runtime
        .root_compact("root summary after rollback".to_string(), 1)
        .expect("compact root");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let materialized = replayed
        .materialize_history(&raw_after_rollback)
        .expect("materialize");
    assert_eq!(materialized.len(), 1);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root summary after rollback")
            )
    ));
}

#[test]
fn checkpoint_before_user_msg_records_recoverable_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let context = vec![text_item("kept context")];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime
        .checkpoint_before_user_msg(&rollout, 0, &context)
        .expect("write checkpoint");
    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("first user"))
        .expect("shift user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(0)
        .expect("read checkpoint");
    assert_eq!(checkpoint.version, CHECKPOINT_VERSION);
    assert_eq!(checkpoint.checkpoint_id, "pre-user-00000000000000000000");
    assert_eq!(checkpoint.rollout_path, rollout.display().to_string());
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.token_seq, 2);
    assert_eq!(checkpoint.raw_live_hash, hash_raw_live(&[]));
    assert_eq!(checkpoint.context_len, 1);
    assert_eq!(checkpoint.cursor, "1.1");
    assert_eq!(
        checkpoint.parse_stack.symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
        ]
    );
    assert_eq!(checkpoint.tree_meta.len(), 2);
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert_eq!(
        checkpoint.h_ps_hash,
        hash_response_items(&context).expect("hash")
    );
}

#[test]
fn initial_checkpoint_records_root_open_without_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .checkpoint_initial(&rollout, &[])
        .expect("write initial checkpoint");
    let checkpoint = runtime
        .store
        .initial_checkpoint_for_test()
        .expect("read initial checkpoint");

    assert_eq!(checkpoint.checkpoint_id, "initial");
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.context_len, 0);
    assert_eq!(checkpoint.cursor, "1.1");
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert!(matches!(
        checkpoint.parse_stack.symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root))
        ] if root.id == NodeId::root_epoch(1).child(1)
            && root.summary == "root"
    ));
}

#[test]
fn rollback_uses_pre_user_checkpoint_to_restore_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed.parse_stack().symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &replayed.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &replayed.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
            }]),
        ]
    );
    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![text_item("kept")]
    );
}

#[test]
fn rollback_checkpoint_replays_new_live_append_after_cut() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime.observe_raw_items(1).expect("observe new raw");
    runtime
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("observe new user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![text_item("kept"), text_item("after rollback")]
    );
    let Some(Symbol::SpineTreeNodes(nodes)) = replayed.parse_stack().symbols.last() else {
        panic!("expected root nodes after replay")
    };
    assert!(matches!(
        nodes.as_slice(),
        [
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                ..
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 1,
                },
                ..
            },
        ]
    ));
}

#[test]
fn rollback_checkpoint_new_open_reuses_restored_sibling_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(spine_call(SPINE_TOOL_OPEN, "new-open")),
        Some(function_output("new-open")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_raw_items(1)
        .expect("observe new open request");
    runtime
        .observe_context_item(2, 1, &spine_call(SPINE_TOOL_OPEN, "new-open"))
        .expect("observe new open request");
    runtime
        .stage_open("new-open".to_string(), "restored sibling".to_string())
        .expect("stage new open");
    runtime
        .observe_raw_items(1)
        .expect("observe new open output");
    runtime
        .observe_context_item(3, 2, &function_output("new-open"))
        .expect("observe new open output");
    runtime
        .maybe_commit_output("new-open", None)
        .expect("commit new open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open root"), "{tree}");
    assert!(
        tree.contains("- [1.1.1] Current restored sibling"),
        "{tree}"
    );
    assert!(matches!(
        replayed.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
            Symbol::Control(ControlSymbol::Open(child)),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    ..
                }]
            )
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 1
            && child.summary == "restored sibling"
    ));
}

#[test]
fn rollback_without_pre_user_checkpoint_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None, Some(text_item("new turn"))];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_context_item(2, 1, &text_item("new turn"))
        .expect("observe new user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback without checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}

#[test]
fn checkpoint_missing_required_field_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
        .expect("write checkpoint");
    let checkpoint_path = runtime.store.checkpoint_path(1);
    let mut checkpoint = serde_json::to_value(
        runtime
            .store
            .checkpoint_for_test(1)
            .expect("read checkpoint"),
    )
    .expect("checkpoint to json value");
    checkpoint
        .as_object_mut()
        .expect("checkpoint object")
        .remove("parse_stack");
    std::fs::write(
        &checkpoint_path,
        serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
    )
    .expect("overwrite checkpoint for missing field test");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("checkpoint with missing required field must fail closed");
    assert!(
        err.to_string().contains("missing field `parse_stack`"),
        "unexpected error: {err}"
    );
}

#[test]
fn corrupt_checkpoint_hash_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
        .expect("write checkpoint");
    let checkpoint_path = runtime.store.checkpoint_path(1);
    let mut checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    checkpoint.h_ps_hash = "bad-hash".to_string();
    std::fs::write(
        &checkpoint_path,
        serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
    )
    .expect("overwrite checkpoint for corruption test");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("corrupt checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("spine checkpoint h(PS) hash mismatch"),
        "unexpected error: {err}"
    );
}
