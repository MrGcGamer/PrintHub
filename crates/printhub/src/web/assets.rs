use axum::{
    extract::Path,
    http::header,
    response::{IntoResponse, Response},
};

use super::error::AppError;

const HTMX: &[u8] = include_bytes!("../../static/htmx.min.js");
const HTMX_SSE: &[u8] = include_bytes!("../../static/htmx-ext-sse.min.js");
const STYLESHEET: &[u8] = include_bytes!("../../static/app.css");

pub async fn serve(Path(file): Path<String>) -> Result<Response, AppError> {
    let (body, content_type) = match file.as_str() {
        "htmx.min.js" => (HTMX, "text/javascript"),
        "htmx-ext-sse.min.js" => (HTMX_SSE, "text/javascript"),
        "app.css" => (STYLESHEET, "text/css"),
        _ => return Err(AppError::NotFound),
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response())
}
