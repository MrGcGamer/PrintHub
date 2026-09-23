//! Runs the fake printer until Ctrl-C and prints the environment that points PrintHub at it.
//! `FAKE_PRINT_SECONDS=120` makes every started print run to completion in that many seconds.

use fakeprinter::{FakePrinter, Options};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let printer = FakePrinter::start(Options::default()).await?;
    if let Some(seconds) = std::env::var("FAKE_PRINT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        printer.simulate_prints(std::time::Duration::from_secs(seconds));
    }
    println!("PRINTER_HOST=127.0.0.1");
    println!("PRINTER_SN={}", printer.serial);
    println!("PRINTER_ACCESS_CODE={}", printer.password);
    println!("PRINTER_MQTT_PORT={}", printer.mqtt_addr.port());
    println!("PRINTER_UPLOAD_PORT={}", printer.upload_addr.port());
    println!("PRINTER_CAMERA_PORT={}", printer.camera_addr.port());

    tokio::signal::ctrl_c().await?;
    Ok(())
}
