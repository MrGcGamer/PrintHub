use std::{net::Ipv4Addr, process::ExitCode, time::Duration};

use clap::{Parser, Subcommand};
use printhub::{config::Config, probe, web};
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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let default_level = match cli.command {
        Command::Serve => "info",
        Command::Probe | Command::Healthcheck => "warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = match Config::from_env() {
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
    };
    outcome.unwrap_or_else(|err| {
        eprintln!("error: {err:#}");
        ExitCode::FAILURE
    })
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
