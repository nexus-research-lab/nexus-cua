//! Nexus Computer Use Runtime service entry point.

mod commands;
mod diagnostics;
mod error;
mod paths;

use std::process::ExitCode;

use clap::Parser;

use commands::{Cli, run};
use diagnostics::init_logging;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = init_logging(cli.log_format, &cli.log_level) {
        eprintln!("nexus-cua: {error}");
        return ExitCode::FAILURE;
    }
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nexus-cua: {error}");
            ExitCode::FAILURE
        }
    }
}
