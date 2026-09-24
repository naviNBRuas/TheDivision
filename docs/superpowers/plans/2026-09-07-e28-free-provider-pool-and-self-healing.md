# E28 — Free-provider pool, adaptive routing, and self-healing autonomy Implementation Plan

**Status (2026-09-24): shipped.** Pool engine (`pool/`), `single-pool`, self-heal (`self_heal/`). Since then: the pool discovers real model ids per provider (`pool/models.rs`) and runs as a multi-step coding agent (`pool_coder.rs`).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** absorb the ~40-provider free-LLM landscape as first-class `ProviderSpec` presets, add a native pooled agent (`single-pool`) with an adaptive routing/quota engine, teach the E27 coordinator to hold a goal open across capacity exhaustion instead of failing it ("auto-continue"), and make the daemon/coordinator/agent layer self-heal and self-resume across restarts.

**Architecture:** everything sits on top of the E27 coordinator (`crates/single-runtime/src/coordinator/`) — new goal states, new tick behaviour, additive tables, no rewrite. New module tree `crates/single-runtime/src/pool/` (`client`, `ledger`, `cooldown`, `backoff`, `pools`, `bandit`, `degrade`, `handoff`) is the Rust port of freellmapi's router/quota engine (studied, not vendored — clean-room). `single-core/src/free_pool.rs` holds the vendored ~40-provider catalog as pure data. `single-runtime/src/pool_agent.rs` is the `single-pool` agent adapter. `single-runtime/src/self_heal.rs` is the periodic self-repair pass. `single task run` / `orchestrate-*` / `single goal|session|acp|loop|serve` (E27) are unchanged; `single-pool` is opt-in per agent/route.

**Tech stack:** unchanged from E27 — Rust 2021, `rusqlite` (bundled), `reqwest` (blocking, already a workspace dep — `task::run` is sync), `tokio` (daemon only), `serde`/`serde_json`, `toml` 0.8, `clap` 4 derive, `chrono`. No new dependency (spec §2 non-goals: no Node, no freellmapi runtime).

**Spec:** `docs/superpowers/specs/2026-09-07-e28-free-provider-pool-and-self-healing-design.md` (authoritative; this plan resolves its §17 open questions per the instructions below and follows its §16 build order 1:1 as Phases 1–10).

**Builds on:** E27 (v0.10.0 — coordinator subsystem, `single acp`, `single serve --openai`, per-task token accounting, `single loop`). Plan: `docs/superpowers/plans/2026-09-06-e27-coordinator.md`.

## §17 open questions — resolved for this plan

- **`pool.toml` vs env-only:** split, per the spec's own lean. `~/.config/single/pool.toml` holds the operator-facing knobs (`retry_budget_ms`, `cooldown_ceiling`, degraded-mode thresholds) with `Default` + lazy-write-on-first-use, same pattern as `coordinator.toml` (E27 `routing::CoordinatorConfig::load`). Every one of those knobs is *also* overridable by an env var (`SINGLE_POOL_RETRY_BUDGET_MS`, `SINGLE_POOL_COOLDOWN_CEILING_MS`, `SINGLE_POOL_DEGRADED_*` — spec §6.5) for one-off/CI overrides; env wins over file when both are set. One-shot toggles that are never operator-tuned config (`SINGLE_POOL_COOLDOWN_PROBE_DISABLED`) stay env-only, not mirrored into the file.
- **Bandit priors:** `Beta(1, 1)` uniform prior for every `(platform, model, key)` with no `pool_outcomes` history yet — no community feed exists (D3), so there's nothing to seed from. The seam is documented with a doc-comment on `bandit::posterior()` pointing at spec §6.4 "community_* priors" and this decision, so a future signed-feed loader (spec §2 non-goals) has an obvious hook.
- **Media models:** text-only v1. Part A registers `siliconflow` (FLUX/CosyVoice2) and any other media-only entries in the catalog with `Limits`/`Wire` intact, but `client.rs`/`pool_agent.rs` only implement chat-completions-shaped wires this iteration; a media-capable provider with no text model is registered-but-unroutable (bandit never picks it — no text model to score) and `key-status`/`list-free` say so. Non-text dispatch is a documented follow-up, not built here.
- **`sail` / `modelscope` / `qianfan` / `volcengine` / `xfyun`:** registered in the vendored table (Part A) but **default-disabled** in `sync-pool`'s generated `free-pool.toml` (`enabled = false`), each with a `key-status` reason string:
  - `sail` → `"needs a payment method on file (flex-only credit model)"`
  - `modelscope` → `"needs a China account + real-name verification"`
  - `qianfan` → `"needs Chinese real-name auth (Baidu Cloud)"`
  - `volcengine` → `"needs Chinese real-name auth (Volcengine)"`
  - `xfyun` → `"needs a China console account (APIPassword)"`
  They never enter the default pool (`sync-pool` writes them `enabled = false`; `bandit::pick` and admission both skip disabled providers), and `single doctor`/pool health surface the reason instead of looping cooldowns (spec §5.2 "region-walled" note). An operator can flip `enabled = true` by hand once they have the account.

## Global constraints

- **Version:** bump workspace `version` `0.10.0` → `0.11.0` in the root `Cargo.toml` `[workspace.package]` only (all crates are `version.workspace = true`). One `## [0.11.0]` block in `CHANGELOG.md`, added at the end (Phase 10), not per-phase. New tables additive only — no migration for existing tables.
- **Build discipline (machine constraint, not in git):** `cargo build` / `cargo test` in the FOREGROUND, `-j2`, one or two crates at a time (`-p single-runtime -p single-core`, add `-p single-protocol`/`-p single-cli` only when those crates changed). Never background a build.
- **Acceptance gate per phase:** `cargo build -p <touched crates> -j2` clean; relevant `cargo test -p <crate> <module>::` passes. Full-workspace `cargo build --workspace` + `cargo test --workspace` only at Phase 10 (mirrors E27's Task 14).
- **Commits:** sole-author, atomic per logical change, `type: description` subject (`feat` for new subsystems/subcommands, `fix` for bugs, `test` where a fix is just a regression test). NO `Co-Authored-By`, NO trailers. `git-commit-standards` skill applies.
- **Doc-comment style:** terse lowercase `///`/`//`, explain the WHY, mark confirmed-vs-assumed — match `coordinator/scheduler.rs`, `providers.rs`, `provider_keys.rs`.
- **Compatibility (spec §15):** `single task run` / `orchestrate-*` / `single goal|session|acp|loop|serve` (E27) keep working unchanged. `single-pool` is opt-in per agent/route. No provider key is ever required — an unkeyed pool provider is silently skipped by admission, never an error.
- **DB access pattern:** same as E27 — `crate::state::open(&ctx.dirs.db_path())?`, WAL + 5s busy-timeout. Each handler opens its own connection; coordinator/pool modules take `&Connection`, never open their own except on a spawned thread (mirror `task::run_background`, the E27 tick-timer thread).
- **No LLM process, no new HTTP surface:** `single-pool` calls provider HTTP APIs directly (not through a model CLI) — this is the one new kind of "brain-less" dispatch; it does not add an always-on model or a new inbound HTTP surface (`single serve --openai`, E27, already covers that direction).
- **Real network stays `#[ignore]`:** every pool test that would hit a live provider is `#[ignore]`, run manually/live-verification only. Everything else — ledger, cooldown, backoff, pools, bandit, degrade, handoff, self_heal, resume — is deterministic against in-memory/tempdir SQLite or fixtures.

---

## File structure

### New — provider catalog + key storage (`crates/single-core/src/`)

| File | Responsibility | Approx |
|---|---|---|
| `free_pool.rs` | `FreeProvider`, `Wire`, `Auth`, `Limits`, `PoolShape`, `Quirks` types (spec §5.1) + the vendored ~40-entry static table (spec §5.2) + parsers/validators (unique id, valid `base_url`) | ~650 (mostly data) |
| `pool_keys.rs` | thin wrapper over `provider_keys.rs`/`secrets.rs` conventions for pool provider keys — `secret_name(platform, key_id) -> "pool-key:{platform}:{key_id}"`, `pool_provider_keys` table CRUD (`add`, `list`, `mark_valid`, `disable`) | ~180 |

### New — pool engine (`crates/single-runtime/src/pool/`)

| File | Responsibility | Approx |
|---|---|---|
| `mod.rs` | public API re-exports, `ensure_pool_schema` (all 5 new tables), `PoolOutcome` enum | ~120 |
| `ledger.rs` | `pool_usage` + `pool_leases` tables; 4-D sliding-window admission check; lease acquire/release; degraded-DB memory fallback | ~380 |
| `cooldown.rs` | `pool_cooldowns` table; ladder escalation; provenance; probe job | ~350 |
| `backoff.rs` | `Retry-After` / body-shape / prose parser, clamp to 24h, number-only | ~150 |
| `pools.rs` | `infer_pool_key`, aggregate shared-pool gating, `is_shared_pool` | ~140 |
| `bandit.rs` | `pool_outcomes` table; Beta posterior + Thompson sample; strategy weights; `pick()` | ~320 |
| `degrade.rs` | healthy-ratio state machine, hysteresis | ~150 |
| `handoff.rs` | in-memory session store, context-handoff message builder | ~140 |
| `client.rs` | HTTP dispatch: OpenAI-compat wire + native wires (`PoolWire` trait), retry budget + hedge, tool-call rescue | ~520 |

### New — pool agent + self-heal (`crates/single-runtime/src/`)

| File | Responsibility | Approx |
|---|---|---|
| `pool_agent.rs` | `single-pool` adapter implementing the `task::execute` run contract via `pool::` | ~260 |
| `self_heal.rs` | the self-heal pass (infra/coordinator/agent categories), `self_heal_events` table, `self_heal.toml` | ~420 |

### Modified

| File | Change |
|---|---|
| `crates/single-core/src/lib.rs` | `pub mod free_pool; pub mod pool_keys;` |
| `crates/single-runtime/src/lib.rs` | `pub mod pool; pub mod pool_agent; pub mod self_heal;` |
| `crates/single-protocol/src/lib.rs` | `ProviderSpec`/`AgentDefinition` additions if needed for `single-pool` registration; new CLI-facing view structs for `provider list-free`/`key-status`/`pool status` |
| `crates/single-runtime/src/coordinator/graph.rs` | `NodeStatus`/`GoalStatus` get `WaitingOnCapacity`, `Paused` variants (parse-tolerant per E27 precedent) |
| `crates/single-runtime/src/coordinator/goal.rs` | `goals` gains `capacity_reason`, `earliest_retry_at_ms`, `capacity_waits` (via `add_column_if_missing`); `graph_nodes` gains `earliest_retry_at_ms` |
| `crates/single-runtime/src/coordinator/scheduler.rs` | `tick_pure` grows the exhaustion branch (`PoolOutcome::Exhausted` → `WaitingOnCapacity`), skip/re-admit on `earliest_retry_at_ms`, resume-budget check; `on_task_finished`/dispatch wired to `pool_agent` when `agent == "single-pool"` |
| `crates/single-runtime/src/coordinator/routing.rs` | `select_agent` can return `"single-pool"`; `coordinator.toml` gains `prefer_pool: bool` |
| `crates/single-runtime/src/coordinator/mod.rs` | `resume_interrupted()` (Part F), `single goal resume` support |
| `crates/single-runtime/src/server.rs` | startup: `pool::ensure_pool_schema`, `self_heal::run_pass` (once), `coordinator::resume_interrupted()`, after the existing `reconcile` block |
| `crates/single-runtime/src/bin/single-runtimed.rs` | periodic `self_heal::run_pass` on its own interval (default 300s), alongside the existing tick timer; probe job (§6.2) started here too |
| `crates/single-runtime/src/handlers.rs` | route `single acp`'s `capacity_wait` → `agent_thought_chunk` translation stays in Phase 2's acp bridge, not here — handlers only need the coordinator status/events plumbing already generic from E27 |
| `crates/single-cli/src/main.rs` | new subcommands (§ per phase below) |
| `crates/single-cli/src/render.rs` | pretty-printers for the new response types |
| root `Cargo.toml`, `CHANGELOG.md` | version bump (Phase 10) |

### New config files (created lazily on first use, seeded defaults)

- `~/.config/single/free-pool.toml` — generated from the vendored table by `sync-pool`; `sail`/`modelscope`/`qianfan`/`volcengine`/`xfyun` written `enabled = false` with their reason strings (see §17 resolution above).
- `~/.config/single/pool.toml` — engine tunables, env-overridable (see §17 resolution above).
- `~/.config/single/self_heal.toml` — `[categories] infra/coordinator/agent = bool` (all `true` default), intervals, grace windows.
- `~/.config/single/routing.toml` (E27, extended) — `[pool]` block: `strategy`, `key_selection`, `custom` weights.
- `~/.config/single/coordinator.toml` (E27, extended) — `prefer_pool`, `max_capacity_waits_per_goal` (20), `max_capacity_wait_minutes` (720).

---

## Phase 1 — Part A: provider catalog (spec §5, build-order item 1)

**Goal:** pure data + CLI, no risk, lands first.

### Task 1: `free_pool.rs` types + vendored table

**Files:** create `crates/single-core/src/free_pool.rs`; modify `crates/single-core/src/lib.rs`.

**Interfaces:**
- Produces: `FreeProvider { id, display, base_url, wire: Wire, auth: Auth, signup_url, limits: Limits, pool: Option<PoolShape>, timeout: Duration, quirks: Quirks, free_note }` and the ~40-entry `pub const FREE_PROVIDERS: &[FreeProvider]` (spec §5.1–5.2 table verbatim — every row, including the 5 region-walled/real-name ones with `quirks.region_wall = true, quirks.real_name_auth = true`).
- `Wire` enum: `OpenAiCompat, Gemini, Cohere, Cloudflare, Zhipu, AiHorde, Sail, ModelScope, Pollinations, ElectronHub, Experiential` (one variant per native wire named in spec §6.6, plus `OpenAiCompat` for the 33 compat providers).
- `Auth` enum: `Bearer, XApiKey, Compound(&'static str), Keyless(&'static str), Header(&'static str)`.
- `Limits { rpm: Option<u32>, rpd: Option<u32>, tpm: Option<u32>, tpd: Option<u64> }`.
- `PoolShape` enum: `Free, Project, Account, CreditPool { rpm: u32 }, DailyTokens { n: u64 }`.
- `Quirks { force_single_tool_call: bool, no_tools: bool, no_stream: bool, browser_ua: bool, min_max_tokens: Option<u32>, validate_url: Option<&'static str>, validate_cache: Option<Duration>, region_wall: bool, real_name_auth: bool }` — all `Default` false/None.
- `fn by_id(id: &str) -> Option<&'static FreeProvider>`.
- `fn default_disabled_reason(id: &str) -> Option<&'static str>` — the §17 reason strings for `sail`/`modelscope`/`qianfan`/`volcengine`/`xfyun`, `None` for everything else.

- [ ] **Step 1: Write failing tests** in `free_pool.rs`:
  - `every_provider_id_is_unique`
  - `every_base_url_is_a_valid_url_or_empty` (empty allowed for native wires per spec)
  - `region_walled_providers_have_a_default_disabled_reason` — exactly the 5 named ids, `default_disabled_reason` returns `Some`, everything else `None`
  - `provider_count_matches_spec_table` (assert `FREE_PROVIDERS.len() == 40` or the exact count once you've transcribed the table — pin the number so a future accidental drop is caught)
- [ ] **Step 2:** `cargo test -p single-core free_pool -- --nocapture`, expect FAIL (module doesn't exist).
- [ ] **Step 3:** implement. Transcribe spec §5.2's table row-for-row — do not paraphrase limits/quirks; every `free_note` string included for `list-free` display.
- [ ] **Step 4:** PASS; `cargo build -p single-core -j2`.
- [ ] **Step 5: Commit:** `feat: vendored free-provider catalog (free_pool.rs)`

---

### Task 2: `pool_keys.rs` — key storage

**Files:** create `crates/single-core/src/pool_keys.rs`; modify `lib.rs`.

**Interfaces:**
- Consumes: `crate::secrets::SecretTool` (existing), `rusqlite::Connection`.
- Produces:
  - `fn secret_name(platform: &str, key_id: &str) -> String` → `"pool-key:{platform}:{key_id}"` (distinct namespace from `provider:{name}` and `provider-key:{provider}:{label}`, same pattern as `provider_keys.rs`'s doc comment explains).
  - `fn ensure_schema(conn: &Connection) -> Result<()>` — `pool_provider_keys(platform TEXT NOT NULL, key_id TEXT NOT NULL, secret_ref TEXT NOT NULL, added_at TEXT NOT NULL, last_validated_at TEXT, valid INTEGER NOT NULL DEFAULT 0, disabled INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (platform, key_id))` (spec §12).
  - `fn add(conn, platform: &str, key_id: &str) -> Result<()>` — writes the row; caller stores the actual secret via `secrets::SecretTool` under `secret_name(...)` separately (mirrors `provider_keys.rs::add`'s separation of registry-row vs keychain value).
  - `fn list(conn, platform: Option<&str>) -> Result<Vec<PoolProviderKey>>`
  - `fn mark_validated(conn, platform, key_id, valid: bool) -> Result<()>` — bumps `last_validated_at`, sets `valid`.
  - `fn disable(conn, platform, key_id) -> Result<()>` / `fn is_disabled(conn, platform, key_id) -> Result<bool>`.

- [ ] **Step 1:** failing test `add_list_mark_validated_roundtrip` (tempdir/in-mem db).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** PASS; `cargo build -p single-core -j2`.
- [ ] **Step 5: Commit:** `feat: pool provider key storage`

---

### Task 3: `single provider list-free` / `add-free` / `sync-pool` / `key-status` CLI

**Files:** modify `crates/single-cli/src/main.rs`, `render.rs`; may need a small `single-runtime` or `single-core` helper for `sync-pool`'s write into `providers.toml`.

**Interfaces:**
- `single provider list-free` — table: id, display, limits, signup_url, and (for the 5 region-walled) the disabled reason. Pure read of `FREE_PROVIDERS`.
- `single provider add-free <id> [--key <k>]` — resolves `free_pool::by_id`, prompts for the key if not given (or reads `--key`), stores via `secrets::SecretTool` under `pool_keys::secret_name`, writes `pool_keys::add`, then runs the provider's `quirks.validate_url` probe if present (best-effort HTTP GET, non-blocking failure — logs, doesn't error the command) and calls `pool_keys::mark_validated`.
- `single provider sync-pool` — reconciles `FREE_PROVIDERS` into `providers.toml` as `single-<id>` presets (E27 `single-<name>` aliasing precedent), `enabled = false` until a key exists **and** (for the 5 region-walled ids) `enabled = false` regardless, always, with the reason written into `free-pool.toml`'s per-provider comment/field. Idempotent — re-running doesn't clobber operator overrides already in `free-pool.toml` (same upsert discipline as `providers::add`).
- `single provider key-status [--platform <id>]` — per-provider: keyed?, `last_validated_at`, current cooldown (from `pool::cooldown`, wired once Phase 2 lands — Phase 1 shows "n/a" until then), today's RPD/TPD headroom (same — "n/a" until `ledger.rs` exists; wire fully in Phase 2's follow-up commit, noted below).

- [ ] **Step 1:** failing arg-parse tests (`try_parse_from`) for the 4 new subcommands: `list_free_parses`, `add_free_parses_with_key_flag`, `sync_pool_parses`, `key_status_parses_optional_platform`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `list-free`/`add-free`/`sync-pool` fully; `key-status` with cooldown/headroom columns stubbed `"n/a"` and a `// TODO(Phase 2): wire ledger/cooldown once pool/ exists` comment — call this out explicitly in the commit body (not a silent placeholder).
- [ ] **Step 4:** PASS; `cargo build -p single-cli -p single-core -j2`.
- [ ] **Step 5: Commit:** `feat: single provider list-free/add-free/sync-pool/key-status`

---

## Phase 2 — Part B core: ledger, cooldown, backoff, pools (spec §6.1–6.3, build-order item 2a)

Pure / in-mem SQLite, exhaustively unit-tested, no HTTP yet.

### Task 4: `pool/mod.rs` schema + `ledger.rs`

**Files:** create `crates/single-runtime/src/pool/mod.rs`, `ledger.rs`; modify `lib.rs`.

**Interfaces:**
- `pool::ensure_pool_schema(conn) -> Result<()>` — creates `pool_usage`, `pool_leases`, `pool_cooldowns`, `pool_outcomes` (this task creates all 4 up front, even though `cooldown`/`bandit` fill in later, matching E27 Task 1's "schema first" precedent) plus delegates to `pool_keys::ensure_schema` and `self_heal`'s table (created in Phase 8, but the fn signature is stable now).
- `ledger::acquire_lease(conn, platform, model, key_id, est_tokens: u64) -> Result<LeaseId>` — records an in-memory `Lease` + a `pool_leases` row; `release_lease(conn, LeaseId)` (idempotent — a double-release is a no-op, not an error); a lease auto-expires after a 2-min backstop (checked lazily on next admission call, not a timer).
- `ledger::admit(conn, leases: &[Lease], platform, model, key_id, est_tokens, limits: &Limits, now_ms: i64) -> AdmitResult` where `enum AdmitResult { Ok, Denied { window: WindowKind, retry_after_ms: i64 } }` — for every window with `Some(limit)` in `Limits`, sums `recorded (pool_usage, sliding: RPM/TPM 60s, RPD/TPD to next UTC midnight) + in_flight (leases) + estimate` and denies if `>= limit`; `None` limit → window skipped (unknown-limit case, spec §6.1).
- `ledger::record(conn, platform, model, key_id, kind: UsageKind, amount: u64, at_ms: i64) -> Result<()>`.
- `ledger::next_utc_midnight_ms(now_ms: i64) -> i64` (pure helper for RPD/TPD window math).
- Degraded-DB fallback: if the SQLite write in `record`/`admit` fails, fall back to an in-process `Mutex<VecDeque<UsageEvent>>` capped at N entries (pruned on push) so admission math still works for the life of the process.

- [x] **Step 1:** failing tests:
  - `admission_denies_when_recorded_plus_inflight_plus_estimate_exceeds_limit`
  - `admission_skips_unset_limit_windows`
  - `rpd_window_resets_at_utc_midnight_not_24h_rolling`
  - `lease_acquire_release_is_idempotent`
  - `stale_lease_backstop_expires_after_2_minutes`
  - `degraded_db_fallback_keeps_admitting_from_memory` (simulate a write failure by pointing at a read-only path or an already-closed connection wrapper)
- [x] **Step 2:** `cargo test -p single-runtime pool::ledger -- --nocapture`, expect FAIL.
- [x] **Step 3:** implement. Reuse E27's `parse_or_estimate_tokens` seam for `chars/4` estimates (spec §6.1) — CHECK its current location (`single-runtime` or `single-core`) and import rather than reimplement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool quota ledger — 4-D admission, leases, UTC-midnight windows`

---

### Task 5: `backoff.rs` — Retry-After / body / prose parser

**Files:** create `crates/single-runtime/src/pool/backoff.rs`.

**Interfaces:**
- `fn parse_retry_after(header: Option<&str>) -> Option<Duration>` — delta-seconds or HTTP-date.
- `fn parse_body_shape(body: &serde_json::Value) -> Option<Duration>` — depth-capped (≤6) walk for `retryDelay`/`retry_after`/`retryAfterSeconds`, including Gemini's `error.details[].RetryInfo.retryDelay="17s"`.
- `fn parse_prose(text: &str) -> Option<Duration>` — anchored patterns: "try again in N seconds/minutes", "retry after Nm/Nh".
- `fn resolve(header: Option<&str>, body: Option<&serde_json::Value>, prose_fallback: Option<&str>) -> Option<Duration>` — header wins, else body, else prose; result clamped to 24h; **only the numeric duration is returned/kept, the caller never logs the raw body/prose** (spec §6.2 explicit privacy note — enforce by the type signature: this fn never returns the source string).

- [x] **Step 1:** failing tests: `header_delta_seconds`, `header_http_date`, `gemini_retryinfo_shape`, `snake_case_retry_after_shape`, `prose_try_again_in_30_seconds`, `prose_retry_after_2m`, `clamps_to_24h`, `header_wins_over_body_wins_over_prose`, `returns_none_when_nothing_parses`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement (no regex crate if avoidable — CHECK if `regex` is already a workspace dep before adding one; a small hand-rolled scanner is fine and keeps "no new dependency" honest if it isn't).
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool back-off parser (Retry-After, error-body shapes, prose)`

---

### Task 6: `cooldown.rs` — ladder + provenance (probe job deferred to Phase 3's client wiring)

**Files:** create `crates/single-runtime/src/pool/cooldown.rs`.

**Interfaces:**
- `ensure_schema` folded into Task 4's `pool::ensure_pool_schema` call graph (this task fills in the table's actual use).
- `enum Provenance { Heuristic, Authoritative, Credit, Tier }`.
- `fn bench(conn, platform, model, key_id, kind: BenchKind, now_ms: i64) -> Result<i64>` where `enum BenchKind { Transient, Escalated, PaymentRequired, TierGate, AuthBenched, Local, Authoritative(Duration) }` → returns `until_ms`. Ladder: `Transient` 90s → escalation 2m→10m→1h→1day over a rolling 24h hit window (tracked via `hits`/`hits_window_start_ms` columns); `PaymentRequired`/`TierGate` → 1 day, `Provenance::Credit`/`Tier` (never probed); `AuthBenched` → ~5m, until next health cycle; `Local` → 5s, **never enters the ladder** (doesn't increment `hits`); a successful request (`clear_hits`) resets the hit counter.
- `fn is_benched(conn, platform, model, key_id, now_ms) -> Result<Option<i64>>` (returns `until_ms` if still benched).
- `fn clear_hits(conn, platform, model, key_id) -> Result<()>` — called on a success.
- `fn heuristic_probe_candidates(conn, now_ms) -> Result<Vec<(String,String,String)>>` (platform, model, key_id) — `Heuristic` provenance, half the bench elapsed, >60s remains; never `Authoritative`/`Credit`/`Tier`.
- `fn cooldown_ceiling(conn) -> Duration` / `fn set_cooldown_ceiling(conn, Duration)` — operator override (`single pool cooldown-ceiling`), caps ladder + 402/403 benches, never shortens a provider-stated `Authoritative` time.
- `fn clear(conn, key: Option<&str>) -> Result<()>` — `single pool cooldown-clear [--key <id>]`.

- [x] **Step 1:** failing tests: `ladder_escalates_over_scripted_hit_sequence` (90s→2m→10m→1h→1day), `success_clears_hit_counter`, `payment_required_benches_one_day_as_credit_provenance`, `local_error_never_enters_ladder`, `heuristic_capped_at_operator_ceiling`, `authoritative_never_shortened_by_ceiling`, `probe_candidates_only_heuristic_past_half_elapsed`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool cooldown ladder with provenance and operator ceiling`

---

### Task 7: `pools.rs` — provider-wide pool inference and shared-pool gating

**Files:** create `crates/single-runtime/src/pool/pools.rs`.

**Interfaces:**
- `fn infer_pool_key(platform: &str, model: &str) -> String` — per spec §6.3 examples (`openrouter::free`, `google::project`, `nvidia::credit-pool`, `groq::account`, `xkiro::free`, `navyai::daily-tokens`, …), driven by `free_pool::by_id(platform).pool`.
- `fn is_shared_pool(platform: &str) -> bool` — true for the spec §6.3 list (`routeway, bazaarlink, unorouter, orcarouter, xkiro, anyapi, navyai, nara, sea-lion, aion, requesty`).
- `fn aggregate_gate(conn, platform, key_id, models: &[&str], now_ms) -> AdmitResult` — sums per-model `pool_usage` windows for the same `platform+key_id` and admits/denies as one gate (uses `ledger::admit`'s window math per model, then combines).

- [x] **Step 1:** failing tests: `infer_pool_key_matches_spec_examples` (table-driven over the named platforms), `is_shared_pool_matches_spec_list`, `aggregate_gate_sums_across_models_for_same_key`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: provider-wide pool inference and shared-pool aggregate gating`

---

## Phase 3 — Part B rest: bandit, degrade, handoff (build-order item 2b)

### Task 8: `bandit.rs` — Thompson-sampling scorer

**Files:** create `crates/single-runtime/src/pool/bandit.rs`.

**Interfaces:**
- `fn record_outcome(conn, platform, model, key_id, ok: bool, latency_ms: u64, tokens: u64, at_ms: i64) -> Result<()>` — appends `pool_outcomes` (7-day retention, prune on write).
- `fn posterior(conn, platform, model, key_id, now_ms) -> (f64, f64)` — `(α, β)` = `(1 + decay_weighted_successes, 1 + decay_weighted_failures)`, `weight = 0.5^(age_days/2)`, 7-day window. **Doc comment**: `Beta(1,1)` uniform prior — no community feed to seed from (E28 spec §17/D3); documents the seam for a future signed-feed loader.
- `fn thompson_sample(alpha: f64, beta: f64, rng_seed: u64) -> f64` — bounded `[0,1]` (no `rand` crate dependency unless already present — CHECK; else a small deterministic sampler seeded from a counter/time is acceptable for a heuristic scorer, note the tradeoff in the commit).
- `fn speed_score(latency_ms: u64, ttfb_ms: Option<u64>) -> f64` — normalized inverse latency.
- `fn intelligence_score(platform: &str, model: &str) -> f64` — from `free_pool` catalog rank/size-label, normalized per provider (Phase 1's `FreeProvider` needs an `intelligence_rank`/`size_label` field if not already added in Task 1 — CHECK and backfill there if missed, don't add scope-creep here).
- `enum Strategy { Balanced, Smartest, Fastest, Reliable, Custom { w_rel: f64, w_speed: f64, w_intel: f64 }, Priority }` with `Balanced` weights `(0.5, 0.25, 0.25)`.
- `fn effective_score(base: f64, headroom_factor: f64, ratelimit_factor: f64) -> f64` — `base * headroom * ratelimit`, both factors `∈ [floor, 1]`.
- `fn pick(conn, kind, effort, strategy: &Strategy, candidates: &[(String,String,String)], now_ms) -> Option<(String,String,String)>` — scores every `(platform, model, key_id)` candidate not benched/disabled, applies headroom from `ledger::admit` remaining-fraction, returns the top pick (or `priority` strategy: first admissible in chain order, no scoring).
- CLI-facing: `fn key_selection(mode: KeySelection, platform: &str, candidates: &[String]) -> Option<String>` where `enum KeySelection { Auto, LeastRemaining }` — `LeastRemaining` skipped for `pools::is_shared_pool` platforms (every key reports the same number there).

- [x] **Step 1:** failing tests: `posterior_starts_at_uniform_beta_1_1`, `posterior_decays_with_2day_half_life`, `thompson_sample_bounded_0_1`, `strategy_weights_sum_to_one_for_every_named_strategy`, `effective_score_is_base_times_headroom_times_ratelimit`, `timeout_counts_as_reliability_fail_and_speed_sample` (a `record_outcome(ok=false, latency_ms=timeout_value, ...)` still contributes a speed sample), `priority_strategy_ignores_score_takes_first_admissible`, `least_remaining_skipped_for_shared_pools`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool bandit — Thompson-sampled reliability/speed/intelligence scoring`

---

### Task 9: `degrade.rs` — degraded-mode state machine

**Files:** create `crates/single-runtime/src/pool/degrade.rs`.

**Interfaces:**
- `struct DegradeState { mode: Mode, since_ms: Option<i64> }` where `enum Mode { Normal, Degraded }`.
- `fn healthy_ratio(usable_keys: usize, enabled_providers: usize) -> f64` (unknown key status counts as healthy per spec — caller passes it pre-counted that way).
- `fn tick(state: &mut DegradeState, ratio: f64, enabled_count: usize, now_ms: i64, cfg: &DegradeConfig) -> Mode` — pure transition function: `Normal → Degraded` when `ratio < 0.5` sustained for `entry_grace` (60s default); `Degraded → Normal` when `ratio >= 0.5` sustained for `exit_grace` (120s default, hysteresis); no transition at all below `min_providers` (3 default) enabled.
- `DegradeConfig { healthy_ratio: f64, min_providers: usize, entry_grace_ms: i64, exit_grace_ms: i64 }` + `Default` + env overrides `SINGLE_POOL_DEGRADED_{HEALTHY_RATIO,MIN_PROVIDERS,ENTRY_GRACE_MS,EXIT_GRACE_MS}`.
- `fn exploration_enabled(mode: Mode) -> bool` — `false` in `Degraded` (bandit sticks to scored order of survivors, no Thompson exploration).

- [x] **Step 1:** failing tests: `enters_degraded_after_entry_grace_below_threshold`, `does_not_flap_before_entry_grace_elapsed`, `exits_degraded_after_exit_grace_above_threshold`, `no_transition_below_min_providers`, `exploration_disabled_in_degraded_mode`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool degraded-mode health state machine`

---

### Task 10: `handoff.rs` — context handoff on model switch

**Files:** create `crates/single-runtime/src/pool/handoff.rs`.

**Interfaces:**
- `struct SessionEntry { messages_summary: String, last_provider_model: (String, String) }`, TTL 3h, stored in a process-global `Mutex<HashMap<String, (SessionEntry, Instant)>>` (memory-only, never disk/logs, per spec §6.7).
- `fn session_key(explicit: Option<&str>, first_user_message: &str) -> String` — explicit id if given, else SHA-1 hex of `first_user_message` (reuse an existing SHA-1 impl if the workspace already has one via a dep — CHECK `single-core`; else the small dependency-free implementation is fine since SHA-1 here is a stable-key hash, not security-sensitive).
- `fn inject(store: &HandoffStore, session_key: &str, new_provider: &str, new_model: &str, messages: &mut Vec<ChatMessage>) -> bool` — returns whether it injected; prepends the exact spec §6.7 system-message template **only** when: an entry exists for the key, `last_provider_model != (new_provider, new_model)`, and no handoff message is already present in `messages`. Updates the stored `last_provider_model` regardless.
- `fn record_summary(store, session_key, summary: String, provider: &str, model: &str)` — called after each turn to keep `messages_summary`/`last_provider_model` current.

- [x] **Step 1:** failing tests: `injects_on_model_switch`, `does_not_inject_on_first_request` (no prior entry), `does_not_inject_on_same_model_continuation`, `does_not_inject_twice_if_already_present`, `session_key_uses_explicit_id_when_given`, `session_key_falls_back_to_sha1_of_first_message`, `entry_expires_after_ttl` (inject with a manually-aged timestamp → treated as absent).
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool context-handoff on model switch`

---

## Phase 4 — Part B `client.rs`: HTTP dispatch (spec §6.6, build-order item 3)

### Task 11: OpenAI-compat wire (covers 33 providers)

**Files:** create `crates/single-runtime/src/pool/client.rs`.

**Interfaces:**
- `trait PoolWire { fn dispatch(&self, req: &PoolRequest, provider: &FreeProvider, key: &str) -> Result<PoolResponse>; }`.
- `struct OpenAiCompatWire;` impl — `POST {base_url}/chat/completions`, `Authorization: Bearer {key}` (or per `Auth` variant), SSE or non-stream per `quirks.no_stream`, `<think>` extraction, truncation detection (`finish_reason == "length"`), first-byte budget tracked for `bandit::record_outcome`'s TTFB.
- Quirks applied: `browser_ua` header, `force_single_tool_call` (drop all but the first tool call in a request), `no_tools` (strip tool defs, surface a typed error if the caller required tools), `min_max_tokens` (bump a too-low `max_tokens` up), per-provider `timeout` from `FreeProvider.timeout`.
- **Retry budget + hedge:** `SINGLE_POOL_RETRY_BUDGET_MS` (default 45000) checked before each retry; attempt 0 and the first failover always run regardless of remaining budget; a budget expiry mid-attempt aborts the in-flight `reqwest` via a `tokio_util::sync::CancellationToken`-equivalent (CHECK if that crate is already a dep; else a simple `Arc<AtomicBool>` polled between chunks is enough for a blocking client) — this abort (`HedgeAbort`) is explicitly **not** a health signal, no `cooldown::bench` call, disarmed on first byte received.
- **Tool-call rescue:** `fn rescue_tool_calls(text: &str) -> Option<Vec<ToolCall>>` — parses models that emit tool calls as prose into real `tool_calls`; only applied when the request needed tools and the model claims `supports_tools`.
- `fn dispatch_openai_compat(...) -> Result<PoolResponse, PoolError>` where `PoolError` distinguishes `RateLimited { retry: Option<Duration> }` / `PaymentRequired` / `TierGate` / `AuthFailed` / `Transport` / `Other` — feeds directly into `cooldown::bench`'s `BenchKind`.

- [x] **Step 1:** failing tests against a hand-rolled `TcpListener` mock (or CHECK if `wiremock` is already a dev-dependency; if not, a `std::net::TcpListener` fixture is fine, no new dep):
  - `request_shape_has_correct_url_and_bearer_auth`
  - `browser_ua_quirk_sets_header`
  - `force_single_tool_call_drops_extra_tool_calls`
  - `no_tools_quirk_strips_tool_defs`
  - `sse_stream_parses_incremental_chunks`
  - `429_maps_to_rate_limited_pool_error_with_parsed_retry_after`
  - `hedge_abort_on_budget_expiry_is_not_recorded_as_a_health_failure`
  - `tool_call_rescue_parses_prose_tool_call`
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement `OpenAiCompatWire` + shared retry/hedge machinery reusable by the native wires in Task 12.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool client — OpenAI-compat wire with retry budget and hedge-abort`

---

### Task 12: native wires

**Files:** modify `client.rs` (or split into `client/native.rs` if it grows past ~500 lines — CHECK size after Task 11, note the split in the commit if taken).

**Interfaces:** one small `impl PoolWire` per native platform per spec §6.6:
- `GoogleWire` (Gemini `generateContent`, 60s timeout)
- `CohereWire`
- `CloudflareWire` (compound `account_id:token` auth)
- `ZhipuWire` (domestic→global host re-probe, 60s)
- `AiHordeWire` (queue submit + poll, keyless sentinel, `min_max_tokens=16`, array `stop`, `no_tools`, `no_stream`, 120s)
- `SailWire` (Responses API background poll — registered per §17 resolution but `enabled=false` by default; wire still implemented so an operator who flips it on gets a working path)
- `ModelScopeWire`, `PollinationsWire`, `ElectronHubWire`, `ExperientialWire` (custom validate-probe pattern)

- [x] **Step 1:** failing tests, one per wire, minimum: request shape (URL/auth placement) against the mock server; `ZhipuWire`'s host-reprobe behavior; `AiHordeWire`'s queue-poll loop (mock returns `queued` then `done`); `SailWire`'s background-poll shape.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement each wire, reusing Task 11's retry/hedge/backoff plumbing.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: pool client — native wires (google, cohere, cloudflare, zhipu, aihorde, sail, modelscope, pollinations, electronhub, experiential)`

---

## Phase 5 — Part C: the `single-pool` agent (spec §7, build-order item 4)

### Task 13: `pool_agent.rs` + registry entry

**Files:** create `crates/single-runtime/src/pool_agent.rs`; modify agent registry (CHECK exact location — likely `single-agent-sdk` or `single-runtime::registry` — grep `AgentDefinition` before writing).

**Interfaces:**
- `AgentDefinition` entry `single-pool`: `adapter = "pool"`, `home_requirement = None`, `max_concurrency = None` (ledger caps it, not a slot — spec §7).
- `pool_agent::execute(ctx, prompt: &str, cwd: &Path, timeout: Duration, session_key: Option<&str>) -> Result<PoolAgentOutcome>` where `enum PoolAgentOutcome { Ok(PoolResponse), Exhausted { earliest_recovery_ms: i64 } }` — implements the same run contract `task::execute` expects:
  1. build messages (system preamble from cwd context, then prompt),
  2. `bandit::pick(kind, effort, strategy)` → `(provider, model, key)`,
  3. `handoff::inject`,
  4. `ledger::acquire_lease`,
  5. `client::dispatch`,
  6. success → `ledger::record`, `bandit::record_outcome(ok=true)`, write the task artifact (same `task_artifact_path` as any task; real `usage` when the provider reports it),
  7. 429/5xx → `cooldown::bench` (+ `backoff::resolve`), `bandit::record_outcome(ok=false)`, pick the next candidate within the retry budget,
  8. all eligible exhausted → `PoolAgentOutcome::Exhausted { earliest_recovery_ms }` (the earliest `until_ms` across attempted cooldowns) — **not** a plain `Err`, this is what Phase 6's auto-continue consumes.
- `single task run --agent single-pool "…"` works standalone — a one-shot fallback loop, no coordinator required.

- [x] **Step 1:** failing tests (fake `PoolWire`/dispatcher injected, no real network): `execute_picks_and_dispatches_via_bandit`, `execute_injects_handoff_on_model_switch`, `execute_records_outcome_on_success`, `execute_benches_and_retries_next_candidate_on_429`, `execute_returns_exhausted_when_all_candidates_fail`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: single-pool agent adapter`

---

### Task 14: `single task run --agent single-pool` end-to-end wiring

**Files:** modify wherever `task::run`/`task::execute` dispatches by adapter kind (grep `adapter ==` / `match adapter` before editing).

**Interfaces:** the existing dispatch switch gains a `"pool"` arm calling `pool_agent::execute` instead of shelling a CLI binary; `PoolAgentOutcome::Exhausted` maps to the task's existing "rate limited" terminal shape so today's `single task run` UX (which already understands rate-limited tasks per E27) doesn't need new plumbing yet — full `waiting_on_capacity` goal-level semantics land in Phase 6.

- [x] **Step 1:** failing integration-shaped test with a fake dispatcher: `task_run_with_agent_single_pool_calls_pool_agent_not_a_cli`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement the dispatch arm.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: wire single-pool into task::run dispatch`

---

## Phase 6 — Part C coordinator wiring + Part D auto-continue (spec §8, build-order items 5–6)

### Task 15: `bandit::pick` as a coordinator routing target

**Files:** modify `crates/single-runtime/src/coordinator/routing.rs`.

**Interfaces:**
- `routing.toml` `[kind.*]` lists can name `single-pool` like any other agent string.
- `coordinator.toml` gains `prefer_pool: bool` (default `false`) — when `true`, `select_agent` tries `single-pool` first for every kind unless that kind's list explicitly overrides (spec §7 last bullet).
- `select_agent`'s existing signature (`table, kind, effort, health`) is unchanged; it just needs to treat `"single-pool"` as always-available (no `detected_authed`/`rate_limited` health check the way CLI agents get one — the pool engine has its own admission) rather than filtering it out as an undetected binary.

- [x] **Step 1:** failing tests: `select_agent_returns_single_pool_when_named_in_kind_list`, `prefer_pool_true_tries_single_pool_first_unless_kind_overrides`, `single_pool_is_never_filtered_as_undetected`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: coordinator routing recognizes single-pool as a routable agent`

---

### Task 16: `WaitingOnCapacity` goal state + scheduler wiring

**Files:** modify `crates/single-runtime/src/coordinator/graph.rs` (new enum variants), `goal.rs` (new columns via `add_column_if_missing`), `scheduler.rs` (`tick_pure` branch), `events.rs` (`capacity_wait`/`capacity_resumed` event kinds).

**Interfaces:**
- `GoalStatus` gains `WaitingOnCapacity` (serde `waiting_on_capacity`), parse-tolerant fallback per E27 precedent (spec §12).
- `goals` gains `capacity_reason TEXT`, `earliest_retry_at_ms INTEGER`, `capacity_waits INTEGER DEFAULT 0`; `graph_nodes` gains `earliest_retry_at_ms INTEGER` — both via `add_column_if_missing` (CHECK its exact name/location — used by E27 for `tasks.prompt_tokens` per the E27 plan Phase 3).
- `tick_pure` (or its db-shell caller in `scheduler.rs`, since exhaustion is a dispatch-result, not a pure-graph fact) gains: when a node's dispatch result is `PoolOutcome::Exhausted { earliest_recovery_ms }` (from `pool_agent`) or a CLI agent's fallback chain is fully rate-limited (existing E27 signal, reused) → node → `Pending` with `earliest_retry_at_ms` stamped, goal → `WaitingOnCapacity` with `capacity_reason` (which providers/pools, earliest window). The scheduler's ready-set/admit pass **skips** any node whose `earliest_retry_at_ms > now` and **re-admits** once it passes — driven by real cooldown/ledger state, not a fixed sleep.
- **Resume budget**: `coordinator.toml` `max_capacity_waits_per_goal` (20) / `max_capacity_wait_minutes` (720) — past either, `capacity_waits` increment stops helping and the goal goes `Blocked` with `"waited {h}h for capacity, still exhausted"`. `single goal amend <id> capacity-budget=N` raises `max_capacity_waits_per_goal` for that goal (reuse E27's existing `budget=N` amend-text parsing pattern, extend it to also recognize `capacity-budget=N`).
- On resume: node's prompt rebuilt via `handoff::inject` (different provider/model can pick up cleanly), dependency outputs re-attached (already how E27 builds a node's dispatch options — no new plumbing, just re-triggered).
- Events: `capacity_wait` (body = reason + ETA), `capacity_resumed`.

- [x] **Step 1:** failing tests (fake dispatcher returning `Exhausted`, per E27 Task 8's `Dispatcher` seam):
  - `exhausted_dispatch_moves_node_to_pending_with_retry_stamp_and_goal_to_waiting_on_capacity`
  - `tick_skips_node_whose_retry_stamp_is_in_the_future`
  - `tick_readmits_node_once_retry_stamp_passes`
  - `resume_budget_exhaustion_moves_goal_to_blocked_with_reason`
  - `capacity_budget_amend_raises_the_per_goal_limit`
- [x] **Step 2:** `cargo test -p single-runtime coordinator::scheduler -- --nocapture`, expect FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: waiting_on_capacity goal state and auto-continue scheduling`

---

### Task 17: `single coordinator status` / `single acp` surfacing

**Files:** modify `crates/single-cli/src/render.rs`, `crates/single-cli/src/acp.rs` (if it exists yet in this repo state — CHECK; if native `single acp` from E27 Phase 2 isn't landed, this sub-step is a no-op placeholder noted in the commit, not silently skipped).

**Interfaces:** `single coordinator status` line: `waiting: goal_x — nvidia+groq pools spent, resumes ~14:03Z`. If `single acp` exists: translate `capacity_wait` events to an `agent_thought_chunk` ("all providers rate-limited; holding, resumes ~14:03Z") so the Zed panel shows a live hold rather than a stall.

- [x] **Step 1:** failing test `coordinator_status_renders_waiting_on_capacity_line_with_reason_and_eta`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement; CHECK whether `single acp` exists in this checkout (`ls crates/single-cli/src/acp.rs`) — if not, note in the commit body that the ACP translation is deferred to whenever `single acp` lands, per spec §8.
- [x] **Step 4:** PASS; `cargo build -p single-cli -j2`.
- [x] **Step 5: Commit:** `feat: surface waiting_on_capacity in coordinator status (and acp, if present)`

---

## Phase 7 — Part F: self-healing / self-resuming sessions (spec §10, build-order item 7)

Landing before Part E (self-heal) per spec §16's stated order — resume logic is scoped tighter and unblocks self-heal's "coordinator self-correction" category, which references re-ticking.

### Task 18: `resume_interrupted()` + `Paused` state

**Files:** modify `crates/single-runtime/src/coordinator/mod.rs`, `server.rs`.

**Interfaces:**
- `GoalStatus` gains `Paused` (new terminal-ish state; `resume_interrupted` treats it as "re-tick").
- `coordinator::resume_interrupted(conn) -> Result<usize>` — called from `server.rs`'s startup block, **after** the existing `reconcile` call (which already handles `graph_nodes.status='running'` with dead tasks → `failed`):
  - a goal in `Running`/`Planning`/`WaitingOnCapacity` with no live node and not all-terminal → re-ticked (scheduler recomputes ready-set, re-dispatches `Pending` nodes); a node with a partial `output_ref` keeps it, fed back via `handoff::inject`.
  - a goal in `Planning` with an empty graph → `brain::plan` re-run (the planner call was interrupted).
  - writes a `session_resumed` event per goal touched.
- `single daemon stop` (clean) marks its non-terminal goals `Paused` instead of leaving them `Running` (distinguishing clean-stop from crash — a crash leaves rows `Running`, caught by the existing PID-check reconcile).
- `single goal resume <id>` — CLI subcommand, manual trigger of the same re-tick path for a `Blocked`/`Failed` goal a human judges recoverable.

- [x] **Step 1:** failing tests (tempdir daemon-start path): `running_goal_with_pending_node_and_no_live_task_is_reticked_and_events_session_resumed`, `planning_goal_with_empty_graph_reruns_plan` (stub planner), `paused_goal_is_reticked_on_resume_interrupted`, `clean_stop_marks_nonterminal_goals_paused`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: coordinator resume_interrupted — re-tick on daemon restart, paused state on clean stop`

---

### Task 19: `single acp` session/load re-attach (if `single acp` exists) + `single goal resume` CLI

**Files:** modify `crates/single-cli/src/acp.rs` (if present) and `main.rs`.

**Interfaces:** after `session/load` replay (E27), if the session has a `Running`/`WaitingOnCapacity` goal, re-attach its event long-poll so a restarted Zed panel keeps streaming. `single goal resume <id>` CLI wraps `coordinator::resume_interrupted`'s single-goal path.

- [x] **Step 1:** failing test `goal_resume_parses_and_dispatches` (arg-parse) + (if acp present) a scripted-client test `session_load_reattaches_long_poll_for_inflight_goal`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement; if `single acp` doesn't exist in this checkout, note the deferral explicitly in the commit.
- [x] **Step 4:** PASS; `cargo build -p single-cli -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: single goal resume and acp session/load re-attach for in-flight goals`

---

## Phase 8 — Part E: self-heal / self-fix (spec §9, build-order item 8)

### Task 20: `self_heal.rs` — infra category

**Files:** create `crates/single-runtime/src/self_heal.rs`; modify `lib.rs`.

**Interfaces:**
- `self_heal_events(at, category, action, detail, ok)` table, created by `ensure_schema`.
- `~/.config/single/self_heal.toml` — `[categories] infra = true, coordinator = true, agent = true` + intervals (`self_heal_interval_secs` default 300), `db_backup_interval_secs` (3600), grace windows (`blocked_reeval_minutes` 30, `provider_key_grace_hours` 24) — lazy-write-on-first-use like `coordinator.toml`.
- `fn run_pass(ctx, conn, category_filter: Option<Category>) -> Result<PassReport>` — `catch_unwind`s each sub-step so one failure can't wedge the rest; logs a `self_heal_events` row per action; gated per-category by the toml.
- **infra** sub-steps: stale-socket removal (`runtime.sock` exists, no live PID → remove); zombie-row reconcile (extends `reconcile_orphaned_tasks` + `coordinator::scheduler::reconcile` — call both from the pass, not just startup); corrupt-config repair (every `*.toml` under `~/.config/single/` parse-checked; restore from newest `*.bak-*` sibling, else move-aside + regenerate defaults); DB integrity (`PRAGMA integrity_check`; on failure restore from newest `single.db.bak-*`, add the periodic backup writer here too, or last-resort schema rebuild + re-seed, logged loudly); dead-agent-binary re-detection (re-run `augmented_path` + `cached_discover`; missing-but-previously-detected → mark + queue reinstall if `agent` category enabled); cooldown probe job lives here too (§6.2, started from `single-runtimed`).
- `single self-heal log` / `single self-heal disable <category>` CLI.

- [x] **Step 1:** failing tests (tempdir): `stale_socket_removed_when_no_live_pid`, `corrupt_toml_restored_from_newest_backup`, `corrupt_toml_with_no_backup_moved_aside_and_regenerated`, `db_integrity_check_failure_restores_from_backup`, `every_action_writes_a_self_heal_events_row`, `disabled_category_is_a_noop`, `one_substep_panic_does_not_wedge_the_rest` (inject a panicking sub-step, assert others still ran).
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: self-heal pass — infra category (sockets, zombies, config, db integrity, agent binaries)`

---

### Task 21: `self_heal.rs` — coordinator self-correction category

**Files:** modify `self_heal.rs`.

**Interfaces:** per spec §9.2 —
- a goal `Blocked` for `> blocked_reeval_minutes` with a `capacity`/`supervisor` reason → re-evaluated (if the blocking condition cleared, move to `Running` and re-tick), bounded by `max_auto_reevals_per_goal` (3 default).
- a goal failing repeatedly on the **same node with the same agent** → the pass edits that node's routing (next agent in the kind's list, or `single-pool`), logs, re-ticks. Bounded, never touches a goal a human `amend`ed in the last hour.
- `routing.toml` drift: an agent undetected for > 24h in a routing list → commented out (file written, backup first), re-added on `sync-pool`/next detection.
- `coordinator.toml` sanity: absurd values (`max_parallel = 0`, `max_goal_minutes < 1`) reset to defaults, logged.
- Hard rule enforced in code, not just docs: the pass never deletes a goal, never force-kills a running node, never edits a config a human touched in the last hour (needs a `last_human_edit_at` marker — CHECK if `amend`/config-write paths already stamp one; if not, add it as part of this task, called out in the commit).

- [x] **Step 1:** failing tests: `long_blocked_capacity_goal_is_reevaluated_and_reticked_when_cleared`, `reeval_bounded_by_max_auto_reevals`, `repeated_same_node_same_agent_failure_reroutes_to_next_agent`, `routing_toml_drift_comments_out_undetected_agent_after_24h`, `coordinator_toml_absurd_values_reset_to_defaults`, `human_edited_config_in_last_hour_is_never_touched`, `pass_never_deletes_a_goal_or_force_kills_a_running_node`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement (add the `last_human_edit_at` marker if missing).
- [x] **Step 4:** PASS; `cargo build -p single-runtime -j2`.
- [x] **Step 5: Commit:** `feat: self-heal pass — coordinator self-correction category`

---

### Task 22: `self_heal.rs` — agent self-install/repair category

**Files:** modify `self_heal.rs`.

**Interfaces:** per spec §9.3 —
- node needs agent `X`, not detected-and-authed → (if enabled) runs `bootstrap::run_one`/`single setup --yes` for `X`, re-probes. CHECK exact fn names in the existing bootstrap module before wiring.
- auth repair: `X` installed but `has_live_login()` false → no interactive login attempt (headless); emits a `self_heal_events` row + coordinator `Blocked` reason `"agent X needs single agent login X"`, routes the node to an alternative. `keyring`-auth agents (codex/cursor, E27) get `cli_reports_logged_in()` re-checked.
- `single-pool` provider keys: a key failing validation for `> provider_key_grace_hours` (24 default) → auto-disabled in the pool (`pool_keys::disable`), logged; `single provider key-status` flags it.
- every install timeout-bounded, `-j` respectful, one-agent-at-a-time (reuse the E27 doctor 4-permit gate — CHECK its exact name, likely `DoctorGuard`/a semaphore in `doctor.rs`). `SINGLE_SELF_HEAL_AGENT_INSTALL=0` disables the whole category via env (in addition to the toml toggle).
- `single doctor` prints whether this category is on (safety note per spec: agent installs shell package managers, most likely category to want off on a shared box).

- [x] **Step 1:** failing tests: `missing_agent_triggers_reinstall_when_enabled`, `auth_repair_never_attempts_interactive_login_only_blocks_and_routes_away`, `stale_pool_key_auto_disabled_after_grace_period`, `env_var_disables_agent_install_category`, `installs_are_serialized_one_at_a_time` (reuse/extend the doctor-guard test pattern), `doctor_reports_agent_category_on_off_state`.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -p single-cli -j2`.
- [x] **Step 5: Commit:** `feat: self-heal pass — agent self-install/repair category`

---

### Task 23: wire `self_heal::run_pass` into daemon start + timer + `single doctor --fix`

**Files:** modify `bin/single-runtimed.rs`, `server.rs`, `single-cli/src/main.rs` (`doctor --fix` path).

**Interfaces:** on daemon start (after `resume_interrupted`), every `self_heal_interval_secs` (own thread/timer, same "one pass at a time" guard pattern as the E27 tick timer), and on `single doctor --fix`.

- [x] **Step 1:** failing test: a fake-clock/tempdir integration test asserting `run_pass` fires on the interval and is a no-op re-entrant call while one is in flight.
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement.
- [x] **Step 4:** PASS; `cargo build -p single-runtime -p single-cli -j2`.
- [x] **Step 5: Commit:** `feat: run self-heal pass on daemon start, timer, and doctor --fix`

---

## Phase 9 — Part G: new agent adapters (spec §11, build-order item 9)

### Task 24: `cline`, `continue`, `roo`, `mimo`, `atomcode`, `deepseek-harness` adapters

**Files:** modify wherever E27's `configure_mcp`-pattern adapters live (grep `AgentAdapter` / `fn configure` before writing — likely `single-agent-sdk/src/adapters/` or similar).

**Interfaces:** one adapter each, per spec §11's table (config file path, wire, notes). Each follows the existing `AgentAdapter` pattern: structural merge, timestamped backup, `0600` perms (E27 `configure_mcp` precedent). The `single-<name>`-aliased pool providers (Part A) and `single-pool` itself become selectable models in each config. `single install-integrations` learns these 6 new targets. `goose`, `aider`, `qwen-code`, `crush`, `kilocode` (already exist) just need the pool providers synced in — verify, don't reimplement.

- [x] **Step 1:** failing tests, one per adapter: `<agent>_configure_preserves_unrelated_keys_and_writes_backup_and_0600` (structural-merge snapshot test, tempdir).
- [x] **Step 2:** FAIL.
- [x] **Step 3:** implement each of the 6.
- [x] **Step 4:** PASS; `cargo build -p single-agent-sdk -j2` (or wherever they land).
- [x] **Step 5: Commit:** `feat: agent adapters for cline, continue, roo, mimo, atomcode, deepseek-harness`

---

## Phase 10 — version bump, CHANGELOG, deploy, live verification (build-order item 10)

### Task 25: workspace build + test + version bump

- [ ] **Step 1:** `cargo build --workspace -j2` (foreground) — must be clean.
- [ ] **Step 2:** `cargo test --workspace -j2` — capture every `test result:` line.
- [ ] **Step 3:** fix any breakage (most likely: exhaustive matches on `GoalStatus`/`NodeStatus`/`Request`/`ResponseData` in `render.rs`/`handlers.rs`).
- [ ] **Step 4:** bump `Cargo.toml` `[workspace.package] version` → `0.11.0`; add `CHANGELOG.md` `## [0.11.0]` block summarizing the free-provider pool, `single-pool` agent, auto-continue, self-heal, and session resume.
- [ ] **Step 5: Commit:** `chore: bump version to 0.11.0`

### Task 26: deploy + live verification

1. Reinstall release binaries to `~/.local/bin`: `--release -j2`, copy `single single-runtimed single-mcp singlecli-mcp single-lsp single-agent` from `target/release/`.
2. `systemctl --user stop single-runtimed; pkill -x single-runtimed; rm -f ~/.config/single/state/runtime.sock; systemctl --user start single-runtimed`.
3. `single --version` → `0.11.0`; `single daemon status` → running.
4. **Provider catalog:** `single provider list-free` shows ~40 entries; `single provider sync-pool` writes `free-pool.toml` with the 5 region-walled ids `enabled = false` and their reason strings.
5. **Keyed pool run:** `single provider add-free groq --key <k>` (or another easy free provider); `single task run --agent single-pool "say PONG"` → answers, records usage (`single provider key-status groq` shows headroom).
6. **Forced-429 cooldown:** exhaust a low-limit provider's RPM (or use a deliberately-wrong key against a provider whose 401 path is easy to trigger) → `single pool status` shows the bench; a second attempt hops to the next candidate.
7. **Degraded-mode trip:** disable/misconfigure enough providers to drop the healthy ratio below 0.5 for `entry_grace` → `single pool status` shows `degraded since <ts>`.
8. **Auto-continue hold+resume:** submit a coordinator goal routed through `single-pool` against providers you can force into exhaustion → goal shows `waiting_on_capacity` with a reason + ETA; once a cooldown clears, the goal resumes without re-prompting.
9. **Daemon-restart goal resume:** kill `single-runtimed` mid-goal, restart → the goal re-ticks and continues (not `failed`), `session_resumed` event present.
10. **`doctor --fix` on a deliberately-corrupted config:** hand-corrupt one `*.toml` under `~/.config/single/`, run `single doctor --fix` → restored from backup, `self_heal_events` row present, `single self-heal log` shows it.

---

## Self-Review

**Spec coverage:**
- §5 Part A catalog — Phase 1 (Tasks 1–3). ✓
- §6.1–6.3 ledger/cooldown/backoff/pools — Phase 2 (Tasks 4–7). ✓
- §6.4–6.7 bandit/degrade/handoff — Phase 3 (Tasks 8–10). ✓
- §6.6 client (compat + native wires) — Phase 4 (Tasks 11–12). ✓
- §7 `single-pool` agent — Phase 5 (Tasks 13–14). ✓
- §7 last bullet, coordinator wiring — Phase 6 Task 15. ✓
- §8 auto-continue — Phase 6 Tasks 16–17. ✓
- §10 Part F resume — Phase 7 (Tasks 18–19), landed before Part E per spec §16 note it's a distinct build-order item (7) ahead of self-heal (8). ✓
- §9 Part E self-heal, all 3 categories — Phase 8 (Tasks 20–23). ✓
- §11 Part G adapters — Phase 9 (Task 24). ✓
- §12 data model — every table assigned: `pool_provider_keys` (Task 2), `pool_usage`/`pool_leases` (Task 4), `pool_cooldowns` (Task 6), `pool_outcomes` (Task 8), `self_heal_events` (Task 20), `goals`/`graph_nodes` new columns (Task 16). ✓
- §13 config files — `free-pool.toml` (Task 3), `pool.toml` (§17 resolution, referenced throughout Phase 2–4, written by Phase 2's first config-touching task — CHECK it lands no later than Task 4), `self_heal.toml` (Task 20), `routing.toml`/`coordinator.toml` extensions (Tasks 15–16). ✓
- §14 testing matrix — every layer has a task with the named test cases: `free_pool` (T1), `ledger` (T4), `cooldown` (T6), `pools` (T7), `bandit` (T8), `degrade` (T9), `handoff` (T10), `client` (T11–12), `pool_agent` `#[ignore]` integration deferred to live verification (Task 26 step 5), auto-continue (T16), self_heal (T20–22), resume (T18), adapters (T24). ✓
- §15 migration/compatibility — Global Constraints section; version bump isolated to Task 25. ✓
- §16 build order — Phases 1–10 map 1:1 to build-order items 1–10. ✓
- §17 open questions — resolved explicitly above, each decision threaded into the specific task that implements it (pool.toml split → Task 4/13 doc comments; Beta(1,1) → Task 8; text-only v1 → Task 1/11 scope notes; region-walled default-disabled → Task 1/3/16).

**Placeholder scan:** every code task has concrete signatures + named test cases. The one intentionally-deferred piece (`key-status`'s cooldown/headroom columns in Task 3, before `pool/` exists) is explicitly flagged with a `// TODO(Phase 2)` and called out in its own commit body, not hidden — and Phase 2 doesn't separately re-open Task 3, so track it as a one-line follow-up inside Task 6's or Task 4's commit if not already wired by render-time.

**Type consistency:** `FreeProvider`/`Wire`/`Auth`/`Limits`/`PoolShape`/`Quirks` — Task 1, consumed by Tasks 3, 7, 8, 11, 12. `Lease`/`AdmitResult` — Task 4, consumed by Task 13. `Provenance`/`BenchKind` — Task 6, consumed by Task 11 (`PoolError` → `BenchKind` mapping) and Task 13. `Strategy`/`KeySelection` — Task 8, consumed by Task 13, 15. `DegradeState`/`Mode` — Task 9, consumed by Task 8's `pick()` exploration flag and Task 13. `PoolOutcome`/`PoolAgentOutcome` — Task 13, consumed by Task 16 (scheduler exhaustion branch). `GoalStatus::WaitingOnCapacity`/`Paused` — Task 16/18, consumed by Task 17, 19, and every `render.rs`/`handlers.rs` exhaustive match touched in Task 25.
