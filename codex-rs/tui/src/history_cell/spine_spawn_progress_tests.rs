use super::plain_lines;
use super::spine_spawn_progress::SpineSpawnOverlay;
use crate::multi_agents::AgentActivityPathDisplay;
use crate::multi_agents::AgentActivityPreview;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use codex_app_server_protocol::SpineSpawnTaskProgress;
use codex_app_server_protocol::ThreadItem;

#[test]
fn renders_live_mixed_child_statuses() {
    let cell = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![
            SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "inspect native events".to_string(),
                agent_path: Some("/root/inspector".to_string()),
                status: CollabAgentStatus::Completed,
            },
            SpineSpawnTaskProgress {
                ordinal: 1,
                summary: "verify cancellation".to_string(),
                agent_path: Some("/root/verifier".to_string()),
                status: CollabAgentStatus::Running,
            },
        ],
    });

    let rendered = plain_lines(cell.display_lines("  │  ", true, 80))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("spine.spawn 1 running · 1 complete"),
        "{rendered}"
    );
    assert!(rendered.contains("├┈ ✓ [0] inspect native events"));
    assert!(rendered.contains("└┈ ◐ [1] verify cancellation"));
    assert!(rendered.contains("Waiting for activity..."));
    assert_eq!(cell.display_lines("  │  ", true, 80).len(), 6);
}

#[test]
fn activity_refresh_keeps_the_newest_three_lines() {
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "inspect events".to_string(),
            agent_path: Some("/root/inspector".to_string()),
            status: CollabAgentStatus::Running,
        }],
    });
    let items = (1..=4)
        .map(|index| ThreadItem::AgentMessage {
            id: format!("message-{index}"),
            text: format!("activity {index}"),
            phase: None,
            memory_citation: None,
        })
        .collect::<Vec<_>>();
    assert!(overlay.update_activity(
        "/root/inspector",
        AgentActivityPreview::from_items(items.iter().rev(), AgentActivityPathDisplay::Hide),
        /*status*/ None,
    ));

    let rendered = plain_lines(overlay.display_lines("  ", true, 80))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("activity 1"));
    assert!(rendered.contains("activity 2\n"));
    assert!(rendered.contains("activity 3\n"));
    assert!(rendered.ends_with("activity 4"));
    assert_eq!(overlay.display_lines("  ", true, 80).len(), 5);
}

#[test]
fn aggregate_status_keeps_terminal_outcomes_truthful() {
    let cell = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![
            SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "completed".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Completed,
            },
            SpineSpawnTaskProgress {
                ordinal: 1,
                summary: "interrupted".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Interrupted,
            },
            SpineSpawnTaskProgress {
                ordinal: 2,
                summary: "failed".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Errored,
            },
            SpineSpawnTaskProgress {
                ordinal: 3,
                summary: "stopped".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Shutdown,
            },
        ],
    });
    let rendered = plain_lines(cell.display_lines("  ", true, 80))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("0 running"), "{rendered}");
    assert!(rendered.contains("1 complete"), "{rendered}");
    assert!(rendered.contains("1 failed"), "{rendered}");
    assert!(rendered.contains("1 interrupted"), "{rendered}");
    assert!(rendered.contains("1 stopped"), "{rendered}");
    assert!(!rendered.contains("✓ spine.spawn"));
}

#[test]
fn narrow_width_preserves_tree_prefixes_and_fixed_activity_rows() {
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "a deliberately long task summary that needs wrapping".to_string(),
            agent_path: Some("/root/worker".to_string()),
            status: CollabAgentStatus::Running,
        }],
    });
    let items = (1..=3)
        .map(|index| ThreadItem::AgentMessage {
            id: format!("message-{index}"),
            text: format!("activity {index} with a long description"),
            phase: None,
            memory_citation: None,
        })
        .collect::<Vec<_>>();
    overlay.update_activity(
        "/root/worker",
        AgentActivityPreview::from_items(items.iter().rev(), AgentActivityPathDisplay::Hide),
        None,
    );
    let lines = overlay.display_lines("  ", false, 36);
    assert!(lines.iter().all(|line| line.width() <= 36));
    let activity_rows = &lines[lines.len() - 3..];
    assert_eq!(activity_rows.len(), 3);
    assert!(
        activity_rows
            .iter()
            .all(|line| line.to_string().starts_with("  │     "))
    );
}
