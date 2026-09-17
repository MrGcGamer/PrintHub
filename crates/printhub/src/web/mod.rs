//! The HTTP interface: server-rendered pages, a live status stream and the camera relay.

mod admin;
mod assets;
mod chart;
mod error;
mod filament;
mod guard;
mod live;
mod pages;
mod queue;
mod session;
mod stats;
mod views;

use std::{ops::Deref, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify, watch},
};

pub use error::AppError;

use crate::{
    accounts::{self, Role},
    auth::{self, LoginLimiter},
    camera::{self, CameraHub},
    cc2::{LinkState, upload::Uploader},
    config::Config,
    dispatcher,
    inventory::{self, Binding, InventoryError},
    jobs::{self, JobError, MountedNozzle},
    printer::PrinterLink,
    slicer::Slicer,
    store::{self, Db},
};

/// How long open camera and status streams get to finish after a shutdown signal. They never
/// end on their own, so without a limit shutdown would wait for every browser tab to close.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Room for the multipart framing and form fields around an upload of `MAX_UPLOAD_MB`, so
/// a file just over the limit gets the upload handler's message rather than a bare 413.
const UPLOAD_OVERHEAD: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState(Arc<Shared>);

pub struct Shared {
    pub db: Db,
    pub config: Config,
    pub printer: PrinterLink,
    pub camera: Option<CameraHub>,
    /// `None` when OrcaSlicer or its profiles are missing; only G-code can be uploaded then.
    pub slicer: Option<Arc<Slicer>>,
    pub uploader: Uploader,
    pub limiter: LoginLimiter,
    /// Every tray binding with its spool, so the printer card needs no query per status update.
    pub bindings: watch::Sender<Vec<Binding>>,
    /// Serialises reloads of `bindings`, so a slow reload cannot publish an older read over a
    /// newer one.
    bindings_reload: Mutex<()>,
    /// The nozzle people recorded as mounted, so the printer card needs no query to show it.
    pub nozzle: watch::Sender<MountedNozzle>,
    /// Wakes the dispatcher after a change it would otherwise only notice on its next tick.
    pub queue_changed: Notify,
}

impl Deref for AppState {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AppState {
    pub async fn new(
        db: Db,
        config: Config,
        printer: PrinterLink,
        camera: Option<CameraHub>,
        slicer: Option<Slicer>,
    ) -> anyhow::Result<Self> {
        let bindings = inventory::bindings(&db)
            .await
            .context("loading tray bindings")?;
        let nozzle = jobs::mounted_nozzle(&db, config.nozzle)
            .await
            .context("loading the mounted nozzle")?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let uploader = Uploader::new(
            http,
            format!(
                "http://{}:{}",
                url_host(&config),
                config.printer_upload_port
            ),
            config.printer_access_code.clone(),
        );
        Ok(Self(Arc::new(Shared {
            db,
            config,
            printer,
            camera,
            slicer: slicer.map(Arc::new),
            uploader,
            limiter: LoginLimiter::default(),
            bindings: watch::Sender::new(bindings),
            bindings_reload: Mutex::new(()),
            nozzle: watch::Sender::new(nozzle),
            queue_changed: Notify::new(),
        })))
    }

    /// Call after every change to spools or bindings.
    pub async fn refresh_bindings(&self) -> Result<(), InventoryError> {
        let _reload = self.bindings_reload.lock().await;
        let bindings = inventory::bindings(&self.db).await?;
        self.bindings.send_if_modified(|current| {
            let changed = *current != bindings;
            if changed {
                *current = bindings;
            }
            changed
        });
        Ok(())
    }

    /// Call after recording a nozzle. Wakes the dispatcher: a job held for another nozzle may
    /// now fit.
    pub async fn refresh_nozzle(&self) -> Result<(), JobError> {
        let nozzle = jobs::mounted_nozzle(&self.db, self.config.nozzle).await?;
        self.nozzle.send_replace(nozzle);
        self.queue_changed.notify_one();
        Ok(())
    }
}

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
            get(queue::schedule_page).post(queue::add_rule),
        )
        .route("/admin/schedule/{id}/delete", post(queue::delete_rule))
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
    let slicer = load_slicer(&config);
    tokio::spawn(purge_expired(db.clone()));

    let listen = config.listen_addr;
    let state = AppState::new(db, config, printer, camera, slicer).await?;
    tokio::spawn(unbind_emptied_trays(state.clone()));
    tokio::spawn(dispatcher::run(state.clone()));
    let app = router(state);
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

fn load_slicer(config: &Config) -> Option<Slicer> {
    if !config.orca_slicer.is_file() {
        tracing::warn!(
            path = %config.orca_slicer.display(),
            "OrcaSlicer not found (ORCA_SLICER); STL uploads are disabled"
        );
        return None;
    }
    match Slicer::new(
        config.orca_slicer.clone(),
        &config.orca_profiles,
        config.slice_timeout,
    ) {
        Ok(slicer) => Some(slicer),
        Err(err) => {
            tracing::warn!(%err, "slicer profiles unusable (ORCA_PROFILES); STL uploads are disabled");
            None
        }
    }
}

/// The printer host as it goes into a URL, with an IPv6 address in brackets.
fn url_host(config: &Config) -> String {
    let host = &config.printer_host;
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.clone()
    }
}

pub fn camera_url(config: &Config) -> String {
    format!(
        "http://{}:{}/",
        url_host(config),
        config.printer_camera_port
    )
}

/// Drops the binding of every tray the printer reports empty, so a spool that was taken out
/// does not stay listed as loaded. Runs for the life of the printer link.
pub async fn unbind_emptied_trays(state: AppState) {
    let mut printer = state.printer.subscribe();
    loop {
        let emptied = {
            let snapshot = printer.borrow_and_update();
            match (&snapshot.link, &snapshot.canvas) {
                (LinkState::Registered, Some(canvas)) => {
                    inventory::emptied_bindings(canvas, &state.bindings.borrow())
                }
                _ => Vec::new(),
            }
        };
        if !emptied.is_empty() {
            let unbound = async {
                inventory::unbind_trays(&state.db, &emptied).await?;
                state.refresh_bindings().await
            };
            match unbound.await {
                Ok(()) => tracing::info!(trays = ?emptied, "unbound spools from emptied trays"),
                Err(err) => tracing::warn!(%err, "unbinding emptied trays failed"),
            }
        }
        if printer.changed().await.is_err() {
            return;
        }
    }
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
