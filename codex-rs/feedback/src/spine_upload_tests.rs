use super::*;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::thread;
use std::time::Duration;
use std::time::Instant;

fn attachment(filename: &str, content_type: &str, len: usize) -> FeedbackAttachment {
    FeedbackAttachment {
        filename: filename.to_string(),
        content_type: Some(content_type.to_string()),
        buffer: vec![0; len],
    }
}

#[test]
fn validates_bounded_allowlisted_attachments() {
    let attachments = [
        attachment(
            SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME,
            ROLLOUT_DEBUG_CONTENT_TYPE,
            8,
        ),
        attachment("screenshot-1.png", SCREENSHOT_CONTENT_TYPE, 8),
    ];
    validate_attachments(&attachments).expect("allowlisted attachments should pass");

    let arbitrary = [attachment("thread-raw.log", "text/plain", 8)];
    assert!(validate_attachments(&arbitrary).is_err());
    let missing_rollout = [attachment("screenshot-1.png", SCREENSHOT_CONTENT_TYPE, 8)];
    assert!(validate_attachments(&missing_rollout).is_err());
}

#[test]
fn enforces_note_and_total_attachment_byte_limits() {
    let oversized_note = "x".repeat(SPINE_FEEDBACK_MAX_NOTE_BYTES + 1);
    assert!(validate_note(Some(&oversized_note)).is_err());

    let exact = [attachment(
        SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME,
        ROLLOUT_DEBUG_CONTENT_TYPE,
        SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES,
    )];
    validate_attachments(&exact).expect("documented attachment limit is inclusive");

    let oversized = [attachment(
        SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME,
        ROLLOUT_DEBUG_CONTENT_TYPE,
        SPINE_FEEDBACK_MAX_ATTACHMENT_BYTES + 1,
    )];
    assert!(validate_attachments(&oversized).is_err());
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).expect("read request");
        if read == 0 {
            break request.len();
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let content_length = request[..header_end]
        .split(|byte| *byte == b'\n')
        .find_map(|line| {
            let line = line.strip_suffix(b"\r")?;
            let colon = line.iter().position(|byte| *byte == b':')?;
            let (name, value) = line.split_at(colon);
            if name.eq_ignore_ascii_case(b"content-length") {
                std::str::from_utf8(value.get(1..)?)
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let read = stream.read(&mut chunk).expect("read request body");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    request
}

#[test]
fn transport_rejects_redirect_without_forwarding_the_envelope() {
    let destination = TcpListener::bind("127.0.0.1:0").expect("bind redirect destination");
    let destination_address = destination.local_addr().expect("destination address");
    destination
        .set_nonblocking(true)
        .expect("set destination nonblocking");
    let destination_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match destination.accept() {
                Ok((mut stream, _)) => return Some(read_request(&mut stream)),
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept redirect destination: {err}"),
            }
        }
    });

    let redirect = TcpListener::bind("127.0.0.1:0").expect("bind redirect server");
    let redirect_address = redirect.local_addr().expect("redirect address");
    let redirect_thread = thread::spawn(move || {
        let (mut stream, _) = redirect.accept().expect("accept request");
        let request = read_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 307 Redirect\r\nLocation: http://{destination_address}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .expect("write redirect response");
        request
    });

    let attachments = [attachment(
        SPINE_ROLLOUT_DEBUG_ATTACHMENT_FILENAME,
        ROLLOUT_DEBUG_CONTENT_TYPE,
        8,
    )];
    let dsn = format!("http://public-key@{redirect_address}/42");
    let result = upload_with_config(
        SpineFeedbackUpload {
            note: Some("do not redirect"),
            attachments: &attachments,
        },
        TransportConfig {
            dsn: &dsn,
            timeout: Duration::from_secs(1),
            disable_proxy: true,
        },
    );
    assert!(result.is_err());
    assert!(!redirect_thread.join().expect("redirect server").is_empty());
    assert_eq!(destination_thread.join().expect("destination server"), None);
}
