//! G-code upload over the stock firmware's `PUT /upload`, chunked and authenticated the way
//! Elegoo's `elegoo-link` SDK does it (`elegoo_fdm_cc2_http_transfer.cpp`).

use md5::{Digest, Md5};
use reqwest::{
    StatusCode,
    header::{ACCEPT, CONTENT_RANGE, CONTENT_TYPE},
};
use serde::Deserialize;
use thiserror::Error;

/// The SDK's limit: "strictly follow Elegoo API requirements, max 1MB per chunk".
pub const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("refusing to upload an empty file")]
    Empty,
    #[error("upload request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("printer rejected the access code")]
    AccessDenied,
    #[error("printer is busy")]
    Busy,
    #[error("printer answered HTTP {0}")]
    Status(StatusCode),
    #[error("printer rejected the chunk at byte {offset} with error code {code}")]
    Printer { offset: usize, code: i64 },
    #[error("printer reply to the chunk at byte {offset} had no error_code")]
    NoErrorCode { offset: usize },
}

#[derive(Clone)]
pub struct Uploader {
    http: reqwest::Client,
    base_url: String,
    access_code: String,
}

impl Uploader {
    /// `base_url` is scheme, host and port, e.g. `http://192.168.33.40:80`.
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        access_code: impl Into<String>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            access_code: access_code.into(),
        }
    }

    /// `filename` travels in an HTTP header, so it must be plain ASCII; callers generate it
    /// rather than passing a user's original file name.
    pub async fn upload(&self, filename: &str, data: &[u8]) -> Result<(), UploadError> {
        if data.is_empty() {
            return Err(UploadError::Empty);
        }
        let md5 = md5_hex(data);
        let url = format!("{}/upload", self.base_url);
        let total = data.len();

        for (index, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
            let offset = index * CHUNK_SIZE;
            let end = offset + chunk.len() - 1;
            let response = self
                .http
                .put(&url)
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_RANGE, format!("bytes {offset}-{end}/{total}"))
                .header("X-File-Name", filename)
                .header("X-File-MD5", &md5)
                .header("X-Token", &self.access_code)
                .body(chunk.to_vec())
                .send()
                .await?;

            match response.status() {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(UploadError::AccessDenied);
                }
                StatusCode::TOO_MANY_REQUESTS => return Err(UploadError::Busy),
                status if !status.is_success() => return Err(UploadError::Status(status)),
                _ => {}
            }

            #[derive(Deserialize)]
            struct ChunkReply {
                error_code: Option<i64>,
            }
            match response.json::<ChunkReply>().await?.error_code {
                Some(0) => {}
                Some(code) => return Err(UploadError::Printer { offset, code }),
                None => return Err(UploadError::NoErrorCode { offset }),
            }
        }
        Ok(())
    }
}

/// Lowercase hex, as the SDK sends it.
pub fn md5_hex(data: &[u8]) -> String {
    Md5::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vector() {
        assert_eq!(md5_hex(b"hello world"), "5eb63bbbe01eeed093cb22bb8f5acdc3");
    }
}
