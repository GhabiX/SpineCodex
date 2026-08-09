use super::*;
use crate::config::MAX_TOOL_DESCRIPTION_BYTES;
use pretty_assertions::assert_eq;

#[test]
fn recursively_merges_tables_and_replaces_leaf_values() {
    let mut base: TomlValue = toml::from_str(
        r#"
schema_version = 1
names = ["base"]
[limits]
trim_threshold_bytes = 10000
[prompt]
jit = "base jit"
node = "base node"
"#,
    )
    .unwrap();
    let overlay: TomlValue = toml::from_str(
        r#"
names = ["overlay"]
[prompt]
node = "overlay node"
"#,
    )
    .unwrap();

    merge_toml_values(&mut base, overlay);

    let expected: TomlValue = toml::from_str(
        r#"
schema_version = 1
names = ["overlay"]
[limits]
trim_threshold_bytes = 10000
[prompt]
jit = "base jit"
node = "overlay node"
"#,
    )
    .unwrap();
    assert_eq!(base, expected);
}

#[test]
fn merged_partial_layer_uses_bundled_defaults() {
    let mut merged: TomlValue = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let overlay: TomlValue = toml::from_str(
        r#"
[limits]
trim_threshold_bytes = 2048
"#,
    )
    .unwrap();

    merge_toml_values(&mut merged, overlay);
    let config = parse_merged_config(merged).unwrap();

    let mut expected = SpineConfig::v1();
    expected.trim_threshold_bytes = 2048;
    assert_eq!(config, expected);
}

#[test]
fn merged_config_remains_strictly_validated() {
    let mut merged: TomlValue = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let overlay: TomlValue = toml::from_str("unknown = true").unwrap();
    merge_toml_values(&mut merged, overlay);

    assert!(matches!(
        parse_merged_config(merged),
        Err(ConfigError::InvalidToml(_))
    ));
}

#[test]
fn discovered_layers_share_the_model_visible_text_boundary() {
    let mut merged: TomlValue = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let overlay: TomlValue = toml::from_str(&format!(
        "[tools.spawn]\ndescription = \"{}\"",
        "x".repeat(MAX_TOOL_DESCRIPTION_BYTES + 1),
    ))
    .unwrap();
    merge_toml_values(&mut merged, overlay);

    assert_eq!(
        parse_merged_config(merged),
        Err(ConfigError::PromptTooLong {
            name: "tools.spawn.description",
            max: MAX_TOOL_DESCRIPTION_BYTES,
            actual: MAX_TOOL_DESCRIPTION_BYTES + 1,
        })
    );
}

#[test]
fn source_files_match_merge_precedence_and_trust_policy() {
    let loader = SpineConfigLoader::new("/work")
        .with_home_directory("/home/test")
        .with_custom_path("/explicit/spine.toml");
    assert_eq!(
        loader.optional_source_files(),
        vec![
            PathBuf::from("/home/test/.spine/spine.toml"),
            PathBuf::from("/work/.spine/spine.toml"),
            PathBuf::from("/work/spine.toml"),
        ]
    );
    assert_eq!(
        loader.required_source_file(),
        Some(PathBuf::from("/explicit/spine.toml"))
    );

    let loader = loader.without_working_directory_layers();
    assert_eq!(
        loader.optional_source_files(),
        vec![PathBuf::from("/home/test/.spine/spine.toml")]
    );
    assert_eq!(
        loader.required_source_file(),
        Some(PathBuf::from("/explicit/spine.toml"))
    );
}
