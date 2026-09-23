//! End-to-end: `task run --allow-fallback` (exercised through the public
//! `divisi_runtime::task::run` API with `allow_fallback: true`) hops to the
//! next entry of a configured fallback chain when the first agent fails
//! with a rate-limit signal. Uses two stub agent *scripts* on `$PATH` — no
//! real agent CLI needed — plus a real custom-agent definition and a real
//! `fallback.toml`, so the whole chain is exercised the same way a live
//! dispatch would be.

use divisi_core::DivisiDirs;
use divisi_protocol::TaskStatus;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Writes an executable `bin/<name>` script with `body` (used as a stub
/// agent CLI), making it discoverable via `which` and spawnable like a real
/// agent binary once its directory is on `$PATH`.
fn write_stub_script(bin: &std::path::Path, name: &str, body: &str) {
    let path = bin.join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// A custom agent definition (`~/.config/divisi/agents/<name>.toml`) whose
/// `command` is a stub script on `$PATH`; the `[run]` spec makes it a
/// one-shot prompt→completion wrapper. The scripts here ignore their prompt
/// argument entirely.
fn write_stub_agent(agents_dir: &std::path::Path, name: &str, command: &str) {
    std::fs::write(
        agents_dir.join(format!("{name}.toml")),
        format!(
            r#"
name = "{name}"
command = "{command}"

[run]
mode = "flag"
value = "-c"
"#
        ),
    )
    .unwrap();
}

#[test]
fn allow_fallback_hops_to_the_next_agent_on_a_429_rate_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("DIVISI_CONFIG_DIR", dir.path());

    // Stub agent CLIs live in a bin dir prepended to the test's `$PATH` so
    // both discovery (`which`) and subprocess spawn find them.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_stub_script(
        &bin,
        "divisi-stub-rate-limited",
        "#!/bin/sh\necho '429 Too Many Requests' >&2\nexit 1\n",
    );
    write_stub_script(
        &bin,
        "divisi-stub-recovery",
        "#!/bin/sh\necho 'recovered via fallback'\nexit 0\n",
    );
    let path = {
        let mut path = bin.to_str().unwrap().to_string();
        if let Ok(existing) = std::env::var("PATH") {
            path.push(':');
            path.push_str(&existing);
        }
        path
    };
    std::env::set_var("PATH", path);

    let dirs = DivisiDirs::from_root(dir.path().to_path_buf());
    dirs.ensure_created().unwrap();

    // First hop: prints a 429 banner and exits 1 (a detectably
    // rate-limited failure). Second hop: succeeds.
    write_stub_agent(&dirs.agents_dir(), "rate-limited-agent", "divisi-stub-rate-limited");
    write_stub_agent(&dirs.agents_dir(), "recovery-agent", "divisi-stub-recovery");

    // The fallback chain that makes the hop legal: rate-limited-agent ->
    // recovery-agent.
    use divisi_protocol::AgentAccountRef;
    divisi_core::fallback::set(
        &dirs.fallback_registry_file(),
        vec![
            AgentAccountRef { agent: "rate-limited-agent".into(), account: None },
            AgentAccountRef { agent: "recovery-agent".into(), account: None },
        ],
    )
    .unwrap();

    let ctx = divisi_runtime::Context {
        dirs,
        resolved: divisi_core::ResolvedConfig::default(),
        registry: divisi_core::builtin_registry(),
    };
    let conn = divisi_runtime::state::open(&ctx.dirs.db_path()).unwrap();
    divisi_runtime::task::ensure_schema(&conn).unwrap();

    let cwd = dir.path().to_path_buf();
    let opts = divisi_runtime::task::RunTaskOptions {
        description: "probe the stub rate-limited agent",
        agent: "rate-limited-agent",
        cwd: &cwd,
        use_worktree: false,
        account: None,
        real_home: true, // skip isolated-home materialization, not under test here
        no_memory_context: true,
        timeout: Duration::from_secs(10),
        allow_fallback: true,
        usage_json: false,
        require_structured_output: false,
        pool_agentic: true,
    };
    let original = divisi_runtime::task::run(&conn, &ctx, opts).unwrap();

    // The original run is a recorded, rate-limited failure.
    assert_eq!(original.status, TaskStatus::Failed, "the 429 stub must fail");
    assert!(original.rate_limited, "the 429 output must be flagged as a rate limit");

    // `--allow-fallback` spun a follow-up task against the next agent in
    // the chain, and that follow-up succeeded.
    let tasks = divisi_runtime::task::list(&conn).unwrap();
    assert_eq!(tasks.len(), 2, "expected exactly the original run plus one fallback hop");
    let follow_up = tasks
        .iter()
        .find(|t| t.agent == "recovery-agent")
        .unwrap_or_else(|| panic!("expected a follow-up task for recovery-agent"));
    assert_ne!(follow_up.id, original.id, "the hop must be a brand-new task");
    assert_eq!(follow_up.status, TaskStatus::Completed, "the fallback target must have succeeded");

    // The follow-up carries the fallback marker, so this is provably a
    // chain hop rather than some second, unrelated run.
    assert!(
        follow_up.description.contains(&format!("[fallback from #{}, rate-limited-agent looked rate-limited]", original.id)),
        "follow-up description was: {}",
        follow_up.description
    );

    // And the follow-up's captured artifact holds the recovery stub's real
    // output, proving the command actually ran end-to-end.
    let artifact = follow_up
        .artifact_path
        .as_deref()
        .expect("a completed fallback task must have a captured artifact");
    let captured = std::fs::read_to_string(artifact).unwrap();
    assert!(captured.contains("recovered via fallback"), "artifact was: {captured}");
}