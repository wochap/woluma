//! Daemon side of the control socket: instance lock, binding, self-healing and requests.

use super::{Hub, Request, Response, LOCK_NAME, SOCKET_NAME};
use anyhow::{anyhow, bail, Context, Result};
use futures_lite::{future, AsyncBufReadExt, AsyncWriteExt};
use smol::io::BufReader;
use smol::net::unix::{UnixListener, UnixStream};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the socket file is checked and rebound if it was removed or replaced.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

const ALREADY_RUNNING: &str = "another woluma daemon is already running";

/// A mutating request and where to send its response.
pub type Command = (Request, async_channel::Sender<Response>);

pub fn runtime_dir() -> Result<PathBuf> {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => bail!("XDG_RUNTIME_DIR is not set; it is required for the control socket"),
    }
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET_NAME)
}

/// Device, inode and mode of the socket file we bound.
type Identity = (u64, u64, u32);

fn identity(path: &Path) -> Option<Identity> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino(), meta.mode()))
}

pub struct Server {
    path: PathBuf,
    listener: UnixListener,
    identity: Option<Identity>,
    _lock: File,
}

impl Server {
    /// Takes the instance lock and binds the socket, replacing a stale socket file.
    pub fn bind(dir: &Path) -> Result<Self> {
        let lock_path = dir.join(LOCK_NAME);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("unable to open {}", lock_path.display()))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!(ALREADY_RUNNING);
            }
            return Err(anyhow!(error).context(format!("unable to lock {}", lock_path.display())));
        }

        let path = socket_path(dir);
        if std::fs::symlink_metadata(&path).is_ok() {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                bail!(ALREADY_RUNNING);
            }
            log::info!("Removing stale socket {}", path.display());
            std::fs::remove_file(&path)
                .with_context(|| format!("unable to remove {}", path.display()))?;
        }
        let (listener, identity) = listen(&path)?;
        Ok(Self {
            path,
            listener,
            identity,
            _lock: lock,
        })
    }

    /// Accepts clients until the task is dropped. Mutating requests go to `commands`.
    pub async fn serve(
        mut self,
        hub: Arc<Hub>,
        commands: async_channel::Sender<Command>,
        health: Duration,
    ) {
        let mut next_check = Instant::now() + health;
        loop {
            let accepted = future::or(async { Some(self.listener.accept().await) }, async {
                smol::Timer::at(next_check).await;
                None
            })
            .await;
            match accepted {
                Some(Ok((stream, _))) => {
                    smol::spawn(handle(stream, hub.clone(), commands.clone())).detach();
                }
                Some(Err(e)) => log::warn!("Control socket accept failed: {e}"),
                None => {
                    next_check = Instant::now() + health;
                    self.heal();
                }
            }
        }
    }

    fn heal(&mut self) {
        if identity(&self.path).is_some() && identity(&self.path) == self.identity {
            return;
        }
        log::warn!(
            "Control socket {} was removed or replaced, rebinding",
            self.path.display()
        );
        let _ = std::fs::remove_file(&self.path);
        match listen(&self.path) {
            Ok((listener, identity)) => {
                self.listener = listener;
                self.identity = identity;
            }
            Err(e) => log::error!("{e:#}"),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if identity(&self.path) == self.identity {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn listen(path: &Path) -> Result<(UnixListener, Option<Identity>)> {
    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("unable to bind control socket {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("unable to set permissions on {}", path.display()))?;
    let listener = UnixListener::try_from(listener)?;
    Ok((listener, identity(path)))
}

async fn write_line<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    Ok(())
}

async fn handle(stream: UnixStream, hub: Arc<Hub>, commands: async_channel::Sender<Command>) {
    let mut writer = stream.clone();
    let mut line = String::new();
    match BufReader::new(stream).read_line(&mut line).await {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    let response = match Request::parse(line.trim()) {
        Err(error) => Response::error(error),
        Ok(Request::Status) => Response::status(hub.snapshot()),
        Ok(Request::Subscribe) => {
            let updates = hub.subscribe();
            while let Ok(snapshot) = updates.recv().await {
                if write_line(&mut writer, &snapshot).await.is_err() {
                    break;
                }
            }
            return;
        }
        Ok(request) => {
            let (reply, response) = async_channel::bounded(1);
            match commands.send((request, reply)).await {
                Ok(()) => response
                    .recv()
                    .await
                    .unwrap_or_else(|_| Response::error("daemon is shutting down")),
                Err(_) => Response::error("daemon is shutting down"),
            }
        }
    };
    let _ = write_line(&mut writer, &response).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader as StdBufReader, Write};
    use std::os::unix::net::UnixStream as StdStream;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("woluma-ctl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Serves on `dir` with a fake controller that answers every command with `ok: true`.
    fn start(dir: &Path, health: Duration) -> Arc<Hub> {
        let server = Server::bind(dir).unwrap();
        let hub = Arc::new(Hub::new(&["eDP-1".into(), "DP-1".into()], true, "none"));
        let (commands, inbox) = async_channel::unbounded::<Command>();
        let names = hub.names();
        let serving = hub.clone();
        std::thread::spawn(move || {
            smol::block_on(async move {
                smol::spawn(async move {
                    while let Ok((request, reply)) = inbox.recv().await {
                        let target = match request {
                            Request::Toggle { target }
                            | Request::Pause { target, .. }
                            | Request::Resume { target } => target,
                            _ => unreachable!(),
                        };
                        let response = match super::super::Target::parse(&target).resolve(&names) {
                            Ok(_) => Response::outputs(vec![]),
                            Err(e) => Response::error(e),
                        };
                        let _ = reply.send(response).await;
                    }
                })
                .detach();
                server.serve(serving, commands, health).await;
            })
        });
        hub
    }

    fn request(dir: &Path, line: &str) -> Response {
        let mut stream = StdStream::connect(socket_path(dir)).unwrap();
        writeln!(stream, "{line}").unwrap();
        let mut reply = String::new();
        StdBufReader::new(stream).read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    #[test]
    fn requires_runtime_dir() {
        // Only checks the message; the variable itself is left alone.
        let message = "XDG_RUNTIME_DIR is not set; it is required for the control socket";
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            assert_eq!(runtime_dir().unwrap_err().to_string(), message);
        }
    }

    #[test]
    fn serves_requests() {
        let dir = temp_dir("serve");
        start(&dir, HEALTH_INTERVAL);
        let mode = std::fs::metadata(socket_path(&dir)).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600);

        let status = request(&dir, r#"{"cmd":"status"}"#);
        assert!(status.ok);
        assert_eq!(status.status.unwrap().outputs.len(), 2);

        let unknown = request(&dir, r#"{"cmd":"explode"}"#);
        assert_eq!(unknown, Response::error("unknown command: explode"));

        let bad = request(&dir, r#"{"cmd":"toggle","target":"HDMI-9"}"#);
        assert!(!bad.ok);
        assert!(bad
            .error
            .unwrap()
            .contains("HDMI-9 (configured: eDP-1, DP-1)"));
    }

    #[test]
    fn subscribe_streams_updates() {
        let dir = temp_dir("subscribe");
        let hub = start(&dir, HEALTH_INTERVAL);
        let mut stream = StdStream::connect(socket_path(&dir)).unwrap();
        writeln!(stream, r#"{{"cmd":"subscribe"}}"#).unwrap();
        let mut lines = StdBufReader::new(stream).lines();
        let first: super::super::Snapshot =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first.outputs[0].luma, None);
        hub.set_input(0, Some(88), None, None);
        let next: super::super::Snapshot =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(next.outputs[0].luma, Some(88));
    }

    #[test]
    fn second_daemon_refused() {
        let dir = temp_dir("second");
        start(&dir, HEALTH_INTERVAL);
        let error = Server::bind(&dir).err().unwrap();
        assert_eq!(error.to_string(), ALREADY_RUNNING);
        assert!(
            request(&dir, r#"{"cmd":"status"}"#).ok,
            "first keeps serving"
        );
    }

    #[test]
    fn stale_socket_replaced() {
        let dir = temp_dir("stale");
        // A socket file nobody listens on.
        drop(std::os::unix::net::UnixListener::bind(socket_path(&dir)).unwrap());
        assert!(socket_path(&dir).exists());
        start(&dir, HEALTH_INTERVAL);
        assert!(request(&dir, r#"{"cmd":"status"}"#).ok);
    }

    #[test]
    fn deleted_socket_restored() {
        let dir = temp_dir("heal");
        start(&dir, Duration::from_millis(50));
        std::fs::remove_file(socket_path(&dir)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(request(&dir, r#"{"cmd":"status"}"#).ok);
    }
}
