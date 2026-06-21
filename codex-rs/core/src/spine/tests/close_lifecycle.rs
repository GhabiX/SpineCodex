use super::*;

// Close, reduce, and materialized history.

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
        .stage_close("close".to_string(), "test node memory".to_string())
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
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 0..3)),
        )
        .expect("commit close");

    let events = event_log(&runtime);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                SpineLedgerEvent::Msg { raw_ordinal, .. } => Some(*raw_ordinal),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![2],
        "only the real child suffix item should shift as Msg"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SpineLedgerEvent::Msg {
                raw_ordinal: 3 | 4,
                ..
            }
        )),
        "close request/output carriers must not shift as Msg"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SpineLedgerEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(SpineLedgerEvent::ToolCall { .. })
    ));
    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("close should reduce task tree into a tree node inside Nodes")
    };
    assert_eq!(nodes.len(), 2);
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
    assert!(matches!(
        &nodes[1],
        SpineTreeNode::ToolCallAsLeafNode { segments }
            if segments == &vec![tool_req(3, 1), tool_resp(4, 2)]
    ));
    assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
    assert_eq!(meta.index, 0);
    assert_eq!(meta.summary, "child");
    assert!(matches!(
        children.as_slice(),
        [
            SpineTreeNode::ToolCallAsLeafNode {
                segments,
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 2,
                },
                ..
            },
        ] if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
    ));
    assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
    assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));

    let memory_archive =
        std::fs::read_to_string(runtime.store.root.join(memory_path)).expect("memory archive");
    assert!(memory_archive.contains("compact_id: mem-1-1-1-0-3"));
    assert!(memory_archive.contains("source_context_range: [0..4)"));
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
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
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
fn task_tree_reduce_archive_failure_leaves_symbols_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = SpineArchive::staged_with_memory_body(
        dir.path().to_path_buf(),
        "bad-memory".to_string(),
        "wrong body".to_string(),
    );
    let node_id = NodeId::root_epoch(1).child(1);
    let meta = tree_meta(&archive, node_id.clone(), 0, "child".to_string()).expect("meta");
    let memory = memory_ref(
        &archive,
        "bad-memory".to_string(),
        node_id,
        sha1_hex(b"expected body"),
        0..1,
        0..1,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
            Symbol::Control(ControlSymbol::Close(memory)),
        ],
    };
    let before = parse_stack.symbols.clone();

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("archive failure must abort close reduction");
    assert!(
        err.to_string().contains("staged memory body hash mismatch"),
        "unexpected archive failure: {err}"
    );
    assert_eq!(
        parse_stack.symbols, before,
        "failed close reduction must not pop/truncate the live symbols"
    );
}

#[test]
fn open_toolcall_leaf_makes_close_suffix_non_empty() {
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
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(3, 3, &function_output("close"))
        .expect("observe close output");

    let commit = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 0..2)),
        )
        .expect("close open-only child")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close));
    assert_eq!(runtime.store.mems().expect("read mems").len(), 1);
    assert!(
        runtime.store.root.join("memory/mem-1-1-1-0-2.md").exists(),
        "close must archive memory for the open toolcall suffix"
    );
    assert!(
        runtime.store.root.join("nodes/1/1/1").exists(),
        "close must archive the child TaskTree"
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
        .stage_close("dup-close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("dup-close")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("dup-close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "dup-close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect("commit close");

    let events_after_first_commit = event_log_debug(&runtime);
    let mems_after_first_commit = runtime.store.mems().expect("read mems");
    assert_eq!(mems_after_first_commit.len(), 1);
    assert_eq!(
        runtime
            .maybe_commit_output(
                "dup-close",
                Some(memory_assembly_with_context_range("1.1.1", suffix_start..5)),
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
        .stage_close("close".to_string(), "test node memory".to_string())
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
            .contains("spine.close requires a validated source plan for memory assembly"),
        "unexpected close failure: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
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
fn close_artifact_write_failure_does_not_publish_parse_stack_or_ledger() {
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
        .stage_close("close".to_string(), "test node memory".to_string())
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
    let body_path = runtime.store.root.join("memory/mem-1-1-1-2-5.md");
    if let Some(parent) = body_path.parent() {
        std::fs::create_dir_all(parent).expect("create memory dir");
    }
    std::fs::create_dir_all(&body_path).expect("block memory body write with directory");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 2..5)),
        )
        .expect_err("artifact write failure should fail commit");
    assert!(
        !err.to_string().is_empty(),
        "expected artifact write failure to surface"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        !runtime.store.root.join("nodes/1/1/1/Memory.md").exists(),
        "artifact failure must not flush node Memory.md"
    );
    assert!(
        matches!(
            runtime.pending_commit("close").expect("pending retained"),
            Some(SpinePendingCommit::Close { .. })
        ),
        "failed artifact commit should retain pending close"
    );
}

#[test]
fn close_persistence_failure_leaves_retryable_close_token() {
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
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    std::fs::create_dir(runtime.store.mem_path()).expect("poison mem ledger path");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect_err("close mem persistence failure must fail");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("os error 21")
            || err.to_string().contains("Permission denied"),
        "unexpected close persistence failure: {err}"
    );

    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before,
        "failed close must not publish the reduced task tree"
    );
    assert_eq!(
        event_log_debug(&runtime),
        events_before,
        "failed close must not publish ledger events"
    );
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_)))),
        "failed close must retain the zero-width Close token for retry"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn close_retry_reduces_existing_pending_close_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open", "child");
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-retry");
    runtime
        .stage_close("close-retry".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-retry")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = current_context_len(&runtime, &raw) - 1;
    observe_function_output(&mut runtime, &mut raw, "close-retry");
    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);

    let prepared_memory = runtime
        .prepared_close_memory_for_test(
            Some(memory_assembly.clone()),
            SpineTokenBaselines::default(),
        )
        .expect("prepare close commit");
    runtime
        .parse_stack
        .shift_pending_close(prepared_memory, &runtime.archive())
        .expect("simulate retryable pending Close token");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));

    let commit = runtime
        .maybe_commit_output("close-retry", Some(memory_assembly))
        .expect("retry close")
        .expect("close should commit on retry");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("commit markers")
            .len(),
        1
    );
}

#[test]
fn close_retry_reuses_matching_prepared_memory() {
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
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);
    let compact_id = "mem-1-1-1-0-3";
    let prepared_mem = MemRecord {
        compact_id: compact_id.to_string(),
        kind: MemKind::Suffix,
        node: NodeId(vec![1, 1, 1]),
        raw_start: 0,
        raw_end: 3,
        context_start: suffix_start,
        context_end: close_request_index,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: memory_assembly.memory_output_tokens,
        body_path: format!("memory/{compact_id}.md"),
        body_hash: sha1_hex(memory_assembly.body.as_bytes()),
    };
    runtime
        .store
        .write_memory_body(&prepared_mem.compact_id, &memory_assembly.body)
        .expect("write prepared memory body");
    runtime
        .store
        .append_mem(&prepared_mem)
        .expect("append prepared mem");

    let commit = runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("retry close with matching prepared memory")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert_eq!(
        runtime.store.mems().expect("read mems after retry").len(),
        1,
        "retry must reuse matching suffix mem instead of appending duplicate"
    );
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("read commit markers")
            .len(),
        1,
        "retry should publish the explicit close commit proof"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_none(),
        "successful retry must clear pending close"
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
        .observe_context_item(5, 5, &spine_call(SPINE_TOOL_CLOSE, "close-inner"))
        .expect("observe inner close request");
    runtime
        .stage_close("close-inner".to_string(), "test node memory".to_string())
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
            Some(memory_assembly_with_context_range(
                "1.1.1.1",
                inner_suffix_start..5,
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
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments },
                ]
                    if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                        && meta.id == NodeId::root_epoch(1).child(1).child(1).child(1)
                        && meta.summary == "inner"
                        && segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
    ));

    runtime
        .observe_raw_items(1)
        .expect("record outer close request");
    runtime
        .observe_context_item(7, 7, &spine_call(SPINE_TOOL_CLOSE, "close-outer"))
        .expect("observe outer close request");
    runtime
        .stage_close("close-outer".to_string(), "test node memory".to_string())
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
            Some(memory_assembly_with_context_range(
                "1.1.1",
                outer_suffix_start..7,
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
            },
            SpineTreeNode::ToolCallAsLeafNode { segments },
        ] if meta.id == NodeId::root_epoch(1).child(1).child(1)
            && meta.summary == "outer"
            && segments == &vec![tool_req(7, 1), tool_resp(8, 2)]
            && matches!(
                children.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta: inner, children: inner_children, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments: inner_close_segments },
                ] if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                    && inner.summary == "inner"
                    && matches!(
                        inner_children.as_slice(),
                        [
                            SpineTreeNode::ToolCallAsLeafNode { segments },
                            SpineTreeNode::MsgAsLeafNode { .. },
                        ] if segments == &vec![tool_req(2, 2), tool_resp(3, 3)]
                    )
                    && inner_close_segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
            && trajs_path == &PathBuf::from("nodes/1/1/1/Trajs.md")
    ));
    let outer_trajs = std::fs::read_to_string(runtime.store.root.join("nodes/1/1/1/Trajs.md"))
        .expect("outer trajs");
    assert!(outer_trajs.contains("compact_id=mem-1-1-1-1-2-5"));
    assert!(outer_trajs.contains("node_id=1.1.1.1"));
    assert!(outer_trajs.contains("body_path="));
    assert!(outer_trajs.contains("memory_path=nodes/1/1/1/1/Memory.md"));
    assert!(outer_trajs.contains("trajs_path=nodes/1/1/1/1/Trajs.md"));
    assert!(!outer_trajs.contains("body_hash:"));
    assert!(!outer_trajs.contains("body:"));
    assert!(!outer_trajs.contains("Spine Memory 1.1.1.1"));
    assert!(!outer_trajs.contains("inner assistant traj"));
}
