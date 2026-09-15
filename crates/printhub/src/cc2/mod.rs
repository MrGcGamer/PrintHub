//! Everything that talks to the Centauri Carbon 2 itself.

pub mod client;
pub mod discovery;
pub mod methods;
pub mod model;
pub mod status;
pub mod upload;

pub use client::{ClientConfig, CommandError, LinkState, PrinterClient, PrinterSnapshot, Timing};
