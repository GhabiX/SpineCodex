use image::DynamicImage;
use image::ImageDecoder;
use image::ImageEncoder;
use image::ImageFormat;
use image::ImageReader;
use image::Limits;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPDecoder;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use tempfile::Builder;

#[derive(Debug, Clone)]
pub enum PasteImageError {
    ClipboardUnavailable(String),
    NoImage(String),
    InvalidImage(String),
    EncodeFailed(String),
    IoError(String),
}

impl std::fmt::Display for PasteImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteImageError::ClipboardUnavailable(msg) => write!(f, "clipboard unavailable: {msg}"),
            PasteImageError::NoImage(msg) => write!(f, "no image on clipboard: {msg}"),
            PasteImageError::InvalidImage(msg) => write!(f, "invalid image: {msg}"),
            PasteImageError::EncodeFailed(msg) => write!(f, "could not encode image: {msg}"),
            PasteImageError::IoError(msg) => write!(f, "io error: {msg}"),
        }
    }
}
impl std::error::Error for PasteImageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedImageFormat {
    Png,
    Jpeg,
    Other,
}

impl EncodedImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            EncodedImageFormat::Png => "PNG",
            EncodedImageFormat::Jpeg => "JPEG",
            EncodedImageFormat::Other => "IMG",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PastedImageInfo {
    pub width: u32,
    pub height: u32,
    pub encoded_format: EncodedImageFormat, // Always PNG for now.
}

pub(crate) const SPINE_FEEDBACK_MAX_SCREENSHOTS: usize = 3;
pub(crate) const SPINE_FEEDBACK_MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;
pub(crate) const SPINE_FEEDBACK_MAX_TOTAL_SCREENSHOT_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const SPINE_FEEDBACK_MAX_SIDE: u32 = 8192;
pub(crate) const SPINE_FEEDBACK_MAX_PIXELS: u64 = 16_000_000;

const SPINE_FEEDBACK_MAX_SOURCE_BYTES: u64 = 20 * 1024 * 1024;
const SPINE_FEEDBACK_MAX_DECODE_ALLOC_BYTES: u64 = SPINE_FEEDBACK_MAX_PIXELS * 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedFeedbackScreenshot {
    pub(crate) png: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

struct CappedVecWriter {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl CappedVecWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_exceeded: false,
        }
    }
}

impl Write for CappedVecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.limit_exceeded = true;
            return Err(std::io::Error::other(
                "prepared feedback screenshot exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn feedback_decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(SPINE_FEEDBACK_MAX_SIDE);
    limits.max_image_height = Some(SPINE_FEEDBACK_MAX_SIDE);
    limits.max_alloc = Some(SPINE_FEEDBACK_MAX_DECODE_ALLOC_BYTES);
    limits
}

fn validate_feedback_dimensions(width: u32, height: u32) -> Result<(), PasteImageError> {
    if width == 0 || height == 0 {
        return Err(PasteImageError::InvalidImage(
            "screenshot dimensions must be non-zero".to_string(),
        ));
    }
    if width > SPINE_FEEDBACK_MAX_SIDE || height > SPINE_FEEDBACK_MAX_SIDE {
        return Err(PasteImageError::InvalidImage(format!(
            "screenshot dimensions must not exceed {SPINE_FEEDBACK_MAX_SIDE} pixels per side"
        )));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > SPINE_FEEDBACK_MAX_PIXELS {
        return Err(PasteImageError::InvalidImage(format!(
            "screenshot must not exceed {SPINE_FEEDBACK_MAX_PIXELS} pixels"
        )));
    }
    Ok(())
}

fn validate_feedback_total_bytes(
    existing_total_bytes: usize,
    prepared_bytes: usize,
) -> Result<(), PasteImageError> {
    let Some(total) = existing_total_bytes.checked_add(prepared_bytes) else {
        return Err(PasteImageError::InvalidImage(
            "total screenshot size exceeds the allowed limit".to_string(),
        ));
    };
    if total > SPINE_FEEDBACK_MAX_TOTAL_SCREENSHOT_BYTES {
        return Err(PasteImageError::InvalidImage(format!(
            "screenshots must not exceed {} MiB in total",
            SPINE_FEEDBACK_MAX_TOTAL_SCREENSHOT_BYTES / (1024 * 1024)
        )));
    }
    Ok(())
}

fn encode_feedback_rgba(
    rgba: &image::RgbaImage,
    existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    let (width, height) = rgba.dimensions();
    validate_feedback_dimensions(width, height)?;
    validate_feedback_total_bytes(existing_total_bytes, 0)?;

    let mut output = CappedVecWriter::new(SPINE_FEEDBACK_MAX_SCREENSHOT_BYTES);
    let encode_result = PngEncoder::new(&mut output).write_image(
        rgba.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8,
    );
    if output.limit_exceeded {
        return Err(PasteImageError::InvalidImage(format!(
            "a screenshot must not exceed {} MiB after PNG encoding",
            SPINE_FEEDBACK_MAX_SCREENSHOT_BYTES / (1024 * 1024)
        )));
    }
    encode_result.map_err(|err| PasteImageError::EncodeFailed(err.to_string()))?;
    validate_feedback_total_bytes(existing_total_bytes, output.bytes.len())?;

    Ok(PreparedFeedbackScreenshot {
        png: output.bytes,
        width,
        height,
    })
}

fn prepare_feedback_dynamic_image(
    mut image: DynamicImage,
    orientation: image::metadata::Orientation,
    existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    image.apply_orientation(orientation);
    validate_feedback_dimensions(image.width(), image.height())?;
    encode_feedback_rgba(&image.into_rgba8(), existing_total_bytes)
}

fn prepare_feedback_rgba(
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    validate_feedback_dimensions(width, height)?;
    let expected_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            PasteImageError::InvalidImage(
                "clipboard image dimensions overflow the byte length".to_string(),
            )
        })?;
    if bytes.len() != expected_bytes {
        return Err(PasteImageError::InvalidImage(
            "clipboard image RGBA byte length does not match its dimensions".to_string(),
        ));
    }
    let rgba = image::RgbaImage::from_raw(width, height, bytes).ok_or_else(|| {
        PasteImageError::InvalidImage("clipboard image has an invalid RGBA buffer".to_string())
    })?;
    encode_feedback_rgba(&rgba, existing_total_bytes)
}

fn image_reader_for_feedback_path(
    path: &Path,
) -> Result<ImageReader<Cursor<Vec<u8>>>, PasteImageError> {
    let path_metadata =
        std::fs::metadata(path).map_err(|err| PasteImageError::IoError(err.to_string()))?;
    validate_feedback_source_metadata(&path_metadata)?;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Avoid blocking if the path is replaced with a FIFO between the
        // path-level check above and opening the file descriptor.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|err| PasteImageError::IoError(err.to_string()))?;
    // Validate the object that was actually opened, then capture exactly that
    // descriptor length. Decoders see an immutable bounded snapshot even if
    // another process appends to the source after this metadata read.
    let opened_metadata = file
        .metadata()
        .map_err(|err| PasteImageError::IoError(err.to_string()))?;
    validate_feedback_source_metadata(&opened_metadata)?;
    let snapshot = read_feedback_source_snapshot(file, opened_metadata.len())?;
    ImageReader::new(Cursor::new(snapshot))
        .with_guessed_format()
        .map_err(|err| PasteImageError::InvalidImage(err.to_string()))
}

fn read_feedback_source_snapshot(
    mut file: File,
    captured_bytes: u64,
) -> Result<Vec<u8>, PasteImageError> {
    if captured_bytes > SPINE_FEEDBACK_MAX_SOURCE_BYTES {
        return Err(PasteImageError::InvalidImage(format!(
            "screenshot source file must not exceed {} MiB",
            SPINE_FEEDBACK_MAX_SOURCE_BYTES / (1024 * 1024)
        )));
    }
    let captured_bytes = usize::try_from(captured_bytes).map_err(|_| {
        PasteImageError::InvalidImage("screenshot source byte length is unsupported".to_string())
    })?;
    let mut snapshot = vec![0_u8; captured_bytes];
    file.read_exact(&mut snapshot)
        .map_err(|err| PasteImageError::IoError(err.to_string()))?;
    Ok(snapshot)
}

fn validate_feedback_source_metadata(metadata: &std::fs::Metadata) -> Result<(), PasteImageError> {
    if !metadata.is_file() {
        return Err(PasteImageError::InvalidImage(
            "screenshot path must point to a regular file".to_string(),
        ));
    }
    if metadata.len() > SPINE_FEEDBACK_MAX_SOURCE_BYTES {
        return Err(PasteImageError::InvalidImage(format!(
            "screenshot source file must not exceed {} MiB",
            SPINE_FEEDBACK_MAX_SOURCE_BYTES / (1024 * 1024)
        )));
    }
    Ok(())
}

/// Decode a local screenshot path under the feedback limits and return only
/// newly encoded PNG pixels. The returned value never contains the source path
/// or filename.
pub(crate) fn prepare_feedback_image_path(
    path: &Path,
    existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    validate_feedback_total_bytes(existing_total_bytes, 0)?;
    let mut reader = image_reader_for_feedback_path(path)?;
    let format = reader.format().ok_or_else(|| {
        PasteImageError::InvalidImage(
            "only PNG, JPEG, and static WebP screenshots are supported".to_string(),
        )
    })?;

    match format {
        ImageFormat::Png | ImageFormat::Jpeg => {
            // PNG can only accept limits while the decoder is constructed, so
            // install the strict dimensions and allocation budget before
            // parsing format-specific state. Other decoders are limited again
            // below after reserving their output buffer.
            reader.limits(feedback_decode_limits());
            let mut decoder = reader
                .into_decoder()
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            let (width, height) = decoder.dimensions();
            validate_feedback_dimensions(width, height)?;

            let mut remaining_limits = feedback_decode_limits();
            remaining_limits
                .reserve(decoder.total_bytes())
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            decoder
                .set_limits(remaining_limits)
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            let orientation = decoder
                .orientation()
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            let image = DynamicImage::from_decoder(decoder)
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            prepare_feedback_dynamic_image(image, orientation, existing_total_bytes)
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(reader.into_inner())
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            if decoder.has_animation() {
                return Err(PasteImageError::InvalidImage(
                    "animated WebP screenshots are not supported".to_string(),
                ));
            }
            let (width, height) = decoder.dimensions();
            validate_feedback_dimensions(width, height)?;

            let mut remaining_limits = feedback_decode_limits();
            remaining_limits
                .reserve(decoder.total_bytes())
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            decoder
                .set_limits(remaining_limits)
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            let orientation = decoder
                .orientation()
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            let image = DynamicImage::from_decoder(decoder)
                .map_err(|err| PasteImageError::InvalidImage(err.to_string()))?;
            prepare_feedback_dynamic_image(image, orientation, existing_total_bytes)
        }
        _ => Err(PasteImageError::InvalidImage(
            "only PNG, JPEG, and static WebP screenshots are supported".to_string(),
        )),
    }
}

/// Capture a clipboard screenshot and prepare it for the Spine feedback RPC.
///
/// Clipboard file lists and raw RGBA data both pass through the same fixed
/// dimension, pixel, PNG byte, and cumulative byte limits.
#[cfg(not(target_os = "android"))]
pub(crate) fn paste_feedback_image_as_png(
    existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    validate_feedback_total_bytes(existing_total_bytes, 0)?;
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|err| PasteImageError::ClipboardUnavailable(err.to_string()))?;

    let mut first_path_error = None;
    if let Ok(files) = clipboard.get().file_list() {
        for path in files {
            match prepare_feedback_image_path(&path, existing_total_bytes) {
                Ok(prepared) => return Ok(prepared),
                Err(err) => {
                    first_path_error.get_or_insert(err);
                }
            }
        }
    }

    let raw = match clipboard.get_image() {
        Ok(raw) => raw,
        Err(err) => {
            return Err(
                first_path_error.unwrap_or_else(|| PasteImageError::NoImage(err.to_string()))
            );
        }
    };
    let width = u32::try_from(raw.width).map_err(|_| {
        PasteImageError::InvalidImage("clipboard image width is too large".to_string())
    })?;
    let height = u32::try_from(raw.height).map_err(|_| {
        PasteImageError::InvalidImage("clipboard image height is too large".to_string())
    })?;
    prepare_feedback_rgba(width, height, raw.bytes.into_owned(), existing_total_bytes)
}

#[cfg(target_os = "android")]
pub(crate) fn paste_feedback_image_as_png(
    _existing_total_bytes: usize,
) -> Result<PreparedFeedbackScreenshot, PasteImageError> {
    Err(PasteImageError::ClipboardUnavailable(
        "clipboard image paste is unsupported on Android".into(),
    ))
}

/// Capture image from system clipboard, encode to PNG, and return bytes + info.
#[cfg(not(target_os = "android"))]
pub fn paste_image_as_png() -> Result<(Vec<u8>, PastedImageInfo), PasteImageError> {
    let _span = tracing::debug_span!("paste_image_as_png").entered();
    tracing::debug!("attempting clipboard image read");
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()))?;
    // Sometimes images on the clipboard come as files (e.g. when copy/pasting from
    // Finder), sometimes they come as image data (e.g. when pasting from Chrome).
    // Accept both, and prefer files if both are present.
    let files = cb
        .get()
        .file_list()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()));
    let dyn_img = if let Some(img) = files
        .unwrap_or_default()
        .into_iter()
        .find_map(|f| image::open(f).ok())
    {
        tracing::debug!(
            "clipboard image opened from file: {}x{}",
            img.width(),
            img.height()
        );
        img
    } else {
        let _span = tracing::debug_span!("get_image").entered();
        let img = cb
            .get_image()
            .map_err(|e| PasteImageError::NoImage(e.to_string()))?;
        let w = img.width as u32;
        let h = img.height as u32;
        tracing::debug!("clipboard image opened from image: {}x{}", w, h);

        let Some(rgba_img) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return Err(PasteImageError::EncodeFailed("invalid RGBA buffer".into()));
        };

        image::DynamicImage::ImageRgba8(rgba_img)
    };

    let mut png: Vec<u8> = Vec::new();
    {
        let span =
            tracing::debug_span!("encode_image", byte_length = tracing::field::Empty).entered();
        let mut cursor = std::io::Cursor::new(&mut png);
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
        span.record("byte_length", png.len());
    }

    Ok((
        png,
        PastedImageInfo {
            width: dyn_img.width(),
            height: dyn_img.height(),
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

/// Android/Termux does not support arboard; return a clear error.
#[cfg(target_os = "android")]
pub fn paste_image_as_png() -> Result<(Vec<u8>, PastedImageInfo), PasteImageError> {
    Err(PasteImageError::ClipboardUnavailable(
        "clipboard image paste is unsupported on Android".into(),
    ))
}

/// Convenience: write to a temp file and return its path + info.
#[cfg(not(target_os = "android"))]
pub fn paste_image_to_temp_png() -> Result<(PathBuf, PastedImageInfo), PasteImageError> {
    // First attempt: read image from system clipboard via arboard (native paths or image data).
    match paste_image_as_png() {
        Ok((png, info)) => {
            // Create a unique temporary file with a .png suffix to avoid collisions.
            let tmp = Builder::new()
                .prefix("codex-clipboard-")
                .suffix(".png")
                .tempfile()
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            std::fs::write(tmp.path(), &png)
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            // Persist the file (so it remains after the handle is dropped) and return its PathBuf.
            let (_file, path) = tmp
                .keep()
                .map_err(|e| PasteImageError::IoError(e.error.to_string()))?;
            Ok((path, info))
        }
        Err(e) => {
            #[cfg(target_os = "linux")]
            {
                try_wsl_clipboard_fallback(&e).or(Err(e))
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(e)
            }
        }
    }
}

/// Attempt WSL fallback for clipboard image paste.
///
/// If clipboard is unavailable (common under WSL because arboard cannot access
/// the Windows clipboard), attempt a WSL fallback that calls PowerShell on the
/// Windows side to write the clipboard image to a temporary file, then return
/// the corresponding WSL path.
#[cfg(target_os = "linux")]
fn try_wsl_clipboard_fallback(
    error: &PasteImageError,
) -> Result<(PathBuf, PastedImageInfo), PasteImageError> {
    use PasteImageError::ClipboardUnavailable;
    use PasteImageError::NoImage;

    if !is_probably_wsl() || !matches!(error, ClipboardUnavailable(_) | NoImage(_)) {
        return Err(error.clone());
    }

    tracing::debug!("attempting Windows PowerShell clipboard fallback");
    let Some(win_path) = try_dump_windows_clipboard_image() else {
        return Err(error.clone());
    };

    tracing::debug!("powershell produced path: {}", win_path);
    let Some(mapped_path) = convert_windows_path_to_wsl(&win_path) else {
        return Err(error.clone());
    };

    let Ok((w, h)) = image::image_dimensions(&mapped_path) else {
        return Err(error.clone());
    };

    // Return the mapped path directly without copying.
    // The file will be read and base64-encoded during serialization.
    Ok((
        mapped_path,
        PastedImageInfo {
            width: w,
            height: h,
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

/// Try to call a Windows PowerShell command (several common names) to save the
/// clipboard image to a temporary PNG and return the Windows path to that file.
/// Returns None if no command succeeded or no image was present.
#[cfg(target_os = "linux")]
fn try_dump_windows_clipboard_image() -> Option<String> {
    // Powershell script: save image from clipboard to a temp png and print the path.
    // Force UTF-8 output to avoid encoding issues between powershell.exe (UTF-16LE default)
    // and pwsh (UTF-8 default).
    let script = r#"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $img = Get-Clipboard -Format Image; if ($img -ne $null) { $p=[System.IO.Path]::GetTempFileName(); $p = [System.IO.Path]::ChangeExtension($p,'png'); $img.Save($p,[System.Drawing.Imaging.ImageFormat]::Png); Write-Output $p } else { exit 1 }"#;

    for cmd in ["powershell.exe", "pwsh", "powershell"] {
        match std::process::Command::new(cmd)
            .args(["-NoProfile", "-Command", script])
            .output()
        {
            // Executing PowerShell command
            Ok(output) => {
                if output.status.success() {
                    // Decode as UTF-8 (forced by the script above).
                    let win_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !win_path.is_empty() {
                        tracing::debug!("{} saved clipboard image to {}", cmd, win_path);
                        return Some(win_path);
                    }
                } else {
                    tracing::debug!("{} returned non-zero status", cmd);
                }
            }
            Err(err) => {
                tracing::debug!("{} not executable: {}", cmd, err);
            }
        }
    }
    None
}

#[cfg(target_os = "android")]
pub fn paste_image_to_temp_png() -> Result<(PathBuf, PastedImageInfo), PasteImageError> {
    // Keep error consistent with paste_image_as_png.
    Err(PasteImageError::ClipboardUnavailable(
        "clipboard image paste is unsupported on Android".into(),
    ))
}

/// Normalize pasted text for a single-line search query.
pub(crate) fn normalize_pasted_search_query(pasted: &str) -> Option<String> {
    let normalized = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

/// Normalize pasted text that may represent a filesystem path.
///
/// Supports:
/// - `file://` URLs (converted to local paths)
/// - Windows/UNC paths
/// - shell-escaped single paths (via `shlex`)
pub fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| pasted.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(pasted);

    // file:// URL → filesystem path
    if let Ok(url) = url::Url::parse(unquoted)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }

    // TODO: We'll improve the implementation/unit tests over time, as appropriate.
    // Possibly use typed-path: https://github.com/openai/codex/pull/2567/commits/3cc92b78e0a1f94e857cf4674d3a9db918ed352e
    //
    // Detect unquoted Windows paths and bypass POSIX shlex which
    // treats backslashes as escapes (e.g., C:\Users\Alice\file.png).
    // Also handles UNC paths (\\server\share\path).
    if let Some(path) = normalize_windows_path(unquoted) {
        return Some(path);
    }

    // shell-escaped single path → unescaped
    let parts: Vec<String> = shlex::Shlex::new(pasted).collect();
    if parts.len() == 1 {
        let part = parts.into_iter().next()?;
        if let Some(path) = normalize_windows_path(&part) {
            return Some(path);
        }
        return Some(PathBuf::from(part));
    }

    None
}

#[cfg(target_os = "linux")]
pub(crate) fn is_probably_wsl() -> bool {
    // Primary: Check /proc/version for "microsoft" or "WSL" (most reliable for standard WSL).
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let version_lower = version.to_lowercase();
        if version_lower.contains("microsoft") || version_lower.contains("wsl") {
            return true;
        }
    }

    // Fallback: Check WSL environment variables. This handles edge cases like
    // custom Linux kernels installed in WSL where /proc/version may not contain
    // "microsoft" or "WSL".
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some()
}

#[cfg(target_os = "linux")]
fn convert_windows_path_to_wsl(input: &str) -> Option<PathBuf> {
    if input.starts_with("\\\\") {
        return None;
    }

    let drive_letter = input.chars().next()?.to_ascii_lowercase();
    if !drive_letter.is_ascii_lowercase() {
        return None;
    }

    if input.get(1..2) != Some(":") {
        return None;
    }

    let mut result = PathBuf::from(format!("/mnt/{drive_letter}"));
    for component in input
        .get(2..)?
        .trim_start_matches(['\\', '/'])
        .split(['\\', '/'])
        .filter(|component| !component.is_empty())
    {
        result.push(component);
    }

    Some(result)
}

fn normalize_windows_path(input: &str) -> Option<PathBuf> {
    // Drive letter path: C:\ or C:/
    let drive = input
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
        && input.get(1..2) == Some(":")
        && input
            .get(2..3)
            .map(|s| s == "\\" || s == "/")
            .unwrap_or(false);
    // UNC path: \\server\share
    let unc = input.starts_with("\\\\");
    if !drive && !unc {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            return Some(converted);
        }
    }

    Some(PathBuf::from(input))
}

/// Infer an image format for the provided path based on its extension.
pub fn pasted_image_format(path: &Path) -> EncodedImageFormat {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => EncodedImageFormat::Png,
        Some("jpg") | Some("jpeg") => EncodedImageFormat::Jpeg,
        _ => EncodedImageFormat::Other,
    }
}

#[cfg(test)]
mod spine_feedback_image_tests {
    use super::*;
    use std::fs::OpenOptions;
    use tempfile::NamedTempFile;

    const ANIMATED_WEBP: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x84, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x58, 0x0a, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x41, 0x4e, 0x49, 0x4d, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41,
        0x4e, 0x4d, 0x46, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x64, 0x00, 0x00, 0x02, 0x56, 0x50, 0x38, 0x4c, 0x0f, 0x00, 0x00,
        0x00, 0x2f, 0x01, 0x40, 0x00, 0x00, 0x07, 0x10, 0xfd, 0x8f, 0xfe, 0x07, 0x22, 0xa2, 0xff,
        0x01, 0x00, 0x41, 0x4e, 0x4d, 0x46, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x56, 0x50, 0x38, 0x4c,
        0x0f, 0x00, 0x00, 0x00, 0x2f, 0x01, 0x40, 0x00, 0x00, 0x07, 0xd0, 0xff, 0x88, 0xfe, 0x07,
        0x22, 0xa2, 0xff, 0x01, 0x00,
    ];

    fn fixture_image() -> image::RgbaImage {
        image::RgbaImage::from_raw(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 192, 0, 0, 255, 128, 255, 255, 255, 64,
            ],
        )
        .expect("valid fixture pixels")
    }

    fn encode_fixture(format: ImageFormat) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(fixture_image())
            .write_to(&mut cursor, format)
            .expect("fixture encoding should succeed");
        cursor.into_inner()
    }

    fn write_temp(bytes: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temporary image");
        file.write_all(bytes).expect("write temporary image");
        file.flush().expect("flush temporary image");
        file
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn rewrite_png_dimensions(mut png: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        assert_eq!(&png[12..16], b"IHDR");
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        png
    }

    fn inject_png_text_chunk(png: &[u8], text: &[u8]) -> Vec<u8> {
        let mut offset = 8usize;
        let mut iend_offset = None;
        while offset + 12 <= png.len() {
            let length = u32::from_be_bytes(
                png[offset..offset + 4]
                    .try_into()
                    .expect("PNG chunk length"),
            ) as usize;
            if &png[offset + 4..offset + 8] == b"IEND" {
                iend_offset = Some(offset);
                break;
            }
            offset += 12 + length;
        }
        let iend_offset = iend_offset.expect("fixture has IEND");
        let mut chunk = Vec::with_capacity(12 + text.len());
        chunk.extend_from_slice(&(text.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(text);
        let mut crc_input = Vec::with_capacity(4 + text.len());
        crc_input.extend_from_slice(b"tEXt");
        crc_input.extend_from_slice(text);
        chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());

        let mut result = Vec::with_capacity(png.len() + chunk.len());
        result.extend_from_slice(&png[..iend_offset]);
        result.extend_from_slice(&chunk);
        result.extend_from_slice(&png[iend_offset..]);
        result
    }

    fn png_chunk_names(png: &[u8]) -> Vec<[u8; 4]> {
        let mut result = Vec::new();
        let mut offset = 8usize;
        while offset + 12 <= png.len() {
            let length = u32::from_be_bytes(
                png[offset..offset + 4]
                    .try_into()
                    .expect("PNG chunk length"),
            ) as usize;
            let name: [u8; 4] = png[offset + 4..offset + 8]
                .try_into()
                .expect("PNG chunk name");
            result.push(name);
            offset += 12 + length;
            if name == *b"IEND" {
                break;
            }
        }
        result
    }

    #[test]
    fn spine_feedback_accepts_png_jpeg_and_static_webp_as_fresh_png() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let file = write_temp(&encode_fixture(format));
            let prepared =
                prepare_feedback_image_path(file.path(), 0).expect("supported screenshot");
            assert_eq!((prepared.width, prepared.height), (2, 2));
            assert!(prepared.png.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert!(prepared.png.len() <= SPINE_FEEDBACK_MAX_SCREENSHOT_BYTES);
            let decoded = image::load_from_memory_with_format(&prepared.png, ImageFormat::Png)
                .expect("prepared PNG decodes");
            assert_eq!((decoded.width(), decoded.height()), (2, 2));
        }
    }

    #[test]
    fn spine_feedback_rejects_gif_animated_webp_and_unknown_bytes() {
        for (bytes, expected) in [
            (
                encode_fixture(ImageFormat::Gif),
                "only PNG, JPEG, and static WebP",
            ),
            (ANIMATED_WEBP.to_vec(), "animated WebP"),
            (b"not an image".to_vec(), "only PNG, JPEG, and static WebP"),
        ] {
            let file = write_temp(&bytes);
            let error = prepare_feedback_image_path(file.path(), 0)
                .expect_err("unsupported screenshot must fail")
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn spine_feedback_rejects_zero_side_and_pixel_limit_before_decode() {
        let valid_png = encode_fixture(ImageFormat::Png);
        for (width, height, expected) in [
            (0, 1, None),
            (SPINE_FEEDBACK_MAX_SIDE + 1, 1, None),
            (SPINE_FEEDBACK_MAX_SIDE, 1954, Some("16000000 pixels")),
        ] {
            let file = write_temp(&rewrite_png_dimensions(valid_png.clone(), width, height));
            let error = prepare_feedback_image_path(file.path(), 0)
                .expect_err("invalid dimensions must fail")
                .to_string();
            if let Some(expected) = expected {
                assert!(error.contains(expected), "{error}");
            }
        }
    }

    #[test]
    fn spine_feedback_enforces_png_and_cumulative_byte_limits() {
        let small =
            prepare_feedback_rgba(2, 2, fixture_image().into_raw(), 0).expect("small screenshot");
        prepare_feedback_rgba(
            2,
            2,
            fixture_image().into_raw(),
            SPINE_FEEDBACK_MAX_TOTAL_SCREENSHOT_BYTES - small.png.len(),
        )
        .expect("exact cumulative boundary is allowed");
        let cumulative_error = prepare_feedback_rgba(
            2,
            2,
            fixture_image().into_raw(),
            SPINE_FEEDBACK_MAX_TOTAL_SCREENSHOT_BYTES - small.png.len() + 1,
        )
        .expect_err("cumulative overflow must fail")
        .to_string();
        assert!(cumulative_error.contains("in total"), "{cumulative_error}");

        let width = 1280u32;
        let height = 1280u32;
        let byte_len = width as usize * height as usize * 4;
        let mut state = 0x1234_5678u32;
        let mut noisy_rgba = Vec::with_capacity(byte_len);
        for _ in 0..byte_len {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            noisy_rgba.push(state as u8);
        }
        let per_image_error = prepare_feedback_rgba(width, height, noisy_rgba, 0)
            .expect_err("oversized encoded PNG must fail")
            .to_string();
        assert!(
            per_image_error.contains("after PNG encoding"),
            "{per_image_error}"
        );
    }

    #[test]
    fn spine_feedback_reencoding_strips_png_text_metadata_and_preserves_pixels() {
        let original_pixels = fixture_image();
        let source_png =
            inject_png_text_chunk(&encode_fixture(ImageFormat::Png), b"secret\0do-not-upload");
        assert!(png_chunk_names(&source_png).contains(b"tEXt"));
        let file = write_temp(&source_png);

        let prepared = prepare_feedback_image_path(file.path(), 0).expect("prepare screenshot");
        assert!(!png_chunk_names(&prepared.png).contains(b"tEXt"));
        let decoded = image::load_from_memory_with_format(&prepared.png, ImageFormat::Png)
            .expect("prepared PNG decodes")
            .into_rgba8();
        assert_eq!(decoded, original_pixels);
    }

    #[test]
    fn spine_feedback_rejects_malformed_raw_rgba() {
        let error = prepare_feedback_rgba(2, 2, vec![0; 15], 0)
            .expect_err("short RGBA buffer must fail")
            .to_string();
        assert!(error.contains("byte length"), "{error}");

        let error = prepare_feedback_rgba(0, 2, Vec::new(), 0)
            .expect_err("zero-width RGBA must fail")
            .to_string();
        assert!(error.contains("non-zero"), "{error}");
    }

    #[test]
    fn spine_feedback_rejects_non_regular_paths() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let error = prepare_feedback_image_path(directory.path(), 0)
            .expect_err("directories are not screenshot files")
            .to_string();
        assert!(error.contains("regular file"), "{error}");
    }

    #[test]
    fn spine_feedback_rejects_oversized_encoded_sources_before_decode() {
        let file = write_temp(&encode_fixture(ImageFormat::Png));
        file.as_file()
            .set_len(SPINE_FEEDBACK_MAX_SOURCE_BYTES + 1)
            .expect("extend source fixture");

        let error = prepare_feedback_image_path(file.path(), 0)
            .expect_err("oversized encoded source must fail")
            .to_string();

        assert!(error.contains("source file"), "{error}");
        assert!(error.contains("20 MiB"), "{error}");
    }

    #[test]
    fn spine_feedback_source_snapshot_excludes_bytes_appended_after_capture() {
        let source = encode_fixture(ImageFormat::Jpeg);
        let mut file = write_temp(&source);
        let captured_bytes = file.as_file().metadata().expect("source metadata").len();
        file.as_file_mut()
            .write_all(&vec![0x5a; 1024 * 1024])
            .expect("append after captured boundary");
        file.as_file_mut().flush().expect("flush appended bytes");

        let reader = OpenOptions::new()
            .read(true)
            .open(file.path())
            .expect("reopen appended source");
        let snapshot = read_feedback_source_snapshot(reader, captured_bytes)
            .expect("capture bounded source snapshot");

        assert_eq!(snapshot, source);
        assert_eq!(snapshot.len() as u64, captured_bytes);
    }
}

#[cfg(test)]
mod pasted_search_query_tests {
    use super::*;

    #[test]
    fn collapses_whitespace() {
        assert_eq!(
            normalize_pasted_search_query("  alpha\n\tbeta\r\n gamma  "),
            Some(String::from("alpha beta gamma"))
        );
    }
}

#[cfg(test)]
mod pasted_paths_tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn normalize_file_url() {
        let input = "file:///tmp/example.png";
        let result = normalize_pasted_path(input).expect("should parse file URL");
        assert_eq!(result, PathBuf::from("/tmp/example.png"));
    }

    #[test]
    fn normalize_file_url_windows() {
        let input = r"C:\Temp\example.png";
        let result = normalize_pasted_path(input).expect("should parse file URL");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            converted
        } else {
            PathBuf::from(r"C:\Temp\example.png")
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(r"C:\Temp\example.png");
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_shell_escaped_single_path() {
        let input = "/home/user/My\\ File.png";
        let result = normalize_pasted_path(input).expect("should unescape shell-escaped path");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_simple_quoted_path_fallback() {
        let input = "\"/home/user/My File.png\"";
        let result = normalize_pasted_path(input).expect("should trim simple quotes");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_single_quoted_unix_path() {
        let input = "'/home/user/My File.png'";
        let result = normalize_pasted_path(input).expect("should trim single quotes via shlex");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_multiple_tokens_returns_none() {
        // Two tokens after shell splitting → not a single path
        let input = "/home/user/a\\ b.png /home/user/c.png";
        let result = normalize_pasted_path(input);
        assert!(result.is_none());
    }

    #[test]
    fn pasted_image_format_png_jpeg_unknown() {
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.PNG")),
            EncodedImageFormat::Png
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.jpg")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.JPEG")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c")),
            EncodedImageFormat::Other
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.webp")),
            EncodedImageFormat::Other
        );
    }

    #[test]
    fn normalize_single_quoted_windows_path() {
        let input = r"'C:\\Users\\Alice\\My File.jpeg'";
        let unquoted = r"C:\\Users\\Alice\\My File.jpeg";
        let result =
            normalize_pasted_path(input).expect("should trim single quotes on windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(unquoted)
        {
            converted
        } else {
            PathBuf::from(unquoted)
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(unquoted);
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_double_quoted_windows_path() {
        let input = r#""C:\\Users\\Alice\\My File.jpeg""#;
        let unquoted = r"C:\\Users\\Alice\\My File.jpeg";
        let result =
            normalize_pasted_path(input).expect("should trim double quotes on windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(unquoted)
        {
            converted
        } else {
            PathBuf::from(unquoted)
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(unquoted);
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_unquoted_windows_path_with_spaces() {
        let input = r"C:\\Users\\Alice\\My Pictures\\example image.png";
        let result = normalize_pasted_path(input).expect("should accept unquoted windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            converted
        } else {
            PathBuf::from(r"C:\\Users\\Alice\\My Pictures\\example image.png")
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(r"C:\\Users\\Alice\\My Pictures\\example image.png");
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_unc_windows_path() {
        let input = r"\\\\server\\share\\folder\\file.jpg";
        let result = normalize_pasted_path(input).expect("should accept UNC windows path");
        assert_eq!(
            result,
            PathBuf::from(r"\\\\server\\share\\folder\\file.jpg")
        );
    }

    #[test]
    fn pasted_image_format_with_windows_style_paths() {
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\c.PNG")),
            EncodedImageFormat::Png
        );
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\c.jpeg")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\noext")),
            EncodedImageFormat::Other
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn normalize_windows_path_in_wsl() {
        // This test only runs on actual WSL systems
        if !is_probably_wsl() {
            // Skip test if not on WSL
            return;
        }
        let input = r"C:\\Users\\Alice\\Pictures\\example image.png";
        let result = normalize_pasted_path(input).expect("should convert windows path on wsl");
        assert_eq!(
            result,
            PathBuf::from("/mnt/c/Users/Alice/Pictures/example image.png")
        );
    }
}
