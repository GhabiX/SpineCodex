use super::*;
use codex_app_server_protocol::SpineSpawnTaskProgress;
use insta::assert_snapshot;

#[test]
fn renders_live_mixed_child_statuses() {
    let cell = SpineSpawnProgressCell::new(SpineSpawnProgressUpdatedNotification {
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

    assert_snapshot!(
        plain_lines(cell.display_lines(80))
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}
