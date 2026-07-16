use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn update_does_not_start_interactive_prompt() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .arg("update")
        .assert()
        .failure()
        .stderr(contains("`codex update` is not available in debug builds"));

    Ok(())
}

#[test]
fn npm_package_exposes_spine_codex_binaries_without_codex_collision() -> Result<()> {
    let package_json_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codex-rs should have a parent repo root")
        .join("codex-cli")
        .join("package.json");
    let package: Value = serde_json::from_str(&std::fs::read_to_string(package_json_path)?)?;
    assert_eq!(
        package.get("name").and_then(Value::as_str),
        Some("@spinejit/spine-codex")
    );

    let bin = package
        .get("bin")
        .and_then(Value::as_object)
        .expect("package bin must be an object");
    assert_eq!(bin.len(), 2);
    assert_eq!(
        bin.get("spine-codex").and_then(Value::as_str),
        Some("bin/codex.js")
    );
    assert_eq!(
        bin.get("spinecodex").and_then(Value::as_str),
        Some("bin/codex.js")
    );
    assert_eq!(bin.get("codex"), None);

    Ok(())
}
