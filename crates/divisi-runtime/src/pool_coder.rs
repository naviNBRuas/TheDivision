//! `single-pool` as a real coding agent: a multi-step loop that reads, edits and runs things in the
//! task's working directory, with every model turn dispatched through the pool (`pool_agent::
//! execute_with`) — so the bandit, ledger, per-key cooldowns and failover all apply per turn, and a
//! run that hits one provider's limit carries on with the next one mid-task.
//!
//! Tools are a plain-text JSON protocol rather than native function calling: the pool's messages are
//! role+content only, eleven different wires serialise tools differently (or not at all), and a
//! conversation made of plain text moves between providers without losing anything. A provider that
//! answers with native `tool_calls` anyway is understood too.

use crate::pool::bandit;
use crate::pool::client::{ChatMessage, PoolRequest, ToolCall};
use crate::pool_agent::{execute_with, DispatchFn, PoolAgentOutcome, SecretFn};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Most model turns in one run.
pub const MAX_STEPS: usize = 60;
/// Longest a single shell command may run.
const COMMAND_SECS: u64 = 180;
/// Longest tool output kept in the conversation (head and tail are kept).
const OUTPUT_CHARS: usize = 6000;
/// Conversation size above which the oldest tool results are shortened; free tiers have small windows.
const CONTEXT_CHARS: usize = 60_000;
/// Replies in a row without an action before the reply is taken as the final answer.
const MAX_IDLE_REPLIES: usize = 2;
/// Longest wait for the pool to free up before giving the task back as rate limited.
const MAX_CAPACITY_WAIT: Duration = Duration::from_secs(120);

const SYSTEM: &str = r#"You are a careful software engineer working in a git checkout. You act by replying with exactly ONE JSON object per message, and nothing else is executed. Available actions:

{"tool": "list", "path": "."}                          list a directory
{"tool": "read_file", "path": "src/main.rs"}           read a file (numbered lines)
{"tool": "write_file", "path": "a/b.md", "content": "..."}   create or replace a whole file
{"tool": "edit_file", "path": "src/x.rs", "old": "exact existing text", "new": "replacement"}   replace one exact, unique snippet
{"tool": "run", "command": "cargo test -p foo 2>&1 | tail -40"}   run a shell command in the checkout (bash, 180 s limit)
{"tool": "done", "summary": "what you changed and how you verified it"}   finish

Rules: paths are relative to the checkout. Look before you change things. Prefer edit_file over rewriting large files. Run the project's tests or build after changing code. Stay on the branch you are on: never switch branches, stash or reset. Commit your work before `done` with a real message such as `fix: correct the sign in add()` or `docs: split E10 into sprints` (the prefix is one of feat, fix, refactor, docs, test, chore; no trailers). Never commit scratch files such as test output or notes to yourself. Never push. If the task is impossible, use `done` and say why."#;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    List(String),
    Read(String),
    Write { path: String, content: String },
    Edit { path: String, old: String, new: String },
    Run(String),
    Done(String),
}

fn action_from(v: &Value) -> Option<Action> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let tool = s("tool").or_else(|| s("action")).or_else(|| s("name"))?;
    Some(match tool.as_str() {
        "list" | "list_dir" | "ls" => Action::List(s("path").unwrap_or_else(|| ".".into())),
        "read_file" | "read" => Action::Read(s("path")?),
        "write_file" | "write" => Action::Write { path: s("path")?, content: s("content")? },
        "edit_file" | "edit" => Action::Edit { path: s("path")?, old: s("old")?, new: s("new")? },
        "run" | "run_shell" | "shell" | "bash" => Action::Run(s("command").or_else(|| s("cmd"))?),
        "done" | "finish" => Action::Done(s("summary").or_else(|| s("message")).unwrap_or_default()),
        _ => return None,
    })
}

/// The first JSON object in `text` that names a known action. Fenced blocks, prose around the object
/// and `<think>` preambles are all tolerated.
pub fn parse_action(text: &str) -> Option<Action> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(off) = text[start..].find('{') {
        let open = start + off;
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for (i, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                _ if esc => esc = false,
                b'\\' if in_str => esc = true,
                b'"' => in_str = !in_str,
                b'{' if !in_str => depth += 1,
                b'}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(a) = serde_json::from_str::<Value>(&text[open..=i]).ok().as_ref().and_then(action_from) {
                            return Some(a);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        start = open + 1;
    }
    None
}

fn action_from_tool_call(tc: &ToolCall) -> Option<Action> {
    let mut v = tc.arguments.clone();
    if let Value::String(s) = &v {
        v = serde_json::from_str(s).ok()?;
    }
    v.as_object_mut()?.insert("tool".into(), Value::String(tc.name.clone()));
    action_from(&v)
}

/// `rel` resolved inside `root`; `..` and absolute paths that leave it are refused.
fn within(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    let joined = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    if out.starts_with(root) {
        Ok(out)
    } else {
        Err(format!("error: {rel} is outside the checkout"))
    }
}

fn clip(s: &str) -> String {
    if s.len() <= OUTPUT_CHARS {
        return s.to_string();
    }
    let cut = |i: usize| (0..=i).rev().find(|&j| s.is_char_boundary(j)).unwrap_or(0);
    let head = cut(OUTPUT_CHARS * 2 / 3);
    let tail = cut(s.len() - OUTPUT_CHARS / 3);
    format!("{}\n[... {} chars cut ...]\n{}", &s[..head], tail - head, &s[tail..])
}

/// Runs one action; the returned text goes back to the model.
pub fn perform(root: &Path, action: &Action) -> String {
    let res = match action {
        Action::List(rel) => within(root, rel).and_then(|p| {
            let mut names: Vec<String> = std::fs::read_dir(&p)
                .map_err(|e| format!("error: {e}"))?
                .filter_map(|e| e.ok())
                .map(|e| {
                    let dir = e.file_type().is_ok_and(|t| t.is_dir());
                    format!("{}{}", e.file_name().to_string_lossy(), if dir { "/" } else { "" })
                })
                .filter(|n| n != ".git/")
                .collect();
            names.sort();
            Ok(names.join("\n"))
        }),
        Action::Read(rel) => within(root, rel).and_then(|p| {
            let text = std::fs::read_to_string(&p).map_err(|e| format!("error: {e}"))?;
            Ok(text.lines().enumerate().map(|(i, l)| format!("{:>5}  {l}", i + 1)).collect::<Vec<_>>().join("\n"))
        }),
        Action::Write { path, content } => within(root, path).and_then(|p| {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("error: {e}"))?;
            }
            std::fs::write(&p, content).map_err(|e| format!("error: {e}"))?;
            Ok(format!("wrote {path} ({} bytes)", content.len()))
        }),
        Action::Edit { path, old, new } => within(root, path).and_then(|p| {
            let text = std::fs::read_to_string(&p).map_err(|e| format!("error: {e}"))?;
            match text.matches(old.as_str()).count() {
                0 => Err(format!("error: `old` text not found in {path}; read the file and copy it exactly")),
                1 => {
                    std::fs::write(&p, text.replacen(old.as_str(), new, 1)).map_err(|e| format!("error: {e}"))?;
                    Ok(format!("edited {path}"))
                }
                n => Err(format!("error: `old` text appears {n} times in {path}; include more surrounding lines")),
            }
        }),
        Action::Run(cmd) => {
            let out = Command::new("timeout")
                .arg(COMMAND_SECS.to_string())
                .args(["bash", "-lc", cmd])
                .current_dir(root)
                .stdin(std::process::Stdio::null())
                .output();
            match out {
                Ok(o) => {
                    let code = o.status.code().map_or("killed".to_string(), |c| c.to_string());
                    Ok(format!("exit {code}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
                }
                Err(e) => Err(format!("error: could not run the command: {e}")),
            }
        }
        Action::Done(s) => Ok(s.clone()),
    };
    clip(&res.unwrap_or_else(|e| e))
}

/// Shortens the oldest tool results until the conversation fits `CONTEXT_CHARS`. The system prompt,
/// the task and the last few turns always stay whole.
fn fit(messages: &mut [ChatMessage]) {
    let total = |m: &[ChatMessage]| m.iter().map(|x| x.content.len()).sum::<usize>();
    let keep_tail = 6;
    let mut i = 2;
    while total(messages) > CONTEXT_CHARS && i + keep_tail < messages.len() {
        let m = &mut messages[i];
        if m.role == "user" && m.content.len() > 400 {
            let head: String = m.content.chars().take(300).collect();
            m.content = format!("{head}\n[older output shortened]");
        }
        i += 1;
    }
}

pub struct CoderRun {
    pub success: bool,
    pub transcript: String,
    pub error: String,
}

/// The loop. `turn` sends the conversation to the pool and returns its outcome; injected for tests.
pub fn run_loop(
    root: &Path,
    task: &str,
    deadline: Instant,
    turn: &mut dyn FnMut(&[ChatMessage]) -> Result<PoolAgentOutcome>,
) -> Result<CoderRun> {
    let mut messages = vec![
        ChatMessage { role: "system".into(), content: SYSTEM.into() },
        ChatMessage { role: "user".into(), content: format!("Checkout: {}\n\nTask:\n{task}", root.display()) },
    ];
    let mut transcript = String::new();
    let mut idle = 0;

    for step in 1..=MAX_STEPS {
        if Instant::now() >= deadline {
            return Ok(CoderRun { success: false, transcript, error: "timed out".into() });
        }
        fit(&mut messages);
        let resp = loop {
            match turn(&messages)? {
                PoolAgentOutcome::Ok(r) => break r,
                PoolAgentOutcome::Exhausted { earliest_recovery_ms } => {
                    let wait = Duration::from_millis((earliest_recovery_ms - crate::pool::ledger::now_ms()).max(1000) as u64);
                    if wait > MAX_CAPACITY_WAIT || Instant::now() + wait >= deadline {
                        return Ok(CoderRun {
                            success: false,
                            transcript,
                            error: format!("single-pool: rate limited — every keyed provider is exhausted or benched (step {step})"),
                        });
                    }
                    std::thread::sleep(wait);
                }
            }
        };
        let (visible, _) = crate::pool::client::extract_think(&resp.content);
        let action = resp.tool_calls.iter().find_map(action_from_tool_call).or_else(|| parse_action(&visible));
        messages.push(ChatMessage { role: "assistant".into(), content: if visible.trim().is_empty() { resp.content.clone() } else { visible.clone() } });

        let Some(action) = action else {
            idle += 1;
            transcript.push_str(&format!(
                "\n== step {step}: no action (finish: {:?}) ==\n{}\n",
                resp.finish_reason,
                resp.content.chars().take(800).collect::<String>()
            ));
            if idle > MAX_IDLE_REPLIES {
                return Ok(CoderRun { success: !visible.trim().is_empty(), transcript, error: "ended without a done action".into() });
            }
            messages.push(ChatMessage { role: "user".into(), content: "Reply with exactly one JSON action object, e.g. {\"tool\": \"list\", \"path\": \".\"}.".into() });
            continue;
        };
        idle = 0;
        if let Action::Done(summary) = &action {
            transcript.push_str(&format!("\n== done ==\n{summary}\n"));
            return Ok(CoderRun { success: true, transcript, error: String::new() });
        }
        let result = perform(root, &action);
        transcript.push_str(&format!("\n== step {step}: {action:?}\n{}\n", result.chars().take(500).collect::<String>()));
        messages.push(ChatMessage { role: "user".into(), content: format!("Result:\n{result}") });
    }
    Ok(CoderRun { success: false, transcript, error: format!("reached {MAX_STEPS} steps without finishing") })
}

/// Production wiring used by `task::execute` for `single-pool` work dispatches.
pub fn run_as_task(
    conn: &Connection,
    root: &Path,
    prompt: &str,
    timeout: Duration,
    require_structured_output: bool,
) -> Result<divisi_protocol::RunOutcome> {
    let started = Instant::now();
    let candidates = crate::pool_agent::candidates_from_keys(conn, require_structured_output)?;
    let strategy = bandit::Strategy::Balanced;
    divisi_core::redact::ensure_schema(conn)?;
    let redact_store = divisi_core::redact::RedactStore { conn };
    let resolved = divisi_core::redact::resolve(&redact_store, &divisi_core::secrets::SecretTool, prompt)?;
    let resolve_secret: &SecretFn = &|platform: &str, key_id: &str| {
        use divisi_core::secrets::{SecretStore, SecretTool};
        SecretStore::get(&SecretTool, &divisi_core::pool_keys::secret_name(platform, key_id)).ok().flatten()
    };
    let dispatch: &DispatchFn = &|req, provider, key| crate::pool::client::native::dispatch_for_wire(req, provider, key);
    let mut turn = |messages: &[ChatMessage]| {
        let build = |_: &str, _: &str| PoolRequest { messages: messages.to_vec(), max_tokens: Some(4096), ..Default::default() };
        execute_with(conn, &strategy, &candidates, resolve_secret, dispatch, &build)
    };
    let run = run_loop(root, &resolved, started + timeout, &mut turn)?;
    Ok(divisi_protocol::RunOutcome {
        success: run.success,
        stdout: run.transcript,
        stderr: run.error,
        exit_code: Some(if run.success { 0 } else { 1 }),
        timed_out: started.elapsed() >= timeout,
        cancelled: false,
        duration_ms: started.elapsed().as_millis(),
        usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::client::PoolResponse;

    fn reply(text: &str) -> PoolAgentOutcome {
        PoolAgentOutcome::Ok(PoolResponse { content: text.into(), ..Default::default() })
    }

    #[test]
    fn parses_an_action_inside_prose_and_fences() {
        let t = "Let me look.\n```json\n{\"tool\": \"read_file\", \"path\": \"a {b}.md\"}\n```";
        assert_eq!(parse_action(t), Some(Action::Read("a {b}.md".into())));
        assert_eq!(parse_action("{\"x\": 1} then {\"tool\":\"run\",\"command\":\"ls\"}"), Some(Action::Run("ls".into())));
        assert_eq!(parse_action("no json here"), None);
    }

    #[test]
    fn native_tool_calls_map_onto_actions() {
        let tc = ToolCall { name: "edit_file".into(), arguments: Value::String("{\"path\":\"a\",\"old\":\"x\",\"new\":\"y\"}".into()) };
        assert_eq!(action_from_tool_call(&tc), Some(Action::Edit { path: "a".into(), old: "x".into(), new: "y".into() }));
    }

    #[test]
    fn paths_cannot_leave_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(perform(dir.path(), &Action::Read("../../etc/passwd".into())).contains("outside the checkout"));
        assert!(perform(dir.path(), &Action::Write { path: "/tmp/x".into(), content: "y".into() }).contains("outside the checkout"));
    }

    #[test]
    fn edit_needs_one_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "a a b").unwrap();
        assert!(perform(dir.path(), &Action::Edit { path: "f".into(), old: "a".into(), new: "c".into() }).contains("2 times"));
        assert_eq!(perform(dir.path(), &Action::Edit { path: "f".into(), old: "b".into(), new: "c".into() }), "edited f");
        assert_eq!(std::fs::read_to_string(dir.path().join("f")).unwrap(), "a a c");
    }

    #[test]
    fn a_run_writes_files_and_finishes_across_turns() {
        let dir = tempfile::tempdir().unwrap();
        let script = [
            r#"{"tool":"write_file","path":"hello.txt","content":"hi"}"#,
            r#"{"tool":"run","command":"cat hello.txt"}"#,
            r#"{"tool":"done","summary":"wrote hello.txt"}"#,
        ];
        let mut i = 0;
        let mut seen_results = Vec::new();
        let mut turn = |m: &[ChatMessage]| {
            seen_results.push(m.last().unwrap().content.clone());
            i += 1;
            Ok(reply(script[i - 1]))
        };
        let run = run_loop(dir.path(), "write hello", Instant::now() + Duration::from_secs(30), &mut turn).unwrap();
        assert!(run.success, "{}", run.error);
        assert!(seen_results[2].contains("exit 0") && seen_results[2].contains("hi"), "{seen_results:?}");
    }

    #[test]
    fn a_turn_that_finds_the_pool_exhausted_for_long_gives_the_task_back_as_rate_limited() {
        let dir = tempfile::tempdir().unwrap();
        let mut turn = |_: &[ChatMessage]| Ok(PoolAgentOutcome::Exhausted { earliest_recovery_ms: crate::pool::ledger::now_ms() + 3_600_000 });
        let run = run_loop(dir.path(), "x", Instant::now() + Duration::from_secs(30), &mut turn).unwrap();
        assert!(!run.success);
        assert!(divisi_core::ratelimit::looks_like_rate_limit(&run.error), "{}", run.error);
    }

    #[test]
    fn replies_without_actions_are_nudged_then_taken_as_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = 0;
        let mut turn = |_: &[ChatMessage]| {
            n += 1;
            Ok(reply("just prose"))
        };
        let run = run_loop(dir.path(), "x", Instant::now() + Duration::from_secs(30), &mut turn).unwrap();
        assert_eq!(n, MAX_IDLE_REPLIES + 1);
        assert!(run.transcript.contains("just prose"));
    }
}
