use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone)]
pub(crate) struct DebugRequestCaptureRecord {
    pub(crate) capture_id: String,
}

pub(crate) fn write_request<T: Serialize>(
    dir: &Path,
    seq: u64,
    transport: &'static str,
    session_id: SessionId,
    thread_id: ThreadId,
    window_generation: u64,
    request: &T,
) -> io::Result<DebugRequestCaptureRecord> {
    let capture_id = format!("{seq:06}");
    let path = dir.join(format!("{capture_id}_{transport}_request.json"));
    let request = serde_json::to_value(request).map_err(json_to_io_error)?;
    write_json(&path, &request)?;
    let meta_path = dir.join(format!("{capture_id}_{transport}_meta.json"));
    write_json(
        &meta_path,
        &json!({
            "version": 1,
            "capture_id": capture_id,
            "captured_at_unix_ms": unix_timestamp_ms(),
            "transport": transport,
            "session_id": session_id.to_string(),
            "thread_id": thread_id.to_string(),
            "window_generation": window_generation,
        }),
    )?;
    Ok(DebugRequestCaptureRecord { capture_id })
}

pub(crate) fn write_response(
    dir: &Path,
    capture_id: &str,
    transport: &'static str,
    upstream_request_id: Option<&str>,
) -> io::Result<()> {
    let request_id_segment = upstream_request_id
        .map(sanitize_filename_segment)
        .filter(|segment| !segment.is_empty())
        .map(|segment| format!("_{segment}"))
        .unwrap_or_default();
    let path = dir.join(format!(
        "{capture_id}_{transport}_response{request_id_segment}.json"
    ));
    write_json(
        &path,
        &json!({
            "version": 1,
            "capture_id": capture_id,
            "captured_at_unix_ms": unix_timestamp_ms(),
            "transport": transport,
            "upstream_request_id": upstream_request_id,
        }),
    )
}

// TODO(spine-debug): Temporary crash-diagnostics hook for the unexpected
// `OutputTextDelta without active item` failure. Remove this writer and its
// callers after the root cause is identified and fixed.
pub(crate) fn write_stream_event_trace<T: Serialize>(
    dir: &Path,
    turn_id: &str,
    trace: &T,
) -> io::Result<PathBuf> {
    let path = dir.join(format!(
        "stream_event_trace_{}_{}.json",
        unix_timestamp_ms(),
        sanitize_filename_segment(turn_id)
    ));
    let trace = serde_json::to_value(trace).map_err(json_to_io_error)?;
    write_json(&path, &trace)?;
    Ok(path)
}

fn write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(value).map_err(json_to_io_error)? + "\n";
    fs::write(path, content)
}

fn json_to_io_error(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

fn unix_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn sanitize_filename_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use tempfile::TempDir;

    #[test]
    fn response_filename_includes_sanitized_upstream_request_id() {
        assert_eq!(
            sanitize_filename_segment("req/abc:123"),
            "req_abc_123".to_string()
        );
    }

    #[test]
    fn write_request_preserves_raw_request_body_and_sidecar_metadata() {
        let dir = TempDir::new().expect("tempdir");
        let session_id = SessionId::new();
        let thread_id = ThreadId::new();
        let request = json!({
            "model": "gpt-test",
            "prompt_cache_key": "thread-key",
            "input": [{"type": "message", "role": "user", "content": "hello"}],
        });

        let record = write_request(
            dir.path(),
            7,
            "responses_http",
            session_id,
            thread_id,
            3,
            &request,
        )
        .expect("write request capture");
        write_response(
            dir.path(),
            &record.capture_id,
            "responses_http",
            Some("req/abc:123"),
        )
        .expect("write response capture");

        let request_path = dir.path().join("000007_responses_http_request.json");
        let meta_path = dir.path().join("000007_responses_http_meta.json");
        let response_path = dir
            .path()
            .join("000007_responses_http_response_req_abc_123.json");

        let request_json: Value =
            serde_json::from_str(&fs::read_to_string(request_path).expect("read request capture"))
                .expect("parse request capture");
        let meta_json: Value =
            serde_json::from_str(&fs::read_to_string(meta_path).expect("read meta capture"))
                .expect("parse meta capture");
        let response_json: Value = serde_json::from_str(
            &fs::read_to_string(response_path).expect("read response capture"),
        )
        .expect("parse response capture");

        assert_eq!(request_json, request);
        assert_eq!(meta_json["capture_id"], json!("000007"));
        assert_eq!(meta_json["transport"], json!("responses_http"));
        assert_eq!(meta_json["session_id"], json!(session_id.to_string()));
        assert_eq!(meta_json["thread_id"], json!(thread_id.to_string()));
        assert_eq!(meta_json["window_generation"], json!(3));
        assert_eq!(response_json["upstream_request_id"], json!("req/abc:123"));
    }
}
