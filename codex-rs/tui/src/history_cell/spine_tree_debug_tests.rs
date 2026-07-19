use super::*;

#[test]
fn tree_renders_ids_statuses_and_rollout_ranges_without_memory() {
    let mut previous = node(
        "1",
        None,
        SpineTreeNodeKind::RootEpoch,
        SpineTreeNodeStatus::Compacted,
    );
    previous.summary = Some("previous root".to_string());
    previous.memory_summary = Some("old scope".to_string());
    previous.end = Some(5);

    let mut outer = node(
        "2",
        None,
        SpineTreeNodeKind::RootEpoch,
        SpineTreeNodeStatus::Opened,
    );
    outer.summary = Some("outer".to_string());
    outer.start = 5;

    let mut active = node(
        "2.1",
        Some("2"),
        SpineTreeNodeKind::Task,
        SpineTreeNodeStatus::Live,
    );
    active.summary = Some("active".to_string());
    active.start = 6;
    active.context_pressure = Some(codex_app_server_protocol::SpineNodeContextPressure {
        open_input_tokens: Some(10_000),
        current_input_tokens: Some(42_000),
        context_tokens: Some(32_000),
        problem: None,
    });

    let cell = new_debug_spine_tree_snapshot(SpineTreeUpdatedNotification {
        thread_id: "thread".to_string(),
        turn_id: "turn".to_string(),
        snapshot_seq: 7,
        active_node_id: "2.1".to_string(),
        nodes: vec![previous, outer, active],
    });

    insta::assert_snapshot!(render(&cell.display_lines(100)), @r###"
    • Debug Spine Tree  current 2.1
      ├ 1 previous root compacted (root epoch, rollout 0..5)
      └ 2 outer open (root epoch, rollout 5..)
        └ 2.1 active current (task, rollout 6.., ~32.0K inclusive context)
    "###);
    assert!(!render(&cell.raw_lines()).contains("old scope"));
}

#[test]
fn node_detail_renders_all_available_fields_and_memory() {
    let mut active = node(
        "2.1",
        Some("2"),
        SpineTreeNodeKind::Task,
        SpineTreeNodeStatus::Live,
    );
    active.summary = Some("active".to_string());
    active.memory_summary = Some("first memory line\nsecond memory line".to_string());
    active.start = 6;
    active.context_pressure = Some(codex_app_server_protocol::SpineNodeContextPressure {
        open_input_tokens: Some(10_000),
        current_input_tokens: Some(42_000),
        context_tokens: Some(32_000),
        problem: None,
    });

    let cell = new_debug_spine_node_snapshot(
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 7,
            active_node_id: "2.1".to_string(),
            nodes: vec![
                node(
                    "2",
                    None,
                    SpineTreeNodeKind::RootEpoch,
                    SpineTreeNodeStatus::Opened,
                ),
                active,
            ],
        },
        "2.1".to_string(),
    );

    insta::assert_snapshot!(render(&cell.display_lines(100)), @r###"
    • Debug Spine Node  2.1  current 2.1
      id: 2.1
      parent: 2
      kind: task
      status: live (current)
      summary: active
      rollout: 6..
      open input tokens: 10000
      current input tokens: 42000
      inclusive context tokens: 32000
      memory:
        first memory line
        second memory line
    "###);
    assert_eq!(
        render(&cell.raw_lines()),
        "Debug Spine Node 2.1 current 2.1\n  id: 2.1\n  parent: 2\n  kind: task\n  status: live (current)\n  summary: active\n  rollout: 6..\n  open input tokens: 10000\n  current input tokens: 42000\n  inclusive context tokens: 32000\n  memory:\n    first memory line\n    second memory line"
    );
}

#[test]
fn node_detail_omits_memory_when_unavailable() {
    let cell = new_debug_spine_node_snapshot(
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 7,
            active_node_id: "1".to_string(),
            nodes: vec![node(
                "1",
                None,
                SpineTreeNodeKind::RootEpoch,
                SpineTreeNodeStatus::Live,
            )],
        },
        "1".to_string(),
    );

    let rendered = render(&cell.display_lines(100));
    assert!(rendered.contains("Debug Spine Node"));
    assert!(!rendered.contains("memory:"));
}

fn render(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn node(
    node_id: &str,
    parent_id: Option<&str>,
    kind: SpineTreeNodeKind,
    status: SpineTreeNodeStatus,
) -> SpineTreeNode {
    SpineTreeNode {
        node_id: node_id.to_string(),
        parent_id: parent_id.map(str::to_string),
        kind,
        status,
        summary: None,
        memory_summary: None,
        start: 0,
        end: None,
        context_pressure: None,
    }
}
