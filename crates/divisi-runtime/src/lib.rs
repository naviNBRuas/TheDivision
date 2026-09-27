pub mod accounting;
pub mod agent_auth;
pub mod agent_cooldown;
pub mod assistant;
pub mod billing;
pub mod bootstrap;
pub mod context;
pub mod coordinator;
pub mod docker;
pub mod doctor;
pub mod documents;
pub mod embeddings;
pub mod handlers;
pub mod integrations;
pub mod knowledge_graph;
pub mod memory;
pub mod orchestrate;
pub mod orchestrate_graph;
pub mod pool;
pub mod pool_agent;
pub mod pool_coder;
pub mod qdrant_backend;
pub mod redis_backend;
pub mod registry;
pub mod self_heal;
pub mod server;
pub mod state;
pub mod task;

pub use context::Context;
pub use handlers::handle;

/// Guards every test in this crate that mutates the process-global `HOME`
/// env var (e.g. via `std::env::set_var("HOME", ...)`) — `HOME` resolution
/// races across any test in the same binary that reads it too, and `cargo
/// test` runs this crate's `integrations.rs` and `handlers.rs` test modules
/// in the same binary, on separate threads, by default. A lock local to
/// just one of those files does not serialize against the other, so this
/// one shared static is acquired by both.
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes `HOME_ENV_LOCK`, recovering it if an earlier test panicked while holding it: `TempHome` restores
/// `HOME` on unwind, so a poisoned lock no longer means a broken environment.
#[cfg(test)]
pub(crate) fn home_env_lock() -> std::sync::MutexGuard<'static, ()> {
    HOME_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Points `HOME` at `path` while holding `HOME_ENV_LOCK`, and puts the original `HOME` back when dropped,
/// even if the test panics. Tests used to `remove_var("HOME")` afterwards, which left every later test in
/// the binary with no `HOME` at all and made the outcome depend on test order.
#[cfg(test)]
pub(crate) struct TempHome {
    original: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TempHome {
    pub(crate) fn set(path: &std::path::Path) -> Self {
        let lock = home_env_lock();
        let original = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { original, _lock: lock }
    }
}

#[cfg(test)]
impl Drop for TempHome {
    fn drop(&mut self) {
        match self.original.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Same reasoning as `HOME_ENV_LOCK`, for `DIVISI_SELF_HEAL_AGENT_INSTALL`
/// — `self_heal::agent`'s test sets/clears it and `doctor`'s test reads
/// it, in the same test binary, on separate threads, by default.
#[cfg(test)]
pub(crate) static SELF_HEAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
