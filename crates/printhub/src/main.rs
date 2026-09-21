use std::{net::Ipv4Addr, process::ExitCode, time::Duration};

use clap::{Parser, Subcommand};
use printhub::{
    config::Config,
    gcode, probe,
    slicer::{self, Plate, SliceSettings, Slicer},
    web,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Shared print queue for an Elegoo Centauri Carbon 2")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server.
    Serve,
    /// Check every printer endpoint the app depends on and print a report.
    Probe,
    /// Exit successfully if the server on LISTEN_ADDR answers; for container health checks.
    Healthcheck,
    /// Slice a test cube with OrcaSlicer and the bundled profiles; needs no printer.
    SliceSelftest,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let default_level = match cli.command {
        Command::Serve => "info",
        Command::Probe | Command::Healthcheck | Command::SliceSelftest => "warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = match cli.command {
        // The self-test never contacts the printer, so it runs without one configured.
        Command::SliceSelftest => Config::from_lookup(|var| {
            std::env::var(var)
                .ok()
                .or_else(|| (var == "PRINTER_HOST").then(|| "unused".into()))
        }),
        Command::Serve | Command::Probe | Command::Healthcheck => Config::from_env(),
    };
    let config = match config {
        Ok(config) => config,
        Err(err) => {
            eprintln!("configuration error: {err}");
            return ExitCode::from(2);
        }
    };

    let outcome = match cli.command {
        Command::Serve => web::serve(config).await.map(|()| ExitCode::SUCCESS),
        Command::Probe => probe::run(&config).await,
        Command::Healthcheck => Ok(healthcheck(&config).await),
        Command::SliceSelftest => slice_selftest(&config).await,
    };
    outcome.unwrap_or_else(|err| {
        eprintln!("error: {err:#}");
        ExitCode::FAILURE
    })
}

async fn slice_selftest(config: &Config) -> anyhow::Result<ExitCode> {
    anyhow::ensure!(
        config.orca_slicer.is_file(),
        "OrcaSlicer not found at {} (ORCA_SLICER)",
        config.orca_slicer.display()
    );
    let slicer = Slicer::new(
        config.orca_slicer.clone(),
        &config.orca_profiles,
        config.slice_timeout,
    )?;
    let nozzle = config.nozzle;
    let machine = slicer::machine_name(nozzle);
    let settings = SliceSettings {
        nozzle,
        plate: Plate::TexturedPei,
        process: slicer::default_process(nozzle),
        filament: "Elegoo PLA @ECC2".into(),
        color_hex: "#2850DF".into(),
        supports: false,
        infill_percent: 15,
        scale_percent: 100.0,
        auto_orient: false,
    };
    // An incompatible profile otherwise surfaces as a bare exit status from the CLI.
    for (kind, name, compatible) in [
        ("process", &settings.process, slicer.processes(nozzle)),
        ("filament", &settings.filament, slicer.filaments(nozzle)),
    ] {
        anyhow::ensure!(
            compatible.contains(name),
            "no {kind} profile {name:?} for {machine}"
        );
    }

    let workdir = std::env::temp_dir().join(format!("printhub-selftest-{}", std::process::id()));
    tokio::fs::create_dir_all(&workdir).await?;
    let sliced = async {
        let model = workdir.join("cube.stl");
        tokio::fs::write(&model, slicer::cube_stl(20.0)).await?;
        let gcode = slicer.slice(&model, &workdir, &settings).await?;
        anyhow::Ok(gcode::read(&gcode).await?)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&workdir).await;
    let info = sliced?;

    anyhow::ensure!(
        info.total_grams() > 0.0,
        "the G-code reports no filament use, so the profiles lost their density"
    );
    println!(
        "sliced a 20 mm cube for {machine} with {} and {}: {:.1} g",
        settings.process,
        settings.filament,
        info.total_grams()
    );
    Ok(ExitCode::SUCCESS)
}

async fn healthcheck(config: &Config) -> ExitCode {
    let mut addr = config.listen_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(Ipv4Addr::LOCALHOST.into());
    }
    let url = format!("http://{addr}/healthz");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => ExitCode::SUCCESS,
        Ok(response) => {
            eprintln!("{url} answered {}", response.status());
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("{url}: {err}");
            ExitCode::FAILURE
        }
    }
}
