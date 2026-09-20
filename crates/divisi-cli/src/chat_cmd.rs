//! `divisi chat`: talk to divisi in plain language. With no subcommand it opens the shared
//! conversation as a REPL; `send`, `tail` and `confirm` are the scriptable pieces the notch uses.

use crate::{client, render};
use anyhow::{bail, Result};
use clap::Subcommand;
use divisi_protocol::{chat_line, progress_line, ChatOutcome, ChatRole, CoordinatorEvent, Request, Response, ResponseData};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Subcommand)]
pub enum ChatCommand {
    /// Send one message and print the reply.
    Send {
        text: Vec<String>,
        /// A session to talk in (default: the shared conversation).
        #[arg(long)]
        session: Option<String>,
        /// Which surface this comes from, recorded with the message.
        #[arg(long, default_value = "cli")]
        surface: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the conversation after an event id, optionally following it.
    Tail {
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long)]
        session: Option<String>,
        /// Keep printing new events until interrupted.
        #[arg(long)]
        follow: bool,
        /// One JSON object per event, for scripts and the notch.
        #[arg(long)]
        json: bool,
    },
    /// Answer a confirmation divisi asked for.
    Confirm {
        approval_id: i64,
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        #[arg(long)]
        deny: bool,
        /// Always give the same answer to this kind of request (only offered where it is safe).
        #[arg(long)]
        remember: bool,
        #[arg(long)]
        json: bool,
    },
}

fn outcome(response: Response) -> Result<ChatOutcome> {
    match response {
        Response::Ok { data: ResponseData::Chat(o) } => Ok(o),
        Response::Ok { data } => bail!("unexpected response: {data:?}"),
        Response::Error { message } => bail!("{message}"),
    }
}

fn history(socket: &Path, session: Option<String>, since: i64) -> Result<ChatOutcome> {
    outcome(client::send(socket, Request::ChatHistory { session, since_event_id: since })?)
}

/// One event as a terminal line, or nothing for events that would only be noise here.
fn print_event(e: &CoordinatorEvent) {
    if chat_line(&e.kind, &e.body).is_some() {
        render::print_chat_event(&e.kind, &e.body);
    } else if let Some(p) = progress_line(&e.kind, &e.body) {
        println!("·       {p}");
    }
}

fn event_json(e: &CoordinatorEvent) -> String {
    serde_json::json!({"id": e.id, "goal_id": e.goal_id, "ts": e.ts, "kind": e.kind, "body": e.body}).to_string()
}

pub fn run(socket: &Path, action: Option<ChatCommand>) -> Result<()> {
    match action {
        None => repl(socket),
        Some(ChatCommand::Send { text, session, surface, json }) => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                bail!("say something first");
            }
            let response = client::send(socket, Request::ChatSend { session, text, surface, mode: None, agent: None })?;
            if json {
                render::print(response, true);
            } else {
                for e in outcome(response)?.events {
                    print_event(&e);
                }
            }
            Ok(())
        }
        Some(ChatCommand::Tail { after, session, follow, json }) => {
            let mut cursor = after;
            let mut session = session;
            loop {
                let o = history(socket, session.clone(), cursor)?;
                session = Some(o.session_id);
                for e in &o.events {
                    cursor = cursor.max(e.id);
                    if json {
                        println!("{}", event_json(e));
                    } else {
                        print_event(e);
                    }
                }
                std::io::stdout().flush().ok();
                if !follow {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
        Some(ChatCommand::Confirm { approval_id, allow, deny, remember, json }) => {
            if allow == deny {
                bail!("say --allow or --deny");
            }
            let response = client::send(socket, Request::ChatConfirm { approval_id, allow, remember })?;
            if json {
                render::print(response, true);
            } else {
                for e in outcome(response)?.events {
                    print_event(&e);
                }
            }
            Ok(())
        }
    }
}

/// Shared between the input loop and the background poller so each event prints exactly once.
struct Feed {
    socket: std::path::PathBuf,
    session: String,
    cursor: Mutex<i64>,
    /// The confirmation currently waiting for a yes or no, if any.
    pending: Mutex<Option<i64>>,
    /// Lines typed in this REPL that have not been echoed back yet.
    own: Mutex<Vec<String>>,
}

/// True (and consumes the entry) when `e` is a message this REPL itself just sent.
fn is_own_echo(own: &Mutex<Vec<String>>, e: &CoordinatorEvent) -> bool {
    if e.kind != "chat_user" {
        return false;
    }
    let Some(text) = serde_json::from_str::<serde_json::Value>(&e.body).ok().and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned)) else { return false };
    let mut own = own.lock().unwrap();
    match own.iter().position(|o| *o == text) {
        Some(i) => {
            own.remove(i);
            true
        }
        None => false,
    }
}

impl Feed {
    /// Prints everything new. The cursor lock serialises the poller and the input loop.
    fn drain(&self) {
        let mut cursor = self.cursor.lock().unwrap();
        let Ok(o) = history(&self.socket, Some(self.session.clone()), *cursor) else { return };
        for e in &o.events {
            *cursor = (*cursor).max(e.id);
            if !is_own_echo(&self.own, e) {
                print_event(e);
            }
            let approval = serde_json::from_str::<serde_json::Value>(&e.body).ok().and_then(|v| v.get("approval_id").and_then(|i| i.as_i64()));
            match (e.kind.as_str(), approval) {
                ("chat_confirm", Some(id)) => *self.pending.lock().unwrap() = Some(id),
                ("chat_result", Some(id)) => {
                    let mut p = self.pending.lock().unwrap();
                    if *p == Some(id) {
                        *p = None;
                    }
                }
                _ => {}
            }
        }
        if !o.events.is_empty() {
            std::io::stdout().flush().ok();
        }
    }
}

fn is_yes(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "y" | "yes" | "ok" | "do it" | "approve")
}
fn is_no(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "n" | "no" | "nope" | "deny" | "don't" | "dont")
}

fn repl(socket: &Path) -> Result<()> {
    let first = history(socket, None, 0)?;
    let feed = Arc::new(Feed { socket: socket.to_path_buf(), session: first.session_id.clone(), cursor: Mutex::new(0), pending: Mutex::new(None), own: Mutex::new(Vec::new()) });
    println!("divisi chat. Say what you want in plain language; `exit` to leave.");
    // Show the tail of the existing conversation, then only what is new.
    let backlog: Vec<&CoordinatorEvent> = first.events.iter().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
    for e in backlog {
        print_event(e);
    }
    *feed.cursor.lock().unwrap() = first.events.iter().map(|e| e.id).max().unwrap_or(0);

    // Goal progress and other surfaces' messages arrive while you are typing.
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let (feed, stop) = (feed.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1500));
                feed.drain();
            }
        })
    };

    let stdin = std::io::stdin();
    loop {
        print!("/ ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "exit" | "quit" | "/exit" | "/quit") {
            break;
        }
        let pending = *feed.pending.lock().unwrap();
        if matches!(&pending, None) {
            feed.own.lock().unwrap().push(line.to_owned());
        }
        let request = match pending {
            Some(id) if is_yes(line) => Request::ChatConfirm { approval_id: id, allow: true, remember: false },
            Some(id) if line.eq_ignore_ascii_case("always") => Request::ChatConfirm { approval_id: id, allow: true, remember: true },
            Some(id) if is_no(line) => Request::ChatConfirm { approval_id: id, allow: false, remember: false },
            _ => Request::ChatSend { session: Some(feed.session.clone()), text: line.to_owned(), surface: "cli".into(), mode: None, agent: None },
        };
        match client::send(socket, request).and_then(outcome) {
            Ok(_) => feed.drain(),
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = poller.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_and_no_answers() {
        for y in ["y", "Yes", "OK", "approve"] {
            assert!(is_yes(y) && !is_no(y), "{y}");
        }
        for n in ["n", "No", "nope", "deny"] {
            assert!(is_no(n) && !is_yes(n), "{n}");
        }
        assert!(!is_yes("yes please cancel everything") && !is_no("no way"), "only whole, short answers count");
    }

    #[test]
    fn a_message_typed_here_is_not_echoed_back_but_others_are() {
        let own = Mutex::new(vec!["status".to_owned()]);
        let user = |text: &str| CoordinatorEvent { id: 1, goal_id: None, ts: "t".into(), kind: "chat_user".into(), body: serde_json::json!({"text": text, "surface": "cli"}).to_string() };
        assert!(is_own_echo(&own, &user("status")), "the line you just typed is already on your screen");
        assert!(!is_own_echo(&own, &user("status")), "each typed line is skipped once only");
        assert!(!is_own_echo(&own, &user("what did the zed thread say")), "a message from another surface still shows");
        let reply = CoordinatorEvent { id: 2, goal_id: None, ts: "t".into(), kind: "chat_assistant".into(), body: serde_json::json!({"text": "status"}).to_string() };
        own.lock().unwrap().push("status".into());
        assert!(!is_own_echo(&own, &reply), "only your own message is ever skipped, never a reply");
    }

    #[test]
    fn an_event_is_serialised_for_scripts() {
        let e = CoordinatorEvent { id: 5, goal_id: None, ts: "t".into(), kind: "chat_user".into(), body: "{}".into() };
        let v: serde_json::Value = serde_json::from_str(&event_json(&e)).unwrap();
        assert_eq!((v["id"].as_i64(), v["kind"].as_str()), (Some(5), Some("chat_user")));
    }
}
