//! The printer camera: an MJPEG stream on port 8080 that serves one viewer at a time.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::{Buf, Bytes, BytesMut};
use futures::StreamExt;
use reqwest::header::CONTENT_TYPE;
use thiserror::Error;
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{Instant, interval, sleep},
};

/// Buffered stream data without a complete frame beyond this means the stream is not the
/// MJPEG it claims to be; the buffer is dropped rather than grown forever.
const MAX_BUFFER: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CameraError {
    #[error("camera request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("camera answered HTTP {0}")]
    Status(reqwest::StatusCode),
    #[error("camera did not send multipart MJPEG (content type {0:?})")]
    NotMjpeg(String),
    #[error("camera stream ended before a complete frame")]
    Ended,
    #[error("no camera frame within {0:?}")]
    Timeout(Duration),
}

/// Splits a `multipart/x-mixed-replace` body into frames, fed in arbitrary chunks.
pub struct MjpegParser {
    delimiter: Vec<u8>,
    buf: BytesMut,
}

impl MjpegParser {
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        let boundary = content_type.split(';').find_map(|param| {
            let (key, value) = param.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("boundary")
                .then(|| value.trim().trim_matches('"'))
        })?;
        // The printer is documented as sending `boundary=--frame_boundary`. Whether its
        // delimiter lines then carry two leading dashes or four, `--frame_boundary` matches.
        let boundary = boundary.trim_start_matches('-');
        if boundary.is_empty() {
            return None;
        }
        Some(Self {
            delimiter: format!("--{boundary}").into_bytes(),
            buf: BytesMut::new(),
        })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(frame) = self.next_frame() {
            if !frame.is_empty() {
                frames.push(frame);
            }
        }
        if self.buf.len() > MAX_BUFFER {
            self.buf.clear();
        }
        frames
    }

    fn next_frame(&mut self) -> Option<Bytes> {
        let start = find(&self.buf, &self.delimiter)?;
        let headers_start = start + self.delimiter.len();
        let headers_len = find(&self.buf[headers_start..], b"\r\n\r\n")?;
        let body_start = headers_start + headers_len + 4;

        let body_end = match content_length(&self.buf[headers_start..headers_start + headers_len]) {
            Some(len) => {
                let end = body_start + len;
                if self.buf.len() < end {
                    return None;
                }
                end
            }
            None => {
                let mut end = body_start + find(&self.buf[body_start..], &self.delimiter)?;
                // With four-dash delimiter lines the match starts two dashes into the line.
                // A JPEG ends in FF D9, so trailing dashes can only belong to the delimiter.
                while end > body_start && self.buf[end - 1] == b'-' {
                    end -= 1;
                }
                if end >= body_start + 2 && self.buf[end - 2..end] == *b"\r\n" {
                    end -= 2;
                }
                end
            }
        };

        let frame = Bytes::copy_from_slice(&self.buf[body_start..body_end]);
        self.buf.advance(body_end);
        Some(frame)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length(headers: &[u8]) -> Option<usize> {
    std::str::from_utf8(headers).ok()?.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

pub fn is_jpeg(frame: &[u8]) -> bool {
    frame.starts_with(&[0xFF, 0xD8])
}

pub async fn grab_frame(
    http: &reqwest::Client,
    url: &str,
    limit: Duration,
) -> Result<Bytes, CameraError> {
    let grab = async {
        let response = http.get(url).send().await?;
        if !response.status().is_success() {
            return Err(CameraError::Status(response.status()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut parser = MjpegParser::from_content_type(&content_type)
            .ok_or(CameraError::NotMjpeg(content_type))?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if let Some(frame) = parser.push(&chunk?).into_iter().next() {
                return Ok(frame);
            }
        }
        Err(CameraError::Ended)
    };
    tokio::time::timeout(limit, grab)
        .await
        .map_err(|_| CameraError::Timeout(limit))?
}

/// How long the upstream connection outlives its last viewer, so a page reload does not cost
/// a reconnect to a camera that admits one client at a time.
pub const IDLE_GRACE: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Part boundary of the stream served to browsers.
pub const BOUNDARY: &str = "printhub-frame";

/// Holds the single upstream connection to the camera and fans its frames out to any number
/// of viewers. The upstream is opened by the first viewer and closed after the last leaves.
#[derive(Clone)]
pub struct CameraHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    http: reqwest::Client,
    url: String,
    idle_grace: Duration,
    frames: broadcast::Sender<Bytes>,
    latest: Mutex<Option<(Bytes, Instant)>>,
    last_error: Mutex<Option<String>>,
    viewers: AtomicUsize,
    upstream: Mutex<Option<JoinHandle<()>>>,
    upstream_connects: AtomicUsize,
}

pub struct Viewer {
    rx: broadcast::Receiver<Bytes>,
    _guard: ViewerGuard,
}

struct ViewerGuard(Arc<HubInner>);

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CameraHub {
    pub fn new(http: reqwest::Client, url: impl Into<String>, idle_grace: Duration) -> Self {
        // Viewers only ever want the newest frame; a lagging receiver skips ahead.
        let (frames, _) = broadcast::channel(2);
        Self {
            inner: Arc::new(HubInner {
                http,
                url: url.into(),
                idle_grace,
                frames,
                latest: Mutex::default(),
                last_error: Mutex::default(),
                viewers: AtomicUsize::new(0),
                upstream: Mutex::default(),
                upstream_connects: AtomicUsize::new(0),
            }),
        }
    }

    pub fn watch(&self) -> Viewer {
        self.inner.viewers.fetch_add(1, Ordering::SeqCst);
        let guard = ViewerGuard(Arc::clone(&self.inner));
        let rx = self.inner.frames.subscribe();
        let mut upstream = self.inner.upstream.lock().unwrap();
        if upstream.is_none() {
            *upstream = Some(tokio::spawn(run_upstream(Arc::clone(&self.inner))));
        }
        Viewer { rx, _guard: guard }
    }

    /// The latest frame if it is younger than `max_age`, otherwise the next one to arrive.
    pub async fn snapshot(&self, max_age: Duration, limit: Duration) -> Result<Bytes, CameraError> {
        let cached = self
            .inner
            .latest
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= max_age)
            .map(|(frame, _)| frame.clone());
        if let Some(frame) = cached {
            return Ok(frame);
        }
        let mut viewer = self.watch();
        match tokio::time::timeout(limit, viewer.next_frame()).await {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(CameraError::Ended),
            Err(_) => Err(CameraError::Timeout(limit)),
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner.last_error.lock().unwrap().clone()
    }

    pub fn viewers(&self) -> usize {
        self.inner.viewers.load(Ordering::SeqCst)
    }

    pub fn upstream_connects(&self) -> usize {
        self.inner.upstream_connects.load(Ordering::SeqCst)
    }
}

impl Viewer {
    pub async fn next_frame(&mut self) -> Option<Bytes> {
        loop {
            match self.rx.recv().await {
                Ok(frame) => return Some(frame),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// One part of the `multipart/x-mixed-replace` stream served to browsers.
pub fn multipart_part(frame: &[u8]) -> Bytes {
    let mut part = format!(
        "--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.len()
    )
    .into_bytes();
    part.extend_from_slice(frame);
    part.extend_from_slice(b"\r\n");
    Bytes::from(part)
}

async fn run_upstream(hub: Arc<HubInner>) {
    let mut idle_since = None;
    loop {
        if hub.should_retire(&mut idle_since) {
            return;
        }
        hub.upstream_connects.fetch_add(1, Ordering::SeqCst);
        match hub.stream_frames(&mut idle_since).await {
            Ok(()) => return,
            Err(err) => {
                tracing::warn!(%err, "camera stream interrupted");
                *hub.last_error.lock().unwrap() = Some(err.to_string());
            }
        }
        sleep(RECONNECT_DELAY).await;
    }
}

impl HubInner {
    /// Returns `true` once the upstream has had no viewers for the grace period, having
    /// cleared its slot so the next viewer starts a fresh one. The viewer count is re-read under
    /// the slot lock: a viewer arriving at that moment either sees the slot still taken and is
    /// served by this upstream, or sees it empty and starts another.
    fn should_retire(&self, idle_since: &mut Option<Instant>) -> bool {
        if self.viewers.load(Ordering::SeqCst) > 0 {
            *idle_since = None;
            return false;
        }
        if idle_since.get_or_insert_with(Instant::now).elapsed() < self.idle_grace {
            return false;
        }
        let mut upstream = self.upstream.lock().unwrap();
        if self.viewers.load(Ordering::SeqCst) > 0 {
            *idle_since = None;
            return false;
        }
        *upstream = None;
        true
    }

    /// Streams until retired (`Ok`) or until the connection fails.
    async fn stream_frames(&self, idle_since: &mut Option<Instant>) -> Result<(), CameraError> {
        let response = self.http.get(&self.url).send().await?;
        if !response.status().is_success() {
            return Err(CameraError::Status(response.status()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut parser = MjpegParser::from_content_type(&content_type)
            .ok_or(CameraError::NotMjpeg(content_type))?;
        let mut stream = response.bytes_stream();
        let mut check = interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    let Some(chunk) = chunk else {
                        return Err(CameraError::Ended);
                    };
                    for frame in parser.push(&chunk?) {
                        *self.latest.lock().unwrap() = Some((frame.clone(), Instant::now()));
                        let _ = self.frames.send(frame);
                    }
                    self.last_error.lock().unwrap().take();
                }
                _ = check.tick() => {
                    if self.should_retire(idle_since) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG_A: &[u8] = &[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9];
    const JPEG_B: &[u8] = &[0xFF, 0xD8, 4, 5, 0xFF, 0xD9];

    fn part(boundary_line: &str, jpeg: &[u8], with_length: bool) -> Vec<u8> {
        let mut out = format!("{boundary_line}\r\nContent-Type: image/jpeg\r\n").into_bytes();
        if with_length {
            out.extend_from_slice(format!("Content-Length: {}\r\n", jpeg.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(jpeg);
        out.extend_from_slice(b"\r\n");
        out
    }

    #[test]
    fn boundary_extraction() {
        for content_type in [
            "multipart/x-mixed-replace; boundary=frame_boundary",
            "multipart/x-mixed-replace; boundary=--frame_boundary",
            "multipart/x-mixed-replace;boundary=\"frame_boundary\"",
        ] {
            let parser = MjpegParser::from_content_type(content_type).unwrap();
            assert_eq!(parser.delimiter, b"--frame_boundary");
        }
        assert!(MjpegParser::from_content_type("image/jpeg").is_none());
        assert!(MjpegParser::from_content_type("multipart/x-mixed-replace; boundary=--").is_none());
    }

    #[test]
    fn frames_with_content_length_split_across_chunks() {
        let mut stream = part("--frame_boundary", JPEG_A, true);
        stream.extend(part("--frame_boundary", JPEG_B, true));
        let mut parser =
            MjpegParser::from_content_type("multipart/x-mixed-replace; boundary=frame_boundary")
                .unwrap();
        let mut frames = Vec::new();
        for chunk in stream.chunks(3) {
            frames.extend(parser.push(chunk));
        }
        assert_eq!(
            frames,
            vec![Bytes::from_static(JPEG_A), Bytes::from_static(JPEG_B)]
        );
    }

    #[test]
    fn frames_without_content_length_wait_for_next_delimiter() {
        let mut parser =
            MjpegParser::from_content_type("multipart/x-mixed-replace; boundary=--frame_boundary")
                .unwrap();
        assert!(
            parser
                .push(&part("----frame_boundary", JPEG_A, false))
                .is_empty()
        );
        let frames = parser.push(&part("----frame_boundary", JPEG_B, false));
        assert_eq!(frames, vec![Bytes::from_static(JPEG_A)]);
    }

    #[test]
    fn content_length_body_may_contain_delimiter_bytes() {
        let tricky: Vec<u8> = [&[0xFF, 0xD8][..], b"--frame_boundary", &[0xFF, 0xD9]].concat();
        let mut parser =
            MjpegParser::from_content_type("multipart/x-mixed-replace; boundary=frame_boundary")
                .unwrap();
        let frames = parser.push(&part("--frame_boundary", &tricky, true));
        assert_eq!(frames, vec![Bytes::from(tricky)]);
    }
}
