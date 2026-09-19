//! Talks to the runtime over its Unix socket when one is already running;
//! otherwise falls back to calling straight into `divisi-runtime` in the
//! current process. This keeps one-shot CLI commands (`single doctor`,
//! `single agent list`, ...) working on a fresh install where no daemon has
//! ever been started, without requiring the CLI to manage a background
//! process lifecycle in Phase 1. The TUI, which genuinely wants a live
//! socket connection, spawns the daemon explicitly instead (see
//! `daemon.rs`).
//!
//! Falling back to the in-process path is only safe *before* a request has
//! been sent — once bytes are on the wire, a slow response (e.g. a
//! long-running `task run`, which can legitimately take minutes) must
//! never silently trigger a second, duplicate in-process execution of the
//! same side-effecting request. So: connection failure falls back;
//! anything that fails *after* the request was written is a hard error.

use anyhow::Result;
use divisi_protocol::{Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub fn send(socket_path: &Path, request: Request) -> Result<Response> {
    match UnixStream::connect(socket_path) {
        Ok(stream) => send_over(stream, &request),
        Err(_) => {
            // No daemon running (or socket stale) — nothing was sent, so
            // it's safe to run the request in-process instead. A
            // `background: true` run spawns its actual work on a detached
            // thread, but *this* process is the one-shot `single` CLI
            // invocation itself — it exits right after printing the
            // response below, killing that thread with it before the
            // agent gets anywhere. Confirmed live: a background task
            // dispatched with no daemon running left no worktree, no
            // live-output file, and no subprocess at all. Warn rather
            // than silently accept a request that can't do what it says.
            if wants_background(&request) {
                eprintln!(
                    "warning: no divisid daemon is running — `--background` has no effect here, since this one-shot command exits (and takes its background thread with it) right after this request. Start the daemon first (`divisid &`) if you want a background/detached run to actually outlive this command."
                );
            }
            let ctx = divisi_runtime::Context::load()?;
            Ok(divisi_runtime::handle(&ctx, request))
        }
    }
}

fn wants_background(request: &Request) -> bool {
    matches!(
        request,
        Request::TaskRun {
            background: true,
            ..
        } | Request::OrchestrateParallel {
            background: true,
            ..
        } | Request::OrchestrateGraph {
            background: true,
            ..
        }
    )
}

fn send_over(stream: UnixStream, request: &Request) -> Result<Response> {
    let mut writer = stream.try_clone()?;
    let mut payload = serde_json::to_string(request)?;
    payload.push('\n');
    writer.write_all(payload.as_bytes())?;

    // No read timeout: a request already sent to a live daemon must be
    // waited out, not abandoned into a duplicate run. The runtime's own
    // per-task timeout is what actually bounds how long this can take.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim_end())?)
}
