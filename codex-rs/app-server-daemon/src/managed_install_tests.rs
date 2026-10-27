use pretty_assertions::assert_eq;

use super::ExecutableIdentity;
use super::executable_identity;
#[cfg(unix)]
use super::managed_codex_version;
use super::parse_codex_version;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[tokio::test]
async fn product_probe_does_not_confuse_upstream_compatibility_with_product_version() {
    let directory = tempfile::tempdir().expect("probe fixture");
    let executable = directory.path().join("codex");
    std::fs::write(&executable, format!(r#"#!/bin/sh
case "$1" in
  --version) printf 'spine-codex {compat_version}\n' ;;
  app-server)
    [ "$2" = '--listen' ] && [ "$3" = 'stdio://' ] || exit 4
    read -r request
    case "$request" in
      *'"method":"initialize"'*) printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"userAgent":"codex_app_server_daemon/{product_version} (Linux)"}}}}' ;;
      *) exit 2 ;;
    esac ;;
  *) exit 3 ;;
esac
"#,
        compat_version = env!("CARGO_PKG_VERSION"),
        product_version = codex_install_context::distribution::PRODUCT_VERSION,
    )).expect("write probe fixture");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
        .expect("make fixture executable");
    assert_eq!(
        managed_codex_version(&executable)
            .await
            .expect("product version"),
        codex_install_context::distribution::PRODUCT_VERSION
    );
}

#[tokio::test]
async fn product_probe_initializes_current_cli_binary() {
    let executable = codex_utils_cargo_bin::cargo_bin("codex").expect("built CLI binary");
    let version = tokio::process::Command::new(&executable)
        .arg("--version")
        .output()
        .await
        .expect("CLI compatibility version");
    assert!(version.status.success());
    assert_eq!(
        parse_codex_version(std::str::from_utf8(&version.stdout).expect("utf8"))
            .expect("CLI version"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        super::managed_codex_version(&executable)
            .await
            .expect("native app-server product handshake"),
        codex_install_context::distribution::PRODUCT_VERSION
    );
}

#[test]
fn parses_codex_cli_version_output() {
    assert_eq!(
        parse_codex_version("codex 1.2.3\n").expect("valid CLI version"),
        "1.2.3"
    );
}

#[test]
fn rejects_malformed_codex_cli_version_output() {
    assert!(parse_codex_version("codex\n").is_err());
}

#[tokio::test]
async fn executable_identity_uses_path_and_binary_contents() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = directory.path().join("codex");
    // Span multiple reads, including a partial final buffer, and preserve the
    // digest stored by older clients that hashed the complete file in memory.
    let mut bytes: Vec<u8> = (0..200_003).map(|index| (index % 251) as u8).collect();
    for contents in [&bytes[..], &[][..]] {
        std::fs::write(&executable, contents).expect("write executable");
        assert_eq!(
            executable_identity(&executable).await.expect("identity"),
            ExecutableIdentity {
                digest: *blake3::hash(contents).as_bytes(),
                path_digest: Some(super::path_digest(
                    &std::fs::canonicalize(&executable).expect("canonical executable"),
                )),
            }
        );
    }
    let copy = directory.path().join("codex-copy");
    std::fs::copy(&executable, &copy).expect("copy executable");
    let identity = executable_identity(&executable).await.expect("identity");
    let copy_identity = executable_identity(&copy).await.expect("copy identity");
    assert_ne!(identity, copy_identity);
    assert!(identity.same_contents(&copy_identity));
    std::fs::write(&executable, &bytes).expect("write executable");
    let old = executable_identity(&executable).await.expect("identity");
    bytes[100_000] ^= 1;
    std::fs::write(&executable, bytes).expect("replace executable");
    assert_ne!(
        executable_identity(&executable)
            .await
            .expect("new identity"),
        old
    );
}
