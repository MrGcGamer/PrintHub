//! The running application: state shared by the web server and the background tasks, and the
//! startup that wires them together.

use std::{ops::Deref, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify, watch},
};

use crate::{
    accounts::{self, Role},
    auth::{self, LoginLimiter},
    camera::{self, CameraHub},
    cc2::{LinkState, PrinterSnapshot, upload::Uploader},
    config::Config,
    dispatcher,
    inventory::{self, Binding, InventoryError},
    jobs::{self, JobError, MountedNozzle},
    printer::PrinterLink,
    slicer::Slicer,
    store::{self, Db},
    web,
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
    /// What the printer answered about the printing file's layer count (method 1046), kept so
    /// the card needs no request per status update.
    pub file_layers: watch::Sender<Option<FileLayers>>,
    /// Wakes the dispatcher after a change it would otherwise only notice on its next tick.
    pub queue_changed: Notify,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileLayers {
    pub filename: String,
    pub layers: i64,
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
            file_layers: watch::Sender::new(None),
            queue_changed: Notify::new(),
        })))
    }

    /// The layer total for the printer card: what the printer says about the file it is printing,
    /// falling back to the count PrintHub read from the running job's own G-code. The live status
    /// carries `total_layer` 0 on firmware 02.01.00.00, so neither is redundant: a print started
    /// outside PrintHub has no job to ask.
    pub async fn layer_total(&self, snapshot: &PrinterSnapshot) -> Option<i64> {
        let filename = match &snapshot.status {
            Some(status) if !status.print_status.filename.is_empty() => {
                &status.print_status.filename
            }
            _ => return None,
        };
        let known = self
            .file_layers
            .borrow()
            .as_ref()
            .filter(|file| &file.filename == filename)
            .map(|file| file.layers);
        match known {
            Some(layers) => Some(layers),
            None => jobs::printing_layers(&self.db).await.unwrap_or_default(),
        }
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
            camera::STALL_TIMEOUT,
        ))
    } else {
        None
    };
    let slicer = load_slicer(&config);

    let listen = config.listen_addr;
    let state = AppState::new(db, config, printer.clone(), camera, slicer).await?;
    spawn_tasks(&state);
    let app = web::router(state);
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    tracing::info!(%listen, "serving");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    let served = tokio::select! {
        served = server => served.map_err(anyhow::Error::from),
        () = async {
            shutdown_signal().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {
            tracing::info!("streams still open after the grace period, exiting");
            Ok(())
        }
    };
    printer.shutdown().await;
    served
}

/// The work that runs beside the web server for the life of the process. The integration
/// tests start the same set, so a task added here is exercised there too.
pub fn spawn_tasks(state: &AppState) {
    tokio::spawn(purge_expired(state.db.clone()));
    tokio::spawn(unbind_emptied_trays(state.clone()));
    tokio::spawn(dispatcher::run(state.clone()));
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
async fn unbind_emptied_trays(state: AppState) {
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
