use super::*;

#[test]
fn renders_ids_statuses_rollout_ranges_and_memory() {
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

    let cell = new_debug_spine_tree_snapshot(SpineTreeUpdatedNotification {
        thread_id: "thread".to_string(),
        turn_id: "turn".to_string(),
        snapshot_seq: 7,
        active_node_id: "2.1".to_string(),
        nodes: vec![previous, outer, active],
    });

    insta::assert_snapshot!(render(&cell.display_lines(100)), @r###"
    • Debug Spine Tree  current 2.1
      ├ 1 previous root compacted (root epoch, rollout 0..5, memory: old scope)
      └ 2 outer open (root epoch, rollout 5..)
        └ 2.1 active current (task, rollout 6..)
    "###);
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
    }
}
