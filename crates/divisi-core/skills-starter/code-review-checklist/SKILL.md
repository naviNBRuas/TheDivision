# Code Review Checklist

Review for correctness and consequence, not style — a linter already handles style.

- **Correctness**: does the code do what the description claims? Trace at least
  one concrete example through the changed logic by hand.
- **Edge cases**: empty input, zero, negative numbers, the maximum size, absent
  optional fields, concurrent access. Ask what happens at each boundary.
- **Error handling**: are failures handled explicitly, or silently swallowed?
  A `catch` that does nothing is usually worse than no `catch` at all.
- **Blast radius**: what does this change affect that isn't obvious from the
  diff — a shared function's other callers, a migration's effect on existing
  data, a config default that changes behavior for everyone already running it?
- **Tests**: do they cover the actual change, including the failure path, or
  only the happy path already covered before?
- **Reversibility**: if this turns out to be wrong in production, how hard is it
  to undo? Flag anything that's expensive or impossible to reverse.

Leave comments as questions or observations when you're not sure ("does this
handle X?"), and as clear asks when you are ("this will double-charge on retry —
needs idempotency here"). Don't block on preferences that aren't backed by a
concrete problem.
