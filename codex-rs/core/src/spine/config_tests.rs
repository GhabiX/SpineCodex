use super::*;
use codex_features::Feature;
use codex_features::Features;
use pretty_assertions::assert_eq;

#[test]
fn trusted_workspace_layers_override_home_configuration() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let working = tempfile::tempdir()?;
    std::fs::create_dir_all(home.path().join(".spine"))?;
    std::fs::write(
        home.path().join(".spine/spine.toml"),
        "[prompt]\nnode = \"home\"\n",
    )?;
    std::fs::write(
        working.path().join("spine.toml"),
        "[prompt]\nnode = \"workspace\"\n",
    )?;

    let mut host_features = Features::default();
    host_features.enable(Feature::SpineJit);
    let managed = ManagedFeatures::from(host_features);
    let (config, _) = load(
        /*path*/ None,
        working.path(),
        Some(home.path()),
        &managed,
        /*project_config_trusted*/ true,
    )?;

    assert_eq!(config.node_prompt(), Some("workspace"));
    Ok(())
}

#[test]
fn untrusted_workspace_layers_are_not_loaded() -> anyhow::Result<()> {
    let working = tempfile::tempdir()?;
    let baseline_working = tempfile::tempdir()?;
    std::fs::write(
        working.path().join("spine.toml"),
        "[prompt]\nnode = \"workspace\"\n",
    )?;

    let mut host_features = Features::default();
    host_features.enable(Feature::SpineJit);
    let managed = ManagedFeatures::from(host_features);
    let (config, _) = load(
        /*path*/ None,
        working.path(),
        /*home_directory*/ None,
        &managed,
        /*project_config_trusted*/ false,
    )?;
    let (baseline, _) = load(
        /*path*/ None,
        baseline_working.path(),
        /*home_directory*/ None,
        &managed,
        /*project_config_trusted*/ false,
    )?;

    assert_eq!(config.node_prompt(), baseline.node_prompt());
    Ok(())
}

#[test]
fn explicit_configuration_is_required_even_for_untrusted_workspace() {
    let working = tempfile::tempdir().unwrap();
    let missing = AbsolutePathBuf::try_from(working.path().join("missing.toml")).unwrap();

    let error = load(
        Some(&missing),
        working.path(),
        /*home_directory*/ None,
        &ManagedFeatures::default(),
        /*project_config_trusted*/ false,
    )
    .unwrap_err();

    assert_eq!(
        error
            .downcast_ref::<io::Error>()
            .expect("filesystem error")
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn managed_host_features_select_sdk_features() {
    let working = tempfile::tempdir().unwrap();
    let mut host_features = Features::default();
    host_features.enable(Feature::SpineJit);
    host_features.enable(Feature::SpineSpawn);
    let managed = ManagedFeatures::from(host_features);

    let (config, _) = load(
        /*path*/ None,
        working.path(),
        /*home_directory*/ None,
        &managed,
        /*project_config_trusted*/ false,
    )
    .unwrap();

    assert_eq!(
        (
            config.is_enabled(spine_core::host::Feature::Jit),
            config.is_enabled(spine_core::host::Feature::Spawn),
        ),
        (true, true),
    );
}

fn load(
    path: Option<&AbsolutePathBuf>,
    working_directory: &Path,
    home_directory: Option<&Path>,
    features: &ManagedFeatures,
    project_config_trusted: bool,
) -> anyhow::Result<(SpineConfig, ToolCatalog)> {
    let resolved = SpineConfiguration::pending(
        path,
        working_directory,
        home_directory,
        project_config_trusted,
    )
    .resolve(/*saved*/ None, features)?;
    Ok((resolved.sdk().clone(), resolved.tools().clone()))
}
