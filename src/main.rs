mod als;
mod brightness;
mod capture;
mod cli;
mod config;
mod control;
mod daemon;
mod hooks;
mod manual_override;
mod rules;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    cli::run(cli::Cli::parse())
}
