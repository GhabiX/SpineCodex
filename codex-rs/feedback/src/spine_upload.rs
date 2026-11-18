use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_install_context::distribution::PRODUCT_NAME;
use codex_install_context::distribution::SPINE_FEEDBACK_SENTRY_DSN;
use reqwest::header::CONTENT_TYPE;
use sentry::protocol::Attachment;
use sentry::protocol::AttachmentType;
use sentry::protocol::Envelope;
use sentry::protocol::EnvelopeItem;
use sentry::protocol::Event;
use sentry::protocol::Level;
use sentry::types::Dsn;

use crate::FeedbackAttachment;

pub const SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME: &str = "rollout-debug.jsonl.gz";
pub const SPINE_FEEDBACK_MAX_NOTE_BYTES: usize = 8 * 1024;
pub const SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(10);
const ENVELOPE_OVERHEAD_BYTES: usize = 64 * 1024;
const EVENT_MESSAGE: &str = "SpineCodex feedback";
const LOGGER: &str = "spinecodex.feedback";
const ENVELOPE_CONTENT_TYPE: &str = "application/x-sentry-envelope";
const ROLLOUT_DEBUG_CONTENT_TYPE: &str = "application/gzip";
const SCREENSHOT_CONTENT_TYPE: &str = "image/png";
const SCREENSHOT_FILENAMES: [&str; 3] =
    ["screenshot-1.png", "screenshot-2.png", "screenshot-3.png"];

pub struct SpineFeedbackUpload<'a> {
    pub note: Option<&'a str>,
    pub attachments: &'a [FeedbackAttachment],
}

struct TransportConfig<'a> {
    dsn: &'a str,
    timeout: Duration,
    disable_proxy: bool,
}

pub fn upload_spine_feedback(options: SpineFeedbackUpload<'_>) -> Result<String> {
    upload_with_config(
        options,
        TransportConfig {
            dsn: SPINE_FEEDBACK_SENTRY_DSN,
            timeout: UPLOAD_TIMEOUT,
            disable_proxy: false,
        },
    )
}

fn upload_with_config(
    options: SpineFeedbackUpload<'_>,
    config: TransportConfig<'_>,
) -> Result<String> {
    validate_note(options.note)?;
    validate_attachments(options.attachments)?;

    let dsn = Dsn::from_str(config.dsn).context("invalid Spine feedback DSN")?;
    let mut event = Event {
        level: Level::Info,
        message: Some(EVENT_MESSAGE.to_string()),
        logger: Some(LOGGER.to_string()),
        release: Some(Cow::Owned(format!(
            "{PRODUCT_NAME}@{}",
            env!("CARGO_PKG_VERSION")
        ))),
        tags: BTreeMap::from([
            (
                "cli_version".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
            (
                "feedback_kind".to_string(),
                "spine_rollout_debug".to_string(),
            ),
            ("product".to_string(), PRODUCT_NAME.to_string()),
        ]),
        ..Event::new()
    };
    if let Some(note) = options.note {
        event.extra.insert(
            "note".to_string(),
            serde_json::Value::String(note.to_string()),
        );
    }
    let event_id = event.event_id;

    let mut envelope = Envelope::new();
    envelope.add_item(EnvelopeItem::Event(event));
    for attachment in options.attachments {
        envelope.add_item(EnvelopeItem::Attachment(Attachment {
            buffer: attachment.buffer.clone(),
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            ty: Some(AttachmentType::Attachment),
        }));
    }

    let mut body = Vec::new();
    envelope
        .to_writer(&mut body)
        .context("serialize Spine feedback envelope")?;
    if body.len() > SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES + ENVELOPE_OVERHEAD_BYTES {
        bail!("Spine feedback envelope is too large: {} bytes", body.len());
    }

    let mut client_builder = reqwest::blocking::Client::builder()
        .timeout(config.timeout)
        .redirect(reqwest::redirect::Policy::none());
    if config.disable_proxy {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .context("build Spine feedback HTTP client")?;
    let auth = dsn
        .to_auth(Some(&format!(
            "{PRODUCT_NAME}/{}",
            env!("CARGO_PKG_VERSION")
        )))
        .to_string();
    let response = client
        .post(dsn.envelope_api_url())
        .header("X-Sentry-Auth", auth)
        .header(CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
        .body(body)
        .send()
        .context("submit Spine feedback envelope")?;
    let status = response.status();
    if !status.is_success() {
        bail!("Spine feedback ingest returned HTTP {status}");
    }

    Ok(event_id.simple().to_string())
}

fn validate_note(note: Option<&str>) -> Result<()> {
    if note.is_some_and(|note| note.len() > SPINE_FEEDBACK_MAX_NOTE_BYTES) {
        bail!("Spine feedback note exceeds {SPINE_FEEDBACK_MAX_NOTE_BYTES} bytes");
    }
    Ok(())
}

fn validate_attachments(attachments: &[FeedbackAttachment]) -> Result<()> {
    let mut rollout_seen = false;
    let mut screenshots_seen = [false; SCREENSHOT_FILENAMES.len()];
    let mut total_bytes = 0_usize;

    for attachment in attachments {
        total_bytes = total_bytes
            .checked_add(attachment.buffer.len())
            .context("Spine feedback attachment size overflow")?;
        if total_bytes > SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES {
            bail!("Spine feedback attachments exceed {SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES} bytes");
        }

        match attachment.filename.as_str() {
            SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME => {
                if rollout_seen {
                    bail!("duplicate rollout debug attachment");
                }
                require_content_type(attachment, ROLLOUT_DEBUG_CONTENT_TYPE)?;
                rollout_seen = true;
            }
            filename => {
                let Some(index) = SCREENSHOT_FILENAMES
                    .iter()
                    .position(|candidate| *candidate == filename)
                else {
                    bail!("unapproved Spine feedback attachment filename");
                };
                if screenshots_seen[index] {
                    bail!("duplicate Spine feedback screenshot attachment");
                }
                require_content_type(attachment, SCREENSHOT_CONTENT_TYPE)?;
                screenshots_seen[index] = true;
            }
        }
    }

    if !rollout_seen {
        bail!("missing rollout debug attachment");
    }
    Ok(())
}

fn require_content_type(attachment: &FeedbackAttachment, expected: &str) -> Result<()> {
    if attachment.content_type.as_deref() != Some(expected) {
        bail!(
            "invalid content type for Spine feedback attachment {}",
            attachment.filename
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "spine_upload_tests.rs"]
mod tests;
