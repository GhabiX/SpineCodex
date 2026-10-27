use std::collections::HashMap;

use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;

use super::FAILURE_ACTION_QUESTION_ID;
use super::MAX_FAILURE_GUIDANCE_CHARS;
use super::SpawnFailureAction;
use super::SpawnFailureDecision;
use super::parse_failure_decision;

fn response(answers: Vec<&str>) -> RequestUserInputResponse {
    RequestUserInputResponse {
        answers: HashMap::from([(
            FAILURE_ACTION_QUESTION_ID.to_string(),
            RequestUserInputAnswer {
                answers: answers.into_iter().map(str::to_string).collect(),
            },
        )]),
    }
}

#[test]
fn parses_each_gate_action() {
    for (label, action) in [
        ("Continue", SpawnFailureAction::Continue),
        ("Retry", SpawnFailureAction::Retry),
        ("Abandon", SpawnFailureAction::Abandon),
    ] {
        assert_eq!(
            parse_failure_decision(response(vec![label])),
            Some(SpawnFailureDecision { action, note: None })
        );
    }
}

#[test]
fn extracts_and_bounds_optional_user_guidance() {
    let long_note = "x".repeat(MAX_FAILURE_GUIDANCE_CHARS + 10);
    let parsed = parse_failure_decision(response(vec![
        "Continue",
        "user_note: first",
        &format!("user_note: {long_note}"),
    ]))
    .expect("valid response");
    let note = parsed.note.expect("note");
    assert!(note.starts_with("first\n"));
    assert_eq!(note.chars().count(), MAX_FAILURE_GUIDANCE_CHARS);
}

#[test]
fn rejects_empty_unknown_and_malformed_answers() {
    assert_eq!(parse_failure_decision(response(Vec::new())), None);
    assert_eq!(parse_failure_decision(response(vec!["Unknown"])), None);
    assert_eq!(
        parse_failure_decision(response(vec!["Retry", "unexpected"])),
        None
    );
}
