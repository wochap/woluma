//! Blocking control-socket client.

use super::{render, Failure};
use crate::control::server::{runtime_dir, socket_path};
use crate::control::{Request, Response, Snapshot};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;

/// Connects and sends `request`. A missing runtime dir or socket, or a refused connection,
/// means the daemon is not running.
pub fn send(request: &Request) -> Result<BufReader<UnixStream>, Failure> {
    let path = socket_path(&runtime_dir().map_err(|_| Failure::NotRunning)?);
    let mut stream = UnixStream::connect(&path).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => Failure::NotRunning,
        _ => Failure::Runtime(format!("unable to connect to {}: {e}", path.display())),
    })?;
    let mut line = serde_json::to_string(request).expect("requests serialize");
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| Failure::Runtime(format!("unable to send request: {e}")))?;
    Ok(BufReader::new(stream))
}

/// Reads the next JSON line, `None` at end of stream.
pub fn read<T: serde::de::DeserializeOwned>(
    reader: &mut BufReader<UnixStream>,
) -> Result<Option<T>, Failure> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) => serde_json::from_str(&line)
            .map(Some)
            .map_err(|e| Failure::Runtime(format!("invalid reply from daemon: {e}"))),
        Err(e) => Err(Failure::Runtime(format!("unable to read reply: {e}"))),
    }
}

/// A one-shot request; `ok: false` becomes a runtime failure.
pub fn request(request: &Request) -> Result<Response, Failure> {
    let mut reader = send(request)?;
    let response: Response = read(&mut reader)?
        .ok_or_else(|| Failure::Runtime("daemon closed the connection".into()))?;
    if response.ok {
        Ok(response)
    } else {
        Err(Failure::Runtime(
            response.error.unwrap_or_else(|| "request failed".into()),
        ))
    }
}

/// toggle / pause / resume: prints the resulting state per output.
pub fn act(request: Request) -> Result<(), Failure> {
    let response = self::request(&request)?;
    let mut out = std::io::stdout().lock();
    for output in response.outputs.unwrap_or_default() {
        render::emit(&mut out, &render::action_line(&output));
    }
    Ok(())
}

pub fn status(json: bool) -> Result<(), Failure> {
    let response = request(&Request::Status)?;
    let snapshot: Snapshot = response
        .status
        .ok_or_else(|| Failure::Runtime("daemon sent no status".into()))?;
    let mut out = std::io::stdout().lock();
    if json {
        render::emit(&mut out, &serde_json::to_string(&snapshot).unwrap());
    } else {
        for line in render::status_table(&snapshot) {
            render::emit(&mut out, &line);
        }
    }
    Ok(())
}
