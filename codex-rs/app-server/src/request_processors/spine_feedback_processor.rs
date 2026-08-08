use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Cursor;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::SpineFeedbackScreenshot;
use codex_app_server_protocol::SpineFeedbackUploadParams;
use codex_app_server_protocol::SpineFeedbackUploadResponse;
use codex_core::RolloutDebugRedactor;
use codex_core::RolloutDebugRedactorError;
use codex_core::StateDbHandle;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_features::Feature;
use codex_feedback::FeedbackAttachment;
use codex_feedback::SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES;
use codex_feedback::SPINE_FEEDBACK_MAX_NOTE_BYTES;
use codex_feedback::SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME;
use codex_feedback::SpineFeedbackUpload;
use codex_feedback::upload_spine_feedback;
use codex_protocol::ThreadId;
use flate2::Compression;
use flate2::GzBuilder;
use image::DynamicImage;
use image::ImageDecoder;
use image::ImageEncoder;
use image::ImageFormat;
use image::ImageReader;
use image::Limits;
use image::RgbaImage;
use image::codecs::png::PngEncoder;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::error_code::internal_error;
use crate::error_code::invalid_request;

const ROLLOUT_DEBUG_SCHEMA: &str = "spine.rollout_debug.v1";
const ROLLOUT_DEBUG_CONTENT_TYPE: &str = "application/gzip";
const SCREENSHOT_CONTENT_TYPE: &str = "image/png";
const SCREENSHOT_FILENAMES: [&str; 3] =
    ["screenshot-1.png", "screenshot-2.png", "screenshot-3.png"];
const MAX_SCREENSHOTS: usize = SCREENSHOT_FILENAMES.len();
const MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;
const MAX_SCREENSHOT_TOTAL_BYTES: usize = 10 * 1024 * 1024;
const MAX_SCREENSHOT_SIDE: u32 = 8192;
const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;
const MAX_SCREENSHOT_DECODE_ALLOC_BYTES: u64 = MAX_SCREENSHOT_PIXELS * 8;
const MAX_SCREENSHOT_BASE64_BYTES: usize = ((MAX_SCREENSHOT_BYTES + 2) / 3) * 4 + 4;
const MAX_SOURCE_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACKAGE_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_TRACKED_THREAD_IDS: usize = 131_072;
const MAX_PACKAGE_SOURCE_RECORDS: u64 = MAX_PACKAGE_TRACKED_THREAD_IDS as u64;
const ROLLOUT_READER_CAPACITY: usize = 64 * 1024;

pub(super) async fn spine_feedback_upload(
    thread_manager: Arc<ThreadManager>,
    config: Arc<Config>,
    state_db: Option<StateDbHandle>,
    params: SpineFeedbackUploadParams,
) -> Result<SpineFeedbackUploadResponse, JSONRPCErrorError> {
    if !config.feedback_enabled {
        return Err(invalid_request(
            "sending feedback is disabled by configuration",
        ));
    }

    let SpineFeedbackUploadParams {
        thread_id,
        note,
        screenshots,
    } = params;
    if note
        .as_ref()
        .is_some_and(|note| note.len() > SPINE_FEEDBACK_MAX_NOTE_BYTES)
    {
        return Err(invalid_request(format!(
            "Spine feedback note exceeds {SPINE_FEEDBACK_MAX_NOTE_BYTES} bytes"
        )));
    }

    let root_thread_id = ThreadId::from_string(&thread_id)
        .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
    let root_thread = thread_manager
        .get_thread(root_thread_id)
        .await
        .map_err(|_| invalid_request("Spine feedback requires an active thread"))?;
    if !spine_feedback_enabled(&root_thread) {
        return Err(invalid_request(
            "feedback/spineUpload requires a Spine-enabled thread",
        ));
    }

    let screenshots = tokio::task::spawn_blocking(move || normalize_screenshots(screenshots))
        .await
        .map_err(|err| internal_error(format!("failed to validate screenshots: {err}")))?
        .map_err(invalid_request)?;
    let screenshot_bytes = screenshots
        .iter()
        .map(|attachment| attachment.buffer.len())
        .sum::<usize>();

    let subtree_thread_ids = thread_manager
        .list_agent_subtree_thread_ids(root_thread_id)
        .await
        .map_err(|err| internal_error(format!("failed to snapshot Spine thread subtree: {err}")))?;
    validate_subtree_thread_count(subtree_thread_ids.len()).map_err(map_bundle_error)?;
    let subtree_thread_ids = normalize_subtree_thread_ids(root_thread_id, subtree_thread_ids);
    let parent_thread_ids =
        resolve_parent_thread_ids(&thread_manager, state_db.as_ref(), &subtree_thread_ids).await;
    let captures = capture_rollout_sources(&thread_manager, state_db.as_ref(), &subtree_thread_ids)
        .await
        .map_err(map_bundle_error)?;

    let rollout_limit = SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES
        .checked_sub(screenshot_bytes)
        .ok_or_else(|| invalid_request("Spine feedback screenshots exceed the attachment limit"))?;
    let rollout_bytes = tokio::task::spawn_blocking(move || {
        build_rollout_debug_attachment(
            root_thread_id,
            captures,
            parent_thread_ids,
            BundleBuildLimits::production(rollout_limit),
        )
    })
    .await
    .map_err(|err| internal_error(format!("failed to build rollout debug attachment: {err}")))?
    .map_err(map_bundle_error)?;

    let mut attachments = Vec::with_capacity(screenshots.len() + 1);
    attachments.push(FeedbackAttachment {
        filename: SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME.to_string(),
        content_type: Some(ROLLOUT_DEBUG_CONTENT_TYPE.to_string()),
        buffer: rollout_bytes,
    });
    attachments.extend(screenshots);

    let upload_result = tokio::task::spawn_blocking(move || {
        upload_spine_feedback(SpineFeedbackUpload {
            note: note.as_deref(),
            attachments: &attachments,
        })
    })
    .await
    .map_err(|err| internal_error(format!("failed to upload Spine feedback: {err}")))?;

    upload_result_to_response(upload_result)
}

pub(super) fn spine_feedback_enabled(thread: &codex_core::CodexThread) -> bool {
    spine_feedback_enabled_by(|feature| thread.enabled(feature))
}

fn spine_feedback_enabled_by(mut enabled: impl FnMut(Feature) -> bool) -> bool {
    [Feature::SpineJit, Feature::SpineTrim, Feature::SpineSpawn]
        .into_iter()
        .any(&mut enabled)
}

fn normalize_subtree_thread_ids(
    root_thread_id: ThreadId,
    thread_ids: Vec<ThreadId>,
) -> Vec<ThreadId> {
    let mut seen = HashSet::new();
    seen.insert(root_thread_id);
    let mut descendants = thread_ids
        .into_iter()
        .filter(|thread_id| *thread_id != root_thread_id && seen.insert(*thread_id))
        .collect::<Vec<_>>();
    descendants.sort_unstable_by_key(ToString::to_string);

    let mut normalized = Vec::with_capacity(descendants.len() + 1);
    normalized.push(root_thread_id);
    normalized.extend(descendants);
    normalized
}

fn validate_subtree_thread_count(thread_count: usize) -> Result<(), BundleBuildError> {
    if thread_count > MAX_PACKAGE_TRACKED_THREAD_IDS {
        return Err(BundleBuildError::SourceWorkLimitExceeded {
            resource: "thread identifiers",
            limit: MAX_PACKAGE_TRACKED_THREAD_IDS as u64,
        });
    }
    Ok(())
}

async fn resolve_parent_thread_ids(
    thread_manager: &ThreadManager,
    state_db: Option<&StateDbHandle>,
    thread_ids: &[ThreadId],
) -> HashMap<ThreadId, ThreadId> {
    let thread_id_set = thread_ids.iter().copied().collect::<HashSet<_>>();
    let mut parents = HashMap::new();

    for thread_id in thread_ids {
        if let Ok(thread) = thread_manager.get_thread(*thread_id).await
            && let Some(parent_thread_id) = thread.config_snapshot().await.parent_thread_id
        {
            parents.insert(*thread_id, parent_thread_id);
        }
    }

    if let Some(state_db) = state_db {
        for parent_thread_id in thread_ids {
            let Ok(child_thread_ids) = state_db.list_thread_spawn_children(*parent_thread_id).await
            else {
                continue;
            };
            for child_thread_id in child_thread_ids {
                if thread_id_set.contains(&child_thread_id) {
                    parents.entry(child_thread_id).or_insert(*parent_thread_id);
                }
            }
        }
    }

    parents
}

async fn capture_rollout_sources(
    thread_manager: &ThreadManager,
    state_db: Option<&StateDbHandle>,
    thread_ids: &[ThreadId],
) -> Result<Vec<CapturedThread>, BundleBuildError> {
    capture_rollout_sources_with_limit(
        thread_manager,
        state_db,
        thread_ids,
        MAX_PACKAGE_SOURCE_BYTES,
    )
    .await
}

async fn capture_rollout_sources_with_limit(
    thread_manager: &ThreadManager,
    state_db: Option<&StateDbHandle>,
    thread_ids: &[ThreadId],
    source_bytes_limit: u64,
) -> Result<Vec<CapturedThread>, BundleBuildError> {
    let mut captures = Vec::with_capacity(thread_ids.len());
    let mut captured_source_bytes = 0_u64;
    for thread_id in thread_ids {
        let source = match thread_manager.get_thread(*thread_id).await {
            Ok(thread) => {
                if thread.flush_rollout().await.is_err() {
                    CapturedSource::FlushFailed
                } else if let Some(path) = thread.rollout_path() {
                    capture_path(path).await?
                } else {
                    CapturedSource::Missing
                }
            }
            Err(_) => match state_db {
                Some(state_db) => match state_db
                    .find_rollout_path_by_id(*thread_id, /*archived_only*/ None)
                    .await
                {
                    Ok(Some(path)) => capture_path(path).await?,
                    Ok(None) => CapturedSource::Missing,
                    Err(_) => CapturedSource::Unavailable,
                },
                None => CapturedSource::Unavailable,
            },
        };
        if let CapturedSource::Ready(snapshot) = &source {
            captured_source_bytes = captured_source_bytes
                .checked_add(snapshot.captured_bytes)
                .ok_or(BundleBuildError::SourceWorkLimitExceeded {
                    resource: "captured bytes",
                    limit: source_bytes_limit,
                })?;
            if captured_source_bytes > source_bytes_limit {
                return Err(BundleBuildError::SourceWorkLimitExceeded {
                    resource: "captured bytes",
                    limit: source_bytes_limit,
                });
            }
        }
        captures.push(CapturedThread {
            thread_id: *thread_id,
            source,
        });
    }
    Ok(captures)
}

async fn capture_path(path: PathBuf) -> Result<CapturedSource, BundleBuildError> {
    match tokio::task::spawn_blocking(move || snapshot_rollout_source(&path)).await {
        Ok(source) => source,
        Err(_) => Ok(CapturedSource::Unavailable),
    }
}

fn rollout_source_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    options
}

fn snapshot_rollout_source(path: &Path) -> Result<CapturedSource, BundleBuildError> {
    let file = match rollout_source_open_options().open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(CapturedSource::Missing),
        Err(err) if is_source_capture_resource_exhaustion(&err) => {
            return Err(BundleBuildError::SourceCaptureResourceExhausted(err));
        }
        Err(_) => return Ok(CapturedSource::Unreadable),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(err) if is_source_capture_resource_exhaustion(&err) => {
            return Err(BundleBuildError::SourceCaptureResourceExhausted(err));
        }
        Err(_) => return Ok(CapturedSource::Unreadable),
    };
    if !metadata.is_file() {
        return Ok(CapturedSource::Unreadable);
    }
    let identity = RolloutSourceIdentity::from_metadata(&metadata)
        .map_err(BundleBuildError::SourceIdentityUnavailable)?;
    Ok(CapturedSource::Ready(CapturedFileSnapshot {
        path: path.to_path_buf(),
        captured_bytes: metadata.len(),
        identity,
    }))
}

fn is_source_capture_resource_exhaustion(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::OutOfMemory {
        return true;
    }
    #[cfg(unix)]
    {
        return matches!(
            error.raw_os_error(),
            Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM)
        );
    }
    #[cfg(windows)]
    {
        return matches!(error.raw_os_error(), Some(4 | 8 | 14));
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn reopen_rollout_source(snapshot: &CapturedFileSnapshot) -> io::Result<File> {
    let file = rollout_source_open_options().open(&snapshot.path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "captured rollout source is no longer a regular file",
        ));
    }
    if RolloutSourceIdentity::from_metadata(&metadata)? != snapshot.identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "captured rollout source identity changed",
        ));
    }
    if metadata.len() < snapshot.captured_bytes {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "captured rollout source shrank",
        ));
    }
    Ok(file)
}

#[derive(Debug)]
struct CapturedThread {
    thread_id: ThreadId,
    source: CapturedSource,
}

#[derive(Debug)]
enum CapturedSource {
    Ready(CapturedFileSnapshot),
    Missing,
    FlushFailed,
    Unavailable,
    Unreadable,
}

#[derive(Debug)]
struct CapturedFileSnapshot {
    path: PathBuf,
    captured_bytes: u64,
    identity: RolloutSourceIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RolloutSourceIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
}

impl RolloutSourceIdentity {
    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Ok(Self::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stable rollout source identity is unavailable on this platform",
            ))
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BundleBuildLimits {
    output_bytes: usize,
    source_line_bytes: usize,
    source_bytes: u64,
    source_records: u64,
}

impl BundleBuildLimits {
    const fn production(output_bytes: usize) -> Self {
        Self {
            output_bytes,
            source_line_bytes: MAX_SOURCE_LINE_BYTES,
            source_bytes: MAX_PACKAGE_SOURCE_BYTES,
            source_records: MAX_PACKAGE_SOURCE_RECORDS,
        }
    }
}

#[derive(Serialize)]
struct RolloutDebugManifest {
    record_type: &'static str,
    schema: &'static str,
    build: &'static str,
    root_thread_local_id: u64,
    thread_count: usize,
    threads: Vec<ManifestThread>,
}

#[derive(Serialize)]
struct ManifestThread {
    thread_local_id: u64,
    parent: ManifestParent,
    source: ManifestSource,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ManifestParent {
    Root,
    Known { thread_local_id: u64 },
    OutsideSnapshot,
    Unknown,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ManifestSource {
    Ready { captured_bytes: u64 },
    Missing,
    FlushFailed,
    Unavailable,
    Unreadable,
}

#[derive(Serialize)]
struct RolloutDebugThreadRecord {
    record_type: &'static str,
    thread_local_id: u64,
    ordinal: u64,
    item: Value,
}

fn build_rollout_debug_attachment(
    root_thread_id: ThreadId,
    captures: Vec<CapturedThread>,
    parent_thread_ids: HashMap<ThreadId, ThreadId>,
    limits: BundleBuildLimits,
) -> Result<Vec<u8>, BundleBuildError> {
    let total_captured_bytes = captures.iter().try_fold(0_u64, |total, capture| {
        let captured_bytes = match &capture.source {
            CapturedSource::Ready(snapshot) => snapshot.captured_bytes,
            CapturedSource::Missing
            | CapturedSource::FlushFailed
            | CapturedSource::Unavailable
            | CapturedSource::Unreadable => 0,
        };
        total
            .checked_add(captured_bytes)
            .ok_or(BundleBuildError::SourceWorkLimitExceeded {
                resource: "captured bytes",
                limit: limits.source_bytes,
            })
    })?;
    if total_captured_bytes > limits.source_bytes {
        return Err(BundleBuildError::SourceWorkLimitExceeded {
            resource: "captured bytes",
            limit: limits.source_bytes,
        });
    }

    let mut redactor = RolloutDebugRedactor::default();
    let mut local_thread_ids = HashMap::with_capacity(captures.len());
    for capture in &captures {
        let local_id = redactor
            .register_thread_id(&capture.thread_id.to_string())
            .map_err(BundleBuildError::Redaction)?;
        local_thread_ids.insert(capture.thread_id, local_id);
    }
    let root_thread_local_id = local_thread_ids[&root_thread_id];
    let manifest_threads = captures
        .iter()
        .map(|capture| ManifestThread {
            thread_local_id: local_thread_ids[&capture.thread_id],
            parent: manifest_parent(
                capture.thread_id,
                root_thread_id,
                &parent_thread_ids,
                &local_thread_ids,
            ),
            source: manifest_source(&capture.source),
        })
        .collect::<Vec<_>>();
    let manifest = RolloutDebugManifest {
        record_type: "manifest",
        schema: ROLLOUT_DEBUG_SCHEMA,
        build: env!("CARGO_PKG_VERSION"),
        root_thread_local_id,
        thread_count: manifest_threads.len(),
        threads: manifest_threads,
    };

    let exceeded = Arc::new(AtomicBool::new(false));
    let capped = CappedWriter::new(limits.output_bytes, Arc::clone(&exceeded));
    let mut gzip = GzBuilder::new()
        .mtime(0)
        .write(capped, Compression::default());
    write_json_line(&mut gzip, &manifest, limits.output_bytes, &exceeded)?;

    let mut source_records = 0_u64;
    for capture in captures {
        let CapturedSource::Ready(snapshot) = capture.source else {
            continue;
        };
        let file = reopen_rollout_source(&snapshot).map_err(BundleBuildError::SourceRead)?;
        let mut reader = BufReader::with_capacity(ROLLOUT_READER_CAPACITY, file);
        let mut remaining = snapshot.captured_bytes;
        let mut ordinal = 0_u64;
        while let Some(line) =
            read_bounded_source_line(&mut reader, &mut remaining, limits.source_line_bytes)
                .map_err(BundleBuildError::SourceRead)?
        {
            source_records =
                source_records
                    .checked_add(1)
                    .ok_or(BundleBuildError::SourceWorkLimitExceeded {
                        resource: "records",
                        limit: limits.source_records,
                    })?;
            if source_records > limits.source_records {
                return Err(BundleBuildError::SourceWorkLimitExceeded {
                    resource: "records",
                    limit: limits.source_records,
                });
            }
            let item = match line {
                BoundedSourceLine::Retained(line) => redactor
                    .redact_json_line_to_value(line.as_slice())
                    .map_err(BundleBuildError::Redaction)?,
                BoundedSourceLine::Oversized => RolloutDebugRedactor::oversized_value(),
            };
            let record = RolloutDebugThreadRecord {
                record_type: "thread_record",
                thread_local_id: local_thread_ids[&capture.thread_id],
                ordinal,
                item,
            };
            write_json_line(&mut gzip, &record, limits.output_bytes, &exceeded)?;
            ordinal = ordinal.saturating_add(1);
        }
        if remaining != 0 {
            return Err(BundleBuildError::SourceRead(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "rollout source changed before its captured boundary",
            )));
        }
    }

    let capped = gzip.finish().map_err(|err| {
        if exceeded.load(Ordering::Relaxed) {
            BundleBuildError::AttachmentTooLarge {
                limit: limits.output_bytes,
            }
        } else {
            BundleBuildError::Encoding(err)
        }
    })?;
    Ok(capped.into_inner())
}

fn manifest_parent(
    thread_id: ThreadId,
    root_thread_id: ThreadId,
    parent_thread_ids: &HashMap<ThreadId, ThreadId>,
    local_thread_ids: &HashMap<ThreadId, u64>,
) -> ManifestParent {
    if thread_id == root_thread_id {
        return ManifestParent::Root;
    }
    let Some(parent_thread_id) = parent_thread_ids.get(&thread_id) else {
        return ManifestParent::Unknown;
    };
    match local_thread_ids.get(parent_thread_id) {
        Some(thread_local_id) => ManifestParent::Known {
            thread_local_id: *thread_local_id,
        },
        None => ManifestParent::OutsideSnapshot,
    }
}

fn manifest_source(source: &CapturedSource) -> ManifestSource {
    match source {
        CapturedSource::Ready(snapshot) => ManifestSource::Ready {
            captured_bytes: snapshot.captured_bytes,
        },
        CapturedSource::Missing => ManifestSource::Missing,
        CapturedSource::FlushFailed => ManifestSource::FlushFailed,
        CapturedSource::Unavailable => ManifestSource::Unavailable,
        CapturedSource::Unreadable => ManifestSource::Unreadable,
    }
}

fn write_json_line<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    output_limit: usize,
    exceeded: &AtomicBool,
) -> Result<(), BundleBuildError> {
    if let Err(err) = serde_json::to_writer(&mut *writer, value) {
        if exceeded.load(Ordering::Relaxed) {
            return Err(BundleBuildError::AttachmentTooLarge {
                limit: output_limit,
            });
        }
        return Err(BundleBuildError::Serialization(err));
    }
    if let Err(err) = writer.write_all(b"\n") {
        if exceeded.load(Ordering::Relaxed) {
            return Err(BundleBuildError::AttachmentTooLarge {
                limit: output_limit,
            });
        }
        return Err(BundleBuildError::Encoding(err));
    }
    Ok(())
}

enum BoundedSourceLine {
    Retained(Vec<u8>),
    Oversized,
}

fn read_bounded_source_line<R: BufRead>(
    reader: &mut R,
    remaining: &mut u64,
    retained_limit: usize,
) -> io::Result<Option<BoundedSourceLine>> {
    if *remaining == 0 {
        return Ok(None);
    }

    let mut retained = Vec::new();
    let mut oversized = false;
    let mut saw_bytes = false;
    loop {
        let (consume_len, line_complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return if saw_bytes {
                    Ok(Some(if oversized {
                        BoundedSourceLine::Oversized
                    } else {
                        BoundedSourceLine::Retained(retained)
                    }))
                } else {
                    Ok(None)
                };
            }
            let available_len = usize::try_from(
                (*remaining).min(u64::try_from(available.len()).unwrap_or(u64::MAX)),
            )
            .unwrap_or(available.len());
            let bounded = &available[..available_len];
            let newline = bounded.iter().position(|byte| *byte == b'\n');
            let consume_len = newline.map_or(available_len, |index| index + 1);
            saw_bytes = saw_bytes || consume_len != 0;
            if !oversized {
                if retained.len().saturating_add(consume_len) > retained_limit {
                    retained.clear();
                    oversized = true;
                } else {
                    retained.extend_from_slice(&bounded[..consume_len]);
                }
            }
            (consume_len, newline.is_some())
        };

        reader.consume(consume_len);
        *remaining = remaining.saturating_sub(u64::try_from(consume_len).unwrap_or(u64::MAX));
        if line_complete || *remaining == 0 {
            return Ok(Some(if oversized {
                BoundedSourceLine::Oversized
            } else {
                BoundedSourceLine::Retained(retained)
            }));
        }
    }
}

struct CappedWriter {
    buffer: Vec<u8>,
    limit: usize,
    exceeded: Arc<AtomicBool>,
}

impl CappedWriter {
    fn new(limit: usize, exceeded: Arc<AtomicBool>) -> Self {
        Self {
            buffer: Vec::with_capacity(limit.min(1024 * 1024)),
            limit,
            exceeded,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.buffer
    }
}

impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.buffer.len().checked_add(bytes.len()) else {
            self.exceeded.store(true, Ordering::Relaxed);
            return Err(io::Error::other("rollout debug attachment size overflow"));
        };
        if next_len > self.limit {
            self.exceeded.store(true, Ordering::Relaxed);
            return Err(io::Error::other("rollout debug attachment limit exceeded"));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Error)]
enum BundleBuildError {
    #[error("rollout debug attachment exceeds {limit} bytes")]
    AttachmentTooLarge { limit: usize },
    #[error("rollout debug source {resource} exceeds package limit {limit}")]
    SourceWorkLimitExceeded { resource: &'static str, limit: u64 },
    #[error("rollout source capture exhausted process resources")]
    SourceCaptureResourceExhausted(#[source] io::Error),
    #[error("stable rollout source identity is unavailable")]
    SourceIdentityUnavailable(#[source] io::Error),
    #[error("failed to read captured rollout source")]
    SourceRead(#[source] io::Error),
    #[error("failed to encode rollout debug attachment")]
    Encoding(#[source] io::Error),
    #[error("failed to serialize rollout debug record")]
    Serialization(#[source] serde_json::Error),
    #[error("rollout debug redaction state limit exceeded")]
    Redaction(#[source] RolloutDebugRedactorError),
}

fn map_bundle_error(error: BundleBuildError) -> JSONRPCErrorError {
    match error {
        BundleBuildError::AttachmentTooLarge { .. }
        | BundleBuildError::SourceWorkLimitExceeded { .. } => invalid_request(error.to_string()),
        BundleBuildError::SourceRead(_)
        | BundleBuildError::SourceCaptureResourceExhausted(_)
        | BundleBuildError::SourceIdentityUnavailable(_)
        | BundleBuildError::Encoding(_)
        | BundleBuildError::Serialization(_)
        | BundleBuildError::Redaction(_) => internal_error(error.to_string()),
    }
}

fn normalize_screenshots(
    screenshots: Vec<SpineFeedbackScreenshot>,
) -> Result<Vec<FeedbackAttachment>, String> {
    if screenshots.len() > MAX_SCREENSHOTS {
        return Err(format!(
            "Spine feedback accepts at most {MAX_SCREENSHOTS} screenshots"
        ));
    }

    let mut attachments = Vec::with_capacity(screenshots.len());
    let mut total_bytes = 0_usize;
    for (index, screenshot) in screenshots.into_iter().enumerate() {
        if screenshot.png_base64.len() > MAX_SCREENSHOT_BASE64_BYTES {
            return Err(format!("screenshot {} is too large", index + 1));
        }
        let input = BASE64_STANDARD
            .decode(screenshot.png_base64.as_bytes())
            .map_err(|_| format!("screenshot {} is not valid base64", index + 1))?;
        if input.len() > MAX_SCREENSHOT_BYTES {
            return Err(format!("screenshot {} is too large", index + 1));
        }
        if image::guess_format(&input).ok() != Some(ImageFormat::Png) {
            return Err(format!("screenshot {} is not a PNG image", index + 1));
        }

        let image = decode_screenshot_png(index, &input)?;
        let normalized = encode_screenshot_png(index, &image.into_rgba8(), MAX_SCREENSHOT_BYTES)?;
        total_bytes = total_bytes
            .checked_add(normalized.len())
            .ok_or_else(|| "screenshot byte count overflowed".to_string())?;
        if total_bytes > MAX_SCREENSHOT_TOTAL_BYTES {
            return Err(format!(
                "Spine feedback screenshots exceed {MAX_SCREENSHOT_TOTAL_BYTES} bytes"
            ));
        }

        attachments.push(FeedbackAttachment {
            filename: SCREENSHOT_FILENAMES[index].to_string(),
            content_type: Some(SCREENSHOT_CONTENT_TYPE.to_string()),
            buffer: normalized,
        });
    }
    Ok(attachments)
}

fn screenshot_decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SCREENSHOT_SIDE);
    limits.max_image_height = Some(MAX_SCREENSHOT_SIDE);
    limits.max_alloc = Some(MAX_SCREENSHOT_DECODE_ALLOC_BYTES);
    limits
}

fn decode_screenshot_png(index: usize, input: &[u8]) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(input), ImageFormat::Png);
    reader.limits(screenshot_decode_limits());
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| format!("screenshot {} is not a valid PNG image", index + 1))?;
    let dimensions = decoder.dimensions();
    validate_screenshot_dimensions(index, dimensions)?;

    let mut remaining_limits = screenshot_decode_limits();
    remaining_limits
        .reserve(decoder.total_bytes())
        .map_err(|_| format!("screenshot {} requires too much decode memory", index + 1))?;
    decoder
        .set_limits(remaining_limits)
        .map_err(|_| format!("screenshot {} exceeds decode limits", index + 1))?;
    DynamicImage::from_decoder(decoder)
        .map_err(|_| format!("screenshot {} is not a valid PNG image", index + 1))
}

fn encode_screenshot_png(index: usize, image: &RgbaImage, limit: usize) -> Result<Vec<u8>, String> {
    let exceeded = Arc::new(AtomicBool::new(false));
    let mut output = CappedWriter::new(limit, Arc::clone(&exceeded));
    let encode_result = PngEncoder::new(&mut output).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    );
    if exceeded.load(Ordering::Relaxed) {
        return Err(format!(
            "normalized screenshot {} exceeds {limit} bytes",
            index + 1
        ));
    }
    encode_result.map_err(|_| format!("screenshot {} could not be normalized", index + 1))?;
    Ok(output.into_inner())
}

fn validate_screenshot_dimensions(index: usize, (width, height): (u32, u32)) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("screenshot {} has zero dimensions", index + 1));
    }
    if width > MAX_SCREENSHOT_SIDE || height > MAX_SCREENSHOT_SIDE {
        return Err(format!(
            "screenshot {} exceeds {MAX_SCREENSHOT_SIDE} pixels per side",
            index + 1
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_SCREENSHOT_PIXELS {
        return Err(format!(
            "screenshot {} exceeds {MAX_SCREENSHOT_PIXELS} pixels",
            index + 1
        ));
    }
    Ok(())
}

fn upload_result_to_response(
    upload_result: anyhow::Result<String>,
) -> Result<SpineFeedbackUploadResponse, JSONRPCErrorError> {
    upload_result
        .map(|report_id| SpineFeedbackUploadResponse { report_id })
        .map_err(|err| internal_error(format!("failed to upload Spine feedback: {err}")))
}

#[cfg(test)]
#[path = "spine_feedback_processor_tests.rs"]
mod tests;
