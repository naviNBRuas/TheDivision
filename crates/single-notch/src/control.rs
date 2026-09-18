//! Control socket: `single notch show|hide` (and `disable`'s
//! quit-before-SIGTERM preference) talk to the running HUD over
//! `notch.sock` with one JSON object per line -- `{"cmd":"show"}` /
//! `{"cmd":"hide"}` / `{"cmd":"quit"}`. See plan Phase 5 Task 11.

use iced::Subscription;
use serde::{Deserialize, Serialize};
use single_core::SingleDirs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCommand {
    Show,
    Hide,
    Quit,
}

#[derive(Serialize, Deserialize)]
struct Wire {
    cmd: ControlCommand,
}

/// Runs the accept loop on a plain OS thread (not the async runtime --
/// a Unix socket accept/read is cheap and blocking here is simpler than
/// threading tokio's net feature through just for this), forwarding each
/// parsed command into the iced subscription's channel via a synchronous
/// `try_send`. A full or closed receiver just drops the command -- the
/// HUD process exiting is exactly when this loop should stop mattering.
fn accept_loop(mut sender: iced::futures::channel::mpsc::Sender<ControlCommand>) {
    let Ok(dirs) = SingleDirs::discover() else { return };
    let socket_path = dirs.notch_socket_path();
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(listener) = UnixListener::bind(&socket_path) else { return };

    for conn in listener.incoming().flatten() {
        let mut reader = BufReader::new(conn);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        if let Ok(wire) = serde_json::from_str::<Wire>(line.trim_end()) {
            let _ = sender.try_send(wire.cmd);
        }
    }
}

pub fn subscription() -> Subscription<ControlCommand> {
    Subscription::run(|| {
        iced::stream::channel(16, async move |sender| {
            std::thread::spawn(move || accept_loop(sender));
            // keep the async side of the channel alive for the stream's
            // lifetime; the real work happens on the spawned OS thread.
            std::future::pending::<()>().await;
        })
    })
}

/// One-line JSON command to a running HUD's control socket. Used by the
/// CLI's `notch show`/`hide` and by `disable`'s quit-then-SIGTERM path.
pub fn send(dirs: &SingleDirs, cmd: ControlCommand) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(dirs.notch_socket_path())?;
    let mut payload = serde_json::to_string(&Wire { cmd })?;
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrips_each_command_as_snake_case_json() {
        for cmd in [ControlCommand::Show, ControlCommand::Hide, ControlCommand::Quit] {
            let json = serde_json::to_string(&Wire { cmd }).unwrap();
            let parsed: Wire = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.cmd, cmd);
        }
    }

    #[test]
    fn show_serializes_to_the_documented_wire_shape() {
        let json = serde_json::to_string(&Wire { cmd: ControlCommand::Show }).unwrap();
        assert_eq!(json, r#"{"cmd":"show"}"#);
    }
}
