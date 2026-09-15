//! The printer camera: an MJPEG stream on port 8080 that serves one viewer at a time.

use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use futures::StreamExt;
use reqwest::header::CONTENT_TYPE;
use thiserror::Error;

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
