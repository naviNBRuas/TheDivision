//! A tiny blocking client for the runtime's Unix socket protocol. The TUI
//! talks to the runtime the same way the CLI does — over IPC, not by
//! calling into `divisi-runtime` in-process — so it exercises the real
//! client/server boundary described in spec section 3.

use anyhow::{Context, Result};
use divisi_protocol::{Request, Response};
use std::io::{BufRead, BufReader, Write};
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
