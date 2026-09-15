//! The HTTP interface: server-rendered pages, a live status stream and the camera relay.

mod admin;
mod assets;
mod error;
mod guard;
mod live;
mod pages;
mod session;
mod views;

use std::{ops::Deref, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use axum::{
    Router, middleware,
    routing::{get, post},
};
use tokio::net::TcpListener;

pub use error::AppError;

use crate::{
    accounts::{self, Role},
    auth::{self, LoginLimiter},
    camera::{self, CameraHub},
    config::Config,
    printer::PrinterLink,
    store::{self, Db},
};

/// How long open camera and status streams get to finish after a shutdown signal. They never
/// end on their own, so without a limit shutdown would wait for every browser tab to close.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AppState(Arc<Shared>);

pub struct Shared {
    pub db: Db,
    pub config: Config,
    pub printer: PrinterLink,
    pub camera: Option<CameraHub>,
    pub limiter: LoginLimiter,
}

impl Deref for AppState {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AppState {
    pub fn new(db: Db, config: Config, printer: PrinterLink, camera: Option<CameraHub>) -> Self {
        Self(Arc::new(Shared {
            db,
            config,
            printer,
            camera,
            limiter: LoginLimiter::default(),
        }))
    }
}

pub fn router(state: AppState) -> Router {
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
        .route("/admin/users", get(admin::users_page))
        .route("/admin/users/{id}/role", post(admin::set_role))
        .route("/admin/users/{id}/disabled", post(admin::set_disabled))
        .route("/admin/users/{id}/reset", post(admin::reset_link))
        .route("/admin/invites", post(admin::create_invite))
        .route("/admin/invites/{id}/revoke", post(admin::revoke_invite))
        .route("/events/printer", get(live::printer_events))
        .route("/printer/{action}", post(live::control))
        .route("/camera/stream", get(live::camera_stream))
        .route("/camera/snapshot.jpg", get(live::camera_snapshot))
        .route("/static/{file}", get(assets::serve))
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            guard::same_origin,
        ))
        .layer(middleware::from_fn(guard::security_headers))
        .with_state(state)
}

pub async fn serve(config: Config) -> anyhow::Result<()> {
    let db = store::open(&config.data_dir).await?;
    bootstrap_admin(&db, &config).await?;

    let printer = PrinterLink::start(&config);
    let camera = if config.camera_enabled {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Some(CameraHub::new(
            http,
            camera_url(&config),
            camera::IDLE_GRACE,
        ))
    } else {
        None
    };
    tokio::spawn(purge_expired(db.clone()));

    let listen = config.listen_addr;
    let app = router(AppState::new(db, config, printer, camera));
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    tracing::info!(%listen, "serving");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    tokio::select! {
        served = server => served?,
        () = async {
            shutdown_signal().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => tracing::info!("streams still open after the grace period, exiting"),
    }
    Ok(())
}

pub fn camera_url(config: &Config) -> String {
    let host = &config.printer_host;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    format!("http://{host}:{}/", config.printer_camera_port)
}

async fn bootstrap_admin(db: &Db, config: &Config) -> anyhow::Result<()> {
    if accounts::active_admin_count(db).await? > 0 {
        if config.admin_bootstrap.is_some() {
            tracing::info!(
                "an admin account exists; ADMIN_USERNAME and ADMIN_PASSWORD are ignored"
            );
        }
        return Ok(());
    }
    let Some(admin) = &config.admin_bootstrap else {
        tracing::warn!(
            "no admin account exists; set ADMIN_USERNAME and ADMIN_PASSWORD to create one"
        );
        return Ok(());
    };
    accounts::validate_username(&admin.username).map_err(|m| anyhow!("ADMIN_USERNAME: {m}"))?;
    accounts::validate_password(&admin.password).map_err(|m| anyhow!("ADMIN_PASSWORD: {m}"))?;
    let hash = auth::hash_password(admin.password.clone()).await?;
    accounts::create_user(db, &admin.username, &hash, Role::Admin, store::now())
        .await
        .with_context(|| format!("creating admin account {:?}", admin.username))?;
    tracing::info!(username = %admin.username, "created admin account");
    Ok(())
}

async fn purge_expired(db: Db) {
    let mut hourly = tokio::time::interval(Duration::from_secs(3600));
    loop {
        hourly.tick().await;
        if let Err(err) = accounts::purge_expired(&db, store::now()).await {
            tracing::warn!(%err, "purging expired sessions failed");
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
