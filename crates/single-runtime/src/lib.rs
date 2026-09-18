pub mod accounting;
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

/// Same reasoning as `HOME_ENV_LOCK`, for `SINGLE_SELF_HEAL_AGENT_INSTALL`
/// — `self_heal::agent`'s test sets/clears it and `doctor`'s test reads
/// it, in the same test binary, on separate threads, by default.
#[cfg(test)]
pub(crate) static SELF_HEAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
