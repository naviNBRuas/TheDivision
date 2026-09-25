//! run_command_live confines host agents when DIVISI_CONFINE_ROOTS is set (own test binary: sets env).
use divisi_agent_sdk::backend::ExecBackend;
use divisi_agent_sdk::run::run_command_live;
use std::time::Duration;

#[test]
fn host_agents_cannot_write_outside_their_working_directory_under_a_confined_root() {
    if std::process::Command::new("bwrap").arg("--version").output().is_err() {
        eprintln!("bwrap not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let work = root.join("work");
    let shared = root.join("shared-checkout");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::env::set_var("DIVISI_CONFINE_ROOTS", &root);

    let script = format!("echo ok > in.txt; echo leak > {}/leak.txt", shared.display());
    let backend = ExecBackend::Host { home: None, extra_env: None };
    let _ = run_command_live("sh", &["-c".into(), script], &work, &backend, None, Duration::from_secs(30), None);

    assert!(work.join("in.txt").exists(), "the agent's own directory stays writable");
    assert!(!shared.join("leak.txt").exists(), "a write into another checkout under the root must fail");
}
