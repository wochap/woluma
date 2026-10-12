//! The `woluma` command line: clap definitions, socket client, renderers and `check`.

mod check;
mod client;
mod render;
mod watch;

use crate::control::Request;
use crate::{config, daemon};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "woluma",
    version,
    about = "Rule-based, screen-content aware brightness daemon for Wayland"
)]
pub struct Cli {
    /// Config file (default: $XDG_CONFIG_HOME/woluma/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// The clap command tree, for generating shell completions.
    #[allow(dead_code)] // used by add-zsh-completions
    pub fn command() -> clap::Command {
        <Self as clap::CommandFactory>::command()
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon and serve the control socket (default)
    Daemon,
    /// Enable or disable automation for an output, or all outputs
    Toggle {
        /// Output name, or `*` for all outputs
        #[arg(default_value = "*")]
        target: String,
    },
    /// Pause automation until resumed or for a duration
    Pause {
        /// Output name, or `*` for all outputs
        target: String,
        /// How long to pause, e.g. `90s`, `15m`, `1h30m`
        #[arg(value_parser = parse_duration)]
        duration: Option<Duration>,
    },
    /// Clear a pause and apply the current rule now
    Resume {
        /// Output name, or `*` for all outputs
        target: String,
    },
    /// Show the daemon's current state
    Status {
        /// Print one line of JSON
        #[arg(long)]
        json: bool,
    },
    /// Stream live luma, ALS, rule and brightness values
    Watch(WatchArgs),
    /// Validate the config and show which rule each output matches now
    Check {
        /// Validate only: no Wayland, socket or hardware access
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Args, Clone, Copy)]
pub struct WatchArgs {
    /// Print only input values (TIME OUTPUT LUMA ALS CLOCK), for calibrating rules
    #[arg(long, conflicts_with = "json")]
    pub raw: bool,
    /// Print one JSON snapshot per line (NDJSON)
    #[arg(long)]
    pub json: bool,
}

/// A failed client command and its exit code.
#[derive(Debug)]
pub enum Failure {
    /// Exit 1.
    Runtime(String),
    /// Exit 3.
    NotRunning,
}

pub const NOT_RUNNING: &str = "woluma daemon is not running; start it with `woluma daemon` or \
                               `systemctl --user start woluma`";

/// Parses `90s`, `15m`, `1h30m`, `2h 5m 10s`: one or more number-unit pairs (h, m, s).
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid = || format!("invalid duration \"{text}\", expected e.g. 90s, 15m or 1h30m");
    let mut rest = text.trim();
    let mut total = Duration::ZERO;
    if rest.is_empty() {
        return Err(invalid());
    }
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(invalid)?;
        let number: u64 = rest[..digits].parse().map_err(|_| invalid())?;
        rest = &rest[digits..];
        let unit = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let secs = match &rest[..unit] {
            "h" => 3600,
            "m" => 60,
            "s" => 1,
            _ => return Err(invalid()),
        };
        total += Duration::from_secs(number.checked_mul(secs).ok_or_else(invalid)?);
        rest = rest[unit..].trim_start();
    }
    if total.is_zero() {
        return Err(invalid());
    }
    Ok(total)
}

pub fn run(cli: Cli) -> ExitCode {
    let config_path = cli.config.as_deref();
    let result = match cli.command.unwrap_or(Command::Daemon) {
        Command::Daemon => config::load(config_path)
            .and_then(daemon::run)
            .map_err(|e| Failure::Runtime(format!("{e:#}"))),
        Command::Toggle { target } => client::act(Request::Toggle { target }),
        Command::Pause { target, duration } => client::act(Request::Pause {
            target,
            duration_secs: duration.map(|d| d.as_secs()),
        }),
        Command::Resume { target } => client::act(Request::Resume { target }),
        Command::Status { json } => client::status(json),
        Command::Watch(args) => watch::run(args, config_path),
        Command::Check { offline } => check::run(config_path, offline),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Runtime(message)) => {
            eprintln!("woluma: {message}");
            ExitCode::from(1)
        }
        Err(Failure::NotRunning) => {
            eprintln!("woluma: {NOT_RUNNING}");
            ExitCode::from(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("2h 5m 10s"), Ok(Duration::from_secs(7510)));
        for bad in ["", "soon", "15", "m", "0s", "1.5h", "-1m", "10d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    fn help(args: &[&str]) -> String {
        let error = Cli::try_parse_from(args).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        error.to_string()
    }

    #[test]
    fn top_level_help_lists_commands() {
        let help = help(&["woluma", "--help"]);
        for line in [
            "  daemon  Run the daemon and serve the control socket (default)",
            "  toggle  Enable or disable automation for an output, or all outputs",
            "  pause   Pause automation until resumed or for a duration",
            "  resume  Clear a pause and apply the current rule now",
            "  status  Show the daemon's current state",
            "  watch   Stream live luma, ALS, rule and brightness values",
            "  check   Validate the config and show which rule each output matches now",
            "      --config <PATH>  Config file (default: $XDG_CONFIG_HOME/woluma/config.toml)",
        ] {
            assert!(help.contains(line), "missing {line:?} in:\n{help}");
        }
    }

    #[test]
    fn subcommand_help() {
        let expect = [
            (
                "toggle",
                "Usage: woluma toggle [OPTIONS] [TARGET]",
                "[default: *]",
            ),
            (
                "pause",
                "Usage: woluma pause [OPTIONS] <TARGET> [DURATION]",
                "1h30m",
            ),
            (
                "resume",
                "Usage: woluma resume [OPTIONS] <TARGET>",
                "Output name",
            ),
            ("status", "Usage: woluma status [OPTIONS]", "--json"),
            ("watch", "Usage: woluma watch [OPTIONS]", "--raw"),
            ("check", "Usage: woluma check [OPTIONS]", "--offline"),
            ("daemon", "Usage: woluma daemon [OPTIONS]", "--config"),
        ];
        for (command, usage, detail) in expect {
            let help = help(&["woluma", command, "--help"]);
            assert!(help.contains(usage), "{command}:\n{help}");
            assert!(help.contains(detail), "{command}:\n{help}");
        }
    }

    #[test]
    fn usage_errors() {
        let kind = |args: &[&str]| Cli::try_parse_from(args).err().map(|e| e.kind());
        assert_eq!(
            kind(&["woluma", "pause", "eDP-1", "soon"]),
            Some(clap::error::ErrorKind::ValueValidation)
        );
        assert_eq!(
            kind(&["woluma", "watch", "--raw", "--json"]),
            Some(clap::error::ErrorKind::ArgumentConflict)
        );
        assert!(kind(&["woluma", "pause"]).is_some());
        assert!(kind(&["woluma"]).is_none());
        assert!(matches!(
            Cli::try_parse_from(["woluma", "toggle"]).unwrap().command,
            Some(Command::Toggle { target }) if target == "*"
        ));
    }
}
