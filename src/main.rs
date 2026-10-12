mod brightness;
mod capture;
mod config;
mod daemon;
mod hooks;
mod manual_override;
mod rules;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "woluma",
    version,
    about = "Rule-based, screen-content aware brightness daemon for Wayland"
)]
struct Cli {
    /// Config file (default: $XDG_CONFIG_HOME/woluma/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (default)
    Daemon,
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    let result = match cli.command.unwrap_or(Command::Daemon) {
        Command::Daemon => config::load(cli.config.as_deref()).and_then(daemon::run),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
