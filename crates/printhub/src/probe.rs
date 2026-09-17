//! `printhub probe`: checks, one at a time, every printer endpoint the app relies on, and
//! prints what it found. Nothing is written to the printer.

use std::{
    fmt::Display,
    net::{IpAddr, SocketAddr},
    process::ExitCode,
    time::Duration,
};

use tokio::{net::TcpStream, time::timeout};

use crate::{
    camera,
    cc2::{
        ClientConfig, PrinterClient, Timing,
        discovery::{self, DISCOVERY_PORT},
        model::TrayState,
    },
    config::{Config, DEFAULT_ACCESS_CODE},
};

pub async fn run(config: &Config) -> anyhow::Result<ExitCode> {
    let mut report = Report::default();

    let ip = match discovery::resolve(&config.printer_host).await {
        Ok(ip) => {
            report.ok("resolve", format!("{} is {ip}", config.printer_host));
            ip
        }
        Err(err) => {
            report.fail("resolve", err);
            return Ok(report.finish());
        }
    };

    let discovered = match discovery::discover_at(
        SocketAddr::new(ip, DISCOVERY_PORT),
        Duration::from_secs(3),
        2,
    )
    .await
    {
        Ok(info) => {
            report.ok(
                "discovery",
                format!(
                    "{} {:?}, serial {}",
                    info.machine_model, info.host_name, info.sn
                ),
            );
            if info.lan_only() {
                report.ok("lan only mode", "enabled");
            } else {
                report.fail(
                    "lan only mode",
                    "disabled; enable it under Settings > Network on the printer",
                );
            }
            if info.access_code_set() && config.printer_access_code == DEFAULT_ACCESS_CODE {
                report.warn(
                    "access code",
                    "the printer has one set but PRINTER_ACCESS_CODE is not configured",
                );
            }
            Some(info)
        }
        Err(err) => {
            report.warn(
                "discovery",
                format!("{err}; set PRINTER_SN if UDP {DISCOVERY_PORT} is filtered"),
            );
            None
        }
    };

    let Some(serial) = config.printer_sn.clone().or(discovered.map(|info| info.sn)) else {
        report.fail("serial number", "not discovered and PRINTER_SN is not set");
        return Ok(report.finish());
    };

    check_tcp(&mut report, "upload port", ip, config.printer_upload_port).await;

    let (client, supervisor) = PrinterClient::start(ClientConfig {
        host: ip.to_string(),
        port: config.printer_mqtt_port,
        serial,
        password: config.printer_access_code.clone(),
        timing: Timing::default(),
    });
    if let Err(state) = client.wait_until_registered(Duration::from_secs(20)).await {
        report.fail("mqtt register", format!("gave up in state {state:?}"));
        client.shutdown(supervisor).await;
        return Ok(report.finish());
    }
    report.ok("mqtt register", format!("client id {}", client.client_id()));

    match client.attributes().await {
        Ok(attributes) => report.ok(
            "attributes",
            format!(
                "firmware {}, camera connected {}, video connections {}/{}",
                attributes.software_version.ota_version,
                attributes.camera_connected,
                attributes.video_connections,
                attributes.max_video_connections,
            ),
        ),
        Err(err) => report.fail("attributes", err),
    }

    let mut snapshots = client.subscribe();
    let mut printing_file = String::new();
    match timeout(
        Duration::from_secs(5),
        snapshots.wait_for(|snapshot| snapshot.status.is_some()),
    )
    .await
    {
        Ok(Ok(snapshot)) => {
            let status = snapshot.status.as_ref().expect("waited for it");
            printing_file = status.print_status.filename.clone();
            report.ok(
                "status",
                format!(
                    "{:?} (sub-status {}), progress {}%, layer {}/{}, file {:?}",
                    status.machine_status.state(),
                    status.machine_status.sub_status,
                    status.machine_status.progress,
                    status.print_status.current_layer,
                    status.print_status.total_layer,
                    status.print_status.filename,
                ),
            );
        }
        _ => report.fail("status", "no full status frame within 5s"),
    }
    drop(snapshots);

    // The status stream carries no layer total; 1046 is where a file's own metadata lives.
    if !printing_file.is_empty() {
        match client.file_detail(&printing_file).await {
            Ok(detail) => report.ok(
                "file detail",
                format!(
                    "{:?}: layers {:?}, print time {}s, filament {:.1}",
                    printing_file,
                    detail.layers(),
                    detail.print_time,
                    detail.total_filament_used,
                ),
            ),
            Err(err) => report.warn("file detail", format!("1046 for {printing_file:?}: {err}")),
        }
    }

    // On firmware 02.01.00.00 a finished print leaves no trace in the status: idle, sub-status
    // 0, filename cleared. The task history is the only record of how a print ended.
    match client
        .request(
            crate::cc2::methods::PRINT_TASK_LIST,
            serde_json::json!({"page": 1, "page_size": 3}),
        )
        .await
    {
        Ok(envelope) => {
            let raw = envelope.result.to_string();
            report.ok("task list", raw.chars().take(900).collect::<String>());
        }
        Err(err) => report.warn("task list", format!("1036: {err}")),
    }

    match client.canvas().await {
        Ok(canvas) if canvas.canvas_list.is_empty() => {
            report.warn("canvas", "no CANVAS unit reported");
        }
        Ok(canvas) => {
            for unit in &canvas.canvas_list {
                for tray in &unit.tray_list {
                    let state = match tray.state() {
                        TrayState::Empty => "empty".to_owned(),
                        TrayState::Loaded => "loaded".to_owned(),
                        TrayState::Active => "active".to_owned(),
                        TrayState::Unknown(code) => format!("unknown status {code}"),
                    };
                    report.ok(
                        &format!("canvas {} tray {}", unit.canvas_id, tray.tray_id),
                        format!(
                            "{state}: {} {} {}",
                            tray.brand, tray.filament_type, tray.filament_color
                        ),
                    );
                }
            }
        }
        Err(err) => report.fail("canvas", err),
    }

    let http = reqwest::Client::new();
    let camera_url = format!(
        "http://{}/",
        SocketAddr::new(ip, config.printer_camera_port)
    );
    let limit = Duration::from_secs(10);
    match camera::grab_frame(&http, &camera_url, limit).await {
        Ok(frame) => report.ok("camera", describe_frame(&frame)),
        Err(first) => {
            // Whether VIDEO_STREAM (1042) must be sent before the stream delivers is
            // unconfirmed on stock firmware, so the answer is found out here.
            if let Err(err) = client.set_video_stream(true).await {
                report.fail(
                    "camera",
                    format!("{first}; enabling it via 1042 failed: {err}"),
                );
            } else {
                match camera::grab_frame(&http, &camera_url, limit).await {
                    Ok(frame) => report.ok(
                        "camera",
                        format!("{} (only after enabling via 1042)", describe_frame(&frame)),
                    ),
                    Err(second) => report.fail(
                        "camera",
                        format!("{first}; after enabling via 1042: {second}"),
                    ),
                }
            }
        }
    }

    client.shutdown(supervisor).await;
    Ok(report.finish())
}

async fn check_tcp(report: &mut Report, check: &str, ip: IpAddr, port: u16) {
    let addr = SocketAddr::new(ip, port);
    match timeout(Duration::from_secs(3), TcpStream::connect(addr)).await {
        Ok(Ok(_)) => report.ok(check, format!("{addr} accepts connections")),
        Ok(Err(err)) => report.fail(check, format!("{addr}: {err}")),
        Err(_) => report.fail(check, format!("{addr}: no answer within 3s")),
    }
}

fn describe_frame(frame: &[u8]) -> String {
    let kind = if camera::is_jpeg(frame) {
        "JPEG"
    } else {
        "not a JPEG"
    };
    format!("one frame, {} bytes, {kind}", frame.len())
}

#[derive(Default)]
struct Report {
    failed: bool,
}

impl Report {
    fn line(&self, level: &str, check: &str, detail: impl Display) {
        println!("{level:<5} {check:<20} {detail}");
    }

    fn ok(&mut self, check: &str, detail: impl Display) {
        self.line("ok", check, detail);
    }

    fn warn(&mut self, check: &str, detail: impl Display) {
        self.line("warn", check, detail);
    }

    fn fail(&mut self, check: &str, detail: impl Display) {
        self.failed = true;
        self.line("FAIL", check, detail);
    }

    fn finish(self) -> ExitCode {
        if self.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}
