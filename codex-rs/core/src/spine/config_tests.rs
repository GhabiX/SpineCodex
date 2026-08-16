use super::*;
use codex_features::Feature;
use codex_features::Features;
use pretty_assertions::assert_eq;

#[test]
fn trusted_workspace_layers_override_home_configuration() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let working = tempfile::tempdir()?;
    std::fs::create_dir_all(home.path().join(".spine"))?;
    std::fs::write(
        home.path().join(".spine/spine.toml"),
        "[limits]\ntrim_threshold_bytes = 2048\n",
    )?;
    std::fs::write(
        working.path().join("spine.toml"),
        "[limits]\ntrim_threshold_bytes = 4096\n",
    )?;

    let (config, _) = load(
        /*path*/ None,
        working.path(),
        Some(home.path()),
        &ManagedFeatures::default(),
        /*project_config_trusted*/ true,
    )?;

    assert_eq!(config.trim_threshold_bytes(), 4096);
    Ok(())
}

#[test]
fn untrusted_workspace_layers_are_not_loaded() -> std::io::Result<()> {
    let working = tempfile::tempdir()?;
    std::fs::write(
        working.path().join("spine.toml"),
        "[limits]\ntrim_threshold_bytes = 2048\n",
    )?;

    let (config, _) = load(
        /*path*/ None,
        working.path(),
        /*home_directory*/ None,
        &ManagedFeatures::default(),
        /*project_config_trusted*/ false,
    )?;

    assert_eq!(
        config.trim_threshold_bytes(),
        SpineConfig::v1().trim_threshold_bytes()
    );
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

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn managed_host_features_select_sdk_features() {
    let working = tempfile::tempdir().unwrap();
    let mut host_features = Features::default();
    host_features.enable(Feature::SpineJit);
    host_features.enable(Feature::SpineSpawn);
    host_features.disable(Feature::SpineTrim);
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
            config.is_enabled(spine_core::host::Feature::Trim),
            config.is_enabled(spine_core::host::Feature::Spawn),
        ),
        (true, false, true),
    );
}
