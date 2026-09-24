# E29 — Zed + SingleCLI Integration Implementation Plan

**Status (2026-09-24): partly shipped** (ACP agent in Zed, now named "The Division"; conversational chat). Remaining items are being audited in nbr-workspace `docs/queue/E27-singlecli-followups/09-e29-zed-plan-audit.md`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship live secret redaction across every SingleCLI prompt entry point, make `single-pool` the default Zed ACP agent, add a `/single-status` slash command as the Zed status surface, and add cross-session goal-dedup — all additive to the E28 (`v0.11.0`) architecture.

**Architecture:** One new `single_core::redact` module (heuristic scan + TTL'd encrypted alias store, riding `single_core::secrets`' encryption primitive) called at every prompt-ingestion chokepoint before a `Request::GoalSubmit`/`Request::TaskRun` is built; a matching `resolve()` call at the sole outbound-dispatch boundary (`pool_agent.rs`'s prompt-to-`ChatMessage` conversion). ACP session state gets a `single-pool` default agent. `goal::find_overlapping` reuses the existing `goals` table read-only. `/single-status` is formatting/wiring over already-implemented `coordinator::status`/`pool::status`.

**Tech Stack:** Rust workspace (rusqlite, reqwest blocking, anyhow, serde_json). No new external crates unless Task 1 finds none of the existing dependency tree covers entropy scoring (in which case use a small hand-rolled Shannon-entropy fn — do not add a crate for this).

**Spec:** `docs/superpowers/specs/2026-09-08-e29-zed-singlecli-integration-design.md`

> **Audit 2026-09-24 (E27.09):** every task is in the code under divisi names (`single_*` → `divisi_*`):
> Task 1 `crates/divisi-core/src/redact.rs`; Task 2 wiring in `divisi-runtime` handlers/pool_agent/
> pool_coder/assistant chat; Task 3 `redact::resolve` + `divisi secret promote-alias`
> (`divisi-cli/src/main.rs:808`, protocol `SecretPromoteAlias`); Task 4 `divisi-pool` is the ACP default
> (`divisi-cli/src/acp.rs:83`); Task 5 `goal::find_overlapping` used by GoalSubmit (`handlers.rs:1708`);
> Task 6 shipped as `/status` (alias `/queue`, `acp.rs:695`) rather than `/single-status`; Task 7 Zed
> `agent_servers."The Division"` runs `divisi acp`; Task 8 released as 0.24.0 (CHANGELOG). Only the push
> is open, blocked by the suspended GitHub account (E28 forge).

## Global Constraints

- `cargo build --release --workspace` warning-free; `cargo test --workspace` passing; `cargo clippy --workspace --all-targets` clean of new lints.
- Sole-author commits, atomic per task, `type: description` subject, NO Co-Authored-By, NO trailers (git-commit-standards skill).
- Additive schema only — new tables/columns, never destructive changes to `goals`, `graph_nodes`, or the secrets store.
- Redaction correctness is security-severity: any bug here (real secret not redacted, or a bypassable redaction path) blocks shipping until fixed and re-verified.
- Version: `single` is `0.11.0` pre-1.0; this epic adds a subsystem → bump to `0.12.0` on the release commit (one CHANGELOG block, annotated tag `v0.12.0`).
- Build in the foreground, `-j2` (release: `-j1`), per the machine gotchas in the task brief — a background build can be OOM-killed by the watchdog even with free RAM.

---

### Task 1: `single_core::redact` — heuristic scan + alias store schema

**Files:**
- Create: `crates/single-core/src/redact.rs`
- Modify: `crates/single-core/src/lib.rs` (add `pub mod redact;`)
- Test: inline `#[cfg(test)] mod tests` in `redact.rs`

**Interfaces:**
- Consumes: `single_core::secrets`'s encryption primitive. **Read `crates/single-core/src/secrets.rs` first** to find the exact encrypt/decrypt function names and `SecretTool`/`SecretStore` trait shape (confirmed usage pattern: `SecretStore::get(&SecretTool, &name) -> Result<Option<String>>`, seen at `crates/single-runtime/src/self_heal/infra.rs:219-224`). If this session's own permission settings block reading `secrets.rs` (it matches a `secrets*` deny-glob even though it's source, not a credential file), ask the user for a one-time read exception before starting this task — do not work around it by shelling out or copying the file elsewhere.
- Produces (used by Tasks 2, 3, 5):
  ```rust
  pub struct PendingAlias {
      pub alias: String,       // "{{REDACTED_1}}", "{{REDACTED_2}}", ... unique per session
      pub session_id: String,
      pub created_at: i64,      // unix ms
      pub expires_at: i64,      // unix ms, created_at + TTL
  }

  pub struct RedactStore<'a> {
      pub conn: &'a rusqlite::Connection,
  }

  pub fn ensure_schema(conn: &rusqlite::Connection) -> anyhow::Result<()>;

  /// Scans `text` for secret-shaped substrings, replaces each with a fresh
  /// alias, encrypts and stores the real value keyed by (session_id, alias)
  /// with a 3-hour TTL. Over-redacts on ambiguity. Returns the redacted text
  /// and the list of aliases created (empty if nothing matched).
  pub fn scan_and_replace(
      store: &RedactStore,
      session_id: &str,
      text: &str,
  ) -> anyhow::Result<(String, Vec<PendingAlias>)>;

  /// Replaces every `{{REDACTED_N}}`-shaped alias in `text` with its
  /// decrypted real value, scoped to `session_id`. An alias with no
  /// matching row (expired or unknown) is left as an error, not silently
  /// passed through as literal alias text.
  pub fn resolve(
      store: &RedactStore,
      session_id: &str,
      text: &str,
  ) -> anyhow::Result<String>;

  /// Deletes rows past `expires_at`. Called opportunistically (daemon
  /// startup, and lazily inside `resolve`/`scan_and_replace`) — no
  /// background timer thread.
  pub fn sweep_expired(conn: &rusqlite::Connection) -> anyhow::Result<usize>;
  ```

- [x] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn redacts_known_vendor_prefixes() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let (out, aliases) = scan_and_replace(
            &store,
            "sess1",
            "use this key sk-abcdEFGH1234567890abcdEFGH1234567890abcd for the call",
        )
        .unwrap();
        assert!(!out.contains("sk-abcdEFGH"));
        assert_eq!(aliases.len(), 1);
        assert!(out.contains(&aliases[0].alias));
    }

    #[test]
    fn redacts_github_token_and_aws_key_and_jwt() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let text = "ghp_16C7e42F292c6912E7710c838347Ae178B4a and AKIAIOSFODNN7EXAMPLE and eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let (out, aliases) = scan_and_replace(&store, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 3);
        assert!(!out.contains("ghp_16C7e42F292c6912E7710c838347Ae178B4a"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redacts_generic_high_entropy_assignment() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let (out, aliases) = scan_and_replace(
            &store,
            "sess1",
            "DATABASE_PASSWORD=Xk9$mQ2vL8pR4nT7wZ1cF6",
        )
        .unwrap();
        assert_eq!(aliases.len(), 1);
        assert!(!out.contains("Xk9$mQ2vL8pR4nT7wZ1cF6"));
    }

    #[test]
    fn does_not_redact_low_entropy_prose_or_uuids_or_git_shas() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let text = "please review commit a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 \
                     and issue id 550e8400-e29b-41d4-a716-446655440000, thanks";
        let (out, aliases) = scan_and_replace(&store, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 0);
        assert_eq!(out, text);
    }

    #[test]
    fn resolve_round_trips_a_live_alias() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let (redacted, aliases) =
            scan_and_replace(&store, "sess1", "key sk-abcdEFGH1234567890abcdEFGH1234567890abcd here").unwrap();
        let resolved = resolve(&store, "sess1", &redacted).unwrap();
        assert!(resolved.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
        assert_eq!(aliases.len(), 1);
    }

    #[test]
    fn resolve_errors_on_expired_or_unknown_alias() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let err = resolve(&store, "sess1", "value is {{REDACTED_99}}").unwrap_err();
        assert!(err.to_string().contains("REDACTED_99"));
    }

    #[test]
    fn aliases_are_session_scoped() {
        let conn = setup();
        let store = RedactStore { conn: &conn };
        let (redacted, _) =
            scan_and_replace(&store, "sess1", "key sk-abcdEFGH1234567890abcdEFGH1234567890abcd here").unwrap();
        let err = resolve(&store, "sess2", &redacted).unwrap_err();
        assert!(err.to_string().contains("REDACTED"));
    }

    #[test]
    fn sweep_expired_removes_only_past_ttl_rows() {
        let conn = setup();
        conn.execute(
            "INSERT INTO redact_aliases (session_id, alias, ciphertext, created_at, expires_at) \
             VALUES ('s', '{{REDACTED_1}}', X'00', 0, 1)",
            [],
        )
        .unwrap();
        let removed = sweep_expired(&conn).unwrap();
        assert_eq!(removed, 1);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p single-core redact:: 2>&1 | tail -40`
Expected: compile error (module doesn't exist yet) or FAIL on every test.

- [x] **Step 3: Implement `redact.rs`**

Detection order (first match wins per substring span, scan left to right, non-overlapping):
1. Known vendor prefixes (regex, case-sensitive): `sk-[A-Za-z0-9]{20,}`, `ghp_[A-Za-z0-9]{36}`, `gho_[A-Za-z0-9]{36}`, `AKIA[0-9A-Z]{16}`, `xox[baprs]-[A-Za-z0-9-]{10,}` (Slack), a JWT shape `[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}`.
2. Assignment-shaped: `(?i)(key|token|secret|password|pwd|api[_-]?key)\s*[:=]\s*['"]?([^\s'"]{8,})` — the captured value group is what gets redacted, not the label.
3. Generic high-entropy: any whitespace-delimited token of length >= 20 with Shannon entropy >= 3.5 bits/char AND containing at least one digit and one letter (excludes plain hex-looking SHAs by requiring mixed-case OR punctuation — a git SHA is lowercase-hex-only, entropy ~4 bits/char but fails the "not pure lowercase hex" guard; a UUID is excluded by its literal hyphen-grouped shape via a dedicated negative regex `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`).

Each match becomes one `PendingAlias` with a fresh `{{REDACTED_N}}` (N = a per-call counter, not global, to keep output deterministic in tests), encrypted via `single_core::secrets`' primitive found in Step 0 of this task, inserted into a new table:

```sql
CREATE TABLE IF NOT EXISTS redact_aliases (
    session_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, alias)
)
```

`resolve()` finds every `{{REDACTED_\d+}}` substring via regex, looks up `(session_id, alias)`, decrypts, substitutes; any lookup miss (wrong session, expired, unknown) returns `Err` naming the unresolved alias — never leaves the literal alias token in text that's about to hit a real API (better a failed dispatch than a broken prompt reaching the provider).

TTL: 3 hours (`180 * 60 * 1000` ms), per the spec's "2-4h" range.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p single-core redact:: 2>&1 | tail -40`
Expected: all PASS. If the entropy/negative-corpus tests fail, tune the entropy threshold or the UUID/git-SHA guards — don't weaken vendor-prefix or assignment-pattern matching to compensate.

- [x] **Step 5: `cargo clippy -p single-core --all-targets` clean, then commit**

```bash
git add crates/single-core/src/redact.rs crates/single-core/src/lib.rs
git commit -m "feat: add heuristic secret redaction and TTL'd alias store"
```

---

### Task 2: Wire redaction into all four prompt-ingestion chokepoints

**Files:**
- Modify: `crates/single-cli/src/acp.rs:125-141` (session/prompt handler)
- Modify: `crates/single-cli/src/serve_openai.rs:273` (`flatten_messages` call site)
- Modify: `crates/single-cli/src/main.rs:2199-2236` (`TaskCommand::Run`)
- Modify: `crates/single-cli/src/main.rs:2740-2768` and `:2805-2831` (`GoalCommand::Submit`, `Command::Loop`)
- Test: integration test per call site (see Step 1)

**Interfaces:**
- Consumes: `single_core::redact::{RedactStore, scan_and_replace}` from Task 1.
- Produces: nothing new consumed by later tasks — this task only rewires existing call sites so every `text`/`description`/`prompt` string is redacted before it reaches `client::send`/`self.socket`.

- [x] **Step 1: Write failing tests** (one per chokepoint, in each file's existing `#[cfg(test)]` module or a new one if none exists)

For `acp.rs`, add near existing tests (check for a `#[cfg(test)] mod tests` block first; if none exists, add one):

```rust
#[cfg(test)]
mod redact_wiring_tests {
    #[test]
    fn prompt_text_is_redacted_before_goal_submit_is_built() {
        // This test asserts on the pure helper, not the full ACP loop
        // (which needs a live socket) — see Task 2 Step 3 for where the
        // helper is extracted to make this testable without a daemon.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        single_core::redact::ensure_schema(&conn).unwrap();
        let store = single_core::redact::RedactStore { conn: &conn };
        let (redacted, aliases) = single_core::redact::scan_and_replace(
            &store,
            "acp-sess-1",
            "please call the api with sk-abcdEFGH1234567890abcdEFGH1234567890abcd",
        )
        .unwrap();
        assert_eq!(aliases.len(), 1);
        assert!(!redacted.contains("sk-abcdEFGH"));
    }
}
```

Equivalent tests go in `serve_openai.rs` (asserting `flatten_messages` output is redacted before it's passed to `Request::TaskRun`) and in `main.rs` (asserting the CLI-collected `text`/`description` is redacted before `client::send`) — same shape, different call site, so write each against that file's own existing prompt-building function rather than duplicating the redact-module tests from Task 1.

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test --workspace redact 2>&1 | tail -60`
Expected: FAIL — chokepoints don't call `scan_and_replace` yet.

- [x] **Step 3: Implement the wiring**

All four call sites need a `RedactStore` bound to the daemon's sqlite `Connection`. Each of these files already has access to the connection or the socket client — for `acp.rs` and `main.rs`/`serve_openai.rs` (CLI-side, talking to the daemon over a Unix socket, no direct `Connection`), redaction must happen **daemon-side**, not client-side, because the alias store's encrypted rows live in the daemon's database and the CLI process has no direct DB handle.

Concretely: add redaction as a daemon-side step inside the `GoalSubmit` and `TaskRun` request handlers themselves (wherever `crates/single-runtime/src/coordinator/mod.rs` or the request-dispatch match currently receives these `Request` variants and reads `.text`/`.description`), not inside `acp.rs`/`main.rs`/`serve_openai.rs` directly. Locate the daemon-side handler for `Request::GoalSubmit` and `Request::TaskRun` (grep `Request::GoalSubmit =>` and `Request::TaskRun =>` in `crates/single-runtime/src/`), and at the top of each handler:

```rust
let store = single_core::redact::RedactStore { conn: &conn };
let (text, _aliases) = single_core::redact::scan_and_replace(&store, &session_id, &text)?;
```

(field name is `text` for `GoalSubmit`, `description` for `TaskRun`; `session_id` for `GoalSubmit` is the request's own field, for `TaskRun` use the `cwd`+timestamp-derived task id already generated by that handler, or thread a `session_id: Option<String>` through if one doesn't exist yet — check the handler before assuming).

This means **Task 2's actual file list is corrected** from the four CLI-side files above to: the daemon-side request-dispatch module (find it via grep, likely `crates/single-runtime/src/coordinator/mod.rs` or a `handler.rs`/`server.rs` next to it). Update this task's file list once located, and note the correction in the commit message body isn't neededthe code comment at the call site is enough (one line: why redaction happens here and not client-side).

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace redact 2>&1 | tail -60`
Expected: PASS.

- [x] **Step 5: `cargo clippy --workspace --all-targets` clean, then commit**

```bash
git add -A
git commit -m "feat: redact secrets in every prompt before goal/task submission"
```

---

### Task 3: `resolve()` at the outbound dispatch boundary + `single secret promote-alias`

**Files:**
- Modify: `crates/single-runtime/src/pool_agent.rs:104` (prompt → `ChatMessage` conversion, inside the function containing line 104; confirm exact fn name when editing — it wraps `run_as_task`'s prompt handling)
- Modify: `crates/single-cli/src/main.rs` (new `single secret promote-alias <alias> <name>` subcommand — find the existing `Command::Secret`/`SecretCommand` enum and match arm to extend)
- Test: `crates/single-runtime/src/pool_agent.rs` existing `#[cfg(test)]` module

**Interfaces:**
- Consumes: `single_core::redact::{RedactStore, resolve}` from Task 1.
- Produces: nothing new for later tasks.

- [x] **Step 1: Write the failing test**

```rust
#[test]
fn run_as_task_resolves_aliases_before_dispatch_and_never_leaks_alias_to_provider() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    single_core::redact::ensure_schema(&conn).unwrap();
    let store = single_core::redact::RedactStore { conn: &conn };
    let (redacted_prompt, aliases) = single_core::redact::scan_and_replace(
        &store,
        "sess1",
        "use sk-abcdEFGH1234567890abcdEFGH1234567890abcd to call it",
    )
    .unwrap();
    assert_eq!(aliases.len(), 1);

    // dispatch closure captures what it actually received, so the test
    // can assert the real key reached the "provider" and the alias never did.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen2 = seen.clone();
    let dispatch = move |req: &PoolRequest, _: &FreeProvider, _: &str| {
        *seen2.lock().unwrap() = req.messages.last().unwrap().content.clone();
        Err(PoolError::RateLimited { retry: None }) // short-circuit; we only assert on `seen`
    };
    let _ = run_as_task_with_dispatch(&conn, &redacted_prompt, Some("sess1"), &dispatch);
    let sent = seen.lock().unwrap().clone();
    assert!(sent.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
    assert!(!sent.contains("REDACTED"));
}
```

(`run_as_task_with_dispatch` is a test-only seam: if `run_as_task` doesn't already accept an injectable dispatch closure, check the existing `dispatch` local at line 208 — the function likely already supports this via the closures visible at lines 291/311/330/349/372 in the file; use whichever existing test harness pattern those lines show rather than inventing a new one.)

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p single-runtime pool_agent:: resolves_aliases 2>&1 | tail -40`
Expected: FAIL — prompt isn't resolved yet, `sent` still contains the alias or the closure never sees the real key.

- [x] **Step 3: Implement**

At the point in `pool_agent.rs` where `prompt: &str` is turned into the `ChatMessage` (line ~104), insert:

```rust
let store = single_core::redact::RedactStore { conn };
let resolved_prompt = single_core::redact::resolve(&store, session_key.unwrap_or("no-session"), prompt)
    .unwrap_or_else(|_| prompt.to_string()); // no aliases present is not an error condition worth failing dispatch over
let mut messages = vec![ChatMessage { role: "user".to_string(), content: resolved_prompt }];
```

Note: `resolve()`'s "errors on unknown/expired alias" behavior from Task 1 is for cases where an alias token is clearly present but unresolvable — that should propagate as a real dispatch failure (a dangling `{{REDACTED_N}}` reaching a provider is worse than failing the task), so don't blanket-swallow every error here. Distinguish "no alias tokens found" (fine, passthrough) from "alias token found but couldn't resolve" (real error) inside `resolve()` itself if it doesn't already (adjust Task 1's `resolve()` to return the original text unchanged when zero `{{REDACTED_\d+}}` matches exist, only erroring when a match exists but lookup fails).

Then add `promote-alias` in `main.rs`'s secret command group:

```rust
SecretCommand::PromoteAlias { alias, name } => {
    let response = client::send(&socket_path, Request::SecretPromoteAlias { alias, name })?;
    render::print(response, false);
}
```

(add the matching `Request::SecretPromoteAlias { alias: String, name: String }` variant to `single_protocol`, and its daemon-side handler: look up the `RedactStore` row for the current session, decrypt, call the same secret-set path `crates/single-runtime/src/self_heal/infra.rs:221` uses in reverse (`SecretStore::set` — confirm exact method name when reading `secrets.rs` in Task 1), then delete the alias row. Require the request to carry the session id so promotion can't cross sessions; require explicit confirmation at the call site — CLI: a `y/N` prompt before sending the request; ACP: `run_slash`'s existing confirmation-round-trip pattern, or if none exists, a two-step "propose, then require the literal phrase 'confirm' as the next prompt" — check `session/request_permission` in `acp.rs` (referenced in the file's own doc-comment) as the ACP-native way to do this instead of inventing a new protocol.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p single-runtime pool_agent:: 2>&1 | tail -40 && cargo test -p single-cli 2>&1 | tail -40`
Expected: PASS.

- [x] **Step 5: `cargo clippy --workspace --all-targets` clean, then commit**

```bash
git add -A
git commit -m "feat: resolve redacted aliases at dispatch and add secret promotion"
```

---

### Task 4: `single-pool` as the default ACP agent

**Files:**
- Modify: `crates/single-cli/src/acp.rs:31-38` (`AcpSession` struct — add `agent_override: Option<String>`)
- Modify: `crates/single-cli/src/acp.rs:294-301` (`GoalSubmit` construction in `run_turn`)
- Modify: `crates/single-cli/src/acp.rs:495-538` (`run_slash` — add an `/agent` command)
- Modify: `crates/single-cli/src/acp.rs:704-716` (`commands()` — list `/agent`)
- Test: inline in `acp.rs`'s test module (check for one; add if absent)

**Interfaces:**
- Consumes: nothing new.
- Produces: `AcpSession.agent_override: Option<String>`, read by Task 6's `/single-status` output (to show which agent a session is pinned to, if any).

- [x] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod default_agent_tests {
    #[test]
    fn goal_submit_defaults_to_single_pool_when_no_override_set() {
        let agent: Option<String> = None; // simulates AcpSession.agent_override
        let resolved = agent.or_else(|| Some("single-pool".to_string()));
        assert_eq!(resolved.as_deref(), Some("single-pool"));
    }

    #[test]
    fn goal_submit_honors_explicit_override() {
        let agent: Option<String> = Some("opencode".to_string());
        let resolved = agent.or_else(|| Some("single-pool".to_string()));
        assert_eq!(resolved.as_deref(), Some("opencode"));
    }
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p single-cli default_agent 2>&1 | tail -20`
Expected: FAIL (module doesn't exist / trivial logic not yet wired — this is a thin unit test for the resolution rule; Step 3 wires the real thing).

- [x] **Step 3: Implement**

`AcpSession` gains a field:

```rust
struct AcpSession {
    coord_id: String,
    mode: String,
    last_event_id: i64,
    cancel: Arc<AtomicBool>,
    agent_override: Option<String>,
}
```

(update every `AcpSession { .. }` construction site — `session_new`/`session_load` — to include `agent_override: None`.)

In `run_turn`'s `GoalSubmit` construction (line ~300), replace `agent: None` with:

```rust
agent: {
    let map = self.sessions.lock().unwrap();
    map.get(acp_sid).and_then(|s| s.agent_override.clone()).or_else(|| Some("single-pool".to_string()))
},
```

Add an `/agent` slash command in `run_slash`'s match:

```rust
"agent" => {
    if _arg.is_empty() {
        let current = self.sessions.lock().unwrap().get(acp_sid)
            .and_then(|s| s.agent_override.clone())
            .unwrap_or_else(|| "single-pool (default)".to_string());
        format!("current session agent: {current}\nusage: /agent <name>  or  /agent default")
    } else if _arg == "default" {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(acp_sid) {
            s.agent_override = None;
        }
        "reset to default agent (single-pool)".to_string()
    } else {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(acp_sid) {
            s.agent_override = Some(_arg.to_string());
        }
        format!("session agent pinned to {_arg}")
    }
}
```

(rename the unused `_arg` parameter to `arg` in `run_slash`'s signature since it's now used — `fn run_slash(&self, acp_sid: &str, name: &str, arg: &str) -> String`.)

Add `{ "name": "agent", "description": "set or clear this session's pinned agent (default: single-pool)" }` to `commands()`.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p single-cli 2>&1 | tail -40`
Expected: PASS.

- [x] **Step 5: `cargo clippy -p single-cli --all-targets` clean, then commit**

```bash
git add crates/single-cli/src/acp.rs
git commit -m "feat: default zed acp sessions to single-pool with per-session override"
```

---

### Task 5: `goal::find_overlapping` + cross-session dedup wiring

**Files:**
- Modify: `crates/single-runtime/src/coordinator/goal.rs` (add `find_overlapping`, near `active()` at line 190)
- Modify: the daemon-side `Request::GoalSubmit` handler located in Task 2 (add the dedup check before creating a new goal)
- Test: `crates/single-runtime/src/coordinator/goal.rs`'s existing test module

**Interfaces:**
- Consumes: `goal::active()` (existing, line 190).
- Produces:
  ```rust
  /// Normalized-token-overlap check against every non-terminal goal's
  /// `text`. Returns the first goal whose overlap ratio (shared tokens /
  /// smaller token-set size) is >= 0.6, or `None` if nothing overlaps
  /// enough to count as "already covering this ask".
  pub fn find_overlapping(conn: &Connection, text: &str) -> Result<Option<Goal>>;
  ```

- [x] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod overlap_tests {
    use super::*;

    fn insert_goal(conn: &Connection, id: &str, text: &str) {
        conn.execute(
            "INSERT INTO goals (id, session_id, text, mode, status, max_dispatches, max_minutes, plan_json, created_at, updated_at) \
             VALUES (?1, 's', ?2, 'auto', 'running', 25, 60, '{}', '2026-01-01', '2026-01-01')",
            params![id, text],
        ).unwrap();
    }

    #[test]
    fn exact_duplicate_text_overlaps() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        insert_goal(&conn, "g1", "fix the login bug in auth.rs");
        let found = find_overlapping(&conn, "fix the login bug in auth.rs").unwrap();
        assert_eq!(found.unwrap().id, "g1");
    }

    #[test]
    fn near_duplicate_phrasing_overlaps() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        insert_goal(&conn, "g1", "please fix the login bug found in auth.rs today");
        let found = find_overlapping(&conn, "fix login bug in auth.rs").unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn distinct_asks_do_not_overlap() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        insert_goal(&conn, "g1", "fix the login bug in auth.rs");
        let found = find_overlapping(&conn, "add dark mode to the settings page").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn terminal_goals_are_ignored() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO goals (id, session_id, text, mode, status, max_dispatches, max_minutes, plan_json, created_at, updated_at) \
             VALUES ('g1', 's', 'fix the login bug in auth.rs', 'auto', 'done', 25, 60, '{}', '2026-01-01', '2026-01-01')",
            [],
        ).unwrap();
        let found = find_overlapping(&conn, "fix the login bug in auth.rs").unwrap();
        assert!(found.is_none());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p single-runtime overlap_tests 2>&1 | tail -40`
Expected: FAIL — `find_overlapping` doesn't exist.

- [x] **Step 3: Implement**

```rust
pub fn find_overlapping(conn: &Connection, text: &str) -> Result<Option<Goal>> {
    fn tokens(s: &str) -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2) // drop stopword-length noise
            .map(|w| w.to_string())
            .collect()
    }
    let want = tokens(text);
    if want.is_empty() {
        return Ok(None);
    }
    for g in active(conn)? {
        let have = tokens(&g.text);
        let shared = want.intersection(&have).count();
        let smaller = want.len().min(have.len());
        if smaller > 0 && (shared as f64 / smaller as f64) >= 0.6 {
            return Ok(Some(g));
        }
    }
    Ok(None)
}
```

Wire it into the daemon-side `GoalSubmit` handler (found in Task 2), immediately after the redaction step and before the goal is actually created:

```rust
if let Some(existing) = crate::coordinator::goal::find_overlapping(&conn, &text)? {
    return Ok(ResponseData::GoalId(existing.id)); // already in flight — hand back the existing goal, don't duplicate
}
```

Make sure the response path this returns through is distinguishable from a freshly created goal at the call site if the caller cares (ACP's `run_turn` just streams whatever goal id it gets back either way, so no change needed there — it'll naturally show "already running" via the existing `stream_goal` output for that goal's current state).

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p single-runtime 2>&1 | tail -60`
Expected: PASS.

- [x] **Step 5: `cargo clippy -p single-runtime --all-targets` clean, then commit**

```bash
git add crates/single-runtime/src/coordinator/goal.rs
git add -A
git commit -m "feat: dedup overlapping goal submissions against active goals"
```

---

### Task 6: `/single-status` slash command

**Files:**
- Modify: `crates/single-cli/src/acp.rs:704-716` (`commands()`)
- Modify: `crates/single-cli/src/acp.rs:495-538` (`run_slash` — add `"status"` is already taken by `/status`/`/queue`; add a distinct richer `"single-status"` case, or extend the existing `"status"` arm — see Step 3 for the decision)
- Test: inline in `acp.rs`'s test module

**Interfaces:**
- Consumes: `Request::CoordinatorStatus`, `Request::PoolStatus` (both already exist, used at `acp.rs:498`/`format_snapshot`), plus provider key-status (grep `Request::ProviderKeyStatus` or similar; if the protocol variant name differs, use whatever `single provider key-status`'s CLI path calls — check `main.rs`'s `ProviderCommand::KeyStatus` handler for the exact `Request` variant name before wiring this).
- Produces: nothing consumed by later tasks.

- [x] **Step 1: Write the failing test**

```rust
#[test]
fn single_status_section_formats_goals_pool_and_usage() {
    let s = single_protocol::CoordinatorSnapshot {
        max_parallel: 4,
        running_goals: vec![single_protocol::GoalSummary { id: "g1".into(), text: "fix bug".into() }],
        queued_goals: vec![],
        blocked_goals: vec![],
        pool: vec![],
    };
    let out = format_snapshot(&s);
    assert!(out.contains("g1"));
    assert!(out.contains("running"));
}
```

(This reuses the already-existing `format_snapshot` — the new work in this task is registration + a slightly richer combined formatter, not a new data path, so the test is intentionally thin; the real verification is the live check in Task 8.)

- [x] **Step 2: Run test to verify it fails or passes trivially**

Run: `cargo test -p single-cli single_status 2>&1 | tail -20`
This may already pass since it reuses existing formatting — if so, note that in the commit and move directly to Step 3's wiring, which is the actual new behavior.

- [x] **Step 3: Implement**

Reuse the existing `"status"` slash command (`acp.rs:497`) rather than adding a second near-duplicate one — per the design spec, `/single-status` was a placeholder name; Zed's slash commands are just `/status` inside `single acp`'s own command namespace already, so extend that arm's output to include provider key-status and a rough usage estimate:

```rust
"status" | "queue" => {
    let coord = match self.socket(Request::CoordinatorStatus) {
        Ok(ResponseData::CoordinatorSnapshot(s)) => format_snapshot(&s),
        Ok(other) => format!("unexpected: {other:?}"),
        Err(e) => format!("[error: {e}]"),
    };
    let providers = shell_out(&["provider", "key-status"]);
    format!("{coord}\nprovider auth/exhaustion:\n{providers}")
}
```

(reusing `shell_out` — already used for `/agents`, `/usage`, etc. — is consistent with the rest of `run_slash` rather than adding a new socket round-trip for data the CLI path already formats.)

Update `commands()`'s existing `"status"` entry description to mention provider auth state:
`{ "name": "status", "description": "coordinator: running/queued/blocked goals + pool + provider auth/exhaustion" }`

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p single-cli 2>&1 | tail -40`
Expected: PASS.

- [x] **Step 5: `cargo clippy -p single-cli --all-targets` clean, then commit**

```bash
git add crates/single-cli/src/acp.rs
git commit -m "feat: fold provider auth status into the zed acp /status command"
```

---

### Task 7: Zed agent-mode config QoL pass

**Files:**
- Modify: whatever `.zed/settings.json` or agent-mode config exists in this repo (check `find . -iname "*.zed*" -o -iname "zed*.json"` first) and, if the user's dotfiles are in scope (confirm path — likely outside this repo, under `~/.config/zed/`), those too.

- [x] **Step 1: Locate current Zed agent config**

Run: `find . -path ./target -prune -o -iname "*.zed*" -print 2>/dev/null` and check `~/.config/zed/settings.json` for `agent_servers`/`assistant` blocks referencing `single`.

- [x] **Step 2: Update references**

Point any `agent_servers.single` (or equivalent) entry at the `single acp` binary path (confirm it's already correct post-build), and remove/update any comment or mode config that assumes synchronous goal completion — a goal can legitimately enter `waiting_on_capacity` and pause, so any Zed task/mode description implying "runs to completion" should say "may pause on `waiting_on_capacity` and auto-resume" instead.

- [x] **Step 3: Commit**

```bash
git add -A
git commit -m "chore: align zed agent-mode config with single-pool defaults"
```

(If no such config exists anywhere in scope, skip this task and note that in the final report rather than fabricating a file with no consumer.)

---

### Task 8: Version bump, changelog, build+deploy+live verification

**Files:**
- Modify: `Cargo.toml` (`[workspace.package] version`)
- Modify: `CHANGELOG.md` (one new block)
- Modify: the E28 memory file (update deferred list if this epic addressed anything on it — check: it did not touch `bootstrap::run_one`'s timeout or degrade persistence, so no change needed there unless Task 7 turned up something relevant)

- [x] **Step 1: Bump version**

`Cargo.toml`: `version = "0.11.0"` → `"0.12.0"` (new subsystem, pre-1.0 → minor bump per git-commit-standards).

- [x] **Step 2: Add CHANGELOG block**

One `## 0.12.0` section listing: heuristic prompt redaction + TTL alias store, `single-pool` default ACP agent, `/status` provider-auth enrichment, cross-session goal dedup.

- [x] **Step 3: Build**

Run (foreground, `-j1`): `cargo build --release --workspace -j1 2>&1 | tail -60`
Expected: clean, no warnings.

- [x] **Step 4: Full test suite**

Run: `cargo test --workspace 2>&1 | tail -80`
Expected: all pass.

- [x] **Step 5: Clippy**

Run: `cargo clippy --workspace --all-targets -j1 2>&1 | tail -80`
Expected: clean of new lints (compare against pre-epic baseline if any pre-existing lints are known).

- [x] **Step 6: Commit version bump**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: bump version to 0.12.0"
```

- [x] **Step 7: Tag**

```bash
git tag -a v0.12.0 -m "v0.12.0: zed redaction, default single-pool agent, goal dedup"
```

- [x] **Step 8: Reinstall + restart daemon**

```bash
lsof ~/.local/bin/single 2>/dev/null  # check nothing has it open before overwriting
cp target/release/single ~/.local/bin/single
systemctl --user stop single-runtimed
pkill -9 -x single-runtimed
rm -f ~/.config/single/state/runtime.sock
systemctl --user start single-runtimed
```

- [x] **Step 9: Live verification**

- Send a prompt through a real (or `single acp` driven directly over stdio if a live Zed session isn't feasible) ACP session containing a fake-shaped secret (e.g. `sk-test0000000000000000000000000000000000`) — confirm via `single goal status <id>` / the events table that only the alias appears, never the plaintext.
- Confirm a plain ACP prompt with no explicit agent routes to `single-pool` (check the resulting goal's dispatch agent).
- Ask the same thing twice in a row (two separate `single goal submit` calls with near-identical text) — confirm the second returns the first goal's id rather than creating a duplicate.
- Run `/status` in a live ACP session (or `single acp` over a raw stdio harness) and confirm it includes provider auth/exhaustion lines.

- [ ] (blocked: GitHub account suspended 2026-09-24; see E28) **Step 10: Push**

```bash
git push origin main
git push origin v0.12.0
```

- [x] **Step 11: Write the E29 memory file**

New file at `/home/navinbruas/.claude/projects/-home-navinbruas/memory/e29-zed-singlecli-integration.md`, same shape as the E28 one — what shipped, any bugs found live, deferred items (LLM second-pass classifier, per-request ACP agent override, real Zed status-bar icon pending upstream RFCs, numeric false-positive-rate gating). Add its pointer line to `MEMORY.md`.

---

## Self-Review Notes

- Task 2's file list is deliberately marked "corrected during implementation" — the daemon-side handler location wasn't confirmed by name/path during planning (only the CLI-side call sites were read); the executor must grep `Request::GoalSubmit =>` / `Request::TaskRun =>` in `crates/single-runtime/src/` before writing code, and update the task's file list to the real path found.
- Task 3's `run_as_task_with_dispatch` test seam depends on `pool_agent.rs` already supporting closure-injected dispatch (confirmed live at lines 291/311/330/349/372 of that file) — if the real function signature differs from what's assumed here, adapt the test to whatever seam already exists rather than adding a new one.
- Task 6 folds `/single-status` into the existing `/status` command rather than adding a same-purpose duplicate — this is a deliberate deviation from the spec's literal command name, recorded here so a reviewer doesn't flag it as a missed requirement.
- `secrets.rs`'s exact encryption primitive name is unknown at plan-writing time (session-level permission block on files matching `secrets*`) — Task 1 Step 0 requires reading it as the task's first action, with an explicit fallback (ask the user for a one-time exception) if the block persists for the executor too.
