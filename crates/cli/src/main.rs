//! Nexus Computer Use Runtime service entry point.

mod agent;
mod authority;
mod commands;
mod diagnostics;
mod error;
mod integration;
mod outcome;
mod paths;
mod service;

use std::process::ExitCode;

use clap::Parser;

use commands::{Cli, run};
use diagnostics::init_logging;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = init_logging(cli.log_format, &cli.log_level) {
        let report = error.report();
        report.emit();
        return ExitCode::from(report.exit_code());
    }
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let report = error.report();
            report.emit();
            ExitCode::from(report.exit_code())
        }
    }
}
