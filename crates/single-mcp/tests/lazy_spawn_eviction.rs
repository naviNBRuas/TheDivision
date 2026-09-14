//! End-to-end verification of `gateway.rs`'s lazy-spawn / reuse / idle-eviction
//! claims against a *real* OS process tree, not the in-process unit tests in
//! `src/gateway.rs` (which only exercise `is_idle`'s pure threshold math and
//! the notes round-trip — neither ever spawns a downstream MCP server).
//!
//! Spawns the compiled `single-mcp` binary as a real child (exactly as an
//! agent CLI would), speaks real MCP protocol to it via `rmcp`'s client
//! transport, and walks `/proc` to confirm: a downstream server's process
//! tree appears only after first use, a second call reuses the same PIDs,
//! and the idle sweep actually terminates those OS processes (not just an
//! in-memory map entry) once the (env-overridden, short) timeout elapses.
//!
//! Requires `npx` on PATH and the `@modelcontextprotocol/server-memory`
//! package resolvable (cached locally after first run) — skips itself with a
//! message if `npx` is unavailable, since CI/sandboxes may lack it.

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use std::collections::HashSet;
use std::time::Duration;

fn npx_available() -> bool {
    std::process::Command::new("npx").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// All PIDs in `/proc` whose parent (recursively) is `root`, including
/// `root` itself. Linux-only — matches this test's `#[cfg(unix)]`-adjacent
/// reliance on `/proc` (no `sysinfo` dependency needed for one test).
fn descendant_pids(root: u32) -> HashSet<u32> {
    let mut parent_of: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
            // format: "pid (comm) state ppid ..." — comm may contain
            // spaces/parens, so split on the LAST ')' before reading fields.
            let Some(after_comm) = stat.rsplit_once(')') else { continue };
            let fields: Vec<&str> = after_comm.1.split_whitespace().collect();
            let Some(ppid) = fields.get(1).and_then(|s| s.parse::<u32>().ok()) else { continue };
            parent_of.push((pid, ppid));
        }
    }
    let mut result = HashSet::new();
    result.insert(root);
    loop {
        let mut grew = false;
        for &(pid, ppid) in &parent_of {
            if result.contains(&ppid) && result.insert(pid) {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    result
}

fn pid_alive(pid: u32) -> bool {
    std::path::Path::new("/proc").join(pid.to_string()).exists()
}

#[tokio::test]
async fn lazy_spawn_reuse_and_idle_eviction_kill_a_real_process() {
    if !npx_available() {
        eprintln!("skipping: npx not on PATH");
        return;
    }

    let config_dir = tempfile::tempdir().unwrap();
    std::env::set_var("SINGLE_CONFIG_DIR", config_dir.path());
    let dirs = single_core::SingleDirs::discover().unwrap();
    single_core::mcp::save(&dirs.mcp_registry_file(), &single_core::mcp::default_servers()).unwrap();

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_single-mcp"));
    command
        .env("SINGLE_CONFIG_DIR", config_dir.path())
        .env("SINGLE_MCP_IDLE_TIMEOUT_SECS", "2")
        .env("SINGLE_MCP_SWEEP_INTERVAL_SECS", "1");
    let transport = TokioChildProcess::new(command.configure(|_| {})).expect("spawning single-mcp");
    let gateway_pid = transport.id().expect("single-mcp child has a pid");
    let session = ().serve(transport).await.expect("initializing single-mcp gateway");

    let baseline = descendant_pids(gateway_pid);
    assert_eq!(baseline.len(), 1, "no downstream server should be running before first use");

    let invoke = |server: &'static str| {
        let mut args = serde_json::Map::new();
        args.insert("server".into(), server.into());
        args.insert("tool".into(), serde_json::Value::Null);
        CallToolRequestParams::new("invoke_mcp").with_arguments(args)
    };

    // First call: must actually spawn the "memory" server's process tree.
    let result = session.call_tool(invoke("memory")).await.expect("first invoke_mcp call");
    assert!(!result.is_error.unwrap_or(false), "invoke_mcp returned an error: {result:?}");
    tokio::time::sleep(Duration::from_millis(200)).await; // let npx finish forking node
    let after_first = descendant_pids(gateway_pid);
    assert!(after_first.len() > baseline.len(), "expected a new descendant process after first invoke_mcp, got {after_first:?}");

    // Second call, same server: must reuse the same process tree, not spawn another.
    let result2 = session.call_tool(invoke("memory")).await.expect("second invoke_mcp call");
    assert!(!result2.is_error.unwrap_or(false), "second invoke_mcp returned an error: {result2:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_second = descendant_pids(gateway_pid);
    assert_eq!(after_second, after_first, "second call to the same server must reuse the existing process, not spawn a new one");

    let spawned: Vec<u32> = after_first.difference(&baseline).copied().collect();
    assert!(spawned.iter().all(|p| pid_alive(*p)), "spawned server process should still be alive before eviction");

    // Idle sweep (1s interval, 2s timeout) must actually kill the OS process,
    // not just drop an in-memory map entry.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let still_alive: Vec<u32> = spawned.iter().copied().filter(|p| pid_alive(*p)).collect();
    assert!(still_alive.is_empty(), "idle eviction did not kill real OS process(es): {still_alive:?} still in /proc");

    drop(session);
}
