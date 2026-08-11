use super::App;
use super::AppServerSession;
use super::Feature;
use super::Result;
use super::TomlValue;
use super::make_test_app_with_channels;
use super::start_config_write_test_app_server;
use crate::app_server_session::ThreadParamsMode;
use crate::legacy_core::config::ConfigBuilder;
use codex_app_server_client::AppServerClient;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

async fn start_config_write_server_with_loader(
    app: &App,
    loader_overrides: LoaderOverrides,
) -> Result<AppServerSession> {
    let state_db =
        crate::init_state_db_for_app_server_target(&app.config, &crate::AppServerTarget::Embedded)
            .await?;
    let client = crate::start_embedded_app_server(
        codex_arg0::Arg0DispatchPaths::default(),
        app.config.clone(),
        Vec::new(),
        loader_overrides,
        /*strict_config*/ false,
        CloudConfigBundleLoader::default(),
        app.feedback.clone(),
        /*log_db*/ None,
        state_db,
        app.environment_manager.clone(),
    )
    .await?;
    Ok(AppServerSession::new(
        AppServerClient::InProcess(client),
        ThreadParamsMode::Embedded,
    ))
}

fn assert_spine_spawn_runtime_state(app: &App, enabled: bool, max_threads: usize) {
    assert_eq!(
        (
            app.config.features.enabled(Feature::SpineSpawn),
            app.config.spine_spawn.max_concurrent_threads_per_session,
        ),
        (enabled, max_threads)
    );
    assert_eq!(
        (
            app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::SpineSpawn),
            app.chat_widget
                .config_ref()
                .spine_spawn
                .max_concurrent_threads_per_session,
        ),
        (enabled, max_threads)
    );
}

fn parsed_user_config(codex_home: &std::path::Path) -> Result<TomlValue> {
    Ok(toml::from_str(&std::fs::read_to_string(
        codex_home.join("config.toml"),
    )?)?)
}

fn spine_spawn_values(config: &TomlValue) -> (Option<bool>, Option<i64>) {
    let root = config.as_table().expect("config should be a TOML table");
    let enabled = root
        .get("features")
        .and_then(TomlValue::as_table)
        .and_then(|features| features.get("spine_spawn"))
        .and_then(TomlValue::as_bool);
    let max_threads = root
        .get("spine_spawn")
        .and_then(TomlValue::as_table)
        .and_then(|spine_spawn| spine_spawn.get("max_concurrent_threads_per_session"))
        .and_then(TomlValue::as_integer);
    (enabled, max_threads)
}

#[tokio::test]
async fn spine_spawn_settings_persist_in_one_native_config_write() -> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(
        codex_home.path().join("config.toml"),
        "spine_spawn = { max_concurrent_threads_per_session = 6 }\n\n[features]\nspine_spawn = false\n",
    )?;
    let mut app_server = start_config_write_test_app_server(&app).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![(Feature::SpineSpawn, true)],
        /*spine_spawn_max_concurrent_threads_per_session*/ Some(10),
    )
    .await;

    assert_spine_spawn_runtime_state(&app, /*enabled*/ true, /*max_threads*/ 10);
    assert!(
        op_rx.try_recv().is_err(),
        "static Spine Spawn settings must not emit a current-session operation"
    );
    let config_text = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(config_text.contains("[spine_spawn]\n"));
    assert!(!config_text.contains("spine_spawn = {"));
    assert_eq!(
        spine_spawn_values(&toml::from_str(&config_text)?),
        (Some(true), Some(10))
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn overridden_spine_spawn_write_adopts_effective_feature_and_capacity() -> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    let user_config_path = codex_home.path().join("config.toml");
    let managed_config_path = codex_home.path().join("managed_config.toml");
    std::fs::write(
        &user_config_path,
        "spine_spawn = { max_concurrent_threads_per_session = 5 }\n\n[features]\nspine_spawn = false\n",
    )?;
    std::fs::write(
        &managed_config_path,
        "[features]\nspine_spawn = false\n\n[spine_spawn]\nmax_concurrent_threads_per_session = 7\n",
    )?;
    let loader_overrides = LoaderOverrides::with_managed_config_path_for_tests(managed_config_path);
    let effective_config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    app.config = effective_config;
    app.loader_overrides = loader_overrides.clone();
    app.chat_widget
        .set_feature_enabled(Feature::SpineSpawn, /*enabled*/ false);
    app.chat_widget
        .set_spine_spawn_max_concurrent_threads_per_session(7);
    let mut app_server = start_config_write_server_with_loader(&app, loader_overrides).await?;

    app.update_feature_flags(
        &mut app_server,
        vec![(Feature::SpineSpawn, true)],
        /*spine_spawn_max_concurrent_threads_per_session*/ Some(12),
    )
    .await;

    assert_spine_spawn_runtime_state(&app, /*enabled*/ false, /*max_threads*/ 7);
    assert_eq!(
        spine_spawn_values(&parsed_user_config(codex_home.path())?),
        (Some(true), Some(12)),
        "the user layer should retain the requested values even though the managed layer wins"
    );
    assert!(
        op_rx.try_recv().is_err(),
        "an overridden static Spine Spawn setting must not emit a current-session operation"
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn non_spine_feature_update_preserves_spine_spawn_capacity() -> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.spine_spawn.max_concurrent_threads_per_session = 9;
    app.chat_widget
        .set_spine_spawn_max_concurrent_threads_per_session(9);
    std::fs::write(
        codex_home.path().join("config.toml"),
        "[spine_spawn]\nmax_concurrent_threads_per_session = 9\n",
    )?;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let next_spine_jit = !app.config.features.enabled(Feature::SpineJit);

    app.update_feature_flags(
        &mut app_server,
        vec![(Feature::SpineJit, next_spine_jit)],
        /*spine_spawn_max_concurrent_threads_per_session*/ None,
    )
    .await;

    assert_eq!(
        (
            app.config.spine_spawn.max_concurrent_threads_per_session,
            app.chat_widget
                .config_ref()
                .spine_spawn
                .max_concurrent_threads_per_session,
            spine_spawn_values(&parsed_user_config(codex_home.path())?).1,
        ),
        (9, 9, Some(9))
    );
    assert!(op_rx.try_recv().is_err());

    app_server.shutdown().await?;
    Ok(())
}
