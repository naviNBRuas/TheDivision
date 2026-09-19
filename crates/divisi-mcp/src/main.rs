//! `divisi-mcp`: exposes SingleCLI's own agent/task/orchestrate/memory/
//! provider commands as MCP tools — see `server.rs`'s module doc.

mod client;
mod server;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = server::DivisiServer::new()?.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Shared test-only helpers. `DivisiDirs::discover()` reads the
/// process-global `SINGLE_CONFIG_DIR`, so every test that points it at a
/// tempdir must serialize on one lock — spanning both `client` and
/// `server` test modules, not one per file.
#[cfg(test)]
pub(crate) mod testutil {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII: takes the lock (poison-tolerant), points `SINGLE_CONFIG_DIR`
    /// at a fresh tempdir, and clears it on `Drop` — so a test that panics
    /// on an assertion still leaves the env clean for the next one.
    pub(crate) struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
    }

    impl EnvGuard {
        pub(crate) fn path(&self) -> &std::path::Path {
            self.dir.path()
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("SINGLE_CONFIG_DIR");
        }
    }

    pub(crate) fn isolated_env() -> EnvGuard {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SINGLE_CONFIG_DIR", dir.path());
        EnvGuard { _lock: lock, dir }
    }
}
