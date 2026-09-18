//! The server's printer connection: finds the serial number, retrying discovery while the
//! printer is off, then keeps one [`PrinterClient`] alive for the life of the process.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use tokio::sync::watch;

use crate::{
    cc2::{ClientConfig, LinkState, PrinterClient, PrinterSnapshot, Timing, discovery},
    config::Config,
};

const DISCOVERY_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct PrinterLink {
    snapshot: watch::Receiver<PrinterSnapshot>,
    client: Arc<OnceLock<PrinterClient>>,
}

struct Settings {
    host: String,
    serial: Option<String>,
    mqtt_port: u16,
    access_code: String,
}

impl PrinterLink {
    pub fn start(config: &Config) -> Self {
        let (tx, snapshot) = watch::channel(PrinterSnapshot {
            link: LinkState::Connecting,
            status: None,
            canvas: None,
            attributes: None,
            last_seen: None,
        });
        let client = Arc::new(OnceLock::new());
        let settings = Settings {
            host: config.printer_host.clone(),
            serial: config.printer_sn.clone(),
            mqtt_port: config.printer_mqtt_port,
            access_code: config.printer_access_code.clone(),
        };
        tokio::spawn(run(settings, tx, Arc::clone(&client)));
        Self { snapshot, client }
    }

    pub fn subscribe(&self) -> watch::Receiver<PrinterSnapshot> {
        self.snapshot.clone()
    }

    pub fn snapshot(&self) -> PrinterSnapshot {
        self.snapshot.borrow().clone()
    }

    /// `None` until the serial number is known and the client has been started.
    pub fn client(&self) -> Option<&PrinterClient> {
        self.client.get()
    }
}

async fn run(
    settings: Settings,
    tx: watch::Sender<PrinterSnapshot>,
    cell: Arc<OnceLock<PrinterClient>>,
) {
    let serial = match settings.serial {
        Some(serial) => serial,
        None => loop {
            match discovery::discover(&settings.host, Duration::from_secs(3), 2).await {
                Ok((_, info)) => {
                    tracing::info!(serial = %info.sn, "printer discovered");
                    break info.sn;
                }
                Err(err) => {
                    tracing::warn!(%err, retry_in = ?DISCOVERY_RETRY, "printer discovery failed");
                    tx.send_modify(|s| s.link = LinkState::Discovering(err.to_string()));
                    tokio::time::sleep(DISCOVERY_RETRY).await;
                }
            }
        },
    };

    let (client, _supervisor) = PrinterClient::start(ClientConfig {
        host: settings.host,
        port: settings.mqtt_port,
        serial,
        password: settings.access_code,
        timing: Timing::default(),
    });
    let mut updates = client.subscribe();
    let _ = cell.set(client);
    loop {
        let snapshot = updates.borrow_and_update().clone();
        tx.send_replace(snapshot);
        if updates.changed().await.is_err() {
            return;
        }
    }
}
