use pretty_assertions::assert_eq;

use super::ExecutableIdentity;
use super::executable_identity;
use super::managed_codex_version;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn product_probe_does_not_confuse_upstream_compatibility_with_product_version() {
    let directory = tempfile::tempdir().expect("probe fixture");
    let executable = directory.path().join("codex");
    std::fs::write(&executable, r#"#!/bin/sh
case "$1" in
  --version) printf 'spine-codex 0.153.4\n' ;;
  mcp-server)
    read -r request
    case "$request" in
      *'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"codex-mcp-server","version":"0.4.0"}}}' ;;
      *) exit 2 ;;
    esac ;;
  *) exit 3 ;;
esac
"#).expect("write probe fixture");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
        .expect("make fixture executable");
    assert_eq!(
        managed_codex_version(&executable)
            .await
            .expect("product version"),
        "0.4.0"
    );
}

#[test]
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
