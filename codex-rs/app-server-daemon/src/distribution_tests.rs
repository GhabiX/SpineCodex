use super::*;
use pretty_assertions::assert_eq;

const UNSUPPORTED: &str = "SpineCodex does not support managed app-server lifecycle operations; use a local session or an explicitly configured app-server endpoint";

#[test]
fn distribution_rejects_shared_state_before_loading_environment() {
    let Err(error) = Daemon::from_environment() else {
        panic!("managed lifecycle must require an owned distribution");
    };
    assert_eq!(error.to_string(), UNSUPPORTED);
}

// Windows may reject elevated callers before reaching the distribution boundary.
#[cfg(unix)]
#[tokio::test]
async fn lifecycle_commands_reject_before_resolving_shared_state() {
    for command in [
        LifecycleCommand::Start,
        LifecycleCommand::Restart {
            feature_overrides: BTreeMap::new(),
        },
        LifecycleCommand::Stop,
        LifecycleCommand::Version,
    ] {
        assert_eq!(run(command).await.unwrap_err().to_string(), UNSUPPORTED);
    }
    assert_eq!(
        start_with_features(&BTreeMap::new())
            .await
            .unwrap_err()
            .to_string(),
        UNSUPPORTED
    );
    assert_eq!(
        restart_with_features(&BTreeMap::new())
            .await
            .unwrap_err()
            .to_string(),
        UNSUPPORTED
    );
    assert_eq!(
        bootstrap(BootstrapOptions {
            remote_control_enabled: false
        })
        .await
        .unwrap_err()
        .to_string(),
        UNSUPPORTED
    );
    assert_eq!(
        ensure_remote_control_ready().await.unwrap_err().to_string(),
        UNSUPPORTED
    );
    assert_eq!(maintain_stderr_log().unwrap_err().to_string(), UNSUPPORTED);
}
