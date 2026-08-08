use std::collections::HashMap;
use std::io::Read;
use std::io::Write;

use flate2::read::GzDecoder;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn feedback_capability_requires_a_spine_feature() {
    assert!(!spine_feedback_enabled_by(|_| false));
    assert!(spine_feedback_enabled_by(|feature| {
        matches!(feature, Feature::SpineTrim)
    }));
}

#[test]
fn subtree_ids_are_root_first_deduplicated_and_stable() {
    let root = ThreadId::new();
    let child_a = ThreadId::new();
    let child_b = ThreadId::new();
    let normalized = normalize_subtree_thread_ids(root, vec![child_b, root, child_a, child_b]);
    assert_eq!(normalized.first(), Some(&root));
    assert_eq!(normalized.len(), 3);
    assert!(
        normalized[1..]
            .windows(2)
            .all(|ids| ids[0].to_string() < ids[1].to_string())
    );
}

#[test]
fn bundle_redacts_content_and_thread_identity() {
    let thread_id = ThreadId::new();
    let mut source = tempfile::NamedTempFile::new().expect("create rollout source");
    writeln!(
        source,
        "{}",
        serde_json::json!({
            "timestamp": "2026-08-08T00:00:00Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "private prompt"}],
                "thread_id": thread_id.to_string(),
            }
        })
    )
    .expect("write rollout source");
    source.flush().expect("flush rollout source");
    let snapshot = match snapshot_rollout_source(source.path()).expect("capture rollout source") {
        CapturedSource::Ready(snapshot) => snapshot,
        other => panic!("expected captured source, got {other:?}"),
    };
    let bytes = build_rollout_debug_attachment(
        thread_id,
        vec![CapturedThread {
            thread_id,
            source: CapturedSource::Ready(snapshot),
        }],
        HashMap::new(),
        BundleBuildLimits::production(128 * 1024),
    )
    .expect("build redacted bundle");
    let mut decoded = String::new();
    GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut decoded)
        .expect("decode redacted bundle");
    assert!(decoded.contains("spine.rollout_debug.v1"));
    assert!(!decoded.contains("private prompt"));
    assert!(!decoded.contains(&thread_id.to_string()));
}

#[test]
fn screenshots_reject_non_png_and_zero_dimensions() {
    let invalid = SpineFeedbackScreenshot {
        png_base64: BASE64_STANDARD.encode(b"not png"),
    };
    assert!(normalize_screenshots(vec![invalid]).is_err());
    assert!(validate_screenshot_dimensions(0, (0, 1)).is_err());
    assert!(validate_screenshot_dimensions(0, (1, 1)).is_ok());
}

#[test]
fn capped_writer_never_publishes_a_partial_overflowing_write() {
    let exceeded = Arc::new(AtomicBool::new(false));
    let mut writer = CappedWriter::new(4, Arc::clone(&exceeded));
    writer.write_all(b"1234").expect("write exact limit");
    assert!(writer.write_all(b"5").is_err());
    assert!(exceeded.load(Ordering::Relaxed));
    assert_eq!(writer.into_inner(), b"1234");
}

#[test]
fn bundle_limits_fail_closed_before_returning_partial_output() {
    let thread_id = ThreadId::new();
    let mut source = tempfile::NamedTempFile::new().expect("create rollout source");
    writeln!(source, "{}", serde_json::json!({"type": "event_msg"})).expect("write rollout source");
    source.flush().expect("flush rollout source");
    let snapshot = match snapshot_rollout_source(source.path()).expect("capture rollout source") {
        CapturedSource::Ready(snapshot) => snapshot,
        other => panic!("expected captured source, got {other:?}"),
    };

    let output_error = build_rollout_debug_attachment(
        thread_id,
        vec![CapturedThread {
            thread_id,
            source: CapturedSource::Ready(CapturedFileSnapshot {
                path: snapshot.path.clone(),
                captured_bytes: snapshot.captured_bytes,
                identity: snapshot.identity,
            }),
        }],
        HashMap::new(),
        BundleBuildLimits {
            output_bytes: 1,
            source_line_bytes: MAX_SOURCE_LINE_BYTES,
            source_bytes: MAX_PACKAGE_SOURCE_BYTES,
            source_records: MAX_PACKAGE_SOURCE_RECORDS,
        },
    )
    .expect_err("one byte cannot hold a complete gzip bundle");
    assert!(matches!(
        output_error,
        BundleBuildError::AttachmentTooLarge { limit: 1 }
    ));

    let source_error = build_rollout_debug_attachment(
        thread_id,
        vec![CapturedThread {
            thread_id,
            source: CapturedSource::Ready(snapshot),
        }],
        HashMap::new(),
        BundleBuildLimits {
            output_bytes: 128 * 1024,
            source_line_bytes: MAX_SOURCE_LINE_BYTES,
            source_bytes: 1,
            source_records: MAX_PACKAGE_SOURCE_RECORDS,
        },
    )
    .expect_err("captured source must respect the package work limit");
    assert!(matches!(
        source_error,
        BundleBuildError::SourceWorkLimitExceeded {
            resource: "captured bytes",
            limit: 1,
        }
    ));
}

#[test]
fn screenshot_limits_are_checked_before_decode_allocation() {
    let screenshots = (0..=MAX_SCREENSHOTS)
        .map(|_| SpineFeedbackScreenshot {
            png_base64: String::new(),
        })
        .collect();
    assert!(normalize_screenshots(screenshots).is_err());
    assert!(validate_screenshot_dimensions(0, (MAX_SCREENSHOT_SIDE, MAX_SCREENSHOT_SIDE)).is_err());
}
