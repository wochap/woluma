//! `woluma watch`: follows the daemon, or samples in-process when none is running.

use super::render::{self, Format};
use super::{client, Failure, WatchArgs};
use crate::config::{self, Backend, Config, Output};
use crate::control::{Request, Snapshot};
use crate::{capture, daemon};
use std::path::Path;

pub fn run(args: WatchArgs, config_path: Option<&Path>) -> Result<(), Failure> {
    let format = if args.json {
        Format::Json
    } else if args.raw {
        Format::Raw
    } else {
        Format::Human
    };
    match client::send(&Request::Subscribe) {
        Ok(mut reader) => {
            exit_on_sigint();
            let mut out = std::io::stdout().lock();
            if let Some(header) = render::watch_header(format) {
                render::emit(&mut out, &header);
            }
            while let Some(snapshot) = client::read::<Snapshot>(&mut reader)? {
                for line in render::watch_lines(&snapshot, format) {
                    render::emit(&mut out, &line);
                }
            }
            Err(Failure::Runtime("daemon closed the connection".into()))
        }
        Err(Failure::NotRunning) => standalone(format, config_path),
        Err(error) => Err(error),
    }
}

/// Ctrl-C ends the stream successfully.
fn exit_on_sigint() {
    extern "C" fn exit(_: libc::c_int) {
        unsafe { libc::_exit(0) };
    }
    unsafe { libc::signal(libc::SIGINT, exit as *const () as libc::sighandler_t) };
}

/// All connected outputs with no rules: luma, ALS and time only.
fn implicit_config() -> Result<Config, Failure> {
    let names = capture::outputs::list()
        .map_err(|e| Failure::Runtime(format!("unable to list outputs: {e:#}")))?;
    Ok(Config {
        general: Default::default(),
        hysteresis: Default::default(),
        als: config::Als::None,
        outputs: names
            .into_iter()
            .map(|name| Output {
                name,
                backend: Backend::Backlight(String::new()),
                rules: Vec::new(),
            })
            .collect(),
    })
}

fn standalone(mut format: Format, config_path: Option<&Path>) -> Result<(), Failure> {
    eprintln!("no daemon running; sampling locally");
    let path = config::resolve_path(
        config_path,
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .map_err(|e| Failure::Runtime(format!("{e:#}")))?;
    let config = if path.exists() {
        match config::load(Some(&path)) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("woluma: {error:#}");
                eprintln!("woluma: showing raw values only");
                if format == Format::Human {
                    format = Format::Raw;
                }
                implicit_config()?
            }
        }
    } else {
        implicit_config()?
    };
    if config.outputs.is_empty() {
        return Err(Failure::Runtime("no outputs to sample".into()));
    }

    let hub = daemon::hub(&config);
    let updates = hub.subscribe();
    std::thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        if let Some(header) = render::watch_header(format) {
            render::emit(&mut out, &header);
        }
        while let Ok(snapshot) = updates.recv_blocking() {
            for line in render::watch_lines(&snapshot, format) {
                render::emit(&mut out, &line);
            }
        }
    });
    daemon::observe(config, hub).map_err(|e| Failure::Runtime(format!("{e:#}")))
}
