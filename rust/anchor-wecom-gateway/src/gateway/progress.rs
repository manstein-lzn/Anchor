//! Best-effort subscription to the Host's channel progress projection.
//!
//! A subscribed task reads the read-only status stream for one admitted event
//! and forwards each line to the transport. Nothing here is durable and nothing
//! here may influence delivery: a failed, refused, cancelled or slow progress
//! stream is simply dropped, and the final reply path is untouched.

use serde_json::Value;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

use super::ProgressLine;

/// Matches the Host's own stream limit so a stuck subscription cannot outlive
/// the turn it describes.
const STREAM_LIMIT: Duration = Duration::from_secs(300);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The transport and the Host admission request are independent, so the first
/// subscribe attempt may arrive before the event exists. A missing event is
/// retried for a short bounded window and then given up on.
const ADMISSION_ATTEMPTS: usize = 8;
const ADMISSION_INTERVAL: Duration = Duration::from_millis(250);
const MAX_LINES: usize = 200;
const MAX_CONTENT_CHARS: usize = 600;

/// Spawn the per-event progress subscription. Returns immediately; the task
/// owns its own lifecycle and ends on `settled`, on cancellation, on the
/// stream ending, or at the hard limit.
pub(super) fn subscribe(
    url: String,
    api_key: Option<String>,
    event_id: String,
    sender_id: String,
    conversation_id: String,
    sender: mpsc::UnboundedSender<ProgressLine>,
    mut cancel: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Cancelled (superseded or already settled): never open the stream.
        if *cancel.borrow() {
            return;
        }
        eprintln!("anchor-wecom-gateway: progress subscription started {event_id}");
        // Progress is an observation, never a delivery fact: every outcome of
        // this task is simply dropped.
        let mut attempts = 0;
        loop {
            let outcome = tokio::select! {
                _ = cancel.changed() => return,
                outcome = read(
                    url.clone(),
                    api_key.clone(),
                    event_id.clone(),
                    sender_id.clone(),
                    conversation_id.clone(),
                    sender.clone(),
                ) => outcome,
            };
            match outcome {
                Outcome::NotYetAdmitted if attempts < ADMISSION_ATTEMPTS => {
                    attempts += 1;
                    tokio::select! {
                        _ = cancel.changed() => return,
                        _ = tokio::time::sleep(ADMISSION_INTERVAL) => {}
                    }
                }
                Outcome::NotYetAdmitted => {
                    eprintln!("anchor-wecom-gateway: progress stream unavailable {event_id}");
                    return;
                }
                Outcome::Unavailable(reason) => {
                    eprintln!("anchor-wecom-gateway: progress stream failed {event_id}: {reason}");
                    return;
                }
                Outcome::Closed => return,
            }
        }
    })
}

enum Outcome {
    /// The stream ended on its own, or the turn settled.
    Closed,
    /// The endpoint answered, but this event is not admitted (yet).
    NotYetAdmitted,
    /// The endpoint could not be used at all, with the reason for the log.
    Unavailable(String),
}

async fn read(
    url: String,
    api_key: Option<String>,
    event_id: String,
    sender_id: String,
    conversation_id: String,
    sender: mpsc::UnboundedSender<ProgressLine>,
) -> Outcome {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(READ_TIMEOUT)
        .build()
    else {
        return Outcome::Unavailable("client unavailable".into());
    };
    let mut request = client.get(event_url(&url, &event_id));
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = match request.header("accept", "text/event-stream").send().await {
        Ok(response) => response,
        Err(error) => return Outcome::Unavailable(format!("request failed: {error}")),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Outcome::NotYetAdmitted;
    }
    if !response.status().is_success() {
        return Outcome::Unavailable(format!("status {}", response.status().as_u16()));
    }
    eprintln!("anchor-wecom-gateway: progress stream connected {event_id}");
    let started = tokio::time::Instant::now();
    let mut buffer = Vec::new();
    let mut lines = 0usize;
    let mut response = response;
    loop {
        if started.elapsed() >= STREAM_LIMIT || lines >= MAX_LINES {
            return Outcome::Closed;
        }
        let chunk = match tokio::time::timeout(READ_TIMEOUT, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => return Outcome::Closed,
            _ => return Outcome::Closed,
        };
        buffer.extend_from_slice(&chunk);
        while let Some(end) = find_event_end(&buffer) {
            let event = buffer.drain(..end).collect::<Vec<u8>>();
            while buffer
                .first()
                .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
            {
                buffer.remove(0);
            }
            let Ok(text) = String::from_utf8(event) else {
                continue;
            };
            let Some((kind, data)) = parse_event(&text) else {
                continue;
            };
            if kind == "error" {
                return Outcome::Closed;
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let settled = value["settled"].as_bool().unwrap_or(false) || kind == "settled";
            let content = value["content"].as_str().unwrap_or_default();
            if !settled && content.trim().is_empty() {
                continue;
            }
            // A Host that predates the category and step fields simply sends
            // neither; the transport still renders one line.
            let category = value["category"].as_str().map(str::to_owned);
            let step = value["step"]
                .as_u64()
                .and_then(|step| u32::try_from(step).ok());
            lines += 1;
            if sender
                .send(ProgressLine {
                    event_id: event_id.clone(),
                    sender_id: sender_id.clone(),
                    conversation_id: conversation_id.clone(),
                    content: content.chars().take(MAX_CONTENT_CHARS).collect(),
                    category,
                    step,
                    settled,
                })
                .is_err()
            {
                return Outcome::Closed;
            }
            if settled {
                return Outcome::Closed;
            }
        }
    }
}

/// The Host route is `/channels/{platform}/progress/{event_id}`; the configured
/// value is the base path, so the identity is appended as one encoded segment.
fn event_url(base: &str, event_id: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        encode_segment(event_id)
    )
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn find_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| index + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
        })
}

fn parse_event(text: &str) -> Option<(String, String)> {
    let mut kind = "message".to_owned();
    let mut data = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            kind = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if data.is_empty() {
        return None;
    }
    Some((kind, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_frames_are_parsed_without_trusting_their_shape() {
        assert_eq!(
            parse_event("event: update\ndata: {\"seq\":1}"),
            Some(("update".to_owned(), "{\"seq\":1}".to_owned()))
        );
        assert_eq!(
            parse_event("data: {\"settled\":true}"),
            Some(("message".to_owned(), "{\"settled\":true}".to_owned()))
        );
        assert_eq!(parse_event(": keep-alive"), None);
        assert_eq!(parse_event("event: settled\n"), None);
        assert_eq!(find_event_end(b"data: {}\n\nrest"), Some(10));
        assert_eq!(find_event_end(b"data: {}\r\n\r\nrest"), Some(12));
        assert_eq!(find_event_end(b"data: {}"), None);
        assert_eq!(
            event_url("http://127.0.0.1:8077/channels/wecom/progress/", "abc/def"),
            "http://127.0.0.1:8077/channels/wecom/progress/abc%2Fdef"
        );
        assert_eq!(encode_segment("msg-1_2.3~4"), "msg-1_2.3~4");
    }

    #[test]
    fn a_status_frame_carries_its_optional_category_and_step() {
        let framed = |data: &str| serde_json::from_str::<Value>(data).unwrap();
        let formatted = framed(
            r#"{"seq":2,"kind":"status","content":"正在查看文件","category":"read_file","step":2}"#,
        );
        assert_eq!(formatted["category"], "read_file");
        assert_eq!(formatted["step"], 2);
        assert_eq!(
            formatted["step"]
                .as_u64()
                .and_then(|s| u32::try_from(s).ok()),
            Some(2)
        );
        // A Host that predates the fields sends neither, and that is not an
        // error: the line still renders.
        let legacy = framed(r#"{"seq":1,"kind":"status","content":"正在查看文件"}"#);
        assert!(legacy["category"].as_str().is_none());
        assert!(
            legacy["step"]
                .as_u64()
                .and_then(|s| u32::try_from(s).ok())
                .is_none()
        );
        // A hostile or malformed step must not become a marker.
        let hostile = framed(r#"{"content":"x","category":7,"step":"two"}"#);
        assert!(hostile["category"].as_str().is_none());
        assert!(hostile["step"].as_u64().is_none());
    }
}
