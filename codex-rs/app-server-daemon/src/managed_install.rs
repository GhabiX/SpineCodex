//! Resolves both package and legacy standalone layouts and compares installed executables.

use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use tokio::fs;
use tokio::process::Command;
use tokio::time::timeout;

/// New daemons own their packages, regardless of how the calling CLI was installed.
/// Preserve legacy launch state, including logs left after a daemon is stopped;
/// settings, installer selections, and lock files alone do not prove a prior launch.
pub(crate) fn package_root(codex_home: &Path) -> PathBuf {
    let dedicated = codex_home.join("packages/app-server-daemon");
    if !matches!(dedicated.join("current").symlink_metadata(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return dedicated;
    }
    let state = codex_home.join("app-server-daemon");
    for (package, artifacts) in [
        (
            "app-server-daemon",
            [
                crate::DAEMON_PID_FILE_NAME,
                "daemon.stderr.log",
                crate::DAEMON_UPDATE_PID_FILE_NAME,
                "daemon-updater.stderr.log",
            ],
        ),
        (
            "standalone",
            [
                crate::LEGACY_PID_FILE_NAME,
                "app-server.stderr.log",
                crate::LEGACY_UPDATE_PID_FILE_NAME,
                "app-server-updater.stderr.log",
            ],
        ),
    ] {
        if artifacts.iter().any(|name| {
            !matches!(state.join(name).symlink_metadata(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        }) {
            return codex_home.join("packages").join(package);
        }
    }
    dedicated
}

/// Resolve both packaged and legacy binaries without requiring a valid install.
pub(crate) fn managed_codex_bin(codex_home: &Path) -> PathBuf {
    let root = package_root(codex_home);
    let current = root.join("current");
    let packaged = current.join("bin").join(managed_codex_file_name());
    let legacy = current.join(managed_codex_file_name());
    if packaged.is_file()
        || !legacy.is_file() && (cfg!(windows) || root.ends_with("app-server-daemon"))
    {
        packaged
    } else {
        legacy
    }
}

/// Only latest-channel stable releases may run the public latest-version updater.
pub(crate) fn is_stable_standalone_release(codex_home: &Path, codex_bin: &Path) -> bool {
    let standalone = package_root(codex_home);
    let Ok(releases) = std::fs::canonicalize(standalone.join("releases")) else {
        return false;
    };
    let Ok(release) = std::fs::canonicalize(standalone.join("current")) else {
        return false;
    };
    if release.parent() != Some(releases.as_path()) {
        return false;
    }
    let Some(release_name) = release.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // GNU packages can seed the new directory; retain legacy updater eligibility.
    if standalone.ends_with("standalone") && release_name.ends_with("-gnu") {
        return false;
    }
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-musl",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
    ];
    let Some(version) = targets
        .iter()
        .find_map(|target| release_name.strip_suffix(&format!("-{target}")))
    else {
        return false;
    };
    let components: Vec<_> = version.split('.').collect();
    components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
        && std::fs::read_to_string(standalone.join("auto-update-version"))
            .is_ok_and(|selected| selected == release_name)
        && std::fs::canonicalize(codex_bin).is_ok_and(|bin| bin.starts_with(&release))
}

/// Older managed binaries can serve app-server requests without owning an updater.
pub(crate) async fn supports_daemon_update_loop(codex_bin: &Path) -> bool {
    supports_daemon_command(codex_bin, &["pid-update-loop", "--help"]).await
}

/// Probe an internal daemon command without running a long-lived process.
pub(crate) async fn supports_daemon_command(codex_bin: &Path, args: &[&str]) -> bool {
    let mut command = Command::new(codex_bin);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    timeout(
        Duration::from_secs(5),
        command
            .args(["app-server", "daemon"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|status| status.success()))
}

pub(crate) async fn resolved_managed_codex_bin(codex_bin: &Path) -> Result<PathBuf> {
    fs::canonicalize(codex_bin).await.with_context(|| {
        format!(
            "failed to resolve managed Codex binary {}",
            codex_bin.display()
        )
    })
}

pub(crate) async fn managed_codex_version(codex_bin: &Path) -> Result<String> {
    // The daemon client receives the product identity from native app-server initialize.
    // The public CLI --version intentionally reports upstream compatibility.
    let mut command = Command::new(codex_bin);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = command
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "failed to probe managed Codex binary {}",
                codex_bin.display()
            )
        })?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "codex_app_server_daemon", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let (output, response) = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut stdin = child
            .stdin
            .take()
            .context("product probe stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("product probe stdout was not piped")?;
        let mut lines = BufReader::new(stdout).lines();
        stdin.write_all(format!("{request}\n").as_bytes()).await?;
        // EOF closes the stdio connection. Keep it open until the response arrives.
        let response = loop {
            let line = lines
                .next_line()
                .await?
                .context("managed Codex app-server closed without an initialize response")?;
            let response: serde_json::Value = serde_json::from_str(&line)
                .context("managed Codex product probe returned malformed JSON-RPC")?;
            if response["id"] == 1 {
                break response;
            }
        };
        drop(stdin);
        let output = child.wait_with_output().await?;
        Ok::<_, anyhow::Error>((output, response))
    })
    .await
    .context("timed out reading managed Codex product identity")??;
    if !output.status.success() {
        return Err(anyhow!(
            "managed Codex binary {} exited with status {}",
            codex_bin.display(),
            output.status
        ));
    }

    let user_agent = response
        .pointer("/result/userAgent")
        .and_then(serde_json::Value::as_str)
        .context("managed Codex app-server initialization did not return a user-agent")?;
    crate::client::parse_version_from_user_agent(user_agent)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutableIdentity {
    pub(crate) digest: [u8; 32],
    // Distinguish release generations whose executable bytes are identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path_digest: Option<[u8; 32]>,
}

impl ExecutableIdentity {
    pub(crate) fn same_contents(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}

pub(crate) async fn executable_identity(executable: &Path) -> Result<ExecutableIdentity> {
    let executable = executable.to_path_buf();
    // Debug executables can be hundreds of MB. Stream the digest off the async
    // runtime instead of allocating the whole file and blocking a runtime thread.
    tokio::task::spawn_blocking(move || {
        let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
        let mut identity = std::fs::File::open(&executable)
            .and_then(executable_identity_from_reader)
            .with_context(|| format!("failed to read executable {}", executable.display()))?;
        identity.path_digest = Some(path_digest(&executable));
        Ok(identity)
    })
    .await
    .context("executable identity task failed")?
}

pub(crate) fn executable_identity_from_reader(
    reader: impl std::io::Read,
) -> std::io::Result<ExecutableIdentity> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(reader)?;
    Ok(ExecutableIdentity {
        digest: *hasher.finalize().as_bytes(),
        path_digest: None,
    })
}

pub(crate) fn path_digest(path: &Path) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(&unit.to_le_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn managed_codex_file_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}

fn parse_codex_version(output: &str) -> Result<String> {
    let version = output
        .split_whitespace()
        .nth(1)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| anyhow!("managed Codex version output was malformed"))?;
    Ok(version.to_string())
}

#[cfg(test)]
#[path = "managed_install_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "managed_install_path_tests.rs"]
mod path_tests;
