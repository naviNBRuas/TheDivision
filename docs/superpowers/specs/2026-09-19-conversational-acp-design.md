# Conversational divisi: design

Status: approved 2026-09-19. Stage 1 (core and `divisi chat`) implemented; stages 2 to 4 in progress.
Successor step: one implementation plan per delivery stage (writing-plans).
Builds on: the divisi rename (`rebrand/divisi`, 0.24.0). Branch from it, not from `main`.

## 1. Goal

Talk to your pool in plain language from Zed, the TUI and the GNOME notch, and have divisi
create goals, answer questions about the pool and control running work, without
memorising commands. Today `divisi acp` is a bridge, not an agent: it runs `/`-commands,
answers status questions with a keyword heuristic (`looks_like_status_query`) and turns
everything else into a goal. Nothing understands intent, and only Zed can be spoken to.

**Non-goals:** a general-purpose chatbot; voice; changing how the coordinator plans or
dispatches goals; new provider or agent pinning (routing stays dynamic with fallback).

## 2. Decisions

| Question | Decision |
|---|---|
| Autonomy | Act freely; confirm risky actions (section 5). |
| Understanding | Rules first, a routed pool model for everything ambiguous. |
| Continuity | One shared conversation across surfaces; Zed threads can branch off. |
| Notch | A Chat tab in the card. |
| Architecture | Daemon-owned assistant. Surfaces are thin clients. |

## 3. Architecture

The conversation, intent handling and confirmations live in `divisid` and are stored in
`divisi.db` beside goals. Zed, the TUI, the notch and a new `divisi chat` CLI talk to it over
the existing Unix socket. No second process, no second store.

New code lives in `crates/divisi-runtime/src/assistant/` (`mod.rs`, `rules.rs`,
`model.rs`, `gate.rs`, `actions.rs`) with the request and event types in
`divisi-protocol`.

## 4. Conversation model and protocol

**One shared thread is a well-known coordinator session.** The daemon creates it on first
use and stores its id under the settings key `chat.main_session`. Chat messages are rows in
`coordinator_events` (append-only, per session) with new `kind` values:

| kind | body (JSON) |
|---|---|
| `chat_user` | `{ "text", "surface" }`, surface is `zed`, `tui`, `notch` or `cli` |
| `chat_assistant` | `{ "text", "intent", "goal_ids": [], "degraded": false }` |
| `chat_confirm` | `{ "approval_id", "summary", "action", "expires_at" }` |
| `chat_result` | `{ "approval_id", "outcome": "approved"\|"denied"\|"expired" }` |

Goals created from chat keep their existing progress events in the same session, so the
thread reads as one story: request, goal created, goal progress, goal done.

**Requests.** Event kinds are snake_case like the existing ones (`chat_user`, and so on). Three
requests are added to `divisi-protocol`:

- `ChatSend { session: Option<String>, text: String, surface: String }`. `session: None`
  means the shared thread. Returns the events appended before the call returns (the
  `chat.user` row and either an immediate `chat.assistant` row or a `chat.confirm` row).
  Model-backed replies arrive later as further events on the same session.
- `ChatHistory { session: Option<String>, since_event_id: i64 }`. Returns every event in a
  conversation after an id (chat lines and the goals' own progress). `session: None` is the
  shared thread, which is how a client learns its id before it has said anything.
- `ChatConfirm { approval_id: i64, allow: bool, remember: bool }`. A thin wrapper over
  `ApprovalResolve` that also executes the stored action exactly once.

**Sessions and Zed.** Each ACP thread still maps to its own coordinator session, so Zed's
thread model is unchanged. The TUI and notch open the shared thread by default and can
attach to any session.

## 5. Intent layer

One structured value is produced for every message:

```
Intent = Status | Usage | PoolQuery
       | GoalCreate { text, mode }
       | GoalControl { goal_id, action }      // action: resume (also "retry") | amend | cancel
       | MergeApply { goal_id }
       | Config { what }                      // provider | key | account | mcp | plugin | daemon
       | Question { text }
       | Clarify { what }
```

**Order of resolution.**

1. **Rules** (`rules.rs`, deterministic, no cost). `/` commands, and a small grammar for
   status, usage, pool queries and goal control ("how much have I used", "cancel goal X",
   "retry X"). The current keyword heuristics move here and are extended.
2. **Model fallback** (`model.rs`) for anything the rules do not recognise. It goes
   through the coordinator's existing brain-role routing, so the provider is chosen
   dynamically with fallback and is never pinned. Input: the last 20 thread messages and a
   compact snapshot (active goals, pool state), capped at about 2,000 tokens. Output must
   be one `Intent` as JSON. Timeout 20 seconds.
3. **Degradation.** If the model output is not valid JSON, retry once. If that fails or
   the pool is exhausted, use rules only: status, usage and control requests still work,
   and a free-form request becomes a `GoalCreate` (today's behaviour) with the reply
   marked `degraded: true` and saying it is in rules-only mode.

`Question` is answered from daemon state where possible (status, usage, goals); otherwise
it is a plain Q&A model turn that creates no goal.

## 6. Risk gate

The gate is code applied to the parsed `Intent` in `gate.rs`. A model can only propose an
intent; nothing executes until the gate has classified it. This matters because the thread
holds untrusted text (goal outputs), so a prompt-injected instruction still has to pass the
gate.

**Runs immediately:** `Status`, `Usage`, `PoolQuery`, `Question`, `Clarify`, `GoalCreate`
(unless a rule below applies), and `GoalControl` with `retry`, `resume` or `amend`.

**Requires confirmation:**

| Intent | Reason |
|---|---|
| `GoalControl { cancel }` | destroys in-flight work |
| `MergeApply` | lands worktree changes on a real branch |
| `Config { … }` | changes how everything runs |
| `GoalCreate` whose text contains a risky verb: `push`, `publish`, `deploy`, `release`, `delete`, `drop`, `force`, `wipe`, `destroy` | deterministic text screen |
| a goal whose plan exceeds the fan-out cap (default 12 nodes) | the coordinator pauses it for approval; the assistant then posts the confirmation |

**Mechanics.** The gate calls `preferences::evaluate_and_learn` with a resource such as
`chat:goal.cancel` and stores the full action as the approval's context. That produces a
`chat.confirm` event and nothing runs until `ChatConfirm` allows it.

- **First answer wins.** Every surface shows the same pending confirmation and updates
  when one answers. The action runs exactly once (a second `ChatConfirm` on a resolved
  approval is a no-op that returns the recorded outcome).
- **Expiry.** Unanswered confirmations expire after 30 minutes and are recorded as
  `chat.result` with `outcome: "expired"`.
- **Remember.** `remember` is honoured only for `GoalControl { cancel }` and risky-verb
  `GoalCreate`. It is rejected for `MergeApply` and `Config`.
- **Planning must verify** that the coordinator can pause a goal on plan size using its
  existing `paused` status. If it cannot, the fan-out rule is dropped from stage 1 and
  recorded as a follow-up; the other four rules do not depend on it.

**Configuration** (`config.toml`, section `[chat]`): `confirm_expiry_secs = 1800`,
`fanout_cap = 12`, `default_mode = "auto"` (goal mode when a message does not say), and
`risky_verbs` (the list above as the default).

**Implemented behaviour worth knowing:**
- Goal ids (`goal_…`) are protected from secret redaction, since they look random enough to be
  mistaken for keys; real secrets are still replaced before anything is logged.
- In rules-only mode a message that opens with a control verb (cancel, stop, retry, merge, …)
  and has no goal id asks for the id instead of becoming a goal.
- An approval acted on immediately is marked used (`preferences::mark_used`), because the
  approval store otherwise treats a resolved approval as a one-time grant for the next request.
- Approving a `Config` intent runs nothing yet: the reply says so and points at the CLI.
- `ChatConfirm` only resolves approvals whose resource starts with `chat:`.

## 7. Surfaces

- **CLI.** `divisi chat` opens a REPL on the shared thread. `divisi chat send "…"` and
  `divisi chat tail [--after N]` script it; `--json` is used by the notch. `divisi chat
  confirm <approval_id> --allow|--deny`. This is the first way to exercise the stack.
- **Zed (ACP).** `/` commands work as today. Plain text calls `ChatSend` instead of the
  keyword heuristic. `chat.assistant` events stream as message chunks; `chat.confirm`
  becomes `session/request_permission`; goal progress events are unchanged. Modes
  (auto, plan, careful, dry) keep working.
- **TUI.** A new Chat tab shows the thread with an input line. The `/` glyph from the
  brand spec prefixes the prompt. Confirmations render inline with y/n.
- **Notch.** A Chat tab in the card: a scrolling thread and a text entry. It calls
  `divisi chat send/tail --json` as subprocesses, the same pattern the extension already
  uses for snapshots. Long replies are truncated with an "open in TUI or Zed" hint.

**Riskiest piece:** keyboard focus for a text entry inside GNOME Shell chrome (a modal
grab is needed, and Esc-to-dismiss must keep working). Stage 4 starts with a headless
gnome-shell spike. If focus proves unworkable, the fallback is a one-line quick-command
box that accepts input only while the card is focused.

## 8. Errors

- Daemon down: existing "unreachable" messaging on every surface.
- Model unavailable or invalid output: section 5 degradation.
- Unknown goal id in a control request: a `chat.assistant` reply naming the ids that exist.
- A confirmation that outlives the daemon restarts as pending until it expires.

## 9. Testing

- Rules: table-driven phrase to `Intent` tests, including the migrated status heuristics.
- Model fallback: a fake router trait returning canned JSON, including malformed output and
  prompt-injection text in goal outputs.
- Gate: a test per risky intent proving it yields a `chat.confirm` and no action.
- Confirmations: state-machine tests for double approve, expiry, remember rules and
  cross-surface answers.
- Surfaces: the scripted ACP client (`crates/divisi-cli/tests/acp_scripted.rs`) extended,
  TUI render tests, and the headless-gnome-shell recipe for the notch.
- All tests run with `DIVISI_CONFIG_DIR` set to a temporary directory.

## 10. Delivery

Four stages, each independently shippable and each with its own plan:

1. **Core.** Event kinds, `ChatSend` and `ChatConfirm`, intent layer, gate, `divisi chat`.
2. **ACP.** Rewire `divisi acp` onto the core.
3. **TUI.** Chat tab.
4. **Notch.** Chat tab, starting with the focus spike.

Each stage that adds a feature is a minor version bump. Existing `/` commands, modes and
the `single-pool` default agent stay unchanged in every stage.

## 11. Risks and open items

- Model-backed intent quality is the main unknown; the rules-first order and the gate
  bound the damage of a bad classification, and the fake-router tests pin the contract.
- Confirmation fatigue: mitigated by `remember` on the two intents where it is safe.
- The shared thread grows without bound; stage 1 caps history reads at the last 200
  events per request, and retention is a follow-up.
- The fan-out rule depends on coordinator pause support (section 6).
