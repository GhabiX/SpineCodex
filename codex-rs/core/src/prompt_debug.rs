use std::collections::HashSet;
use std::sync::Arc;

use codex_exec_server::EnvironmentManager;
use codex_exec_server::ExecServerRuntimePaths;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use serde::Serialize;
use std::str::FromStr;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::resolve_installation_id;
use crate::session::session::Session;
use crate::session::turn::build_prompt;
use crate::session::turn::built_tools;
use crate::state_db_bridge::StateDbHandle;
use crate::thread_manager::ThreadManager;
use crate::thread_manager::thread_store_from_config;
use codex_extension_api::empty_extension_registry;

/// Model-visible prompt payload for a single debug turn.
#[derive(Debug, Clone, Serialize)]
pub struct PromptDebugOutput {
    pub instructions: String,
    pub input: Vec<ResponseItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineScalingLevel {
    Low,
    Medium,
    High,
    Auto,
}

impl FromStr for SpineScalingLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "auto" => Ok(Self::Auto),
            other => Err(format!(
                "unsupported Spine scaling level {other:?}; use low, medium, high, or auto"
            )),
        }
    }
}

/// Build the model-visible prompt payload for a single debug turn.
#[doc(hidden)]
pub async fn build_prompt_input(
    config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
) -> CodexResult<PromptDebugOutput> {
    build_prompt_input_with_spine_scaling(config, input, state_db, None).await
}

#[doc(hidden)]
pub async fn build_prompt_input_with_spine_scaling(
    mut config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
    spine_scaling: Option<SpineScalingLevel>,
) -> CodexResult<PromptDebugOutput> {
    let spine_jit_enabled = config.features.enabled(Feature::SpineJit);
    // SpineJit needs a rollout path for its sidecar even in prompt-debug mode.
    config.ephemeral = !spine_jit_enabled;
    config.dev_debug_prompt_overrides = true;

    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;

    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        config.codex_self_exe.clone(),
        config.codex_linux_sandbox_exe.clone(),
    )?;

    let thread_store = thread_store_from_config(&config, state_db.clone());
    let installation_id = resolve_installation_id(&config.codex_home).await?;
    let thread_manager = ThreadManager::new(
        &config,
        Arc::clone(&auth_manager),
        SessionSource::Exec,
        Arc::new(
            EnvironmentManager::from_codex_home(config.codex_home.clone(), local_runtime_paths)
                .await
                .map_err(|err| CodexErr::Fatal(err.to_string()))?,
        ),
        empty_extension_registry(),
        /*analytics_events_client*/ None,
        thread_store,
        state_db.clone(),
        installation_id,
        /*attestation_provider*/ None,
    );
    let thread = thread_manager.start_thread(config).await?;

    let mut output =
        build_prompt_debug_output_from_session(thread.thread.codex.session.as_ref(), input).await?;
    if spine_jit_enabled {
        append_spine_scaling_prompt(&mut output.instructions, spine_scaling);
    }
    let shutdown = thread.thread.shutdown_and_wait().await;
    let _removed = thread_manager.remove_thread(&thread.thread_id).await;

    shutdown?;
    Ok(output)
}

fn append_spine_scaling_prompt(
    instructions: &mut String,
    spine_scaling: Option<SpineScalingLevel>,
) {
    let Some(spine_scaling) = spine_scaling else {
        return;
    };
    let Some(block) = spine_scaling_prompt_block(spine_scaling) else {
        return;
    };
    if !instructions.is_empty() {
        instructions.push_str("\n\n");
    }
    instructions.push_str(block);
}

fn spine_scaling_prompt_block(spine_scaling: SpineScalingLevel) -> Option<&'static str> {
    match spine_scaling {
        SpineScalingLevel::Low => None,
        SpineScalingLevel::Medium => Some(
            "<spine_scaling>\nTask-level scaling: medium.\nBudget: depth 2 x branch 2; plan up to 4 bottom exploration leaves.\n</spine_scaling>",
        ),
        SpineScalingLevel::High => Some(
            "<spine_scaling>\nTask-level scaling: high.\nBudget: depth 3 x branch 3; plan up to 27 bottom exploration leaves.\n</spine_scaling>",
        ),
        SpineScalingLevel::Auto => Some(
            "<spine_scaling>\nTask-level scaling: auto.\nBudget: choose reasonable depth and branch count for the task.\n</spine_scaling>",
        ),
    }
}

#[cfg(test)]
pub(crate) async fn build_prompt_input_from_session(
    sess: &Session,
    input: Vec<UserInput>,
) -> CodexResult<Vec<ResponseItem>> {
    build_prompt_debug_output_from_session(sess, input)
        .await
        .map(|output| output.input)
}

async fn build_prompt_debug_output_from_session(
    sess: &Session,
    input: Vec<UserInput>,
) -> CodexResult<PromptDebugOutput> {
    let turn_context = sess.new_default_turn().await;
    sess.record_context_updates_and_set_reference_context_item(turn_context.as_ref())
        .await?;

    if !input.is_empty() {
        let input_item = ResponseInputItem::from(input);
        let response_item = ResponseItem::from(input_item);
        sess.record_conversation_items(turn_context.as_ref(), std::slice::from_ref(&response_item))
            .await?;
    }

    let prompt_input = sess
        .clone_history()
        .await
        .for_prompt(&turn_context.model_info.input_modalities);
    let router = built_tools(
        sess,
        turn_context.as_ref(),
        &prompt_input,
        &HashSet::new(),
        Some(turn_context.turn_skills.outcome.as_ref()),
        &CancellationToken::new(),
    )
    .await?;
    let base_instructions = sess.get_base_instructions().await;
    let prompt = build_prompt(
        prompt_input,
        router.as_ref(),
        turn_context.as_ref(),
        base_instructions,
    );

    let input = prompt.get_formatted_input();
    let instructions = prompt.base_instructions.text;

    Ok(PromptDebugOutput {
        instructions,
        input,
    })
}
