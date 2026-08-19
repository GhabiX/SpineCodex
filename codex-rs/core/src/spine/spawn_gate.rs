use std::sync::Arc;

use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputQuestion;
use codex_protocol::request_user_input::RequestUserInputQuestionOption;
use codex_protocol::request_user_input::RequestUserInputResponse;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

pub(crate) const FAILURE_ACTION_QUESTION_ID: &str = "spine_spawn_failure_action";
const CONTINUE_LABEL: &str = "Continue";
const RETRY_LABEL: &str = "Retry";
const ABANDON_LABEL: &str = "Abandon";
const MAX_FAILURE_GUIDANCE_CHARS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnFailureAction {
    Continue,
    Retry,
    Abandon,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnFailureDecision {
    pub(crate) action: SpawnFailureAction,
    pub(crate) note: Option<String>,
}

pub(crate) async fn request_spawn_failure_action(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    call_id: &str,
    failed_count: usize,
    total_count: usize,
) -> Option<SpawnFailureDecision> {
    let response = session
        .request_user_input(
            turn,
            call_id.to_string(),
            RequestUserInputArgs {
                questions: vec![RequestUserInputQuestion {
                    id: FAILURE_ACTION_QUESTION_ID.to_string(),
                    header: "Spawn failed".to_string(),
                    question: format!(
                        "{failed_count} of {total_count} spawned branches failed. Choose what to do with the failed branches."
                    ),
                    is_other: false,
                    is_secret: false,
                    options: Some(vec![
                        RequestUserInputQuestionOption {
                            label: CONTINUE_LABEL.to_string(),
                            description: "Resume with each failed branch's existing context."
                                .to_string(),
                        },
                        RequestUserInputQuestionOption {
                            label: RETRY_LABEL.to_string(),
                            description: "Start each failed branch again in a new agent."
                                .to_string(),
                        },
                        RequestUserInputQuestionOption {
                            label: ABANDON_LABEL.to_string(),
                            description: "Return the failures to the parent agent.".to_string(),
                        },
                    ]),
                }],
                is_blocking: true,
                auto_resolution_ms: None,
            },
        )
        .await?;

    parse_failure_decision(response)
}

fn parse_failure_decision(response: RequestUserInputResponse) -> Option<SpawnFailureDecision> {
    if response.answers.len() != 1 {
        return None;
    }
    let answer = response.answers.get(FAILURE_ACTION_QUESTION_ID)?;
    let action = match answer.answers.first()?.as_str() {
        CONTINUE_LABEL => SpawnFailureAction::Continue,
        RETRY_LABEL => SpawnFailureAction::Retry,
        ABANDON_LABEL => SpawnFailureAction::Abandon,
        _ => return None,
    };
    let notes = answer
        .answers
        .iter()
        .skip(1)
        .map(|answer| answer.strip_prefix("user_note: "))
        .collect::<Option<Vec<_>>>()?;
    let note = notes
        .into_iter()
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let note = (!note.is_empty()).then(|| {
        note.chars()
            .take(MAX_FAILURE_GUIDANCE_CHARS)
            .collect::<String>()
    });
    Some(SpawnFailureDecision { action, note })
}

#[cfg(test)]
#[path = "spawn_gate_tests.rs"]
mod tests;
