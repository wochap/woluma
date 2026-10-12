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
    // Answers `COMPLETE=<shell>` callbacks and exits; must run before any other setup.
    clap_complete::CompleteEnv::with_factory(cli::Cli::command).complete();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    cli::run(cli::Cli::parse())
}
