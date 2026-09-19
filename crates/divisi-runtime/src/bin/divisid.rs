//! The headless SingleCLI runtime daemon. `divisi-cli` and `divisi-tui` are
//! both just clients of this over the Unix socket — per spec section 3,
//! the runtime must be headless and the TUI is only one client.

use divisi_core::DivisiDirs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let dirs = DivisiDirs::discover()?;
    dirs.ensure_created()?;

    // The launcher's pinned `PATH` (the systemd unit's, in practice) is
    // hand-maintained and goes stale — a new agent bin dir or a bumped
    // Node version drops out of detection until it is edited. Append the
    // standard install locations that exist, keeping the launcher's own
    // entries first so nothing it set is overridden.
    if let Ok(home) = divisi_core::paths::real_home_dir() {
        let path = divisi_agent_sdk::augmented_path(std::env::var("PATH").ok().as_deref(), &home);
        std::env::set_var("PATH", path);
    }

    divisi_runtime::server::serve(&dirs.socket_path()).await
}
