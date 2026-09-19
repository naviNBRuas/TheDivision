# Security Review Basics

A first pass to run over any change that touches input handling, auth, or
external data — not a substitute for a real security audit on anything
sensitive.

- **Input validation**: is every value that crosses a trust boundary (user
  input, an API response, a file read from disk) validated before use, not
  just assumed well-formed?
- **Injection**: any string built by concatenation and handed to a shell, SQL
  query, or template engine is a candidate for injection. Prefer parameterized
  queries / argument arrays over string-building.
- **Secrets**: no credentials, API keys, or tokens in source, logs, or error
  messages. Check that a caught exception doesn't leak a secret value in its
  message.
- **AuthZ, not just authN**: confirming *who* the caller is isn't the same as
  confirming they're *allowed* to do this specific thing to this specific
  resource. Check the authorization check exists on every path that needs one,
  not just the obvious one.
- **Least privilege**: does this code request more access (filesystem, network,
  scopes) than the task actually needs?
- **Dependency trust**: for a new dependency, is it maintained, and does it
  need the permissions it's asking for?

When something looks wrong but you're not sure it's exploitable, say so anyway —
"I can't prove this is exploitable, but X and Y look unsafe together" is a
useful finding on its own.
