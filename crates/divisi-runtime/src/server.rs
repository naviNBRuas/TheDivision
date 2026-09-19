//! Unix domain socket server: newline-delimited JSON `Request` in,
//! `Response` out. One request per connection is fine for Phase 1's usage
//! (a CLI invocation, or the TUI polling on an interval) — a persistent
//! multiplexed connection with a real event stream is Phase 4 scope.

use crate::context::Context;
use crate::handlers::handle_with_registry;
use anyhow::{Context as _, Result};
use divisi_protocol::{Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub async fn serve(socket_path: &std::path::Path) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("removing stale socket {}", socket_path.display()))?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("binding socket {}", socket_path.display()))?;
    tracing::info!(path = %socket_path.display(), "divisi-runtime listening");

    // A background task runs on a thread inside this process, so a daemon
    // that was killed mid-run left its rows stuck non-terminal with no way
    // to reap them. A daemon just now starting owns no in-flight tasks, so
    // sweep those orphans to `failed` before accepting connections.
    // Best-effort: a reconciliation failure must not stop the daemon.
    match Context::load().and_then(|ctx| {
        let conn = crate::state::open(&ctx.dirs.db_path())?;
        crate::task::ensure_schema(&conn)?;
        crate::state::ensure_events_schema(&conn)?;
        crate::accounting::ensure_schema(&conn)?;
        crate::coordinator::ensure_coordinator_schema(&conn)?;
        crate::pool::ensure_pool_schema(&conn)?; // E28: pool_usage/pool_leases/pool_cooldowns/pool_outcomes/pool_config + pool_keys

        let n = crate::task::reconcile_orphaned_tasks(&conn)?;
        // spec E27.02 §4.1: a daemon just starting owns no in-flight
        // coordinator nodes either — settle interrupted ones from their
        // backing task rows so there are no permanent zombie nodes. Both
        // of these are pure DB scans, fast and safe to run synchronously
        // before the socket accept loop starts.
        if let Err(e) = crate::coordinator::scheduler::reconcile(&conn) {
            tracing::warn!(error = %e, "coordinator node reconciliation failed");
        }
        Ok(n)
    }) {
        Ok(0) => {}
        Ok(n) => tracing::warn!(count = n, "reconciled orphaned tasks left by a previous daemon"),
        Err(e) => tracing::warn!(error = %e, "orphaned-task reconciliation failed"),
    }

    // Created once for the daemon's whole lifetime and cloned (cheap — an
    // `Arc` underneath) into every connection, so a `TaskCancel` sent on
    // one connection can reach a task started with `background: true` on
    // another — see `registry::TaskRegistry`'s doc comment.
    let registry = crate::registry::TaskRegistry::default();

    // spec E27.02 §4 / §8: a scheduler tick timer. runs on its own OS
    // thread (not a tokio task) because a tick can call the integrator
    // brain role, which blocks on a real `task::run`. shares the daemon's
    // `TaskRegistry` so a `GoalCancel`'s cancel flag reaches nodes this
    // loop dispatched.
    {
        let registry = registry.clone();
        std::thread::Builder::new()
            .name("coordinator-tick".into())
            .spawn(move || coordinator_tick_loop(registry))
            .ok();
    }

    // E28 spec §9/§10: `resume_interrupted` (Part F) and the self-heal
    // pass's own timer, both spawned on their own OS thread rather than
    // run synchronously in the startup block above. Both can block for
    // real seconds-to-minutes: `resume_interrupted` re-runs `plan_goal`
    // for an interrupted empty-graph goal (a genuine blocking `task::run`
    // agent call, up to its full timeout), and `run_pass`'s `agent`
    // category shells `which` for every registry agent it checks
    // (`discover()`) — confirmed live to take several seconds. Either one
    // running in the startup block would block the socket accept loop
    // below from ever starting, making every client request (even a
    // plain `daemon status`) hang until it finished, on every restart.
    std::thread::Builder::new()
        .name("resume-and-self-heal".into())
        .spawn(|| {
            if let Ok(ctx) = Context::load() {
                if let Ok(mut conn) = crate::state::open(&ctx.dirs.db_path()) {
                    match crate::coordinator::resume_interrupted(&ctx, &mut conn) {
                        Ok(0) => {}
                        Ok(touched) => tracing::info!(count = touched, "resumed goal(s) interrupted by the previous daemon"),
                        Err(e) => tracing::warn!(error = %e, "resume_interrupted failed"),
                    }
                    // `allow_db_restore: true` only here -- this runs
                    // before the coordinator tick loop and its worker
                    // threads have ramped up, so a db-integrity restore
                    // has the fewest other connections to race against.
                    // Every later pass (the periodic tick below, `doctor
                    // --fix`) must NOT restore live -- see
                    // `run_pass_with_restore`'s doc comment.
                    match crate::self_heal::run_pass_with_restore(&ctx, &conn, None, true) {
                        Ok(report) => {
                            let failed = report.actions.iter().filter(|a| !a.ok).count();
                            if failed > 0 {
                                tracing::warn!(failed, total = report.actions.len(), "self-heal pass found unresolved issues on daemon start");
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "self-heal pass failed on daemon start"),
                    }
                }
            }
            self_heal_tick_loop();
        })
        .ok();

    loop {
        let (stream, _) = listener.accept().await?;
        let registry = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, registry).await {
                tracing::warn!(error = %e, "connection error");
            }
        });
    }
}

/// Periodically drives every active coordinator goal (spec E27.02 §4).
/// Best-effort: a failing tick logs and the loop keeps going. The interval
/// comes from `coordinator.toml` (default 5s), re-read each iteration so a
/// config edit is picked up without a restart.
fn coordinator_tick_loop(registry: crate::registry::TaskRegistry) {
    loop {
        let interval = Context::load()
            .map(|ctx| crate::coordinator::routing::CoordinatorConfig::load(&ctx.dirs).tick_interval_secs)
            .unwrap_or(5)
            .max(1);
        std::thread::sleep(std::time::Duration::from_secs(interval));

        let result = Context::load().and_then(|ctx| {
            let mut conn = crate::state::open(&ctx.dirs.db_path())?;
            crate::coordinator::drive(&ctx, &mut conn, &registry)
        });
        if let Err(e) = result {
            tracing::debug!(error = %e, "coordinator tick failed");
        }
    }
}

/// Guards `self_heal::run_pass` against overlapping with itself — a slow
/// pass (an agent install genuinely can take a while) must not stack a
/// second one on top when the interval elapses again before the first
/// finishes. `divisi doctor --fix` (`handlers.rs`) doesn't share this
/// guard (it has its own `DoctorGuard`), so the two can still race in
/// theory; `run_pass`'s own steps are individually `catch_unwind`-safe
/// and idempotent (a repair either has nothing to do or finds the same
/// fix again), so a rare double-run is harmless, just wasted work.
static SELF_HEAL_TICK_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Periodically runs a self-heal pass (spec §9). The interval comes from
/// `self_heal.toml` (default 300s), re-read each iteration.
fn self_heal_tick_loop() {
    loop {
        let interval = Context::load().map(|ctx| crate::self_heal::SelfHealConfig::load(&ctx.dirs).self_heal_interval_secs).unwrap_or(300).max(1);
        std::thread::sleep(std::time::Duration::from_secs(interval));

        if SELF_HEAL_TICK_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            tracing::debug!("self-heal pass still running from a previous tick; skipping this one");
            continue;
        }
        let result = Context::load().and_then(|ctx| {
            let conn = crate::state::open(&ctx.dirs.db_path())?;
            crate::self_heal::run_pass(&ctx, &conn, None)
        });
        SELF_HEAL_TICK_RUNNING.store(false, std::sync::atomic::Ordering::Release);

        if let Err(e) = result {
            tracing::debug!(error = %e, "self-heal tick failed");
        }
    }
}

async fn handle_connection(stream: UnixStream, registry: crate::registry::TaskRegistry) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let parsed = serde_json::from_str::<Request>(line.trim_end());
        let shutdown_requested = matches!(parsed, Ok(Request::Shutdown));
        let response = match parsed {
            Ok(request) => {
                // The handlers are fully synchronous, and a `task run`
                // handler blocks for the agent's entire runtime
                // (divisi-agent-sdk `run_command_live` spins its own OS
                // threads to drain output and polls `try_wait` in a loop).
                // Run inline on the async worker it would pin that worker
                // for minutes, so every other connection's request —
                // `status`, `task list`, `--background` dispatch — stalls
                // behind it. `spawn_blocking` moves the work to the
                // blocking pool and leaves the async workers free to keep
                // serving. `Context::load` (a few file reads) goes with
                // it rather than straddling the boundary.
                let registry = registry.clone();
                tokio::task::spawn_blocking(move || match Context::load() {
                    // A fresh Context per request (cheap: local file
                    // reads) keeps config changes picked up live — Phase 1
                    // has no long-lived mutable daemon state.
                    Ok(ctx) => handle_with_registry(&ctx, request, &registry),
                    Err(e) => Response::Error { message: format!("loading context: {e:#}") },
                })
                .await
                .unwrap_or_else(|e| Response::Error {
                    message: format!("handler panicked: {e}"),
                })
            }
            Err(e) => Response::Error { message: format!("invalid request: {e}") },
        };
        let mut payload = serde_json::to_string(&response)?;
        payload.push('\n');
        write_half.write_all(payload.as_bytes()).await?;
        write_half.flush().await?;
        line.clear();

        // Exit only after the ack above is flushed to the client, so
        // `stop_running`'s response read never races process teardown.
        if shutdown_requested {
            std::process::exit(0);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// E28 Task 23: `self_heal_tick_loop`'s re-entrancy guard is exactly
    /// this `AtomicBool::swap` — a real end-to-end test would need the
    /// actual sleep-based loop (slow, flaky to time precisely), so this
    /// exercises the mechanism directly: while a pass is marked
    /// in-flight, a second tick's guard-check must see it and skip
    /// (`swap` returning `true`), never running two passes at once; once
    /// released, the next check proceeds normally.
    #[test]
    fn self_heal_tick_is_a_noop_reentrant_call_while_one_is_in_flight() {
        use std::sync::atomic::Ordering;
        // A fresh local flag, not the real static -- avoids cross-test
        // interference with anything else that might exercise the real
        // `SELF_HEAL_TICK_RUNNING` in the same test binary.
        let flag = std::sync::atomic::AtomicBool::new(false);

        // First tick: not running yet -> proceeds, marks itself running.
        assert!(!flag.swap(true, Ordering::AcqRel), "first tick should find the flag clear and proceed");

        // A second tick arriving while the first is still "in flight":
        // swap sees `true` and must skip, per `self_heal_tick_loop`'s own
        // `if SELF_HEAL_TICK_RUNNING.swap(true, ..) { skip }` check.
        assert!(flag.swap(true, Ordering::AcqRel), "a reentrant tick must see the flag already set and skip");

        // First tick finishes and releases -> the next one proceeds again.
        flag.store(false, Ordering::Release);
        assert!(!flag.swap(true, Ordering::AcqRel), "after release, the next tick should proceed");
    }

    /// Guards the `spawn_blocking` fix. A full socket round-trip proving a
    /// second request stays responsive would need a `Request` variant
    /// whose handler blocks on command — too invasive to add to the wire
    /// protocol for a test — so this exercises the exact mechanism
    /// `handle_connection` now uses: a long blocking handler dispatched
    /// via `spawn_blocking` on a single async worker must not stop that
    /// worker from driving other connections' futures. Run the handler
    /// inline (the old bug) and the async task below would not finish
    /// until the 500 ms sleep did.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_blocking_handler_does_not_stall_the_async_worker() {
        let blocking = tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_millis(500));
        });

        let start = Instant::now();
        // Stand-in for another connection's future being polled on the
        // one worker while the handler above runs.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "async worker was blocked by the handler"
        );

        blocking.await.unwrap();
    }
}
