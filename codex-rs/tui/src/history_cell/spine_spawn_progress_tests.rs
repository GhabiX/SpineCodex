use super::plain_lines;
use super::spine_spawn_progress::SpineSpawnOverlay;
use super::spine_spawn_progress::spine_spawn_status;
use crate::motion::ORGANIC_ACTIVITY_WORDS;
use crate::product_brand::SPINE_BRAND_COLOR;
use crate::style::muted_text_style;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use codex_app_server_protocol::SpineSpawnTaskProgress;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::ThreadStatusChangedNotification;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use ratatui::style::Modifier;
use ratatui::text::Line;
use std::collections::HashSet;
use std::time::Duration;

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
                thread_id: "child-0".to_string(),
                agent_path: Some("/root/inspector".to_string()),
                status: CollabAgentStatus::Completed,
            },
            SpineSpawnTaskProgress {
                ordinal: 1,
                summary: "verify cancellation".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: Some("/root/verifier".to_string()),
                status: CollabAgentStatus::Running,
            },
        ],
    });

    let completed_word = cell
        .activity_word("child-0")
        .expect("completed child should have an activity word");
    let running_word = cell
        .activity_word("child-1")
        .expect("running child should have an activity word");
    assert_ne!(completed_word, running_word);
    let rendered = plain_lines(cell.display_lines("  │  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("spine.spawn"), "{rendered}");
    assert!(rendered.contains("├ ✓"), "{rendered}");
    assert!(rendered.contains("inspect native events"), "{rendered}");
    assert!(!rendered.contains(completed_word), "{rendered}");
    assert!(rendered.contains(&format!("└ {running_word} verify cancellation")));
    assert!(!rendered.contains('•'), "{rendered}");
    assert!(!rendered.contains('◦'), "{rendered}");
    assert!(rendered.contains("Waiting for activity..."));
    assert_eq!(cell.display_lines("  │  ", true, 80, false).len(), 7);

    let lines = cell.display_lines("  │  ", true, 80, false);
    for task_line in [&lines[0], &lines[1]] {
        assert!(
            task_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::DIM),
            "task branch should use the tree prefix style: {task_line:?}"
        );
        let summary = task_line
            .spans
            .last()
            .expect("task line should end with its summary");
        assert!(
            !summary.style.add_modifier.contains(Modifier::DIM),
            "task summary should use the normal foreground: {task_line:?}"
        );
    }
    let running_activity_word = &lines[1].spans[1];
    assert_eq!(running_activity_word.content.as_ref(), running_word);
    let completed_check = lines[0]
        .spans
        .iter()
        .find(|span| span.content.contains('✓'))
        .expect("completed check");
    assert_eq!(
        completed_check.style.fg,
        Some(SPINE_BRAND_COLOR),
        "completed check should use the Spine brand color: {lines:?}"
    );
    assert_eq!(
        running_activity_word.style.fg,
        Some(SPINE_BRAND_COLOR),
        "running activity word should use the Spine brand color: {lines:?}"
    );
    for activity_line in &lines[2..6] {
        assert!(
            activity_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::DIM),
            "activity branch should use the tree prefix style: {activity_line:?}"
        );
        assert_eq!(activity_line.spans[0].style.fg, None);
    }
}

#[test]
fn non_last_activity_connector_matches_the_tree_separator() {
    let mut cell = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![
            SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "inspect events".to_string(),
                thread_id: "child-0".to_string(),
                agent_path: Some("/root/inspector".to_string()),
                status: CollabAgentStatus::Running,
            },
            SpineSpawnTaskProgress {
                ordinal: 1,
                summary: "review findings".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: Some("/root/reviewer".to_string()),
                status: CollabAgentStatus::Running,
            },
        ],
    });
    assert!(
        cell.seed_activity(
            "child-0",
            [ServerNotification::ItemCompleted(
                ItemCompletedNotification {
                    item: ThreadItem::AgentMessage {
                        id: "message-1".to_string(),
                        text: "first task activity".to_string(),
                        phase: None,
                        memory_citation: None,
                    },
                    thread_id: "child-0".to_string(),
                    turn_id: "turn-1".to_string(),
                    completed_at_ms: 1,
                },
            )]
            .into_iter(),
        )
    );

    let lines = cell.display_lines("  │  ", true, 80, false);
    for activity_line in &lines[1..5] {
        let connector = &activity_line.spans[0];
        assert_eq!(connector.content.as_ref(), "  │  │    ");
        assert!(connector.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(connector.style.fg, None);
    }
    assert_eq!(lines[1].spans[1].style, muted_text_style());

    let separator = &lines[5].spans[0];
    assert_eq!(separator.content.as_ref(), "  │  │");
    assert!(separator.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(separator.style.fg, None);
    assert_eq!(separator.style, lines[1].spans[0].style);
}

#[test]
fn activity_refresh_keeps_the_newest_four_lines() {
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "inspect events".to_string(),
            thread_id: "child".to_string(),
            agent_path: Some("/root/inspector".to_string()),
            status: CollabAgentStatus::Running,
        }],
    });
    let notifications = (1..=5).map(|index| {
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            item: ThreadItem::AgentMessage {
                id: format!("message-{index}"),
                text: format!("activity {index}"),
                phase: None,
                memory_citation: None,
            },
            thread_id: "child".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: index,
        })
    });
    assert!(overlay.seed_activity("child", notifications));

    let rendered = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("activity 1"));
    assert!(rendered.contains("activity 2\n"));
    assert!(rendered.contains("activity 3\n"));
    assert!(rendered.contains("activity 4\n"));
    assert!(rendered.contains("activity 5\n"));
    assert_eq!(overlay.display_lines("  ", true, 80, false).len(), 6);
}

#[test]
fn activity_preview_is_identical_with_or_without_animations() {
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "inspect events".to_string(),
            thread_id: "child".to_string(),
            agent_path: Some("/root/inspector".to_string()),
            status: CollabAgentStatus::Running,
        }],
    });
    let notifications = [
        ("message-1", "first structured activity"),
        ("message-2", "second structured activity"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (id, text))| {
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            item: ThreadItem::AgentMessage {
                id: id.to_string(),
                text: text.to_string(),
                phase: None,
                memory_citation: None,
            },
            thread_id: "child".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: index as i64,
        })
    });
    assert!(overlay.seed_activity("child", notifications));

    let static_render = overlay.display_lines("  ", true, 80, false);
    let animated_render = overlay.display_lines("  ", true, 80, true);
    let static_lines = plain_lines(static_render.clone());
    let animated_lines = plain_lines(animated_render);

    assert_eq!(animated_lines, static_lines);
    assert!(
        static_render[1].spans[0]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    assert_eq!(static_render[1].spans[0].style.fg, None);
    assert_eq!(static_render[1].spans[1].style, muted_text_style());
}

#[test]
fn terminal_tasks_render_without_an_aggregate_row() {
    let cell = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![
            SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "completed".to_string(),
                thread_id: "child-0".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Completed,
            },
            SpineSpawnTaskProgress {
                ordinal: 1,
                summary: "interrupted".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Interrupted,
            },
            SpineSpawnTaskProgress {
                ordinal: 2,
                summary: "failed".to_string(),
                thread_id: "child-2".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Errored,
            },
            SpineSpawnTaskProgress {
                ordinal: 3,
                summary: "stopped".to_string(),
                thread_id: "child-3".to_string(),
                agent_path: None,
                status: CollabAgentStatus::Shutdown,
            },
        ],
    });
    let rendered = plain_lines(cell.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("spine.spawn"), "{rendered}");
    let completed_word = cell
        .activity_word("child-0")
        .expect("completed child should retain its assigned word");
    assert!(rendered.contains("✓"), "{rendered}");
    assert!(rendered.contains("completed"), "{rendered}");
    assert!(!rendered.contains(completed_word), "{rendered}");
    for (thread_id, marker, summary) in [
        ("child-1", "!", "interrupted"),
        ("child-2", "×", "failed"),
        ("child-3", "×", "stopped"),
    ] {
        let word = cell
            .activity_word(thread_id)
            .expect("terminal child should retain its activity word");
        assert!(
            rendered.contains(&format!("{marker} {word} {summary}")),
            "{rendered}"
        );
    }
}

#[test]
fn completed_task_retires_word_and_frozen_body_before_check() {
    let mut overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Running));
    let activity = completed_message("before completion");
    assert!(overlay.seed_activity("child", [activity].into_iter()));
    let word = overlay
        .activity_word("child")
        .expect("activity word")
        .to_string();
    assert!(overlay.update_status("child", CollabAgentStatus::Completed));
    let deadline = overlay
        .completion_deadline("child")
        .expect("completion deadline");
    let completed_at = deadline - Duration::from_millis(850);

    let first = overlay.display_lines_at("  ", true, 80, true, completed_at);
    let first_text = plain_lines(first.clone())
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(first.len(), 6);
    assert!(first_text.contains(&word), "{first_text}");
    assert!(first_text.contains("before completion"), "{first_text}");
    assert!(!first_text.contains('✓'), "{first_text}");

    let middle = overlay.display_lines_at(
        "  ",
        true,
        80,
        true,
        completed_at + Duration::from_millis(600),
    );
    assert!(middle.len() < first.len(), "{middle:?}");
    assert!(middle.len() > 1, "{middle:?}");

    let final_lines = overlay.display_lines_at("  ", true, 80, true, deadline);
    let final_text = final_lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(final_lines.len(), 1);
    assert_eq!(final_text, "  └ ✓ task summary");
    assert!(!final_text.contains(&word), "{final_text}");
    assert!(!final_text.contains("before completion"), "{final_text}");
}

#[test]
fn repeated_completion_does_not_restart_retirement() {
    let mut overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Running));
    assert!(overlay.update_status("child", CollabAgentStatus::Completed));
    let deadline = overlay
        .completion_deadline("child")
        .expect("completion deadline");

    assert!(!overlay.update_status("child", CollabAgentStatus::Completed));
    overlay.replace_notification(single_task(CollabAgentStatus::Completed));
    assert_eq!(overlay.completion_deadline("child"), Some(deadline));
}

#[test]
fn truthful_failure_cancels_retiring_success_and_stays_terminal() {
    let mut overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Running));
    assert!(overlay.update_status("child", CollabAgentStatus::Completed));
    assert!(overlay.completion_deadline("child").is_some());

    assert!(overlay.update_status("child", CollabAgentStatus::Errored));
    assert_eq!(overlay.completion_deadline("child"), None);
    assert!(!overlay.update_status("child", CollabAgentStatus::Completed));
    let rendered = overlay
        .display_lines("  ", true, 80, true)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains('×'), "{rendered}");
    assert!(!rendered.contains('✓'), "{rendered}");
}

#[test]
fn late_activity_does_not_mutate_frozen_completion_preview() {
    let mut overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Running));
    assert!(overlay.seed_activity("child", [completed_message("frozen activity")].into_iter(),));
    assert!(overlay.update_status("child", CollabAgentStatus::Completed));
    assert!(!overlay.update_activity("child", &completed_message("late activity"), None,));

    let rendered = overlay
        .display_lines("  ", true, 80, true)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("frozen activity"), "{rendered}");
    assert!(!rendered.contains("late activity"), "{rendered}");
}

#[test]
fn animations_disabled_projects_completed_task_directly_to_new_terminal_shape() {
    let overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Completed));
    let word = overlay.activity_word("child").expect("activity word");
    let lines = overlay.display_lines("  ", true, 80, false);
    let rendered = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(lines.len(), 1);
    assert!(rendered.contains('✓'), "{rendered}");
    assert!(!rendered.contains(word), "{rendered}");
    assert!(!rendered.contains("Waiting for activity..."), "{rendered}");
}

#[test]
fn pending_task_uses_a_pending_specific_empty_state() {
    let cell = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "waiting child".to_string(),
            thread_id: "child".to_string(),
            agent_path: None,
            status: CollabAgentStatus::PendingInit,
        }],
    });

    let rendered = plain_lines(cell.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Waiting to start..."), "{rendered}");
    assert!(!rendered.contains("Waiting for activity..."), "{rendered}");
}

#[test]
fn first_safe_activity_promotes_pending_task_to_running() {
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "active child".to_string(),
            thread_id: "child".to_string(),
            agent_path: None,
            status: CollabAgentStatus::PendingInit,
        }],
    });
    let activity = ServerNotification::ItemCompleted(ItemCompletedNotification {
        item: ThreadItem::AgentMessage {
            id: "message-1".to_string(),
            text: "child produced activity".to_string(),
            phase: None,
            memory_citation: None,
        },
        thread_id: "child".to_string(),
        turn_id: "turn-1".to_string(),
        completed_at_ms: 1,
    });

    assert!(overlay.update_activity("child", &activity, None));
    let rendered = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains('◌'), "{rendered}");
    assert!(!rendered.contains('•'), "{rendered}");
    assert!(!rendered.contains('◦'), "{rendered}");
    assert!(rendered.contains("child produced activity"), "{rendered}");
    assert!(!rendered.contains("Waiting to start..."), "{rendered}");

    overlay.replace_notification(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "active child".to_string(),
            thread_id: "child".to_string(),
            agent_path: None,
            status: CollabAgentStatus::PendingInit,
        }],
    });
    let refreshed = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!refreshed.contains('◌'), "{refreshed}");
    assert!(!refreshed.contains('•'), "{refreshed}");
    assert!(!refreshed.contains('◦'), "{refreshed}");
    assert!(refreshed.contains("child produced activity"), "{refreshed}");
}

#[test]
fn generic_child_failure_waits_for_normalized_progress() {
    let progress = || SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "active child".to_string(),
            thread_id: "child".to_string(),
            agent_path: None,
            status: CollabAgentStatus::PendingInit,
        }],
    };
    let activity = ServerNotification::ItemCompleted(ItemCompletedNotification {
        item: ThreadItem::AgentMessage {
            id: "message-1".to_string(),
            text: "child produced activity".to_string(),
            phase: None,
            memory_citation: None,
        },
        thread_id: "child".to_string(),
        turn_id: "turn-1".to_string(),
        completed_at_ms: 1,
    });
    let failed = ServerNotification::ThreadStatusChanged(ThreadStatusChangedNotification {
        thread_id: "child".to_string(),
        status: ThreadStatus::SystemError,
    });
    let mut overlay = SpineSpawnOverlay::new(progress());

    assert!(overlay.seed_activity("child", [activity, failed].into_iter()));
    let before_progress = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!before_progress.contains('×'), "{before_progress}");
    assert!(overlay.update_status("child", CollabAgentStatus::Errored));
    overlay.replace_notification(progress());
    assert!(!overlay.update_status("child", CollabAgentStatus::Running));

    let rendered = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("×"), "{rendered}");
    assert!(!rendered.contains("Waiting to start..."), "{rendered}");
}

#[test]
fn generic_child_completion_is_not_terminal_authority() {
    let notification = ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: "child".to_string(),
        turn: Turn {
            id: "turn-1".to_string(),
            items: Vec::new(),
            items_view: Default::default(),
            status: TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: Some(1),
            duration_ms: Some(1),
        },
    });
    assert_eq!(spine_spawn_status(&notification), None);

    let mut overlay = SpineSpawnOverlay::new(single_task(CollabAgentStatus::Running));
    overlay.update_activity("child", &notification, spine_spawn_status(&notification));
    assert_eq!(overlay.completion_deadline("child"), None);
    let rendered = plain_lines(overlay.display_lines("  ", true, 80, false))
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains('✓'), "{rendered}");
    assert!(overlay.update_status("child", CollabAgentStatus::Completed));
    assert!(overlay.completion_deadline("child").is_some());
}

#[test]
fn settled_visuals_require_dense_task_ordinals() {
    let mut notification = single_task(CollabAgentStatus::Completed);
    notification.tasks[0].ordinal = 1;
    assert!(
        SpineSpawnOverlay::new(notification)
            .settled_task_visuals()
            .is_none()
    );
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
            thread_id: "child".to_string(),
            agent_path: Some("/root/worker".to_string()),
            status: CollabAgentStatus::Running,
        }],
    });
    let notifications = (1..=4).map(|index| {
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            item: ThreadItem::AgentMessage {
                id: format!("message-{index}"),
                text: format!("activity {index} with a long description"),
                phase: None,
                memory_citation: None,
            },
            thread_id: "child".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: index,
        })
    });
    overlay.seed_activity("child", notifications);
    let lines = overlay.display_lines("  ", false, 36, false);
    assert!(lines.iter().all(|line| line.width() <= 36));
    let activity_rows = &lines[lines.len() - 5..lines.len() - 1];
    assert_eq!(activity_rows.len(), 4);
    assert!(
        activity_rows
            .iter()
            .all(|line| line.to_string().starts_with("  │    "))
    );
    assert_eq!(lines.last().map(Line::to_string).as_deref(), Some("  │"));
}

#[test]
fn random_activity_words_are_unique_within_a_spawn_and_stable_across_refresh() {
    let tasks = (0..6)
        .map(|ordinal| SpineSpawnTaskProgress {
            ordinal,
            summary: format!("task {ordinal}"),
            thread_id: format!("child-{ordinal}"),
            agent_path: None,
            status: CollabAgentStatus::Running,
        })
        .collect::<Vec<_>>();
    let mut overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: tasks.clone(),
    });
    let before = tasks
        .iter()
        .map(|task| {
            overlay
                .activity_word(&task.thread_id)
                .expect("each child should receive an activity word")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        before.iter().cloned().collect::<HashSet<_>>().len(),
        tasks.len()
    );
    assert!(
        before
            .iter()
            .all(|word| ORGANIC_ACTIVITY_WORDS.contains(&word.as_str()))
    );

    let mut refreshed_tasks = tasks;
    refreshed_tasks.reverse();
    refreshed_tasks[0].status = CollabAgentStatus::Completed;
    overlay.replace_notification(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: refreshed_tasks,
    });
    for (ordinal, word) in before.into_iter().enumerate() {
        assert_eq!(
            overlay.activity_word(&format!("child-{ordinal}")),
            Some(word.as_str())
        );
    }
}

#[test]
fn activity_words_remain_unique_beyond_the_base_pool() {
    let task_count = ORGANIC_ACTIVITY_WORDS.len() + 4;
    let tasks = (0..task_count)
        .map(|ordinal| SpineSpawnTaskProgress {
            ordinal: ordinal as u32,
            summary: format!("task {ordinal}"),
            thread_id: format!("child-{ordinal}"),
            agent_path: None,
            status: CollabAgentStatus::Running,
        })
        .collect::<Vec<_>>();
    let overlay = SpineSpawnOverlay::new(SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: tasks.clone(),
    });

    let words = tasks
        .iter()
        .map(|task| {
            overlay
                .activity_word(&task.thread_id)
                .expect("each child should receive an activity word")
        })
        .collect::<HashSet<_>>();
    assert_eq!(words.len(), task_count);
    assert!(words.iter().any(|word| word.starts_with("Further ")));
}

fn single_task(status: CollabAgentStatus) -> SpineSpawnProgressUpdatedNotification {
    SpineSpawnProgressUpdatedNotification {
        thread_id: "parent".to_string(),
        turn_id: "turn-1".to_string(),
        call_id: "spawn-1".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "task summary".to_string(),
            thread_id: "child".to_string(),
            agent_path: None,
            status,
        }],
    }
}

fn completed_message(text: &str) -> ServerNotification {
    ServerNotification::ItemCompleted(ItemCompletedNotification {
        item: ThreadItem::AgentMessage {
            id: format!("message-{text}"),
            text: text.to_string(),
            phase: None,
            memory_citation: None,
        },
        thread_id: "child".to_string(),
        turn_id: "turn-1".to_string(),
        completed_at_ms: 1,
    })
}
