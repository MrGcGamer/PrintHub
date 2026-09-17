use axum::{
    extract::Path,
    http::header,
    response::{IntoResponse, Response},
};

use super::error::AppError;
use crate::wiki;

const HTMX: &[u8] = include_bytes!("../../static/htmx.min.js");
const HTMX_SSE: &[u8] = include_bytes!("../../static/htmx-ext-sse.min.js");
const STYLESHEET: &[u8] = include_bytes!("../../static/app.css");
const LOGO: &[u8] = include_bytes!("../../static/logo.svg");
const FAVICON: &[u8] = include_bytes!("../../static/favicon.ico");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../../static/apple-touch-icon.png");
const ICON_192: &[u8] = include_bytes!("../../static/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../static/icon-512.png");
const ICON_MASKABLE_512: &[u8] = include_bytes!("../../static/icon-maskable-512.png");
const MANIFEST: &[u8] = include_bytes!("../../static/manifest.webmanifest");

pub async fn serve(Path(file): Path<String>) -> Result<Response, AppError> {
    let (body, content_type) = match file.as_str() {
        "htmx.min.js" => (HTMX, "text/javascript"),
        "htmx-ext-sse.min.js" => (HTMX_SSE, "text/javascript"),
        "app.css" => (STYLESHEET, "text/css"),
        "logo.svg" => (LOGO, "image/svg+xml"),
        "apple-touch-icon.png" => (APPLE_TOUCH_ICON, "image/png"),
        "icon-192.png" => (ICON_192, "image/png"),
        "icon-512.png" => (ICON_512, "image/png"),
        "icon-maskable-512.png" => (ICON_MASKABLE_512, "image/png"),
        "manifest.webmanifest" => (MANIFEST, "application/manifest+json"),
        _ => return Err(AppError::NotFound),
    };
    Ok(cached(body, content_type))
}

pub async fn wiki_image(Path(file): Path<String>) -> Result<Response, AppError> {
    let image = wiki::image(&file).ok_or(AppError::NotFound)?;
    Ok(cached(image.bytes, "image/jpeg"))
}

/// Browsers request `/favicon.ico` at the root whatever the page links to.
pub async fn favicon() -> Response {
    cached(FAVICON, "image/x-icon")
}

fn cached(body: &'static [u8], content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}
