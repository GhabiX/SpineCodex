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

/// Build the model-visible prompt payload for a single debug turn.
#[doc(hidden)]
pub async fn build_prompt_input(
    mut config: Config,
    input: Vec<UserInput>,
    state_db: Option<StateDbHandle>,
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

    let output =
        build_prompt_debug_output_from_session(thread.thread.codex.session.as_ref(), input).await?;
    let shutdown = thread.thread.shutdown_and_wait().await;
    let _removed = thread_manager.remove_thread(&thread.thread_id).await;

    shutdown?;
    Ok(output)
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
