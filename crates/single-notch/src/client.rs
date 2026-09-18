//! Unix socket NDJSON send — a straight clone of `single-tui::client::call`'s
//! shape (socket-only, no in-process runtime fallback; the notch is a
//! lightweight poller, not a daemon host).

use anyhow::{Context, Result};
use single_protocol::{Request, Response};
use std::io::{BufReader, Write};
use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::path::Path;

pub fn call(socket_path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("connecting to runtime at {}", socket_path.display()))?;
    let mut payload = serde_json::to_string(request)?;
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let response: Response = serde_json::from_str(line.trim_end())?;
    Ok(response)
}
