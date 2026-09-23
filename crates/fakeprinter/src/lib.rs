//! A stand-in Centauri Carbon 2 for tests and local development: an MQTT broker speaking the
//! printer's topic protocol, the stock firmware's `PUT /upload` endpoint, and an MJPEG camera.
//!
//! It models the documented protocol, not the real firmware, so passing against it proves the
//! client follows the documentation and nothing more.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::{SocketAddr, TcpListener as StdTcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::bail;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::put,
};
use bytes::Bytes;
use rumqttd::{
    Broker, Config, ConnectionSettings, Notification, RouterConfig, ServerSettings, local::LinkTx,
};
use serde_json::{Value, json};

pub const USERNAME: &str = "elegoo";

const AMBIENT: f64 = 25.0;
const NOZZLE_TARGET: f64 = 210.0;
const BED_TARGET: f64 = 60.0;
const SIMULATED_LAYERS: u64 = 150;

#[derive(Debug, Clone)]
pub struct Options {
    pub serial: String,
    pub password: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            serial: "FAKECC2SN0001".into(),
            password: "123456".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Upload {
    pub filename: String,
    pub md5_header: Option<String>,
    pub bytes: Vec<u8>,
}

struct PartialUpload {
    md5_header: Option<String>,
    bytes: Vec<u8>,
}

struct PrinterState {
    status: i64,
    sub_status: i64,
    progress: i64,
    filename: String,
    uuid: String,
    sequence: u64,
    task_counter: u64,
    trays: Vec<Value>,
    files: HashMap<String, Vec<u8>>,
    partial_uploads: HashMap<String, PartialUpload>,
    uploads: Vec<Upload>,
    started: Vec<Value>,
    /// Finished prints, newest last, as method 1036 reports them.
    history: Vec<Value>,
    led: i64,
    /// `[temperature, target]`.
    extruder: [f64; 2],
    heater_bed: [f64; 2],
    /// Seconds a simulated print has run, not counting pauses.
    elapsed: u64,
    registration_reply: String,
    answer_pings: bool,
    requests_seen: Vec<u32>,
}

struct Shared {
    serial: String,
    state: Mutex<PrinterState>,
    link_tx: Mutex<LinkTx>,
    camera_connections: AtomicUsize,
    camera_connects_total: AtomicUsize,
}

pub struct FakePrinter {
    pub serial: String,
    pub password: String,
    pub mqtt_addr: SocketAddr,
    pub upload_addr: SocketAddr,
    pub camera_addr: SocketAddr,
    shared: Arc<Shared>,
}

impl FakePrinter {
    pub async fn start(options: Options) -> anyhow::Result<Self> {
        let mqtt_addr = free_local_addr()?;
        let mut broker = Broker::new(broker_config(mqtt_addr, options.password.clone()));
        let (mut link_tx, mut link_rx) = broker.link("fakeprinter")?;
        std::thread::Builder::new()
            .name("fakeprinter-broker".into())
            .spawn(move || {
                if let Err(err) = broker.start() {
                    tracing::error!(%err, "fake printer broker stopped");
                }
            })?;
        wait_for_listener(mqtt_addr).await?;

        let serial = options.serial.clone();
        link_tx.subscribe(format!("elegoo/{serial}/api_register"))?;
        link_tx.subscribe(format!("elegoo/{serial}/+/api_request"))?;

        let shared = Arc::new(Shared {
            serial: serial.clone(),
            state: Mutex::new(PrinterState::new()),
            link_tx: Mutex::new(link_tx),
            camera_connections: AtomicUsize::new(0),
            camera_connects_total: AtomicUsize::new(0),
        });

        let handler = Arc::clone(&shared);
        tokio::spawn(async move {
            loop {
                match link_rx.next().await {
                    Ok(Some(Notification::Forward(forward))) => {
                        let topic = String::from_utf8_lossy(&forward.publish.topic).into_owned();
                        handler.handle(&topic, &forward.publish.payload);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(%err, "fake printer link closed");
                        return;
                    }
                }
            }
        });

        let upload_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let upload_addr = upload_listener.local_addr()?;
        let upload_app = Router::new()
            .route("/upload", put(upload))
            .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
            .with_state(Arc::clone(&shared));
        tokio::spawn(async move { axum::serve(upload_listener, upload_app).await });

        let camera_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let camera_addr = camera_listener.local_addr()?;
        let camera_app = Router::new()
            .fallback(camera)
            .with_state(Arc::clone(&shared));
        tokio::spawn(async move { axum::serve(camera_listener, camera_app).await });

        Ok(Self {
            serial,
            password: options.password,
            mqtt_addr,
            upload_addr,
            camera_addr,
            shared,
        })
    }

    pub fn set_registration_reply(&self, reply: &str) {
        self.shared.state.lock().unwrap().registration_reply = reply.to_owned();
    }

    pub fn set_answer_pings(&self, answer: bool) {
        self.shared.state.lock().unwrap().answer_pings = answer;
    }

    pub fn set_tray(&self, tray_id: usize, filament_type: &str, color: &str, status: i64) {
        let mut state = self.shared.state.lock().unwrap();
        state.trays[tray_id] = tray(tray_id, filament_type, color, status);
    }

    pub fn add_file(&self, filename: &str, bytes: &[u8]) {
        self.shared
            .state
            .lock()
            .unwrap()
            .files
            .insert(filename.to_owned(), bytes.to_vec());
    }

    pub fn uploads(&self) -> Vec<Upload> {
        self.shared.state.lock().unwrap().uploads.clone()
    }

    /// `params` of every accepted `START_PRINT`.
    pub fn started_prints(&self) -> Vec<Value> {
        self.shared.state.lock().unwrap().started.clone()
    }

    pub fn requests_seen(&self) -> Vec<u32> {
        self.shared.state.lock().unwrap().requests_seen.clone()
    }

    pub fn camera_connections(&self) -> usize {
        self.shared.camera_connections.load(Ordering::Relaxed)
    }

    pub fn camera_connects_total(&self) -> usize {
        self.shared.camera_connects_total.load(Ordering::Relaxed)
    }

    pub fn set_progress(&self, progress: i64) {
        self.shared.state.lock().unwrap().progress = progress;
        self.shared.push_delta(json!({
            "machine_status": {"progress": progress},
            "print_status": {"progress": progress},
        }));
    }

    /// As firmware 02.01.00.00 ends a print: plain idle, no completion sub-status, and the
    /// filename cleared. Only the task history records that it finished.
    pub fn complete_print(&self) {
        self.shared.complete_print();
    }

    /// Runs every started print to completion over `length`: progress, layer and remaining time
    /// advance once a second (not while paused) and the heaters warm to typical PLA targets and
    /// cool afterwards. For local development; the tests drive prints by hand.
    pub fn simulate_prints(&self, length: Duration) {
        let shared = Arc::clone(&self.shared);
        let total = length.as_secs().max(1);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                if shared.simulate_second(total) {
                    shared.complete_print();
                }
            }
        });
    }

    /// Publishes an arbitrary status delta, e.g. to exercise out-of-order sequences.
    pub fn push_raw_status(&self, envelope: Value) {
        self.shared.publish("api_status", &envelope);
    }
}

/// One second of a heater moving a fifth of the way to its target, or back to ambient when off.
fn approach(temperature: f64, target: f64) -> [f64; 2] {
    let goal = if target > 0.0 { target } else { AMBIENT };
    let next = temperature + (goal - temperature) * 0.2;
    let next = if (goal - next).abs() < 0.5 {
        goal
    } else {
        next
    };
    [(next * 10.0).round() / 10.0, target]
}

impl PrinterState {
    /// Zero unless a print is being simulated.
    fn layer(&self) -> u64 {
        if self.status == 2 && self.elapsed > 0 {
            (self.progress.clamp(0, 100) as u64 * SIMULATED_LAYERS / 100).max(1)
        } else {
            0
        }
    }

    fn new() -> Self {
        Self {
            status: 1,
            sub_status: 0,
            progress: 0,
            filename: String::new(),
            uuid: String::new(),
            sequence: 100,
            task_counter: 0,
            trays: vec![
                tray(0, "PLA", "#FFFFFF", 1),
                tray(1, "PLA", "#000000", 1),
                tray(2, "PETG", "#FF0000", 1),
                tray(3, "", "", 0),
            ],
            files: HashMap::new(),
            partial_uploads: HashMap::new(),
            uploads: Vec::new(),
            started: Vec::new(),
            history: Vec::new(),
            led: 0,
            extruder: [AMBIENT, 0.0],
            heater_bed: [AMBIENT, 0.0],
            elapsed: 0,
            registration_reply: "ok".into(),
            answer_pings: true,
            requests_seen: Vec::new(),
        }
    }

    fn full_status(&self) -> Value {
        json!({
            "sequence": self.sequence,
            "machine_status": {
                "status": self.status,
                "sub_status": self.sub_status,
                "exception_status": [],
                "progress": self.progress,
            },
            "print_status": {
                "filename": self.filename,
                "uuid": self.uuid,
                "current_layer": self.layer(),
                "total_layer": if self.layer() > 0 { SIMULATED_LAYERS } else { 0 },
                "print_duration": self.elapsed,
                "total_duration": self.elapsed,
                "remaining_time_sec": 0,
                "progress": self.progress,
                "state": if self.status == 2 { "printing" } else { "standby" },
            },
            "extruder": {"temperature": self.extruder[0], "target": self.extruder[1]},
            "heater_bed": {"temperature": self.heater_bed[0], "target": self.heater_bed[1]},
            "external_device": {"camera": true, "u_disk": false, "type": "0303"},
            "led": {"status": self.led},
        })
    }
}

impl Shared {
    fn complete_print(&self) {
        {
            let mut state = self.state.lock().unwrap();
            let finished = json!({
                "task_id": state.uuid,
                "task_name": state.filename,
                "task_status": 1,
                "begin_time": 0,
                "end_time": state.task_counter,
            });
            state.history.push(finished);
            state.status = 1;
            state.sub_status = 0;
            state.progress = 0;
            state.filename = String::new();
            state.uuid = String::new();
        }
        self.push_delta(json!({
            "machine_status": {"status": 1, "sub_status": 0, "progress": 0},
            "print_status": {"filename": "", "uuid": "", "progress": 0, "state": "standby"},
        }));
    }

    /// Returns whether the simulated print just reached its end.
    fn simulate_second(&self, total: u64) -> bool {
        let (delta, done) = {
            let mut state = self.state.lock().unwrap();
            let printing = state.status == 2;
            let (nozzle, bed) = if printing {
                (NOZZLE_TARGET, BED_TARGET)
            } else {
                (0.0, 0.0)
            };
            let heaters = (state.extruder, state.heater_bed);
            state.extruder = approach(state.extruder[0], nozzle);
            state.heater_bed = approach(state.heater_bed[0], bed);
            let mut delta = json!({});
            if heaters != (state.extruder, state.heater_bed) {
                delta["extruder"] =
                    json!({"temperature": state.extruder[0], "target": state.extruder[1]});
                delta["heater_bed"] =
                    json!({"temperature": state.heater_bed[0], "target": state.heater_bed[1]});
            }
            if printing && state.sub_status == 2075 {
                state.elapsed += 1;
                state.progress = i64::try_from(state.elapsed * 100 / total).unwrap_or(100);
                delta["machine_status"] = json!({"progress": state.progress});
                delta["print_status"] = json!({
                    "progress": state.progress,
                    "current_layer": state.layer(),
                    "total_layer": SIMULATED_LAYERS,
                    "print_duration": state.elapsed,
                    "total_duration": state.elapsed,
                    "remaining_time_sec": total.saturating_sub(state.elapsed),
                });
            }
            (delta, printing && state.elapsed >= total)
        };
        if delta.as_object().is_some_and(|fields| !fields.is_empty()) {
            self.push_delta(delta);
        }
        done
    }

    fn topic(&self, suffix: &str) -> String {
        format!("elegoo/{}/{suffix}", self.serial)
    }

    fn publish(&self, suffix: &str, payload: &Value) {
        let topic = self.topic(suffix);
        if let Err(err) = self
            .link_tx
            .lock()
            .unwrap()
            .publish(topic.clone(), payload.to_string())
        {
            tracing::error!(%err, topic, "fake printer publish failed");
        }
    }

    fn push_delta(&self, mut delta: Value) {
        let sequence = {
            let mut state = self.state.lock().unwrap();
            state.sequence += 1;
            state.sequence
        };
        delta["sequence"] = json!(sequence);
        self.publish(
            "api_status",
            &json!({"id": sequence, "method": 6000, "result": delta}),
        );
    }

    fn handle(&self, topic: &str, payload: &[u8]) {
        let Ok(message) = serde_json::from_slice::<Value>(payload) else {
            return;
        };

        if topic == self.topic("api_register") {
            let reply = self.state.lock().unwrap().registration_reply.clone();
            let request_id = message["request_id"].as_str().unwrap_or_default();
            self.publish(
                &format!("{request_id}/register_response"),
                &json!({"client_id": message["client_id"], "error": reply}),
            );
            return;
        }

        let Some(client_id) = topic
            .strip_prefix(&format!("elegoo/{}/", self.serial))
            .and_then(|rest| rest.strip_suffix("/api_request"))
        else {
            return;
        };
        let response_suffix = format!("{client_id}/api_response");

        if message["type"] == "PING" {
            if self.state.lock().unwrap().answer_pings {
                self.publish(&response_suffix, &json!({"type": "PONG"}));
            }
            return;
        }

        let id = message["id"].clone();
        let Some(method) = message["method"].as_u64().map(|m| m as u32) else {
            return;
        };
        let (result, delta) = self.execute(method, &message["params"]);
        self.publish(
            &response_suffix,
            &json!({"id": id, "method": method, "result": result}),
        );
        if let Some(delta) = delta {
            self.push_delta(delta);
        }
    }

    /// Returns the response `result` and, for commands that change state, a status delta.
    fn execute(&self, method: u32, params: &Value) -> (Value, Option<Value>) {
        let mut state = self.state.lock().unwrap();
        state.requests_seen.push(method);
        let filename = params["filename"].as_str().unwrap_or_default().to_owned();
        match method {
            1001 => (
                json!({
                    "error_code": 0,
                    "hostname": "Fake Carbon 2",
                    "machine_model": "Centauri Carbon 2",
                    "sn": self.serial,
                    "software_version": {"ota_version": "0.0.0-fake"},
                    "camera_connected": true,
                    "max_video_connections": 1,
                    "video_connections": 0,
                }),
                None,
            ),
            1002 => {
                let mut status = state.full_status();
                status["error_code"] = json!(0);
                (status, None)
            }
            2005 => (
                json!({
                    "error_code": 0,
                    "canvas_info": {
                        "active_canvas_id": 0,
                        "active_tray_id": -1,
                        "auto_refill": false,
                        "canvas_list": [{"canvas_id": 0, "connected": 1, "tray_list": state.trays}],
                    }
                }),
                None,
            ),
            1036 => {
                let mut tasks = state.history.clone();
                tasks.reverse();
                (json!({"error_code": 0, "history_task_list": tasks}), None)
            }
            1044 => {
                let files: Vec<Value> = state
                    .files
                    .iter()
                    .map(|(name, bytes)| json!({"name": name, "size": bytes.len()}))
                    .collect();
                (
                    json!({"error_code": 0, "total": files.len(), "files": files}),
                    None,
                )
            }
            1046 => match state.files.get(&filename) {
                Some(bytes) => (
                    json!({
                        "error_code": 0,
                        "filename": filename,
                        "size": bytes.len(),
                        "TotalLayers": 10,
                        "layer": 10,
                        "print_time": 600,
                        "total_filament_used": 1.5,
                        "color_map": [{"color": "#FFFFFF", "name": "PLA", "t": 0}],
                    }),
                    None,
                ),
                None => (json!({"error_code": 1021}), None),
            },
            1020 => {
                if state.status != 1 {
                    return (json!({"error_code": 1009}), None);
                }
                if !state.files.contains_key(&filename) {
                    return (json!({"error_code": 1021}), None);
                }
                state.task_counter += 1;
                state.status = 2;
                state.sub_status = 2075;
                state.progress = 0;
                state.elapsed = 0;
                state.filename = filename.clone();
                state.uuid = format!("fake-task-{}", state.task_counter);
                state.started.push(params.clone());
                (
                    json!({"error_code": 0}),
                    Some(json!({
                        "machine_status": {"status": 2, "sub_status": 2075, "progress": 0},
                        "print_status": {"filename": filename, "uuid": state.uuid, "progress": 0, "state": "printing"},
                    })),
                )
            }
            1021 if state.status == 2 => {
                state.sub_status = 2502;
                (
                    json!({"error_code": 0}),
                    Some(
                        json!({"machine_status": {"sub_status": 2502}, "print_status": {"state": "paused"}}),
                    ),
                )
            }
            1023 if state.status == 2 => {
                state.sub_status = 2075;
                (
                    json!({"error_code": 0}),
                    Some(
                        json!({"machine_status": {"sub_status": 2075}, "print_status": {"state": "printing"}}),
                    ),
                )
            }
            1022 if state.status == 2 => {
                state.status = 1;
                state.sub_status = 2504;
                (
                    json!({"error_code": 0}),
                    Some(
                        json!({"machine_status": {"status": 1, "sub_status": 2504}, "print_status": {"state": "cancelled"}}),
                    ),
                )
            }
            1021..=1023 => (json!({"error_code": 1010}), None),
            1029 => {
                state.led = params["power"].as_i64().unwrap_or_default();
                (
                    json!({"error_code": 0}),
                    Some(json!({"led": {"status": state.led}})),
                )
            }
            1042 => (json!({"error_code": 0}), None),
            _ => (json!({"error_code": 1001}), None),
        }
    }
}

async fn upload(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let header_str = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let Some(filename) = header_str("x-file-name") else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error_code": 1003})));
    };
    let Some((start, end, total)) = header_str("content-range")
        .as_deref()
        .and_then(parse_content_range)
    else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error_code": 1003})));
    };
    if end + 1 - start != body.len() as u64 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error_code": 9000})));
    }

    let mut state = shared.state.lock().unwrap();
    let partial = state
        .partial_uploads
        .entry(filename.clone())
        .or_insert_with(|| PartialUpload {
            md5_header: header_str("x-file-md5"),
            bytes: Vec::new(),
        });
    if partial.bytes.len() as u64 != start {
        state.partial_uploads.remove(&filename);
        return (StatusCode::OK, Json(json!({"error_code": 9000})));
    }
    partial.bytes.extend_from_slice(&body);

    if end + 1 == total {
        let finished = state
            .partial_uploads
            .remove(&filename)
            .expect("inserted above");
        state.files.insert(filename.clone(), finished.bytes.clone());
        state.uploads.push(Upload {
            filename,
            md5_header: finished.md5_header,
            bytes: finished.bytes,
        });
    }
    (
        StatusCode::OK,
        Json(json!({"error_code": 0, "offset": end})),
    )
}

fn parse_content_range(raw: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = raw.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

const CAMERA_BOUNDARY: &str = "frame_boundary";

async fn camera(State(shared): State<Arc<Shared>>) -> Response {
    struct Connection(Arc<Shared>);
    impl Drop for Connection {
        fn drop(&mut self) {
            self.0.camera_connections.fetch_sub(1, Ordering::Relaxed);
        }
    }

    shared.camera_connections.fetch_add(1, Ordering::Relaxed);
    shared.camera_connects_total.fetch_add(1, Ordering::Relaxed);
    let connection = Connection(shared);
    let frames = futures::stream::unfold((connection, 0u32), |(connection, n)| async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Some((Ok::<_, Infallible>(camera_part(n)), (connection, n + 1)))
    });
    (
        [(
            header::CONTENT_TYPE,
            format!("multipart/x-mixed-replace; boundary={CAMERA_BOUNDARY}"),
        )],
        Body::from_stream(frames),
    )
        .into_response()
}

/// A multipart part wrapping a minimal JPEG-shaped payload (SOI, frame counter, EOI).
pub fn camera_part(n: u32) -> Bytes {
    let mut jpeg = vec![0xFF, 0xD8];
    jpeg.extend_from_slice(&n.to_be_bytes());
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    let mut part = format!(
        "--{CAMERA_BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    )
    .into_bytes();
    part.extend_from_slice(&jpeg);
    part.extend_from_slice(b"\r\n");
    Bytes::from(part)
}

fn tray(tray_id: usize, filament_type: &str, color: &str, status: i64) -> Value {
    json!({
        "tray_id": tray_id,
        "brand": if status == 0 { "" } else { "ELEGOO" },
        "filament_type": filament_type,
        "filament_name": filament_type,
        "filament_color": color,
        "min_nozzle_temp": 190,
        "max_nozzle_temp": 230,
        "status": status,
    })
}

fn broker_config(listen: SocketAddr, password: String) -> Config {
    let mut connections = ConnectionSettings {
        connection_timeout_ms: 5000,
        max_payload_size: 8 * 1024 * 1024,
        max_inflight_count: 100,
        auth: None,
        external_auth: None,
        dynamic_filters: true,
    };
    connections.set_auth_handler(move |_client_id, username, pass| {
        let accepted = username == USERNAME && pass == password;
        async move { accepted }
    });
    let server = ServerSettings {
        name: "fakeprinter-v4".into(),
        listen,
        tls: None,
        next_connection_delay_ms: 1,
        connections,
    };
    Config {
        id: 0,
        router: RouterConfig {
            max_connections: 32,
            max_outgoing_packet_count: 200,
            max_segment_size: 16 * 1024 * 1024,
            max_segment_count: 10,
            ..RouterConfig::default()
        },
        v4: Some(HashMap::from([("1".to_owned(), server)])),
        ..Config::default()
    }
}

/// rumqttd takes a fixed listen address and does not report an OS-assigned port, so one is
/// reserved and released just before the broker binds it.
fn free_local_addr() -> anyhow::Result<SocketAddr> {
    let listener = StdTcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?)
}

async fn wait_for_listener(addr: SocketAddr) -> anyhow::Result<()> {
    for _ in 0..100 {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bail!("fake broker never listened on {addr}")
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 0-99/100"), Some((0, 99, 100)));
        assert_eq!(
            parse_content_range("bytes 100-199/300"),
            Some((100, 199, 300))
        );
        assert_eq!(parse_content_range("bytes 5-4/100"), None);
        assert_eq!(parse_content_range("bytes 0-100/100"), None);
        assert_eq!(parse_content_range("0-99/100"), None);
    }

    #[tokio::test]
    async fn starts_and_accepts_tcp() {
        let printer = FakePrinter::start(Options::default())
            .await
            .context("start")
            .unwrap();
        TcpStream::connect(printer.mqtt_addr).unwrap();
        TcpStream::connect(printer.upload_addr).unwrap();
        TcpStream::connect(printer.camera_addr).unwrap();
    }

    #[tokio::test]
    async fn a_simulated_print_heats_advances_pauses_and_finishes() {
        let printer = FakePrinter::start(Options::default()).await.unwrap();
        printer.add_file("a.gcode", b"G28");
        let shared = &printer.shared;
        let status = || shared.state.lock().unwrap().full_status();
        assert!(!shared.simulate_second(4), "nothing to simulate while idle");
        shared.execute(1020, &json!({"filename": "a.gcode"}));

        assert!(!shared.simulate_second(4));
        let running = status();
        assert_eq!(running["machine_status"]["progress"], 25);
        assert_eq!(running["extruder"]["target"], NOZZLE_TARGET);
        assert!(running["extruder"]["temperature"].as_f64().unwrap() > AMBIENT);
        assert_eq!(running["print_status"]["total_layer"], SIMULATED_LAYERS);

        shared.execute(1021, &json!({}));
        assert!(!shared.simulate_second(4));
        assert_eq!(status()["machine_status"]["progress"], 25, "paused");
        shared.execute(1023, &json!({}));

        assert!(!shared.simulate_second(4));
        assert!(!shared.simulate_second(4));
        assert!(
            shared.simulate_second(4),
            "the fourth printing second ends it"
        );
        shared.complete_print();
        let done = status();
        assert_eq!(done["machine_status"]["status"], 1);
        assert_eq!(done["print_status"]["current_layer"], 0);
        assert!(!shared.simulate_second(4));
        assert_eq!(status()["extruder"]["target"], 0.0, "the heaters turn off");
    }
}
