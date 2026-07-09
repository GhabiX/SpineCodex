use codex_api::RawResponseCapture;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
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
    _session_id: SessionId,
    _thread_id: ThreadId,
    _window_generation: u64,
    request: &T,
) -> io::Result<DebugRequestCaptureRecord> {
    let capture_id = format!("{seq:06}");
    let path = dir.join(format!("{capture_id}_{transport}_request.json"));
    let request = serde_json::to_value(request).map_err(json_to_io_error)?;
    write_json(&path, &request)?;
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

pub(crate) struct DebugRawResponseCapture {
    dir: PathBuf,
    capture_id: String,
    transport: &'static str,
    state: Mutex<RawResponseCaptureState>,
}

struct RawResponseCaptureState {
    file: Option<File>,
    error: bool,
    finished: bool,
}

impl DebugRawResponseCapture {
    pub(crate) fn new(dir: PathBuf, capture_id: String, transport: &'static str) -> Self {
        Self {
            dir,
            capture_id,
            transport,
            state: Mutex::new(RawResponseCaptureState {
                file: None,
                error: false,
                finished: false,
            }),
        }
    }

    fn request_path(&self) -> PathBuf {
        self.dir.join(format!(
            "{}_{}_request.json",
            self.capture_id, self.transport
        ))
    }

    fn raw_path(&self) -> PathBuf {
        self.dir.join(format!(
            "{}_{}_response.raw",
            self.capture_id, self.transport
        ))
    }
}

impl RawResponseCapture for DebugRawResponseCapture {
    fn write_chunk(&self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished {
            return;
        }
        if state.file.is_none() {
            state.file = File::create(self.raw_path()).ok();
        }
        if state
            .file
            .as_mut()
            .is_none_or(|file| file.write_all(chunk).is_err())
        {
            state.error = true;
        }
    }

    fn record_error(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.error = true;
    }

    fn finish(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished {
            return;
        }
        state.finished = true;
        let error = state.error;
        drop(state.file.take());
        drop(state);

        if error {
            move_if_exists(
                &self.request_path(),
                &self.dir.join(format!(
                    "{}_{}_request_pinned.json",
                    self.capture_id, self.transport
                )),
            );
            move_if_exists(
                &self.raw_path(),
                &self.dir.join(format!(
                    "{}_{}_response_pinned.raw",
                    self.capture_id, self.transport
                )),
            );
        } else {
            for suffix in ["request.json", "response.raw"] {
                let latest_0 = self
                    .dir
                    .join(format!("latest_0_{}_{}", self.transport, suffix));
                let latest_1 = self
                    .dir
                    .join(format!("latest_1_{}_{}", self.transport, suffix));
                move_if_exists(&latest_0, &latest_1);
            }
            move_if_exists(
                &self.request_path(),
                &self
                    .dir
                    .join(format!("latest_0_{}_request.json", self.transport)),
            );
            move_if_exists(
                &self.raw_path(),
                &self
                    .dir
                    .join(format!("latest_0_{}_response.raw", self.transport)),
            );
        }
    }
}

fn move_if_exists(from: &Path, to: &Path) {
    if from.exists() {
        let _ = fs::rename(from, to);
    }
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
    fn write_request_preserves_raw_request_body() {
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
        let response_path = dir
            .path()
            .join("000007_responses_http_response_req_abc_123.json");

        let request_json: Value =
            serde_json::from_str(&fs::read_to_string(request_path).expect("read request capture"))
                .expect("parse request capture");
        let response_json: Value = serde_json::from_str(
            &fs::read_to_string(response_path).expect("read response capture"),
        )
        .expect("parse response capture");

        assert_eq!(request_json, request);
        assert_eq!(response_json["upstream_request_id"], json!("req/abc:123"));
    }

    #[test]
    fn raw_response_capture_rotates_successes_and_pins_errors() {
        let dir = TempDir::new().expect("tempdir");
        let session_id = SessionId::new();
        let thread_id = ThreadId::new();

        for (seq, marker, body, error) in [
            (1, "one", b"one".as_slice(), false),
            (2, "two", b"two".as_slice(), false),
            (3, "bad", b"bad".as_slice(), true),
            (4, "four", b"four".as_slice(), false),
        ] {
            let request = json!({ "marker": marker });
            let record = write_request(
                dir.path(),
                seq,
                "responses_http",
                session_id,
                thread_id,
                0,
                &request,
            )
            .expect("write request capture");
            let capture = DebugRawResponseCapture::new(
                dir.path().to_path_buf(),
                record.capture_id,
                "responses_http",
            );
            capture.write_chunk(body);
            if error {
                capture.record_error();
            }
            capture.finish();
        }

        assert_eq!(
            fs::read(dir.path().join("latest_0_responses_http_response.raw"))
                .expect("latest 0 raw"),
            b"four"
        );
        assert_eq!(
            fs::read(dir.path().join("latest_1_responses_http_response.raw"))
                .expect("latest 1 raw"),
            b"two"
        );
        assert_eq!(
            fs::read(dir.path().join("000003_responses_http_response_pinned.raw"))
                .expect("pinned raw"),
            b"bad"
        );

        let pinned_request: Value = serde_json::from_str(
            &fs::read_to_string(dir.path().join("000003_responses_http_request_pinned.json"))
                .expect("pinned request"),
        )
        .expect("parse pinned request");

        assert_eq!(pinned_request["marker"], json!("bad"));
    }
}
