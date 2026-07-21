use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn required_string<'a>(metadata: &'a toml::Table, key: &str) -> &'a str {
    metadata
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("workspace.metadata.spinecodex.{key} must be a non-empty string"))
}

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let workspace_manifest = manifest_dir
        .parent()
        .expect("codex-protocol must be directly under the workspace")
        .join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", workspace_manifest.display());

    let manifest = fs::read_to_string(&workspace_manifest).unwrap_or_else(|error| {
        panic!(
            "failed to read workspace manifest {}: {error}",
            workspace_manifest.display()
        )
    });
    let document = toml::from_str::<toml::Value>(&manifest).unwrap_or_else(|error| {
        panic!(
            "failed to parse workspace manifest {}: {error}",
            workspace_manifest.display()
        )
    });
    let metadata = document
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("spinecodex"))
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| panic!("workspace.metadata.spinecodex must be present"));

    let compatibility_version = required_string(metadata, "codex_compat_version");
    let parsed_version = semver::Version::parse(compatibility_version)
        .unwrap_or_else(|error| panic!("codex_compat_version must be valid SemVer: {error}"));
    if !parsed_version.pre.is_empty() || !parsed_version.build.is_empty() {
        panic!("codex_compat_version must be a stable major.minor.patch version");
    }

    let upstream_tag = required_string(metadata, "codex_upstream_tag");
    let expected_tag = format!("rust-v{compatibility_version}");
    if upstream_tag != expected_tag {
        panic!(
            "codex_upstream_tag must match codex_compat_version ({expected_tag}), got {upstream_tag}"
        );
    }

    let upstream_commit = required_string(metadata, "codex_upstream_commit");
    if upstream_commit.len() != 40
        || !upstream_commit
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        panic!("codex_upstream_commit must be a 40-character hexadecimal commit SHA");
    }

    let output = Path::new(&env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("spinecodex_compatibility.rs");
    let generated = format!(
        "pub const CODEX_COMPAT_VERSION: &str = {compatibility_version:?};\n\
         pub const CODEX_UPSTREAM_TAG: &str = {upstream_tag:?};\n\
         pub const CODEX_UPSTREAM_COMMIT: &str = {upstream_commit:?};\n"
    );
    fs::write(&output, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write generated compatibility constants {}: {error}",
            output.display()
        )
    });
}
