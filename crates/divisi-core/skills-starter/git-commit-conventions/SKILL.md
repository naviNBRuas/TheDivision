# Git Commit Conventions

Write commit messages for the reader six months from now, not for the diff.

- Subject line: imperative mood, under ~70 characters ("add retry to the fetch
  client", not "added" or "adds"). It should read naturally after "This commit
  will...".
- Body (when the change needs explaining): focus on *why*, not *what* — the diff
  already shows what changed. Explain the motivation, the constraint that shaped
  the approach, or the incident that prompted it.
- One logical change per commit. If the commit message needs "and" to describe
  it, consider splitting it.
- Don't reference ephemeral context (a ticket number with no other explanation,
  "per Slack discussion") without also capturing the substance — the reference
  will outlive its context.
- Prefer `git commit --amend`/interactive rebase to fix up work-in-progress
  commits *before* they're shared; once pushed to a shared branch, add new
  commits instead of rewriting history.

A good commit message answers: what problem existed, and why this is the right
fix — not just a transcript of the keystrokes.
