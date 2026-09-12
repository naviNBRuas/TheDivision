//! `single-lsp`: a dynamic LSP proxy. Claude Code spawns this one process
//! for every extension registered in its plugin manifest (see
//! `docs/superpowers/specs/2026-08-24-claude-code-singlecli-integration-design.md`);
//! it routes each open document to the real language server SingleCLI's
//! LSP registry maps that extension to, spawning backends lazily and
//! reusing them for documents of the same language — see `proxy.rs`.

mod framing;
mod proxy;

use framing::{read_message, write_message};
use proxy::{load_registry, Multiplexer, Router};
use serde_json::Value;
use std::io::{BufReader, BufWriter};
use std::sync::mpsc::channel;

fn main() -> anyhow::Result<()> {
    let registry = load_registry()?;
    let router = Router::from_registry(registry);

    let (client_out_tx, client_out_rx) = channel::<Value>();
    let multiplexer = Multiplexer::new(router, client_out_tx);
    // Background reaper evicts language servers that have sat idle for a
    // while, so a long session does not hold every spawned backend resident.
    multiplexer.start_reaper();

    // Writer thread: drains backend responses/notifications and the
    // proxy's own replies onto stdout, serialized through one channel so
    // concurrent backend reader threads never interleave partial writes.
    // `write_message` flushes per message, so nothing is stranded in the
    // buffer when the process exits.
    let writer = std::thread::spawn(move || {
        let mut writer = BufWriter::new(std::io::stdout());
        while let Ok(message) = client_out_rx.recv() {
            if message.is_null() {
                break; // end-of-output sentinel — see `close_client_output`
            }
            if write_message(&mut writer, &message).is_err() {
                break; // stdout is gone; the client has hung up
            }
        }
    });

    let mut reader = BufReader::new(std::io::stdin());
    while let Some(message) = read_message(&mut reader)? {
        // `exit` ends the session by protocol, rather than waiting for the
        // client to also close stdin.
        let is_exit = message.get("method").and_then(Value::as_str) == Some("exit");
        if let Err(err) = multiplexer.handle_client_message(message) {
            eprintln!("single-lsp: {err:#}");
        }
        if is_exit {
            break;
        }
    }

    // Drain before exiting: replies queued by the last few messages (a
    // `shutdown` result, say) are still in flight, and returning from `main`
    // would tear the process down out from under the writer thread.
    multiplexer.close_client_output();
    let _ = writer.join();
    Ok(())
}
