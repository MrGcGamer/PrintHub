//! The MQTT session with the printer. The whole app shares one connection, because the printer
//! only admits a few clients and the slicer or phone app may want one too.

use std::{
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rumqttc::{AsyncClient, ClientError, Event, EventLoop, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval, interval_at, sleep, timeout},
};

use super::{
    methods::{
        self, Empty, Envelope, FileRef, Request, SlotMapEntry, StartPrint, VideoStream, error_code,
    },
    model::{Attributes, CanvasInfo, CanvasStatus, FileDetail, StatusView},
    status::{StatusCache, deep_merge},
};

pub const MQTT_PORT: u16 = 1883;
pub const MQTT_USERNAME: &str = "elegoo";

/// Protocol timings. Tests shorten them; production uses [`Timing::default`].
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub heartbeat_interval: Duration,
    /// The printer drops a client after 65 s without a PING; the same silence from the
    /// printer's side means the link is dead even if TCP has not noticed.
    pub heartbeat_timeout: Duration,
    pub register_timeout: Duration,
    pub register_retry: Duration,
    pub command_timeout: Duration,
    /// Tray contents are not part of status deltas, so they are polled.
    pub canvas_refresh: Duration,
    pub full_status_refresh: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(65),
            register_timeout: Duration::from_secs(3),
            register_retry: Duration::from_secs(10),
            command_timeout: Duration::from_secs(10),
            canvas_refresh: Duration::from_secs(30),
            full_status_refresh: Duration::from_secs(300),
        }
    }
}

const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// File lists and base64 thumbnails are far larger than rumqttc's 10 KB default.
const MAX_INCOMING_PACKET: usize = 8 * 1024 * 1024;
const MAX_OUTGOING_PACKET: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub host: String,
    pub port: u16,
    pub serial: String,
    pub password: String,
    pub timing: Timing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum LinkState {
    Connecting,
    /// Connected at the MQTT level, but the printer refused registration, e.g. `too many clients`.
    Rejected(String),
    Registered,
    Disconnected(String),
}

/// Last known printer state. Fields keep their last value across disconnects; `link` says how
/// stale they may be.
#[derive(Debug, Clone, Serialize)]
pub struct PrinterSnapshot {
    pub link: LinkState,
    pub status: Option<StatusView>,
    pub canvas: Option<CanvasInfo>,
    pub attributes: Option<Attributes>,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("not registered with the printer")]
    NotRegistered,
    #[error("printer did not answer method {method} in time")]
    Timeout { method: u32 },
    #[error("connection lost before method {method} was answered")]
    ConnectionLost { method: u32 },
    #[error("printer rejected method {method} with error code {code}")]
    Printer { method: u32, code: i64 },
    #[error("mqtt: {0}")]
    Mqtt(#[from] ClientError),
    #[error("unexpected response to method {method}: {source}")]
    Decode {
        method: u32,
        source: serde_json::Error,
    },
}

impl CommandError {
    pub fn printer_code(&self) -> Option<i64> {
        match self {
            Self::Printer { code, .. } => Some(*code),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct PrinterClient {
    inner: Arc<Inner>,
}

impl PrinterClient {
    /// Spawns the connection supervisor, which reconnects until the returned handle is aborted.
    pub fn start(config: ClientConfig) -> (Self, JoinHandle<()>) {
        let client_id = client_id();
        let request_id = request_id();
        let timing = config.timing;

        let mut options = MqttOptions::new(client_id.clone(), config.host, config.port);
        options.set_keep_alive(Duration::from_secs(60));
        options.set_clean_session(true);
        options.set_credentials(MQTT_USERNAME, config.password);
        options.set_max_packet_size(MAX_INCOMING_PACKET, MAX_OUTGOING_PACKET);
        let (mqtt, eventloop) = AsyncClient::new(options, 64);

        let (snapshot, _) = watch::channel(PrinterSnapshot {
            link: LinkState::Connecting,
            status: None,
            canvas: None,
            attributes: None,
        });
        let inner = Arc::new(Inner {
            topics: Topics::new(&config.serial, &client_id, &request_id),
            mqtt,
            client_id,
            request_id,
            timing,
            next_id: AtomicU64::new(1),
            registered: AtomicBool::new(false),
            full_refresh_in_flight: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            pending: Mutex::default(),
            register_waiter: Mutex::default(),
            last_pong: Mutex::new(Instant::now()),
            cache: Mutex::default(),
            attributes: Mutex::new(Value::Object(Map::new())),
            snapshot,
        });
        let supervisor = tokio::spawn(supervise(Arc::clone(&inner), eventloop));
        (Self { inner }, supervisor)
    }

    pub fn client_id(&self) -> &str {
        &self.inner.client_id
    }

    /// Disconnects cleanly instead of leaving the printer to time the client out.
    pub async fn shutdown(self, supervisor: JoinHandle<()>) {
        self.inner.shutting_down.store(true, Ordering::Relaxed);
        if let Err(err) = self.inner.mqtt.disconnect().await {
            tracing::debug!(%err, "disconnect request failed");
        }
        let abort = supervisor.abort_handle();
        if timeout(Duration::from_secs(2), supervisor).await.is_err() {
            abort.abort();
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<PrinterSnapshot> {
        self.inner.snapshot.subscribe()
    }

    pub fn snapshot(&self) -> PrinterSnapshot {
        self.inner.snapshot.borrow().clone()
    }

    /// Waits for registration, returning the link state it gave up in.
    pub async fn wait_until_registered(&self, limit: Duration) -> Result<(), LinkState> {
        let mut rx = self.subscribe();
        match timeout(limit, rx.wait_for(|s| s.link == LinkState::Registered)).await {
            Ok(Ok(_)) => Ok(()),
            _ => Err(self.inner.snapshot.borrow().link.clone()),
        }
    }

    pub async fn request<P: Serialize>(
        &self,
        method: u32,
        params: P,
    ) -> Result<Envelope, CommandError> {
        self.inner.request(method, params).await
    }

    pub async fn attributes(&self) -> Result<Attributes, CommandError> {
        self.request_as(methods::GET_ATTRIBUTES, Empty {}).await
    }

    pub async fn canvas(&self) -> Result<CanvasInfo, CommandError> {
        let status: CanvasStatus = self
            .request_as(methods::GET_CANVAS_STATUS, Empty {})
            .await?;
        Ok(status.canvas_info)
    }

    pub async fn file_detail(&self, filename: &str) -> Result<FileDetail, CommandError> {
        self.request_as(methods::GET_FILE_DETAIL, FileRef::local(filename))
            .await
    }

    pub async fn start_print(
        &self,
        filename: &str,
        slot_map: Vec<SlotMapEntry>,
    ) -> Result<(), CommandError> {
        self.request(methods::START_PRINT, StartPrint::local(filename, slot_map))
            .await
            .map(drop)
    }

    pub async fn pause(&self) -> Result<(), CommandError> {
        self.request(methods::PAUSE_PRINT, Empty {}).await.map(drop)
    }

    pub async fn resume(&self) -> Result<(), CommandError> {
        self.request(methods::RESUME_PRINT, Empty {})
            .await
            .map(drop)
    }

    pub async fn stop(&self) -> Result<(), CommandError> {
        self.request(methods::STOP_PRINT, Empty {}).await.map(drop)
    }

    pub async fn set_video_stream(&self, enable: bool) -> Result<(), CommandError> {
        self.request(methods::VIDEO_STREAM, VideoStream { enable })
            .await
            .map(drop)
    }

    async fn request_as<T: DeserializeOwned, P: Serialize>(
        &self,
        method: u32,
        params: P,
    ) -> Result<T, CommandError> {
        let envelope = self.request(method, params).await?;
        T::deserialize(envelope.result).map_err(|source| CommandError::Decode { method, source })
    }
}

struct Topics {
    register: String,
    register_response: String,
    request: String,
    response: String,
    status: String,
}

impl Topics {
    fn new(serial: &str, client_id: &str, request_id: &str) -> Self {
        Self {
            register: format!("elegoo/{serial}/api_register"),
            register_response: format!("elegoo/{serial}/{request_id}/register_response"),
            request: format!("elegoo/{serial}/{client_id}/api_request"),
            response: format!("elegoo/{serial}/{client_id}/api_response"),
            status: format!("elegoo/{serial}/api_status"),
        }
    }
}

struct Inner {
    mqtt: AsyncClient,
    topics: Topics,
    client_id: String,
    request_id: String,
    timing: Timing,
    next_id: AtomicU64,
    registered: AtomicBool,
    full_refresh_in_flight: AtomicBool,
    shutting_down: AtomicBool,
    pending: Mutex<HashMap<u64, oneshot::Sender<Envelope>>>,
    register_waiter: Mutex<Option<oneshot::Sender<String>>>,
    last_pong: Mutex<Instant>,
    cache: Mutex<StatusCache>,
    /// Kept raw because attribute events (6008) may carry only the changed fields.
    attributes: Mutex<Value>,
    snapshot: watch::Sender<PrinterSnapshot>,
}

async fn supervise(inner: Arc<Inner>, mut eventloop: EventLoop) {
    let mut session: Option<JoinHandle<()>> = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                backoff = Duration::from_secs(1);
                if let Some(old) = session.take() {
                    old.abort();
                }
                session = Some(tokio::spawn(run_session(Arc::clone(&inner))));
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                inner.route(&publish.topic, &publish.payload);
            }
            Ok(_) => {}
            Err(err) => {
                if let Some(old) = session.take() {
                    old.abort();
                }
                if inner.shutting_down.load(Ordering::Relaxed) {
                    inner.on_disconnect("shut down".into());
                    return;
                }
                tracing::warn!(%err, retry_in = ?backoff, "printer connection lost");
                inner.on_disconnect(err.to_string());
                sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Runs from each ConnAck until the connection drops, at which point the supervisor aborts it.
async fn run_session(inner: Arc<Inner>) {
    inner.set_link(LinkState::Connecting);
    // Subscribed before registering: the registration reply goes to a topic nobody listens on
    // otherwise, and the printer does not repeat it.
    for topic in [
        &inner.topics.response,
        &inner.topics.status,
        &inner.topics.register_response,
    ] {
        if let Err(err) = inner.mqtt.subscribe(topic.clone(), QoS::AtMostOnce).await {
            tracing::warn!(%err, topic, "subscribe failed");
            return;
        }
    }

    loop {
        match inner.register().await {
            Ok(()) => break,
            Err(reason) => {
                tracing::warn!(%reason, retry_in = ?inner.timing.register_retry, "printer refused registration");
                inner.set_link(LinkState::Rejected(reason));
                sleep(inner.timing.register_retry).await;
            }
        }
    }
    *inner.last_pong.lock().unwrap() = Instant::now();
    inner.registered.store(true, Ordering::Relaxed);
    inner.set_link(LinkState::Registered);
    tracing::info!(client_id = %inner.client_id, "registered with printer");

    for method in [
        methods::GET_ATTRIBUTES,
        methods::GET_STATUS,
        methods::GET_CANVAS_STATUS,
    ] {
        if let Err(err) = inner.request(method, Empty {}).await {
            tracing::warn!(%err, method, "initial query failed");
        }
    }

    let timing = inner.timing;
    let mut heartbeat = interval(timing.heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut canvas = interval_at(
        Instant::now() + timing.canvas_refresh,
        timing.canvas_refresh,
    );
    let mut full_status = interval_at(
        Instant::now() + timing.full_status_refresh,
        timing.full_status_refresh,
    );
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let silent_for = inner.last_pong.lock().unwrap().elapsed();
                if silent_for > timing.heartbeat_timeout {
                    tracing::warn!(?silent_for, "no heartbeat reply, forcing reconnect");
                    inner.registered.store(false, Ordering::Relaxed);
                    if let Err(err) = inner.mqtt.disconnect().await {
                        tracing::warn!(%err, "disconnect request failed");
                    }
                    return;
                }
                let ping = inner
                    .mqtt
                    .publish(inner.topics.request.clone(), QoS::AtMostOnce, false, br#"{"type":"PING"}"#.to_vec())
                    .await;
                if let Err(err) = ping {
                    tracing::warn!(%err, "heartbeat publish failed");
                }
            }
            _ = canvas.tick() => {
                if let Err(err) = inner.request(methods::GET_CANVAS_STATUS, Empty {}).await {
                    tracing::warn!(%err, "canvas refresh failed");
                }
            }
            _ = full_status.tick() => {
                if let Err(err) = inner.request(methods::GET_STATUS, Empty {}).await {
                    tracing::warn!(%err, "periodic status refresh failed");
                }
            }
        }
    }
}

impl Inner {
    async fn register(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        *self.register_waiter.lock().unwrap() = Some(tx);
        let payload = serde_json::json!({
            "client_id": self.client_id,
            "request_id": self.request_id,
        })
        .to_string();
        self.mqtt
            .publish(
                self.topics.register.clone(),
                QoS::AtMostOnce,
                false,
                payload,
            )
            .await
            .map_err(|err| err.to_string())?;
        match timeout(self.timing.register_timeout, rx).await {
            Ok(Ok(error)) if error == "ok" => Ok(()),
            Ok(Ok(error)) => Err(error),
            Ok(Err(_)) => Err("registration interrupted".into()),
            Err(_) => Err(format!(
                "no registration reply within {:?}",
                self.timing.register_timeout
            )),
        }
    }

    async fn request<P: Serialize>(
        &self,
        method: u32,
        params: P,
    ) -> Result<Envelope, CommandError> {
        if !self.registered.load(Ordering::Relaxed) {
            return Err(CommandError::NotRegistered);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_vec(&Request { id, method, params })
            .map_err(|source| CommandError::Decode { method, source })?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let published = self
            .mqtt
            .publish(self.topics.request.clone(), QoS::AtMostOnce, false, payload)
            .await;
        if let Err(err) = published {
            self.pending.lock().unwrap().remove(&id);
            return Err(err.into());
        }

        let outcome = timeout(self.timing.command_timeout, rx).await;
        self.pending.lock().unwrap().remove(&id);
        let envelope = match outcome {
            Ok(Ok(envelope)) => envelope,
            Ok(Err(_)) => return Err(CommandError::ConnectionLost { method }),
            Err(_) => return Err(CommandError::Timeout { method }),
        };
        match envelope.error_code() {
            Some(code) if code != error_code::SUCCESS => {
                Err(CommandError::Printer { method, code })
            }
            _ => Ok(envelope),
        }
    }

    /// Called from inside the event loop, so it must never await: a full request channel would
    /// deadlock the loop that drains it. Follow-up requests are spawned instead.
    fn route(self: &Arc<Self>, topic: &str, payload: &[u8]) {
        if topic == self.topics.register_response {
            #[derive(Deserialize)]
            struct RegisterReply {
                #[serde(default)]
                error: String,
            }
            let error = serde_json::from_slice::<RegisterReply>(payload)
                .map(|reply| reply.error)
                .unwrap_or_else(|err| format!("unreadable registration reply: {err}"));
            if let Some(waiter) = self.register_waiter.lock().unwrap().take() {
                let _ = waiter.send(error);
            }
            return;
        }

        let envelope: Envelope = match serde_json::from_slice(payload) {
            Ok(envelope) => envelope,
            Err(err) => {
                tracing::debug!(topic, %err, "ignoring message that is not a JSON envelope");
                return;
            }
        };
        if topic == self.topics.response {
            self.on_response(envelope);
        } else if topic == self.topics.status {
            self.on_status_event(envelope);
        }
    }

    fn on_response(&self, envelope: Envelope) {
        if envelope.is_pong() {
            *self.last_pong.lock().unwrap() = Instant::now();
            return;
        }
        let succeeded = envelope
            .error_code()
            .is_none_or(|code| code == error_code::SUCCESS);
        if succeeded {
            match envelope.method {
                Some(methods::GET_STATUS) => {
                    let mut cache = self.cache.lock().unwrap();
                    cache.apply_full(envelope.id, envelope.result.clone());
                    self.publish_status(&cache);
                }
                Some(methods::GET_CANVAS_STATUS) => {
                    match CanvasStatus::deserialize(&envelope.result) {
                        Ok(status) => self
                            .snapshot
                            .send_modify(|s| s.canvas = Some(status.canvas_info)),
                        Err(err) => tracing::warn!(%err, "unreadable canvas status"),
                    }
                }
                Some(methods::GET_ATTRIBUTES) => {
                    self.update_attributes(envelope.result.clone(), true)
                }
                _ => {}
            }
        }
        if let Some(id) = envelope.id
            && let Some(waiter) = self.pending.lock().unwrap().remove(&id)
        {
            let _ = waiter.send(envelope);
        }
    }

    fn on_status_event(self: &Arc<Self>, envelope: Envelope) {
        match envelope.method {
            Some(methods::EVENT_STATUS) => {
                let needs_full_frame = {
                    let mut cache = self.cache.lock().unwrap();
                    let needs = cache.apply_delta(envelope.id, envelope.result);
                    self.publish_status(&cache);
                    needs
                };
                if needs_full_frame {
                    self.refresh_full_status();
                }
            }
            Some(methods::EVENT_ATTRIBUTES) => self.update_attributes(envelope.result, false),
            _ => {}
        }
    }

    fn refresh_full_status(self: &Arc<Self>) {
        if !self.registered.load(Ordering::Relaxed)
            || self.full_refresh_in_flight.swap(true, Ordering::Relaxed)
        {
            return;
        }
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(err) = inner.request(methods::GET_STATUS, Empty {}).await {
                tracing::warn!(%err, "full status refresh failed");
            }
            inner.full_refresh_in_flight.store(false, Ordering::Relaxed);
        });
    }

    /// A view built before the first full frame would show defaults (status 0 reads as
    /// "initializing") for every field no delta has mentioned yet.
    fn publish_status(&self, cache: &StatusCache) {
        if !cache.has_full_frame() {
            return;
        }
        match cache.view() {
            Ok(view) => self.snapshot.send_modify(|s| s.status = Some(view)),
            Err(err) => tracing::warn!(%err, "status frame does not fit the expected shape"),
        }
    }

    fn update_attributes(&self, result: Value, replace: bool) {
        let mut raw = self.attributes.lock().unwrap();
        if replace {
            *raw = result;
        } else {
            deep_merge(&mut raw, result);
        }
        match Attributes::deserialize(&*raw) {
            Ok(attributes) => self
                .snapshot
                .send_modify(|s| s.attributes = Some(attributes)),
            Err(err) => tracing::warn!(%err, "unreadable printer attributes"),
        }
    }

    fn set_link(&self, link: LinkState) {
        self.snapshot.send_if_modified(|s| {
            if s.link == link {
                return false;
            }
            s.link = link;
            true
        });
    }

    fn on_disconnect(&self, reason: String) {
        self.registered.store(false, Ordering::Relaxed);
        self.full_refresh_in_flight.store(false, Ordering::Relaxed);
        // Dropping the senders fails every in-flight request with `ConnectionLost`.
        self.pending.lock().unwrap().clear();
        self.register_waiter.lock().unwrap().take();
        self.set_link(LinkState::Disconnected(reason));
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn random_u64() -> u64 {
    RandomState::new().hash_one(SystemTime::now())
}

/// Same shape as the printer's own web interface: `0cli`, the last five hex digits of the
/// millisecond clock, random hex, cut to ten characters.
fn client_id() -> String {
    let clock = format!("{:x}", unix_millis());
    let tail = &clock[clock.len().saturating_sub(5)..];
    let mut id = format!("0cli{tail}{:03x}", random_u64() & 0xfff);
    id.truncate(10);
    id
}

fn request_id() -> String {
    format!("{:016x}{:x}", random_u64(), unix_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_shape() {
        let id = client_id();
        assert_eq!(id.len(), 10);
        assert!(id.starts_with("0cli"));
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn topics_follow_protocol() {
        let topics = Topics::new("SN1", "0cliabcdef", "req1");
        assert_eq!(topics.register, "elegoo/SN1/api_register");
        assert_eq!(
            topics.register_response,
            "elegoo/SN1/req1/register_response"
        );
        assert_eq!(topics.request, "elegoo/SN1/0cliabcdef/api_request");
        assert_eq!(topics.response, "elegoo/SN1/0cliabcdef/api_response");
        assert_eq!(topics.status, "elegoo/SN1/api_status");
    }
}
