# Systematic Debugging

Find the root cause before writing a fix. A fix aimed at a symptom instead of a
cause tends to resurface later in a different shape.

1. Reproduce the failure reliably first. If you can't reproduce it on demand, you
   don't have enough information to fix it yet — go get more (logs, a smaller
   repro case, the exact input that triggers it).
2. Form a specific hypothesis about the cause before changing any code. "Something
   about the auth flow" is not a hypothesis; "the token refresh races with the
   request that triggered it" is.
3. Test the hypothesis with the smallest possible change — a log line, an
   assertion, a debugger breakpoint — before touching the real fix.
4. Only once the cause is confirmed, write the fix, and write a test that fails
   without it and passes with it.

Do not fix the first plausible-looking thing you find. If a change makes the
symptom go away but you can't explain *why* it was happening, keep digging —
you likely haven't found the real cause.
