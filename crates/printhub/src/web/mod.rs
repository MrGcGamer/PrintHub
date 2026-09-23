//! The HTTP interface: server-rendered pages, a live status stream and the camera relay.

mod admin;
mod assets;
mod chart;
mod error;
mod filament;
mod guard;
mod help;
mod live;
mod pages;
mod queue;
mod schedule;
mod session;
mod stats;
mod storage;
mod views;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

pub use error::AppError;

// Imported here so every page module reaches it as `super::AppState`.
use crate::app::AppState;

/// Room for the multipart framing and form fields around an upload of `MAX_UPLOAD_MB`, so
/// a file just over the limit gets the upload handler's message rather than a bare 413.
const UPLOAD_OVERHEAD: usize = 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let upload_limit = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(UPLOAD_OVERHEAD);
    Router::new()
        .route("/", get(pages::dashboard))
        .route("/login", get(pages::login_page).post(pages::login))
        .route("/logout", post(pages::logout))
        .route(
            "/invite/{token}",
            get(pages::invite_page).post(pages::redeem),
        )
        .route("/account", get(pages::account_page))
        .route("/account/password", post(pages::change_password))
        .route("/account/theme", post(pages::change_theme))
        .route("/admin/users", get(admin::users_page))
        .route("/admin/users/{id}/role", post(admin::set_role))
        .route("/admin/users/{id}/disabled", post(admin::set_disabled))
        .route(
            "/admin/users/{id}/permissions",
            post(admin::set_permissions),
        )
        .route("/admin/users/{id}/reset", post(admin::reset_link))
        .route("/admin/invites", post(admin::create_invite))
        .route("/admin/invites/{id}/revoke", post(admin::revoke_invite))
        .route(
            "/admin/schedule",
            get(schedule::schedule_page).post(schedule::add_rule),
        )
        .route("/admin/schedule/{id}/delete", post(schedule::delete_rule))
        .route("/admin/storage", get(storage::storage_page))
        .route("/admin/storage/{id}/delete", post(storage::delete_files))
        .route("/inventory", get(filament::inventory_page))
        .route("/spools", post(filament::create_spool))
        .route("/spools/new", get(filament::new_spool_page))
        .route(
            "/spools/{id}",
            get(filament::spool_page).post(filament::update_spool),
        )
        .route("/spools/{id}/edit", get(filament::edit_spool_page))
        .route("/spools/{id}/archived", post(filament::set_archived))
        .route("/spools/{id}/weigh", post(filament::weigh_in))
        .route("/trays/refresh", post(filament::refresh_trays))
        .route("/trays/{canvas}/{tray}/bind", post(filament::bind_tray))
        .route("/trays/{canvas}/{tray}/unbind", post(filament::unbind_tray))
        .route(
            "/jobs",
            get(queue::jobs_page)
                .post(queue::create_job)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/jobs/new", get(queue::new_job_page))
        .route("/jobs/{id}", get(queue::job_page))
        .route("/jobs/{id}/files/{kind}", get(queue::job_file))
        .route("/jobs/{id}/confirm", post(queue::confirm_job))
        .route("/jobs/{id}/cancel", post(queue::cancel_job))
        .route("/jobs/{id}/requeue", post(queue::requeue_job))
        .route("/jobs/{id}/move", post(queue::move_job))
        .route("/stats", get(stats::stats_page))
        .route("/stats/users/{id}", get(stats::user_stats_page))
        .route("/stats/settlements", post(stats::record_payment))
        .route(
            "/stats/settlements/{id}/delete",
            post(stats::delete_payment),
        )
        .route("/printer/bed-clear", post(queue::mark_bed_clear))
        .route("/printer/nozzle", post(pages::set_nozzle))
        .route("/events/printer", get(live::printer_events))
        .route("/printer/{action}", post(live::control))
        .route("/camera/stream", get(live::camera_stream))
        .route("/camera/snapshot.jpg", get(live::camera_snapshot))
        .route("/wiki", get(help::index))
        .route("/wiki/search", get(help::search))
        .route("/wiki/{*slug}", get(help::page))
        .route("/static/{file}", get(assets::serve))
        .route("/static/wiki/{file}", get(assets::wiki_image))
        .route("/favicon.ico", get(assets::favicon))
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            guard::same_origin,
        ))
        .layer(middleware::from_fn(guard::security_headers))
        .with_state(state)
}
