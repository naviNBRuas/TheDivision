# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/) —
pre-1.0, so the minor version (`0.x.0`) carries new functionality and the
patch version (`0.0.x`) carries fixes, per [CONTRIBUTING.md](CONTRIBUTING.md).

## [Unreleased]

## [0.17.1]

- Fixed: a brain role (planner/supervisor/integrator) that produced
  unparseable output permanently blocked its goal on the first miss —
  confirmed live on a goal whose two work nodes both finished
  successfully days earlier but stayed `blocked` forever on "integrator
  failed: brain role produced no parseable JSON". `run_role` now retries
  up to 2 more times before giving up, since a malformed-JSON response
  from a stochastic LLM call is a content-quality problem a fresh sample
  usually fixes.

## [0.17.0]

- Added: a **Goals** tab in the TUI, listing the coordinator's goal list
  (`GoalList`) sorted running/waiting/queued/blocked/failed above
  done/cancelled history — the Coordinator subsystem (E27.02) made goals
  the primary unit of work, but the TUI had no view onto them at all
  before this.
- Added: the Providers tab now shows a **Key** column (keyed/no key),
  cross-referencing `SecretList`'s stored key names against each
  provider's `secret_name` — previously there was no way to tell "known
  preset, never keyed" from "actually has a key stored" without leaving
  the TUI.
- Changed: Zed's `/goals` (via `single acp`) now shows active goals plus
  the 10 most recent failures by default instead of dumping every goal
  ever submitted unsorted — `/goals all` still gives the full history.
- Fixed: the TUI's Pool tab spun on "Loading…" forever — its fetch
  functions ran correctly but the main event loop never polled their
  results.
- Changed: the Agents tab's auth label for an agent with no auth-state
  detection reads "n/a" instead of a bare "-", since that state
  genuinely covers both "keyless agent" and "can't detect" — no
  behavior change, just an honest label.

## [0.16.0]

- Added: `single provider validate [--platform <id>]` re-probes every
  already-keyed free-pool key against its provider's `validate_url`, the
  same best-effort check `add-free` does at registration time — the only
  other way a key's status ever updated. Skips providers with no
  `validate_url` quirk rather than guessing an endpoint, and reports the
  same table `key-status` does.

## [0.15.6]

- Fixed: `single provider key-status` reported a free-pool key as "keyed,
  unvalidated" forever even after it had served real, successful
  `single-pool` task requests — `pool_agent::execute` updated the ledger
  and bandit posterior on every outcome but never `pool_provider_keys`,
  which only the one-time best-effort probe at `add-free` time ever
  touched. A successful dispatch now marks the key valid; an
  authoritative auth rejection marks it invalid.
- Fixed: `single doctor` reported `single-pool` as "not installed" — the
  default `discover()` shells `which single-pool`, but `single-pool` is
  an HTTP-dispatch agent living inside `single-runtimed` that never
  shells a binary at all (see its own adapter doc comment). This made a
  build with the E28 pool engine fully working end-to-end read as
  missing a feature. `PoolAdapter::discover()` now always reports
  detected. Also fixed `key-status`'s help text, which still said
  cooldown/headroom were "n/a until the pool engine lands" — both are
  real, live per-provider state now, not placeholders.
- Docs: `docs/architecture.md` never mentioned the E28 free-provider pool
  (bandit, ledger, cooldown, backoff, degrade, handoff, client — ~2,500
  lines) at all; the phase history stopped at Growth Phase 8. Added a
  section documenting what it does and what's a real, working seam vs.
  genuinely unfinished (no live per-provider model-catalog feed; a
  completed pool task doesn't surface which provider/model/key served it
  on the task record itself, only in the outcomes ledger).

## [0.15.5]

- Fixed: `self_heal::infra::db_integrity`'s corruption restore copied the
  backup straight over the live `single.db` with a plain
  `std::fs::copy(&backup, &db_path)`. That's not atomic — this daemon's
  systemd unit caps its cgroup at `MemoryMax=6G` as a runaway backstop and
  has repeatedly SIGKILLed it under heavy concurrent-agent load (the same
  trigger 0.15.4's `db_backup` fix addressed). A kill landing mid-copy
  here left `single.db` itself truncated. Live-verification finding
  (2026-09-11, well after 0.15.4 shipped): `single approval resolve`
  failed with `disk I/O error: Error code 522: Unable to obtain number of
  requested bytes (file truncated?)` against the live db, and a `single
  daemon restart` was needed to trigger the startup self-heal pass before
  the CLI worked again — 0.15.4 only made *producing* a good backup
  crash-safe, not *restoring* one. Now copies the backup to a temp file
  in the same directory first, then atomically `rename()`s it over
  `db_path` — a kill mid-restore now only ever leaves a stray temp file,
  never a half-written live db.
- Fixed: `single approval resolve <id> --allow` (without `--remember`)
  never actually let the call it was raised for proceed. `task_run` and
  the other MCP-gated tools re-evaluate permission from scratch on every
  invocation (`evaluate_and_learn`), which only ever consulted static
  `permissions.toml` rules and *learned* preferences — a one-time
  resolution recorded no state anywhere `evaluate_and_learn` looked, so
  retrying the identical call after resolving its approval always raised
  a brand-new pending approval instead of proceeding; only `--remember`
  (a standing preference) ever actually unblocked anything. Live-
  verification finding (2026-09-11): five consecutive approve-then-retry
  cycles (ids 13→17) all re-escalated before `--remember` was used as a
  workaround. Added a `Used` approval status and a one-time-resolution
  lookup in `evaluate_and_learn`: a plain `--allow`/`--deny` now unblocks
  exactly the one retry it was meant for, then is marked spent so a
  further call escalates again rather than silently allowing forever
  (that's what `--remember` remains for).
- Fixed: custom agents defined with a `[run]` block in
  `~/.config/single/agents/*.toml` (one-shot `single-agent run --provider
  X --prompt "$2"` wrapper scripts, e.g. `single-cloudflare`,
  `single-typhoon`) reported LLM chat prose as their "version" in `single
  doctor` and `agent_list` — e.g. `single-cloudflare`'s version field held
  text explaining how to read a file called `example.txt`. Root cause:
  `discover()`'s generic `<command> --version` probe doesn't know these
  wrapper scripts blindly forward whatever they're given as `--prompt`,
  so probing them for a version fired a real, billed LLM call and the
  model's free-text reply was reported back as a version string.
  `GenericAdapter::discover()` now skips the version probe entirely for
  any custom agent with a `run` spec (existence via `which` only, version
  always `None`) instead of ever treating model output as a version.

## [0.15.4]

- Fixed: `self_heal::infra::db_backup` forced `PRAGMA
  wal_checkpoint(TRUNCATE)` before copying `single.db` to a timestamped
  backup. TRUNCATE mode truncates the WAL file to zero as part of the
  checkpoint, which removes SQLite's crash-safety net for the duration
  of that operation. Live-verification finding: this daemon's systemd
  unit caps its cgroup at `MemoryMax=6G` as a runaway backstop, and the
  daemon's own journal shows repeated `status=9/KILL` under heavy
  concurrent-agent load (91 tasks in one observed run) — including the
  same night `single.db` was found corrupt twice (`PRAGMA
  integrity_check` real page-level errors, then a second time not even
  parsing as a SQLite file at all). A SIGKILL landing mid-TRUNCATE is a
  textbook way to leave the main db file malformed. Switched to `PRAGMA
  wal_checkpoint(PASSIVE)`, which never truncates or blocks and does as
  much of the checkpoint as it safely can given concurrent readers —
  the resulting backup can be very slightly stale (a few of the most
  recent transactions might still be sitting in the WAL rather than
  applied to the main file) but is never at risk of being corrupted by
  an interrupted checkpoint. Combined with 0.15.3's restore-safety fix,
  this addresses both the corruption trigger and the unsafe recovery
  path found in the same incident.

## [0.15.3]

- Fixed: `self_heal::infra::db_integrity`'s corruption-restore swapped the
  live `single.db` file (a raw `fs::copy` from the newest backup) on
  every corruption finding, including from the periodic self-heal tick
  (every 300s) and `single doctor --fix` — both of which run while other
  daemon threads may hold their own open connections to that same file.
  Live-verification finding: this file-level swap racing against other
  open connections is itself a plausible corruption mechanism, not just
  a fix for one — confirmed live with a `single.db` that failed
  `PRAGMA integrity_check` with real page-level corruption
  (`btreeInitPage() returns error code 11`, duplicate page references)
  after a heavy overnight run with ~90 concurrent coordinator tasks.
  Restore-on-corruption is now only allowed from the daemon-startup
  self-heal pass (`run_pass_with_restore(.., allow_db_restore: true)`,
  before the coordinator tick loop ramps up); the periodic tick and
  `doctor --fix` now only detect and report corruption, so a human
  restarts the daemon to actually restore rather than the file being
  mutated out from under a live multi-connection process.

## [0.15.2]

- Fixed: a coordinator node pinned to a specific CLI agent (e.g. `grok`)
  that went rate-limited with no precise pool-internal recovery marker
  kept retrying that exact same exhausted agent forever —
  `handle_capacity_exhaustion` re-stamped the retry timer but never
  cleared the node's agent pin, so the goal cycled `capacity_wait` →
  `blocked` → self-heal `reeval_blocked_goals` resume → the same
  `capacity_wait` again, permanently wedged (confirmed live: a goal
  reported "waited 50.7h for capacity, still exhausted" with 4 other
  nodes done and 3 downstream nodes stuck pending behind it). Now, when
  no precise pool-internal ETA is present (i.e. a single CLI agent's own
  rate limit tripped, not the whole free-pool being exhausted), the pin
  is cleared so the next tick's `select_agent` routes past the
  just-exhausted agent onto the next available agent/provider, with a
  short 15s buffer instead of the old blind 5-minute hold on the same
  dead agent. Only once every candidate is genuinely exhausted (budget/
  wall-clock caps) does the goal still reach `Blocked`.

## [0.15.1]

- Fixed: `single provider add-free <id> --key ...` hardcoded `key_id` to
  `"default"` unconditionally. `pool_provider_keys` is keyed by
  `(platform, key_id)` and `pool_keys::add` upserts on that pair, so a
  *second* `add-free` call for a platform that already had a key (e.g.
  adding a key from a different account to grow the free-pool's real
  capacity) silently overwrote the first key instead of adding a second
  one — the underlying storage already fully supported multiple keys per
  platform, only this one call site never let you reach it.
  Live-verification finding while preparing to add a large batch of
  additional provider accounts. Added `single provider add-free
  --key-id <ID>` to rotate (replace) one specific existing key on
  purpose; omitting it now auto-picks a fresh, never-before-used id
  (`pool_keys::next_free_key_id`: first key stays `"default"` for
  backward compatibility, every one after that is `key2`, `key3`, ...)
  instead of reusing `"default"`.

## [0.15.0]

- Added: two concrete, well-scoped improvements toward the coordinator
  running independent agents "like a real team" (multiple agents/
  providers, each in their own git worktree, on independent parts of one
  goal) — found by a coordinator goal auditing SingleCLI's own
  `coordinator::scheduler`/`graph`/`single_core::worktree` against that
  aim:
  - **Sibling-node status visibility.** `TaskGraph::sibling_status`
    (read-only, pure) is now included in every dispatched node's prompt
    as a `SIBLING NODE STATUS` section — an agent working on one node can
    see what its siblings in the same goal are doing (id + status),
    without any ability to affect them. Coordinator state stays the sole
    source of truth; this is display-only, never consulted for
    scheduling.
  - **Opt-in merge confirmation.** A goal can now `single goal amend <id>
    auto-merge=true` to have the coordinator offer to merge a node's
    worktree branch once a `review`-kind node depending on it passes,
    instead of the merge staying entirely manual (`single worktree
    merge`). This does **not** merge automatically: `docs/architecture.md`
    states "branches are never auto-merged; that stays a human decision"
    as an explicit invariant, and a first pass at this feature (an
    upfront flag that merged immediately once review passed) was caught
    by this same goal's own review step as a real contradiction of that
    invariant — an upfront flag gives a human no visibility into the
    actual diff at the moment it lands, possibly much later. Fixed before
    landing: a passing review now only queues a
    `single_core::pending_merge` confirmation request (new `pending_merges`
    table); `single goal merge list` shows what's waiting, `single goal
    merge show <id>` prints the real diff (`git diff HEAD...branch`), and
    only `single goal merge confirm <id>` calls `worktree::merge`.
    `single goal merge reject <id>` declines without touching the repo.
    The invariant holds exactly as documented — this just gives a human a
    guided, low-friction way to exercise the decision instead of running
    `worktree diff`/`merge` by hand.

## [0.14.13]

- Fixed: the coordinator self-heal pass's `reroute_repeated_failures`
  substep (runs every 300s) only reset a `Failed`, retries-exhausted node
  back to `Pending` when it had a non-empty `agent` field — i.e. only a
  node explicitly pinned via `/agent`. Live-verification finding: a node
  dispatched through ordinary kind-based routing (`routing.toml`, the
  common case — no pin) has an *empty* `agent` field once the scheduler
  gives up on it, since the exhausted-retries path marks it `Failed`
  without ever setting `agent`. Such a node was invisible to this
  self-heal step and to `goal resume`/`amend` (both only re-tick
  `Pending` nodes) — it sat dead forever, needing a manual DB reset to
  recover, which is exactly how two real overnight goals got stuck
  tonight. Dropped the non-empty-agent requirement: any `Failed` node
  with `attempts >= 2` now gets rerouted, pinned or not.

## [0.14.12]

- Added: Pool tab in the TUI (`single` → Pool) shows a live pool-wide
  health snapshot — total/available/benched key counts and healthy ratio
  — fetched via `Request::PoolStatus` on a background thread so the
  event loop stays responsive. A second table lists per-platform
  free-pool key statuses (platform, label, status, last-error) fetched
  in the same round-trip. The tab refreshes automatically whenever it
  becomes visible and on every `r` keypress; both queries are
  deduplicated so a second press while a fetch is in flight is a no-op.

## [0.14.11]

- Fixed: `stream_goal` read and wrote the shared `AcpSession::last_event_id`
  every loop iteration. Two goals racing on the same ACP session (e.g. a
  `session/load` re-attach overlapping a fresh `session/prompt`) shared
  one mutable cursor — one goal's burst of events could advance it past
  events belonging to the other, silently dropping them from that goal's
  own stream. Each `stream_goal` call now reads its starting cursor once
  and tracks it locally for the rest of the call, only ever pushing the
  shared field forward as a high-water mark (never reading it back) so
  `session_load` re-attach can still skip already-replayed history.
- Fixed: a Zed-submitted goal defaulted `agent` to `single-pool` absent a
  `/agent` override — but `plan_goal` applies that override to every node
  in the planned graph, not just the planning step, so this sent a whole
  goal's task graph (code, test, review, everything) through
  `single-pool`, which has no real tool/file/command execution.
  Live-verification finding: this fabricated a plausible-but-fictional
  cargo test/clippy run when asked to actually run one. Now defaults to
  `None`, letting normal per-node-kind routing (`routing.toml`) pick a
  real tool-capable agent per step, same as every goal submitted via
  `single goal submit` already does. `/agent <name>` is unaffected.

## [0.14.10]

- Fixed: three real clippy warnings, verified against actual source
  before touching anything (`crates/single-runtime/src/`):
  - `task.rs`'s failure-memory `NewMemory` literal already set every
    field the struct defines — `..Default::default()` was dead code.
  - `orchestrate_graph.rs`'s `OnFailure` check duplicated
    `Vec::contains` with a manual `.iter().any(...)`.
  - `task.rs`'s per-agent average-duration calc duplicated
    `checked_div`'s zero-guard by hand.
  No behavior change, cosmetic/style only — surfaced by a coordinator
  goal auditing SingleCLI itself tonight; applied directly after
  independently confirming each one against the real file contents.

## [0.14.9]

- Fixed: `custom_agents::to_agent_definition` set `capabilities.tools`
  from `run_supported` (has a `[run]` block, i.e. can be invoked
  non-interactively) — nothing to do with actually executing tool calls.
  Every `single-agent run --provider X` wrapper custom agent
  (single-openrouter, single-typhoon, single-cerebras, etc.) claimed
  full tool capability despite being a one-shot prompt→completion
  wrapper with no tool-call protocol at all. Live-verification finding:
  one such agent leaked a literal `<tool_call>` token straight into its
  output when it tried to use a tool that was never going to be
  executed; another (the built-in `single-pool`, already correctly
  `tools: false`) fabricated a plausible-sounding but entirely fictional
  cargo test/clippy run when asked to verify real code. `tools` is now
  always `false` for `[run]`-based custom agents. Not yet consumed by
  the coordinator's own routing — that's a real follow-up (steering
  code/test/review-kind work away from non-tool agents automatically
  instead of relying on `routing.toml` being hand-curated correctly) —
  but `doctor`/`agent inspect` now report the truth, and any future
  scheduler logic built on this flag won't inherit the lie.

## [0.14.8]

- Fixed: `single provider add-free` silently accepted a bare token for a
  `Compound`-auth provider (currently just Cloudflare, which needs
  `account_id:token`) with no error and no validation — its empty
  `base_url` means the usual best-effort validate-URL probe never runs
  for it either, so a malformed key read as "keyed, unvalidated",
  indistinguishable from a good one, right up until a real dispatch hit
  `PoolError::AuthFailed` with nothing explaining why. Live-verification
  finding while wiring up a batch of new free-pool keys tonight. Added
  `free_pool::validate_key_shape`, called from `add-free` before the key
  is stored — a `Compound` provider now gets a clear error naming the
  expected `first_half:second_half` form instead of silent acceptance.

## [0.14.7]

- Fixed: a directory that was `git add`-ed while it happened to contain
  its own `.git` (no `git submodule add` ever run, no `.gitmodules`
  entry) gets recorded as a bare gitlink — git's automatic behavior for
  that case. `git worktree add` faithfully reproduces that as an *empty*
  directory in the new worktree, since it has no `.gitmodules` to know
  how to populate it. Live-verification finding: real, populated content
  sitting right next to the worktree was silently invisible to every
  isolated task touching that path — some correctly refused to fabricate
  work against an apparently-empty directory, others didn't, and hours of
  coordinator work across several goals were operating blind on this
  path without it being obvious why. `worktree::add` now symlinks any
  such orphaned gitlink's real directory into the new worktree in place
  of the empty stub (a real `.gitmodules`-registered submodule is left
  to git's own correct init/update handling).

## [0.14.6]

- Fixed: a goal blocked with `waited N.Nh for capacity, still exhausted`
  had no recovery path either — `max_capacity_wait_minutes` (default 720,
  i.e. 12h) is measured from the goal's `created_at` and never resets, so
  any goal simply older than the default hits it permanently on its next
  capacity wait, however brief, with `budget=N`/`minutes=N` both doing
  nothing for it. Live-verification finding while unblocking a real
  overnight goal. Added `single goal amend <id> capacity-minutes=N`
  (`goal::raise_capacity_wait_minutes`, mirroring the existing
  `capacity-budget=N`), and made the ACP raise-prompt and CLI hint pick
  this cap too when it's the one that actually tripped.

## [0.14.5]

- Fixed: a goal blocked on its wall-clock cap (`blocked: time budget
  spent: N min elapsed of M min cap`) had no working recovery path.
  `single goal amend <id> budget=N` only ever raised `max_dispatches`; a
  goal blocked on elapsed *time* re-blocked on the very next tick since
  `max_minutes` never moved. The Zed ACP "Raise budget and continue"
  prompt had the same gap — it always bumped the dispatch cap regardless
  of which cap actually tripped. Added `single goal amend <id> minutes=N`
  (`goal::raise_time_cap`) and made both the ACP prompt and the CLI's
  printed recovery hint pick the cap that matches the real block reason.

## [0.14.4]

- Fixed: the periodic self-heal pass's `zombie_rows` infra substep called
  `task::reconcile_orphaned_tasks` + `coordinator::scheduler::reconcile`
  on every tick (default every 300s), not just at genuine daemon startup.
  Both functions assume "every non-terminal row belongs to a process that
  no longer exists" — true exactly once, right after `serve()` binds the
  socket (where they already run, unconditionally, before this pass ever
  starts) — but false the rest of the daemon's life, since tasks execute
  on in-process threads with no separate liveness check. Live-verification
  finding: any task/node still genuinely running past one self-heal
  interval got killed and mislabeled "interrupted: single-runtimed
  restarted", then retried into the same wall over and over, blocking any
  goal with a step that legitimately runs long. Removed the periodic call;
  the one at true startup (`server.rs::serve`) already covers the real
  case.

## [0.14.3]

- Fixed: `scheduler::reconcile` (runs on daemon start, and periodically via
  self-heal) sent any coordinator node still `running` whose backing task
  died straight to `failed` — including a node simply killed mid-run by a
  daemon restart, not a genuine agent failure. That's terminal: `single
  goal resume` only re-ticks `pending` nodes, so a goal with downstream
  work depending on that node stayed `running` forever with nothing left
  to dispatch. Now runs the same `retry_decision` an ordinary
  crash/timeout goes through — bounced back to `pending` with `attempts +
  1` while retries remain, `failed` only once they're exhausted.

## [0.14.2]

- Fixed: `handle_blocked`'s `session/request_permission` sent a `toolCall`
  object without the required `toolCallId` field. Zed's ACP client rejected
  the request outright (`missing field toolCallId`), so a blocked goal's
  raise-budget/cancel prompt never reached the user — the session just
  emitted the blocked message and ended the turn silently. Now sets
  `toolCallId` to the request's own `srv-N` id.

## [0.14.1]

- Fixed: `claude`'s `home_requirement` was `Either` (isolated-home
  capable), but live testing found a byte-identical copy of
  `~/.claude.json` + `~/.claude/.credentials.json` (unexpired token,
  matching `userID`/`oauthAccount`) into an isolated home still fails
  "Not logged in" — confirmed directly against the `claude` binary
  itself (`HOME=<isolated> claude -p ...`), not just through SingleCLI's
  wrapper; the same files at the real `$HOME` work immediately.
  Reclassified `RealRequired`, matching `codex`/`cursor`'s existing
  real-environment-only auth. `single task run --agent claude` (and
  planning/dispatch through it) now automatically routes to the real
  home instead of silently failing every isolated-home attempt.

## [0.14.0]

Two real reliability bugs found live while running real epics through the
coordinator, both fixed with regression tests:

- Fixed: `resume_interrupted` (daemon startup) and `resume_goal` (`single
  goal resume`) flipped a goal to `running` *before* attempting to
  re-plan it. A failed re-plan left the goal permanently stuck at
  `running` with an empty graph and zero dispatches — invisible to `goal
  status` (`resume_interrupted`'s failure was only a `tracing::warn`;
  `resume_goal`'s propagated to the CLI caller but still left the goal
  itself silently stuck). Both now set the goal `blocked` with the real
  failure reason instead.
- Added: `single_core::ratelimit::looks_like_unavailable` broadens
  rate-limit detection to also recognize authentication failures ("not
  logged in", "please run /login", "unauthorized", ...). An agent whose
  CLI is on `$PATH` but not actually authenticated could previously burn
  every planning/dispatch attempt with no fallback ever triggering,
  since only a rate-limit signal excluded an agent or advanced the
  fallback chain. Wired into `task.rs`'s two `rate_limited` call sites.

## [0.13.2]

- Fixed: `single_core::redact`'s generic high-entropy detector was also
  flagging JSON/code fragments (e.g. a planner prompt's
  `{"id":"s1",...,"depends_on":[]}` schema example) as secret-shaped —
  found live via `single task run` with a real planner-style prompt.
  Tokens containing `{`, `}`, `[`, `]`, or `"` are now excluded from
  that check; `key=value`-style assignment detection is unaffected
  since it already extracts only the value span.

## [0.13.1]

- Fixed: `single_core::redact`'s generic high-entropy detector was
  flagging ordinary filesystem paths and filenames (e.g.
  `docs/queue/E03-vault-evolution/HANDOFF.md`) as secret-shaped,
  corrupting real prompt text before any agent saw it — found live
  submitting real orchestration work. Tokens containing `/` or ending in
  a short common file extension are now excluded from that check; a real
  secret sitting next to path-shaped text is still caught.

## [0.13.0]

- Added: `single secret promote-alias <alias> <name>` — moves a live
  redaction alias (still within its 3h TTL) into a properly named
  secret via the OS keychain, then deletes the pending-alias row.
  Prompts for confirmation unless `--yes` is given. New
  `single_core::redact::take_alias_value` and
  `Request::SecretPromoteAlias` back it.

## [0.12.0]

Zed + SingleCLI deep integration (E29): live secret redaction ahead of
every prompt SingleCLI dispatches, `single-pool` as the default Zed ACP
agent, a richer `/status` in place of a Zed status-bar icon (confirmed
unavailable in Zed's current extension API), and cross-session goal
dedup.

- Added: `single_core::redact` — heuristic secret detection (known
  vendor key prefixes, JWT shape, `key=`/`password=`-style assignments,
  generic high-entropy tokens with UUID/git-SHA negative guards) with a
  TTL'd, `age`-encrypted alias store (`{{REDACTED:<session>:N}}`), keyed
  by a per-machine master passphrase held in the OS keychain via
  `single_core::secrets`. Wired into every prompt-ingestion path
  (`single acp`'s `session/prompt`, `single goal submit`, `single loop`,
  `single task run`, `single serve --openai`) before the text is ever
  submitted to a goal or task; resolved back to plaintext only at
  `single-pool`'s outbound HTTP dispatch boundary — never in anything
  logged or persisted.
- Added: `single acp` sessions default to the `single-pool` agent
  (previously routing decided per-goal with no ACP-level default);
  overridable per session via `/agent <name>` / `/agent default`.
- Changed: `/status` in `single acp` now folds in `provider key-status`
  (auth/exhaustion state) alongside the existing goal/pool summary — the
  closest available substitute for a Zed taskbar/status-bar indicator,
  which Zed's extension API does not support as of this release (no
  general sidebar/panel API either; slash-command output is the only
  structured-info surface available to an extension).
- Added: `goal::find_overlapping` — a token-overlap check against active
  goals, run before every goal submission; an ask that's already in
  flight (from any session) returns the existing goal instead of
  starting a duplicate.

## [0.11.0]

The free-provider pool (E28): SingleCLI absorbs the ~40-provider free-LLM
landscape as a native, adaptive routing/quota engine sitting on top of the
E27 Coordinator — plus the self-healing and self-resuming autonomy that
makes long-running goals survive rate limits, daemon restarts, and its
own broken state without a human babysitting them.

- Added: `single_core::free_pool` — the vendored ~40-provider catalog
  (base URLs, auth shapes, published limits, per-provider quirks), plus
  `single provider list-free/add-free/sync-pool/key-status`. Five
  region-walled/payment-gated providers (`sail`, `modelscope`, `qianfan`,
  `volcengine`, `xfyun`) are registered but `enabled = false` by default
  with a clear reason string.
- Added: `single-runtime::pool` — the adaptive engine: a 4-D quota ledger
  (RPM/RPD/TPM/TPD, UTC-midnight reset, in-flight leases), a cooldown
  ladder with provenance (heuristic/authoritative/credit/tier) and an
  operator ceiling, provider-wide shared-pool gating, a Thompson-sampled
  bandit (reliability/speed/intelligence, 5 strategies), a degraded-mode
  health state machine, and a memory-only context-handoff on model
  switch.
- Added: `pool::client` — OpenAI-compat dispatch (33 providers) plus 10
  native wires (Gemini, Cohere, Cloudflare, Zhipu with domestic→global
  host reprobe, AI Horde's queue submit/poll, Sail, and a shared
  OpenAI-compat-subclass wire for ModelScope/Pollinations/ElectronHub/
  Experiential), with a retry budget, hedge-abort (never a health
  signal), and prose tool-call rescue.
- Added: `single-pool` — a native pooled agent usable as
  `single task run --agent single-pool` or as any coordinator node's
  agent; never shells a binary, dispatches straight to a provider's HTTP
  API via the ledger/bandit/cooldown engine.
- Added: auto-continue — a new `waiting_on_capacity` goal state. When
  every routable candidate is exhausted, a goal holds (with a reason and
  ETA, shown in `coordinator status` and streamed live over `single acp`)
  instead of failing, and resumes on its own once a cooldown lifts —
  bounded by a resume budget (`single goal amend <id> capacity-budget=N`
  raises it per goal).
- Added: self-resuming sessions — `coordinator::resume_interrupted()` on
  daemon start re-plans an interrupted `Planning` goal and un-pauses a
  cleanly-stopped one (`single daemon stop` now marks active goals
  `Paused` rather than leaving them `Running`, distinguishing a clean
  stop from a crash); `single goal resume <id>` for a manual re-tick;
  `single acp`'s `session/load` re-attaches its event stream for an
  in-flight goal.
- Added: self-heal (`single-runtime::self_heal`) — a three-category pass
  (`infra`/`coordinator`/`agent`, independently toggleable in
  `self_heal.toml`) on daemon start, its own timer, and `single doctor
  --fix`: stale-socket/zombie-row cleanup, corrupt-`*.toml` repair from
  backup, DB integrity check + periodic backup, undetected-agent pruning
  from `routing.toml`, absurd-`coordinator.toml`-value reset, long-stalled
  `Blocked`-goal re-evaluation, same-agent-repeated-failure rerouting,
  missing-agent auto-install, headless auth-repair reporting, and
  stale-pool-key auto-disable — every action logged to
  `self_heal_events`, every human edit within the last hour left alone.
- Added: 6 new agent config adapters (`cline`, `continue`, `roo`, `mimo`,
  `atomcode`, `deepseek-harness`) — MCP config sync only this iteration;
  not yet in the built-in agent registry (see Known limitations).
- Version: 0.10.0 → 0.11.0 (new agent, new subsystem, new subcommands).

**Known limitations / deferred to a follow-up:**
- The 6 new Part G adapters' config-file formats are best-effort (no live
  install of cline/continue/roo/mimo/atomcode/dsh was available to
  confirm against a real installed instance) and they carry no
  `AgentDefinition` registry entry yet — `single install-integrations`
  won't auto-discover them until a verified bootstrap-install command is
  added.
- The native wires' endpoints for ModelScope/Pollinations/ElectronHub/
  Experiential are likewise best-effort (Google/Cohere/Cloudflare
  confirmed against public docs; Zhipu/AI Horde matched against
  freellmapi's studied notes).
- No live signed provider-catalog feed, no per-provider real model
  lists (each provider is treated as one nominal model this iteration),
  no bandit community-seeded priors (`Beta(1,1)` uniform prior), no
  media-model routing (text-only `single-pool` this iteration).
- The §6.2 cooldown probe job is a simplified first cut (no persisted
  per-key next-probe scheduling / backoff-doubling yet).

## [0.10.0]

The Coordinator subsystem (`docs/queue/E27-singlecli-followups/02-coordinator-redesign.md`).
SingleCLI can now take a goal and drive a self-organising pool of agents:
a deterministic scheduler owns concurrency, the queue, per-goal budgets,
retries and restart reconciliation, while an LLM is consulted only to
plan, supervise on failure, and integrate. `single task run` and
`single orchestrate-*` are unchanged low-level escape hatches.

- Added: `crates/single-runtime/src/coordinator/` — four additive SQLite
  tables (`sessions`, `goals`, `graph_nodes`, `coordinator_events`),
  created idempotently and reconciled on daemon start so an interrupted
  run leaves no permanent zombie rows.
- Added: a pure `tick()` scheduler — ready-set from the dependency graph,
  critical-path-first admission, global + per-agent concurrency caps,
  a per-goal budget (default 25 dispatches or 60 minutes → the goal
  `blocked`, raised with `single goal amend <id> budget=N`), single-retry
  with agent advance, then a supervisor patch (capped at 5 per goal,
  then `blocked`).
- Added: routing — `~/.config/single/routing.toml` (per `kind` × `effort`
  capability-ranked agent lists) and `~/.config/single/coordinator.toml`
  (`max_parallel`, tick interval, budget caps). Nothing is pinned for
  reasoning; planner / supervisor / integrator route through the same
  table and pool-health filter as work, with dispatch-time fallback.
- Added: socket requests `SessionNew/List/Close`, `GoalSubmit/Status/
  List/Amend/Cancel`, `SessionEvents`, `CoordinatorStatus`, and the CLI
  mirrors `single session {new,list,close}`,
  `single goal {submit,status,list,amend,cancel}`,
  `single coordinator status`.
- Added: a scheduler tick-timer thread in `single-runtimed` that drives
  every active goal on `coordinator.toml`'s interval (default 5s).
- Fixed: the coordinator pool-health probe shelled out `<cmd> --version`
  for every registered agent on every scheduler tick and every
  `CoordinatorStatus` (~24s). It is now an in-process `$PATH` check;
  `single coordinator status` returns in ~10ms.
- Fixed: `single orchestrate-parallel --background` / `orchestrate-graph
  --background` printed `Relay (0 step(s)):` / `Graph (0 node(s)):`, which
  read as "nothing ran" — the daemon returns an empty batch immediately
  for a background run (each sub-task creates its own row on its own
  thread). Now: `dispatched N sub-task(s) in the background — poll
  \`single task list\``. Blocking runs were always fine; `--task` parsing
  was never broken (regression tests added).
- Fixed: syncing a provider into opencode under a name that also exists in
  models.dev's registry (`nvidia`, `openrouter`, `mistral`, …) made
  opencode merge that provider's entire registry catalog — 100+ models,
  EOL'd ones included — over the one or two SingleCLI actually curated
  (`opencode` 1.18.29: 1 declared → 101 listed). Such providers are now
  written under a `single-<name>` key, which isn't in models.dev, so only
  the declared models appear; use `opencode -m single-<name>/<id>`.
- Added: `single serve --openai` — a local OpenAI-compatible HTTP proxy
  over the pool (`GET /v1/models`, `POST /v1/chat/completions`, non-stream
  + single-chunk SSE). Each request flattens the chat messages to a
  prompt, picks an agent (`model` name if it's a real agent, else routed
  like `code/quick`, or `--agent`), and runs one `task run --allow-fallback`
  — so a 429 hops the fallback chain. Point Zed's
  `language_models.openai_compatible` at `http://127.0.0.1:8765/v1`. No
  new dependency (hand-rolled HTTP/1.1 on `std::net`). Requires a bearer
  token (generated and printed unless `--api-key` is given), validates the
  `Host` header, refuses non-loopback binds without `--allow-remote`, and
  caps request size and connection time.
- Added: `single loop <goal> [--agent X] [--max-iters N]` — keep one
  agent iterating until it replies with a line containing only `DONE`,
  or the cap is hit. Sugar over the coordinator's new `careful` goal
  mode: a single-node goal, no LLM planner, re-dispatched with its
  previous output appended each iteration (`GoalSubmit.mode = "careful"`,
  `GoalSubmit.agent` pins the node). Progress shows in `single goal
  status` / the ACP panel like any goal.
- Added: per-task token accounting. Every finished task records
  `prompt_tokens` / `completion_tokens` (`tokens_estimated` flags a
  parse-of-output or chars/4 fallback vs. a real agent-reported count).
  `single task run --usage-json` and `coordinator.toml`'s
  `usage_json_agents` run an agent in a usage-reporting mode where one
  exists — currently `claude --output-format json`. `single goal status`
  and the ACP turn summary now show `tokens ~N in / ~N out`.
- Added: resumable ACP sessions. The `single acp` session id is now the
  coordinator session id, so `session/load` rebinds to a prior thread and
  replays its events after a fresh `single acp` start (Zed panel reload).
- Added: `single acp` — a native Agent Client Protocol stdio bridge
  (newline-delimited JSON-RPC 2.0, protocol v1) for Zed's agent panel.
  It burns no agent itself: `/`-commands and status-y prompts answer
  from the socket, every other prompt becomes a `GoalSubmit` and the
  coordinator's events stream back as ACP `session/update`s. A blocked
  goal asks the human via `session/request_permission`. Replaces the
  Python prototype `nbr-workspace/tools/single-acp`.

## [0.9.6]

Cleanup pass from `docs/queue/E27-singlecli-followups/singlecli-followups.md`.
Kept as a patch release; the additive flags below are conveniences.

- Fixed: `single provider add` on an already-registered provider rebuilt
  the whole entry from the flags passed, silently dropping an existing
  `base_url` or `models` list. It now merges — omitted fields keep their
  value, `models` upsert by id — so `add` is a safe way to add one model
  to a provider (there is still no separate `provider update`).
- Fixed: `--allow-fallback` did not fail over on an Anthropic HTTP 529
  `overloaded_error`; `529` and `overloaded` are now detected signals.
- Added: `--json` on `single worktree diff` / `single worktree merge`
  (the only list-ish commands that lacked it), and a post-merge hint
  pointing at `single task cleanup <id>` since the worktree and branch
  outlive the merge.
- Added: `single provider inspect` shows the provider's `models` list,
  which it persisted but never displayed.
- Added: `SINGLE_MCP_IDLE_TIMEOUT_SECS` / `SINGLE_MCP_SWEEP_INTERVAL_SECS`
  override `single-mcp`'s 600 s / 60 s defaults — mainly so the
  lazy-spawn / reuse / idle-eviction cycle is testable without a
  ten-minute wait. (Verified end-to-end: discovery spawns nothing, first
  `invoke_mcp` spawns one child, a second reuses its PID, idle eviction
  kills the OS process, a later call respawns.)
- Internal: reuse one error binding in `task::execute`'s failure arm; use
  `Context::find_agent` instead of an inline registry scan; correct
  `maybe_fail_over`'s doc comment.
- Docs: backfilled the missing `## [0.6.0]` changelog section.

## [0.9.5]

- Fixed: `single doctor` (and the `status` / `agent list` fan-out) drove
  `single-runtimed` to ~2.6 GB. The ~30 agent probes ran with no
  concurrency cap — each faulting in a full node/bun runtime for
  `--version` — a `try_wait` error left the child unreaped, and captured
  output was unbounded. Now a global 4-permit gate around every probe, a
  reap on the error path, a 64 KiB cap per probe, and a second concurrent
  `doctor` is refused with a clear error.
- Fixed: read-only commands (`status`, `task list`, `usage`,
  `--background` dispatch) hung for minutes while a `task run` executed.
  The daemon ran each request handler inline on the tokio worker, so a
  blocking agent run pinned that worker; handlers now run on the blocking
  pool and a panicking handler returns a structured error.
- Fixed: the daemon's agent detection depended on its launcher's pinned
  `PATH` being complete, so a new agent bin dir or a bumped nvm Node
  version silently read as "not installed". `single-runtimed` now appends
  the standard install locations that exist on disk at startup, keeping
  inherited entries first.

## [0.9.4]

- Fixed: `mistral-vibe` had a registry entry but no adapter, so every run
  fell through to `GenericAdapter` and failed with "no [run] mode
  defined". Added `MistralVibeAdapter` (`vibe -p <prompt> --auto-approve
  --output text --trust`).
- Fixed: `kilocode` passed `kilo run`'s old `--auto` / `--dir` flags,
  which kilo 7.x rejects (hanging the run). Now `kilo run -- <prompt>`
  (kilo run auto-approves by default).
- Fixed: `copy_dir_recursive` unwrapped every `fs::copy`, so one
  unreadable entry in a real agent config dir (an IDE socket, a
  FIFO-backed lock, a file deleted mid-copy) aborted the whole
  isolated-home bootstrap. Now best-effort per file, special types
  skipped.

## [0.9.3]

- Removed: the `gemini` agent and the `gemini` provider preset. Google
  discontinued the standalone Gemini CLI (its agent role is covered by
  `agy`/Antigravity) and its API is now Google AI Studio — the provider
  preset is replaced by `google` (`GOOGLE_API_KEY`, same
  OpenAI-compatible `v1beta/openai` endpoint). `qwen-code` is unaffected.

## [0.9.2]

- Fixed: `single-runtimed` now reconciles orphaned tasks on startup. A
  daemon killed mid-run left its rows stuck `running`/`created` forever
  with no way to reap them; a freshly started daemon sweeps every
  non-terminal row to `failed` (summary "interrupted") and records a
  `task.reconciled` event.
- Added: `--force` on `single task cancel` and `single task cleanup` to
  clear a row wedged non-terminal with no live process behind it.
- Fixed: task-failure memories (`task #N failed`) are written at `task`
  scope instead of `project` and are skipped by the task-run context
  preamble, so they no longer accumulate as noise in every agent's
  prompt. Still queryable via `single memory list --scope task`.
- Fixed: `single task list` truncates each description to its first line,
  clipped, so multi-line agent prompts no longer break the table.

## [0.9.0]

- Added: `crates/single-native-agent` — a native, in-process coding agent
  (binary `single-agent`) that talks directly to any provider registered
  in `providers.toml` via its OpenAI-compatible chat-completions endpoint,
  no third-party CLI required. Ships a minimal tool loop (`read_file`,
  `write_file`, `run_shell`) scoped to `--cwd` with path-traversal and
  absolute-path escapes rejected before any filesystem access, and a
  `call_mcp` tool that spawns `single-mcp` once per run and proxies
  through to its `invoke_mcp` tool — the same lazy MCP-gateway access
  codex/opencode already get. Wired into the agent registry as
  `single-agent`, dispatchable via `single task run --agent single-agent`.
  Provider/model selection currently reads `SINGLE_AGENT_PROVIDER`/
  `SINGLE_AGENT_MODEL` env vars (default `opencode-zen`/
  `laguna-s-2.1-free`) — the standard adapter `run_prompt` signature has
  no field for it yet.
- Added: Kilo Code (`kilocode`) to the agent registry — a real,
  actively-maintained open-source fork of OpenCode
  (github.com/Kilo-Org/kilocode) with its own standalone CLI and free
  models available without an API key.
- Fixed: `single task run`'s failure summary picked whichever line came
  first even when it was pure decoration (box-drawing separators some
  TUI-styled agents print around their real output) — now skips any line
  with no alphanumeric content.
- Fixed: `qwen-code`'s `login()` was unimplemented (`qwen auth` is
  genuinely removed upstream) — now launches `qwen` interactively in its
  isolated home so a user can reach the in-TUI `/auth` command
  themselves. Note: Qwen's free OAuth tier was discontinued 2026-04-15
  upstream — this reaches the right screen, it doesn't restore free auth.
- Fixed: the TUI's Agents-tab status dot was colored solely from whether
  the agent's binary was detected on disk, so a detected-but-not-
  authenticated agent rendered an identical green dot to a fully working
  one. Now factors in auth state: green only when confirmed authenticated,
  yellow when detected-but-unauthenticated or auth state is unconfirmed.
- Fixed: `amp`'s adapter passed `-x -- "<prompt>"`, which strips the
  message from `-x`/`--execute` entirely — that flag takes its value
  inline (`-x, --execute [message]`), not as a following positional, so
  `--` (added to dodge task_run's `---`-prefixed memory preamble) ended
  option parsing before the message could bind. Every real `amp` call
  failed with a parse error regardless of auth. Fixed to
  `--execute=<prompt>` as one token.
- Fixed: `single-native-agent` sent `content: null` on assistant messages
  alongside `tool_calls` — valid per the OpenAI spec, but Cloudflare
  Workers AI's "OpenAI-compatible" endpoint rejected it outright with a
  schema-validation error. Now sends an empty string instead.
- Docs: registered ~40 additional free/freemium LLM providers in
  `providers.toml` (metadata only — Cerebras, Groq-family aggregators,
  OpenRouter, Nebius, Hyperbolic, ModelScope, and others), cross-checked
  against independent verified sources rather than any single vendor's
  own marketing claims.

## [0.8.1]

- Fixed: `single task run`'s failure summary ignored stderr entirely, so
  any agent CLI that wrote its real error to stderr (codex, opencode, and
  most others) surfaced as the useless `exit code Some(1), no output` —
  the actual error sat unread in the task artifact file. `summarize()` now
  falls back to stderr when stdout has nothing useful, and prefers the
  last `Error`/`ERROR`-marked line over whichever line happens to come
  first, since CLIs commonly print startup/banner noise before their real
  failure.

## [0.8.0]

- Added: `single provider add` gains a repeatable `--model <id>:<name>`
  flag, and `single provider sync` can now write a custom/local provider
  into opencode's own `opencode.jsonc` `provider` key (via
  `@ai-sdk/openai-compatible`) — previously, syncing a provider only
  injected its API key as an env var into an agent's process, which got
  the credential to opencode but never told opencode's model picker the
  provider existed. `provider_sync::sync`'s API-key value is never
  written into opencode's config — only an `{env:VAR}` reference, matching
  how the actual secret already reaches opencode's process.

## [0.7.0]

- Added: rate-limit detection (`single_core::ratelimit::looks_like_rate_limit`)
  now runs unconditionally on every task completion, not just when
  `--allow-fallback` is set, and is surfaced as `TaskRecord.rate_limited`.
- Added: `HomeRequirement` (whether an agent needs `real_home: true`,
  breaks under it, works either way, or is unverified) and
  `max_concurrency` are now queryable per agent via `agent_list`/`agent
  inspect`, closing a gap where this was previously tribal knowledge.
- Added: `CapabilityFlags.non_interactive_run` flags agents (currently
  only `codebuff`) with no non-interactive run mode at all, checkable
  before dispatching instead of discovering it via a failed run.
- Added: a per-agent concurrency guard serializes agents with a
  `max_concurrency` limit (currently `opencode`, whose own session
  SQLite lock can't handle two simultaneous instances) instead of
  letting a second concurrent run crash.
- Added: `single worktree diff`/`merge` CLI commands and
  `worktree_merge_preview`/`worktree_merge_apply` MCP tools give an
  assisted (never automatic) way to merge a worktree-isolated task's
  branch back — branches still aren't auto-merged, this just removes
  the manual `git` juggling once you've decided to.
- Fixed: the concurrency guard above originally held its slot through
  `maybe_fail_over`'s fallback path, which could self-deadlock if a
  fallback chain repeated the same concurrency-limited agent — the slot
  is now released as soon as the agent's subprocess actually finishes.

## [0.6.0]

Backfilled 2026-09-06 — this section was missing (the `0.7.0` and
`0.8.0` work landed above it without one). Contents reconstructed from
the `0.5.0..0.6.0` commit range.

- Added: `singlecli-mcp` — an MCP server exposing SingleCLI's own
  commands as tools (`task_run`, `orchestrate_run`,
  `orchestrate_parallel_run`, `orchestrate_graph_run`, plus agent,
  memory, and provider tools), synced alongside the MCP registry/gateway.
- Added: `single-lsp` — a dynamic LSP proxy that spawns the real language
  server for an open file's extension on first use, with a generated
  Claude Code plugin manifest driven by the LSP registry (filename-keyed
  presets that Claude Code's manifest schema rejects are skipped).
- Added: `--real-home` on `provider sync`, `plugin sync`,
  `install-integrations`, and `uninstall-integrations` — write into the
  actual `$HOME` instead of an isolated copy.
- Added: task lifecycle hooks (`single task-hook add/list/remove/test`).
- Added: `single web` pattern-library browsing (`single web patterns
  list|search`) via the new `single-web` crate.
- Fixed: `singlecli-mcp` now canonicalizes `cwd` client-side before
  sending requests.
- Fixed: `plugin sync` backs up `claude`'s `settings.json` before it
  writes into a real home.

## [0.5.0]

- Added: `task run --allow-fallback` fails over to another agent/account
  when a run hits a detected rate limit — `single fallback set
  <agent[:account]>...` saves the ordered chain to try. Detection honors
  an account already marked `rate_limited` (`single account set-status`)
  and, best-effort, generic rate-limit signals in a failed run's output.
  A follow-up task is created and linked, never a silent retry.
- Fixed: the TUI blocked on every tab's data before drawing anything —
  on a cold daemon this could take several seconds with a blank
  terminal. `App::new()` now draws the tab shell immediately with an
  animated loading spinner while the first fetch runs in the background;
  every later `[r]` refresh is non-blocking too.
- Fixed: `Status` and `AgentList` each independently re-discovered every
  registered agent when fired together (as the TUI's startup fetch
  does), doubling the subprocess spawns needed on a cold cache — now
  single-flighted per agent.
- Fixed: MCP, LSP, Plugins, Tools, Providers, Accounts, and the Tasks
  tab's workspace list had no column headers, unlike Agents/Tasks —
  every list in the TUI is a proper table now.

## [0.4.0]

- Added: tasks are now grouped by workspace (a stable identity — git
  remote URL, or root commit hash with no remote — that survives the
  project directory moving) instead of one flat list; the TUI's Tasks tab
  drills from a workspace list into that workspace's tasks. New `single
  workspace list`.
- Added: `single mcp enable-all` / `single lsp enable-all` bulk-enable
  every registered server, skipping only MCP servers still missing a
  required secret.
- Added: MCP gateway mode is now backed by real idle eviction — a lazily
  spawned server unused for 10 minutes is dropped instead of staying
  alive for the whole agent session. The TUI's MCP tab shows gateway
  status and toggles it with `[g]`.
- Fixed: every scrollable list/table in the TUI (Agents, Tasks, MCP, LSP,
  Plugins, Tools, Providers, Accounts) silently truncated past the
  visible height with no way to scroll further; they now show a proper
  scroll window and position indicator.
- Fixed: the daemon re-ran full agent discovery (`which`/`--version` per
  agent) and re-parsed the entire MCP/LSP/plugin/provider registries on
  every single request instead of once per process, making `single`
  slow to open — both are now cached/parallelized.

## [0.3.3]

- Fixed: encrypted backup restore rejected path traversal.

## [0.3.2]

- Fixed: malformed version tags rejected during update comparison.

## [0.3.0]

- Added: dependency-graph task orchestration and orchestrator modes.
- Fixed: OpenCode and Grok tasks silently no-op'd without approval flags.
- Fixed: Codex tasks ran in a read-only sandbox and could never write files.
- Fixed: `--background` with no daemon running now warns instead of
  silently no-op'ing.
- Fixed: OpenCode LSP sync omitted a now-required `extensions` field.

## [0.1.29]

- Added: named account capture/switch for Cursor, Copilot, Grok, Codebuff.

## [0.1.28]

- Added: Providers tab shows only configured providers, not every preset.

## [0.1.27]

- Fixed: `docker exec -e KEY=value` leaked secret values into process
  argv.

## [0.1.26]

- Added: inject stored provider keys into agent runs as environment
  variables.

## [0.1.25]

- Fixed: `SecretStore::list()` found nothing even when secrets were
  genuinely stored.

## [0.1.24]

- Fixed: the Usage tab's fetch could block the whole TUI event loop.

## [0.1.23]

- Added: Usage and Backup TUI pages.

## [0.1.22]

- Fixed: dash-prefixed prompts broke non-interactive runs for nearly
  every agent.

## [0.1.21]

- Fixed: gateway-mode MCP entries accumulated instead of replacing;
  `cursor-agent` needed `--trust`.

## [0.1.20]

- Fixed: honest account-capture error message, dropped a dead-end retry
  hint.

## [0.1.19]

- Added: daemon restart command, real adapters for 8 more agents, an
  honest `agy` auth hint.

## [0.1.18]

- Fixed: preset visibility; expanded tool/agent/provider catalogs.

## [0.1.17]

- Added: real parallel agent execution, knowledge-graph shared context,
  live agent-to-agent messaging.

## [0.1.16]

- Added: 50+ MCP presets, 100+ LSP presets, a plugin marketplace catalog,
  `doctor` coverage for semantic search.

## [0.1.15]

- Fixed: removed a real-home fallback from auth detection and account
  capture.

## [0.1.14]

- Fixed: propagate the real config dir into HOME-swapped child processes.

## [0.1.13]

- Added: real mid-run permission interception.

## [0.1.12]

- Added: permission enforcement and learned preferences.

## [0.1.11]

- Added: opt-in Docker execution backend.

## [0.1.10]

- Added: `single-mcp` dynamic gateway.

## [0.1.9]

- Added: document ingestion (OCR) and semantic memory search.

## [0.1.8]

- Added: memory context injection, agent notes, more presets.

## [0.1.7]

- Fixed: self-update `ETXTBSY` on cross-device installs.

## [0.1.6]

- Fixed: account capture / auth detection.

## [0.1.5]

- Maintenance release.

## [0.1.4]

- Added: `--real-home` opt-out for system-configuration tasks.
- Added: live task output in the TUI, with per-agent detail for
  `orchestrate`.

## [0.1.3]

- Added: Cursor CLI, Aider, and Goose as built-in agents; GitHub Copilot
  CLI, Kiro CLI, and Sourcegraph Cody as three more (Windsurf investigated
  and left out — see `docs/install-methods.md`).
- Added: `single agent login` for real interactive auth into isolated
  homes; every agent run isolated to a SingleCLI-managed home.
- Added: MCP/LSP preset catalogs, full TUI config surface (MCP/LSP/
  Plugins/Tools tabs), plugin management, multi-account isolation, in-TUI
  task creation.
- Added: LSP sync into OpenCode, skills synced into Claude Code, expanded
  tool registry.
- Docs: plugins, multi-account concurrency, LSP/skills sync, presets.

## [0.1.2]

- Fixed: Linux release binaries built against musl, not glibc (a glibc
  build failed to start on a real Debian 12 test machine).
- Added: self-update (`single update`) and a nightly release channel.
- Added: multi-agent orchestration (sequential relay).
- Added: "learn from errors" — task failures automatically become memory
  entries.
- Added: provider presets (including NVIDIA) and in-TUI provider
  configuration.
- Added: MCP/LSP/tool registries seeded with real defaults instead of a
  near-empty list.
- Added: TUI rewritten as a tabbed control center with in-app agent
  install.
- Added: cross-platform release workflow and curl installer.
- Added: pluggable Redis working-memory and Qdrant vector-store backends.
- Added: SQLite-backed knowledge graph memory.
- Added: LLM provider registry with real config sync (Phase 6, partial).
- Added: declarative custom agent adapters (Phase 5).
- Added: multi-account credential switching (`single account ...`).
- Added: Phase 4 single-agent task execution with worktree isolation.
- Added: Phase 3 memory subsystem and project context resolver.
- Added: Phase 2 shared-capability registries (MCP CRUD, LSP, tools,
  secrets, permissions, skills).

## [0.1.0]

Initial public release.

- Added: agent registry for Claude Code, Codex, OpenCode, Antigravity
  (`agy`), and Perplexity (`pplx`), with real detection/version/capability
  observation.
- Added: unified MCP registry synced into each agent's native config
  format, with backups.
- Added: vendor-verified bootstrap installers (`single setup`).
- Added: headless runtime daemon over a Unix socket with a CLI and TUI
  client.
- Added: `single` CLI and TUI dashboard.
- Docs: architecture, ADR, and install-methods documentation; README and
  MIT license.

[Unreleased]: https://github.com/naviNBRuas/SingleCLI/compare/v0.3.3...HEAD
[0.3.3]: https://github.com/naviNBRuas/SingleCLI/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/naviNBRuas/SingleCLI/compare/v0.3.0...v0.3.2
[0.3.0]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.29...v0.3.0
[0.1.29]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.28...v0.1.29
[0.1.28]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.27...v0.1.28
[0.1.27]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.26...v0.1.27
[0.1.26]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.25...v0.1.26
[0.1.25]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.24...v0.1.25
[0.1.24]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.23...v0.1.24
[0.1.23]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.22...v0.1.23
[0.1.22]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.21...v0.1.22
[0.1.21]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.20...v0.1.21
[0.1.20]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.19...v0.1.20
[0.1.19]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.18...v0.1.19
[0.1.18]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/naviNBRuas/SingleCLI/compare/v0.1.0...v0.1.2
[0.1.0]: https://github.com/naviNBRuas/SingleCLI/releases/tag/v0.1.0
