use std::process::ExitCode;

use clap::{Parser, Subcommand};
use printhub::{config::Config, probe};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Shared print queue for an Elegoo Centauri Carbon 2")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check every printer endpoint the app depends on and print a report.
    Probe,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("configuration error: {err}");
            return ExitCode::from(2);
        }
    };

    let outcome = match cli.command {
        Command::Probe => probe::run(&config).await,
    };
    outcome.unwrap_or_else(|err| {
        eprintln!("error: {err:#}");
        ExitCode::FAILURE
    })
}
