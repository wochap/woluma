//! Shell completion: registration scripts and dynamic output-name candidates.

use crate::config;
use crate::control::server::runtime_dir;
use crate::control::Request;
use clap_complete::engine::CompletionCandidate;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Upper bound on the daemon query while completing.
const DAEMON_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

/// Prints the registration script; needs neither config nor daemon.
pub fn print_script(shell: Shell) {
    let name = match shell {
        Shell::Zsh => "zsh",
        Shell::Bash => "bash",
        Shell::Fish => "fish",
    };
    let mut out = std::io::stdout().lock();
    let _ = clap_complete::env::Shells::builtins()
        .completer(name)
        .expect("builtin shell")
        .write_registration("COMPLETE", "woluma", "woluma", "woluma", &mut out);
    if let Shell::Zsh = shell {
        // clap's script starts with `#compdef woluma`; when autoloaded as `_woluma`, its body
        // runs on the first TAB, so complete that TAB too.
        let _ = writeln!(
            out,
            "\nif [ \"$funcstack[1]\" = \"_woluma\" ]; then\n    _clap_dynamic_completer_woluma \"$@\"\nfi"
        );
    }
}

/// Candidates for an `OUTPUT|*` argument.
pub fn output_candidates(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    known_outputs(socket_path().as_deref(), config_path().as_deref())
        .into_iter()
        .filter(|name| name.starts_with(&*prefix))
        .map(CompletionCandidate::new)
        .collect()
}

/// Free-form values: no candidates, and no file-name fallback.
pub fn no_candidates(_current: &OsStr) -> Vec<CompletionCandidate> {
    Vec::new()
}

/// Output names from the daemon, else the config's `[outputs]` keys, then `*`; never fails.
/// Config keys come back sorted, matching the order the daemon manages outputs in.
pub fn known_outputs(socket: Option<&Path>, config: Option<&Path>) -> Vec<String> {
    let mut names = socket
        .and_then(daemon_outputs)
        .or_else(|| config.and_then(config_outputs))
        .unwrap_or_default();
    names.push("*".into());
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

fn daemon_outputs(socket: &Path) -> Option<Vec<String>> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(DAEMON_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(DAEMON_TIMEOUT)).ok()?;
    let mut line = serde_json::to_string(&Request::Status).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    // Only the names matter: read them leniently rather than as a full `Snapshot`.
    let reply: serde_json::Value = serde_json::from_str(&reply).ok()?;
    let outputs = reply.get("status")?.get("outputs")?.as_array()?;
    Some(
        outputs
            .iter()
            .filter_map(|o| Some(o.get("name")?.as_str()?.to_owned()))
            .collect(),
    )
}

/// Keys of `[outputs]` only, so an otherwise invalid config still yields names.
fn config_outputs(path: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    Some(table.get("outputs")?.as_table()?.keys().cloned().collect())
}

fn socket_path() -> Option<PathBuf> {
    runtime_dir()
        .ok()
        .map(|dir| crate::control::server::socket_path(&dir))
}

/// The config path woluma would load, honoring a `--config` on the line being completed.
fn config_path() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let explicit = args.iter().enumerate().find_map(|(i, arg)| {
        arg.strip_prefix("--config=")
            .map(PathBuf::from)
            .or_else(|| {
                (arg == "--config")
                    .then(|| args.get(i + 1).map(PathBuf::from))
                    .flatten()
            })
    });
    config::resolve_path(
        explicit.as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    fn tmp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("woluma-complete-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_config(dir: &Path) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, "[outputs.eDP-1]\nbogus = 1\n[outputs.DP-1]\n").unwrap();
        path
    }

    #[test]
    fn daemon_answers() {
        let dir = tmp("daemon");
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let reply = r#"{"ok":true,"status":{"outputs":[{"name":"HDMI-A-1"},{"name":"DP-1"}]}}"#;
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
        });
        let config = write_config(&dir);
        assert_eq!(
            known_outputs(Some(&socket), Some(&config)),
            ["HDMI-A-1", "DP-1", "*"]
        );
        server.join().unwrap();
    }

    #[test]
    fn socket_missing_uses_config() {
        let dir = tmp("missing");
        let config = write_config(&dir);
        assert_eq!(
            known_outputs(Some(&dir.join("nope")), Some(&config)),
            ["DP-1", "eDP-1", "*"]
        );
    }

    #[test]
    fn hanging_socket_times_out() {
        let dir = tmp("hang");
        let socket = dir.join("s");
        let _listener = UnixListener::bind(&socket).unwrap();
        let config = write_config(&dir);
        let start = Instant::now();
        assert_eq!(
            known_outputs(Some(&socket), Some(&config)),
            ["DP-1", "eDP-1", "*"]
        );
        assert!(start.elapsed() < Duration::from_millis(600));
    }

    #[test]
    fn nothing_available() {
        let dir = tmp("none");
        assert_eq!(known_outputs(None, Some(&dir.join("absent.toml"))), ["*"]);
    }
}
