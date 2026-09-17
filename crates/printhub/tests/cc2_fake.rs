//! The printer client against the fake printer. These prove the client follows the documented
//! protocol; only `printhub probe` against the real printer proves the documentation right.

use std::time::Duration;

use fakeprinter::{FakePrinter, Options};
use printhub::{
    camera,
    cc2::{
        ClientConfig, LinkState, PrinterClient, PrinterSnapshot, Timing,
        methods::{SlotMapEntry, error_code},
        model::{MachineState, TrayState, printing_sub_status, task_status},
        upload::{self, Uploader},
    },
};
use serde_json::json;
use tokio::{task::JoinHandle, time::timeout};

const WAIT: Duration = Duration::from_secs(10);

fn fast_timing() -> Timing {
    Timing {
        heartbeat_interval: Duration::from_millis(200),
        heartbeat_timeout: Duration::from_secs(1),
        register_timeout: Duration::from_secs(1),
        register_retry: Duration::from_millis(500),
        command_timeout: Duration::from_secs(2),
        ..Timing::default()
    }
}

async fn printer() -> FakePrinter {
    FakePrinter::start(Options::default()).await.unwrap()
}

fn start_client(printer: &FakePrinter, password: &str) -> (PrinterClient, JoinHandle<()>) {
    PrinterClient::start(ClientConfig {
        host: "127.0.0.1".into(),
        port: printer.mqtt_addr.port(),
        serial: printer.serial.clone(),
        password: password.into(),
        timing: fast_timing(),
    })
}

async fn connect(printer: &FakePrinter) -> (PrinterClient, JoinHandle<()>) {
    let (client, supervisor) = start_client(printer, &printer.password);
    client.wait_until_registered(WAIT).await.expect("registers");
    (client, supervisor)
}

async fn wait_for(
    client: &PrinterClient,
    what: &str,
    predicate: impl FnMut(&PrinterSnapshot) -> bool,
) {
    let mut rx = client.subscribe();
    if timeout(WAIT, rx.wait_for(predicate)).await.is_err() {
        panic!(
            "timed out waiting for {what}; last snapshot: {:?}",
            client.snapshot()
        );
    }
}

fn machine(snapshot: &PrinterSnapshot) -> Option<(MachineState, i64, i64)> {
    snapshot.status.as_ref().map(|s| {
        (
            s.machine_status.state(),
            s.machine_status.sub_status,
            s.machine_status.progress,
        )
    })
}

#[tokio::test]
async fn registers_and_loads_initial_state() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;

    wait_for(&client, "initial state", |s| {
        s.status.is_some() && s.canvas.is_some() && s.attributes.is_some()
    })
    .await;
    let snapshot = client.snapshot();
    assert_eq!(machine(&snapshot).unwrap().0, MachineState::Idle);
    assert_eq!(snapshot.attributes.unwrap().sn, printer.serial);
    let trays = &snapshot.canvas.unwrap().canvas_list[0].tray_list;
    assert_eq!(trays[0].filament_type, "PLA");
    assert_eq!(trays[0].filament_color, "#FFFFFF");
    assert_eq!(trays[0].state(), TrayState::Loaded);
    assert_eq!(trays[3].state(), TrayState::Empty);
}

#[tokio::test]
async fn wrong_password_never_registers() {
    let printer = printer().await;
    let (client, _supervisor) = start_client(&printer, "not-the-code");
    let state = client
        .wait_until_registered(Duration::from_secs(3))
        .await
        .unwrap_err();
    assert!(
        matches!(state, LinkState::Connecting | LinkState::Disconnected(_)),
        "{state:?}"
    );
}

#[tokio::test]
async fn registration_rejection_is_reported_and_retried() {
    let printer = printer().await;
    printer.set_registration_reply("too many clients");
    let (client, _supervisor) = start_client(&printer, &printer.password);

    wait_for(&client, "rejection", |s| {
        s.link == LinkState::Rejected("too many clients".into())
    })
    .await;

    printer.set_registration_reply("ok");
    client
        .wait_until_registered(WAIT)
        .await
        .expect("retry succeeds");
}

#[tokio::test]
async fn print_lifecycle_updates_snapshot() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;
    wait_for(&client, "status", |s| s.status.is_some()).await;
    printer.add_file("cube.gcode", b"G28\n");

    let slot_map = vec![SlotMapEntry {
        t: 0,
        canvas_id: 0,
        tray_id: 1,
    }];
    client.start_print("cube.gcode", slot_map).await.unwrap();
    wait_for(&client, "printing", |s| {
        machine(s).is_some_and(|(state, _, _)| state == MachineState::Printing)
    })
    .await;
    assert_eq!(
        printer.started_prints()[0]["config"]["slot_map"],
        json!([{"t": 0, "canvas_id": 0, "tray_id": 1}])
    );

    printer.set_progress(42);
    wait_for(&client, "progress 42", |s| {
        machine(s).is_some_and(|(_, _, progress)| progress == 42)
    })
    .await;

    client.pause().await.unwrap();
    wait_for(&client, "paused", |s| {
        machine(s).is_some_and(|(_, sub, _)| sub == printing_sub_status::PAUSED)
    })
    .await;
    client.resume().await.unwrap();

    // Firmware 02.01.00.00 ends a print with plain idle and an empty filename: only the task
    // history says it finished.
    printer.complete_print();
    wait_for(&client, "finished", |s| {
        machine(s).is_some_and(|(state, sub, _)| state == MachineState::Idle && sub == 0)
            && s.status
                .as_ref()
                .is_some_and(|s| s.print_status.filename.is_empty())
    })
    .await;
    let history = client.task_history(10).await.unwrap().history_task_list;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].task_name, "cube.gcode");
    assert_eq!(history[0].task_status, task_status::COMPLETED);
}

#[tokio::test]
async fn printer_error_codes_surface() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;

    let missing = client
        .start_print("missing.gcode", vec![])
        .await
        .unwrap_err();
    assert_eq!(
        missing.printer_code(),
        Some(error_code::PRINT_FILE_NOT_FOUND)
    );

    printer.add_file("a.gcode", b"G28\n");
    client.start_print("a.gcode", vec![]).await.unwrap();
    let busy = client.start_print("a.gcode", vec![]).await.unwrap_err();
    assert_eq!(busy.printer_code(), Some(error_code::PRINTER_BUSY));

    let not_printing = printer_and_idle_stop(&printer).await;
    assert_eq!(not_printing, Some(error_code::NOT_PRINTING));
}

async fn printer_and_idle_stop(printer: &FakePrinter) -> Option<i64> {
    printer.complete_print();
    let (client, _supervisor) = connect(printer).await;
    wait_for(&client, "idle", |s| {
        machine(s).is_some_and(|(state, _, _)| state == MachineState::Idle)
    })
    .await;
    client.stop().await.unwrap_err().printer_code()
}

#[tokio::test]
async fn run_of_sequence_gaps_requests_full_status() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;
    wait_for(&client, "status", |s| s.status.is_some()).await;
    let full_requests = || {
        printer
            .requests_seen()
            .iter()
            .filter(|&&m| m == 1002)
            .count()
    };
    let before = full_requests();

    for sequence in [1000, 1002, 1004, 1006, 1008, 1010] {
        printer.push_raw_status(json!({
            "id": sequence,
            "method": 6000,
            "result": {"sequence": sequence, "machine_status": {"progress": 1}},
        }));
    }

    timeout(WAIT, async {
        while full_requests() == before {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a full status request after a run of gaps");
}

#[tokio::test]
async fn silent_printer_forces_reconnect() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;

    printer.set_answer_pings(false);
    wait_for(&client, "link loss", |s| s.link != LinkState::Registered).await;

    printer.set_answer_pings(true);
    client
        .wait_until_registered(WAIT)
        .await
        .expect("reconnects");
}

#[tokio::test]
async fn shutdown_returns_promptly() {
    let printer = printer().await;
    let (client, supervisor) = connect(&printer).await;
    timeout(Duration::from_secs(3), client.shutdown(supervisor))
        .await
        .expect("shutdown completes");
}

#[tokio::test]
async fn chunked_upload_then_file_detail() {
    let printer = printer().await;
    let (client, _supervisor) = connect(&printer).await;

    let data: Vec<u8> = (0..(upload::CHUNK_SIZE * 2 + 12_345))
        .map(|i| (i * 31 % 251) as u8)
        .collect();
    let uploader = Uploader::new(
        reqwest::Client::new(),
        format!("http://{}", printer.upload_addr),
        printer.password.clone(),
    );
    uploader.upload("big.gcode", &data).await.unwrap();

    let uploads = printer.uploads();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].filename, "big.gcode");
    assert_eq!(uploads[0].bytes, data);
    assert_eq!(
        uploads[0].md5_header.as_deref(),
        Some(upload::md5_hex(&data).as_str())
    );

    let detail = client.file_detail("big.gcode").await.unwrap();
    assert_eq!(detail.size as usize, data.len());
}

#[tokio::test]
async fn camera_frame_from_fake() {
    let printer = printer().await;
    let frame = camera::grab_frame(
        &reqwest::Client::new(),
        &format!("http://{}/", printer.camera_addr),
        WAIT,
    )
    .await
    .unwrap();
    assert!(camera::is_jpeg(&frame));
}
