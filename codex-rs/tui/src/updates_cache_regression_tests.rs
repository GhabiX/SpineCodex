use super::*;
use crate::legacy_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn spine_version_cache_dismissal_does_not_read_or_write_upstream_cache() {
    let home = tempdir().expect("isolated home");
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .expect("config");
    // These names are frozen distribution boundaries, not the implementation's own constant.
    let upstream = home.path().join("version.json");
    let spine = home.path().join("spine-codex-version.json");
    let upstream_bytes = b"{\"latest_version\":\"999.1.0\",\"last_checked_at\":\"2026-10-01T00:00:00Z\",\"dismissed_version\":null}\n";
    std::fs::write(&upstream, upstream_bytes).expect("upstream cache");
    dismiss_version(&config, "0.4.1")
        .await
        .expect("initial dismissal");
    let initial = read_version_info(&spine).expect("Spine cache");
    assert_eq!(
        (
            initial.latest_version.as_str(),
            initial.dismissed_version.as_deref()
        ),
        ("0.4.1", Some("0.4.1"))
    );
    assert_eq!(
        std::fs::read(&upstream).expect("upstream remains"),
        upstream_bytes
    );
    let checked_at = DateTime::parse_from_rfc3339("2026-10-02T00:00:00Z")
        .expect("date")
        .with_timezone(&Utc);
    std::fs::write(
        &spine,
        serde_json::to_vec(&VersionInfo {
            latest_version: "0.4.2".into(),
            last_checked_at: checked_at,
            dismissed_version: None,
        })
        .expect("serialize"),
    )
    .expect("Spine cache update");
    dismiss_version(&config, "0.4.2")
        .await
        .expect("second dismissal");
    let result = read_version_info(&spine).expect("Spine dismissal");
    assert_eq!(
        (
            result.latest_version.as_str(),
            result.dismissed_version.as_deref(),
            result.last_checked_at
        ),
        ("0.4.2", Some("0.4.2"), checked_at)
    );
    assert_eq!(
        std::fs::read(&upstream).expect("upstream remains"),
        upstream_bytes
    );
}
