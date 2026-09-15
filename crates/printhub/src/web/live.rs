use std::{convert::Infallible, time::Duration};

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, State},
    http::header,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::Stream;
use tokio_stream::{StreamExt, wrappers::WatchStream};

use super::{
    AppState,
    error::{AppError, render},
    session::{AdminUser, CurrentUser},
    views::PrinterCard,
};
use crate::camera;

#[derive(Template)]
#[template(path = "printer_card.html")]
pub struct PrinterCardPartial {
    pub card: PrinterCard,
}

#[derive(Template)]
#[template(source = r#"<p class="{{ class }}">{{ text }}</p>"#, ext = "html")]
struct Notice<'a> {
    class: &'a str,
    text: &'a str,
}

/// The printer card, re-rendered on every change but at most twice a second. The watch
/// channel keeps only the newest snapshot, so throttling drops stale states rather than
/// queueing them.
pub async fn printer_events(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let user = current.user;
    let stream = WatchStream::new(state.printer.subscribe())
        .throttle(Duration::from_millis(500))
        .map(move |snapshot| {
            let partial = PrinterCardPartial {
                card: PrinterCard::new(&snapshot, &user),
            };
            let html = partial.render().unwrap_or_else(|err| {
                tracing::error!(%err, "rendering printer card");
                String::new()
            });
            Ok(Event::default().event("printer").data(html))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub async fn control(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(action): Path<String>,
) -> Result<Response, AppError> {
    let Some(client) = state.printer.client() else {
        return notice("error", "The printer is not connected.");
    };
    let (result, done) = match action.as_str() {
        "pause" => (client.pause().await, "Pause sent."),
        "resume" => (client.resume().await, "Resume sent."),
        "stop" => (client.stop().await, "Stop sent."),
        _ => return Err(AppError::NotFound),
    };
    match result {
        Ok(()) => {
            tracing::info!(username = %admin.user.username, action, "printer control");
            notice("notice", done)
        }
        Err(err) => notice("error", &format!("The printer refused: {err}")),
    }
}

fn notice(class: &str, text: &str) -> Result<Response, AppError> {
    Ok(render(&Notice { class, text })?.into_response())
}

pub async fn camera_stream(
    State(state): State<AppState>,
    _current: CurrentUser,
) -> Result<Response, AppError> {
    let hub = state.camera.clone().ok_or(AppError::NotFound)?;
    let frames = futures::stream::unfold(hub.watch(), |mut viewer| async move {
        let frame = viewer.next_frame().await?;
        Some((Ok::<_, Infallible>(camera::multipart_part(&frame)), viewer))
    });
    Ok((
        [
            (
                header::CONTENT_TYPE,
                format!("multipart/x-mixed-replace; boundary={}", camera::BOUNDARY),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Body::from_stream(frames),
    )
        .into_response())
}

pub async fn camera_snapshot(
    State(state): State<AppState>,
    _current: CurrentUser,
) -> Result<Response, AppError> {
    let hub = state.camera.clone().ok_or(AppError::NotFound)?;
    let frame = hub
        .snapshot(Duration::from_secs(2), Duration::from_secs(10))
        .await
        .map_err(|err| AppError::Unavailable(format!("The camera is unavailable: {err}")))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        frame,
    )
        .into_response())
}
