//! E29 spec §"Redaction": heuristic secret detection + a TTL'd alias store.
//! Every prompt reaching an LLM/agent through divisi (ACP, direct
//! `task run`/`goal submit`, the OpenAI-compat server) is scanned here
//! before it leaves the process; a match is replaced with a
//! `{{REDACTED_N}}` alias and the real value is encrypted (age,
//! passphrase-based — same primitive `backup.rs` uses) and stashed in a
//! short-TTL sqlite table. `resolve()` substitutes aliases back to
//! plaintext only at the outbound dispatch boundary
//! (`divisi-runtime::pool_agent`), never anywhere that gets logged or
//! persisted.
//!
//! `secrets.rs` has no in-process cipher of its own (it shells out to the
//! OS keychain) — the master passphrase used here is generated once and
//! stored *in* that keychain under a fixed name, so key custody still
//! rides the existing secrets machinery even though the encrypt/decrypt
//! step happens in this module.

use age::secrecy::SecretString;
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

const MASTER_KEY_SECRET_NAME: &str = "single-redact-master-key";
/// Sliding: every successful `resolve` pushes an alias's expiry out again, so
/// a goal that keeps dispatching (and retrying) keeps its aliases alive.
const TTL_MS: i64 = 24 * 60 * 60 * 1000; // 24 hours
/// scrypt `log_n` for alias ciphertexts. The master passphrase is 256 random
/// bits, so key stretching adds nothing; age's default device-calibrated
/// factor cost ~1s per alias and made decryption refuse under load.
const ALIAS_LOG_N: u8 = 10;
/// Highest `log_n` accepted on decrypt — covers aliases written with age's
/// calibrated default before `ALIAS_LOG_N` existed, whatever the load now.
const MAX_DECRYPT_LOG_N: u8 = 22;

pub struct PendingAlias {
    pub alias: String,
    pub session_id: String,
    pub created_at: i64,
    pub expires_at: i64,
}

pub struct RedactStore<'a> {
    pub conn: &'a Connection,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS redact_aliases (
            session_id TEXT NOT NULL,
            alias TEXT NOT NULL,
            ciphertext BLOB NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            PRIMARY KEY (session_id, alias)
        )",
        [],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// Fetches the machine's redaction master passphrase from the OS
/// keychain, generating and storing one on first use. A missing keychain
/// backend (no `secret-tool`/no daemon) is a hard error here — redaction
/// must not silently fall back to an unencrypted or process-lifetime-only
/// key, since that would weaken the "never plaintext on disk" guarantee
/// this module exists to provide.
fn master_passphrase(store: &dyn single_secret_store::SecretStoreObj) -> Result<SecretString> {
    if let Some(existing) = store.get(MASTER_KEY_SECRET_NAME)? {
        return Ok(SecretString::from(existing));
    }
    let generated = generate_passphrase();
    store.set(MASTER_KEY_SECRET_NAME, &generated)?;
    Ok(SecretString::from(generated))
}

fn generate_passphrase() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // process-independent entropy source: 32 random bytes from the OS via
    // /dev/urandom, hex-encoded. No new RNG crate — this repo has no
    // existing rand dependency in divisi-core, and pulling one in for a
    // one-shot 32-byte read isn't worth it.
    let mut buf = [0u8; 32];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).is_err() {
        // extremely unlikely on any machine this runs on; fall back to a
        // timestamp-seeded mix so the function still returns something
        // usable rather than panicking.
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((seed >> (i % 16 * 4)) & 0xff) as u8;
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn encrypt(passphrase: &SecretString, plaintext: &str) -> Result<Vec<u8>> {
    let mut recipient = age::scrypt::Recipient::new(passphrase.clone());
    recipient.set_work_factor(ALIAS_LOG_N);
    let encryptor = age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
        .context("initializing age encryption")?;
    let mut out = Vec::new();
    let mut writer = encryptor.wrap_output(&mut out).context("initializing age encryption")?;
    writer.write_all(plaintext.as_bytes()).context("writing plaintext")?;
    writer.finish().context("finalizing age encryption")?;
    Ok(out)
}

fn decrypt(passphrase: &SecretString, ciphertext: &[u8]) -> Result<String> {
    let decryptor = age::Decryptor::new(ciphertext).context("stored alias ciphertext is not valid age data")?;
    let mut out = Vec::new();
    let mut identity = age::scrypt::Identity::new(passphrase.clone());
    identity.set_max_work_factor(MAX_DECRYPT_LOG_N);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .context("decrypting alias value")?;
    reader.read_to_end(&mut out).context("reading decrypted alias value")?;
    String::from_utf8(out).context("decrypted alias value is not valid UTF-8")
}

static ALIAS_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Scans `text` for secret-shaped substrings, replaces each with a fresh
/// `{{REDACTED:<session_id>:N}}` alias, and stores the encrypted real
/// value scoped to `session_id` with a 3-hour TTL. Over-redacts on
/// ambiguity: a false positive costs an un-redact, a miss costs a leaked
/// key.
///
/// The session id is embedded in the alias token itself (rather than
/// threaded separately into `resolve`) because a redacted goal's text
/// gets rewritten and re-dispatched through several layers — coordinator
/// planning, node prompts, `divisi-pool`'s dispatch — that don't all
/// carry an explicit session parameter today. `resolve` derives its scope
/// purely by reading the token, so it works uniformly at every dispatch
/// path with no additional plumbing, and a lookup still requires the
/// exact `(session_id, alias)` row this call created — embedding the id
/// does not by itself grant access to another session's secret.
pub fn scan_and_replace(
    store: &RedactStore,
    secret_store: &dyn single_secret_store::SecretStoreObj,
    session_id: &str,
    text: &str,
) -> Result<(String, Vec<PendingAlias>)> {
    let spans = find_secret_spans(text);
    if spans.is_empty() {
        return Ok((text.to_string(), Vec::new()));
    }

    let passphrase = master_passphrase(secret_store)?;
    let now = now_ms();
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut aliases = Vec::new();

    for (start, end) in spans {
        out.push_str(&text[last..start]);
        let n = ALIAS_COUNTER.fetch_add(1, Ordering::SeqCst);
        let alias = format!("{{{{REDACTED:{session_id}:{n}}}}}");
        let real_value = &text[start..end];
        let ciphertext = encrypt(&passphrase, real_value)?;
        let expires_at = now + TTL_MS;
        store.conn.execute(
            "INSERT OR REPLACE INTO redact_aliases (session_id, alias, ciphertext, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, alias, ciphertext, now, expires_at],
        )?;
        aliases.push(PendingAlias { alias: alias.clone(), session_id: session_id.to_string(), created_at: now, expires_at });
        out.push_str(&alias);
        last = end;
    }
    out.push_str(&text[last..]);

    Ok((out, aliases))
}

/// Replaces every `{{REDACTED:<session_id>:N}}` alias in `text` with its
/// decrypted real value — the session scope is read from the token
/// itself (see `scan_and_replace`'s doc comment). Text with no alias
/// tokens passes through unchanged (not an error). An alias token present
/// but unresolvable (expired or unknown) is a real error — a dangling
/// alias reaching a provider is worse than a failed dispatch.
pub fn resolve(store: &RedactStore, secret_store: &dyn single_secret_store::SecretStoreObj, text: &str) -> Result<String> {
    let alias_re_matches = find_alias_tokens(text);
    if alias_re_matches.is_empty() {
        return Ok(text.to_string());
    }

    sweep_expired(store.conn)?;
    let passphrase = master_passphrase(secret_store)?;
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for (start, end, session_id, alias) in alias_re_matches {
        out.push_str(&text[last..start]);
        let ciphertext: Vec<u8> = store
            .conn
            .query_row(
                "SELECT ciphertext FROM redact_aliases WHERE session_id = ?1 AND alias = ?2",
                params![session_id, alias],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("unresolvable alias {alias} (expired or unknown)"))?;
        let real_value = decrypt(&passphrase, &ciphertext)?;
        store.conn.execute(
            "UPDATE redact_aliases SET expires_at = ?1 WHERE session_id = ?2 AND alias = ?3",
            params![now_ms() + TTL_MS, session_id, alias],
        )?;
        out.push_str(&real_value);
        last = end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Deletes rows past `expires_at`. Called opportunistically (daemon
/// startup, and lazily inside `resolve`) — no background timer thread.
pub fn sweep_expired(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("DELETE FROM redact_aliases WHERE expires_at < ?1", params![now_ms()])?)
}

/// Decrypts a single `{{REDACTED:<session_id>:N}}` alias token's real
/// value and deletes its row — used by `divisi secret promote-alias` to
/// move a temp alias into a properly named secret. Errors if `alias_token`
/// isn't a well-formed alias, or the row is unknown/expired.
pub fn take_alias_value(store: &RedactStore, secret_store: &dyn single_secret_store::SecretStoreObj, alias_token: &str) -> Result<String> {
    let trimmed = alias_token.trim();
    let inner = trimmed
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))
        .and_then(|s| s.strip_prefix("REDACTED:"))
        .ok_or_else(|| anyhow!("not a well-formed alias token: {trimmed}"))?;
    let (session_id, n) = inner.rsplit_once(':').ok_or_else(|| anyhow!("not a well-formed alias token: {trimmed}"))?;
    if session_id.is_empty() || n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("not a well-formed alias token: {trimmed}");
    }

    sweep_expired(store.conn)?;
    let passphrase = master_passphrase(secret_store)?;
    let ciphertext: Vec<u8> = store
        .conn
        .query_row(
            "SELECT ciphertext FROM redact_aliases WHERE session_id = ?1 AND alias = ?2",
            params![session_id, trimmed],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("unresolvable alias {trimmed} (expired or unknown)"))?;
    let real_value = decrypt(&passphrase, &ciphertext)?;
    store.conn.execute("DELETE FROM redact_aliases WHERE session_id = ?1 AND alias = ?2", params![session_id, trimmed])?;
    Ok(real_value)
}

/// Parses `{{REDACTED:<session_id>:<n>}}` tokens out of `text`, returning
/// `(start, end, session_id, full_alias_token)` for each, left to right.
fn find_alias_tokens(text: &str) -> Vec<(usize, usize, String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = text[i..].find("{{REDACTED:") {
        let start = i + rel;
        if let Some(rel_end) = text[start..].find("}}") {
            let end = start + rel_end + 2;
            let candidate = &text[start..end];
            let inner = &candidate[2..candidate.len() - 2]; // strip {{ }}
            if let Some(rest) = inner.strip_prefix("REDACTED:") {
                if let Some((session_id, n)) = rest.rsplit_once(':') {
                    if !session_id.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                        out.push((start, end, session_id.to_string(), candidate.to_string()));
                    }
                }
            }
            i = end;
        } else {
            break;
        }
    }
    out
}

/// Returns non-overlapping `(start, end)` byte spans of `text` judged
/// secret-shaped, scanned left to right. Detection order: known vendor
/// prefixes, then `key=`/`password=`-style assignments (only the value
/// half), then generic high-entropy tokens (with UUID/git-SHA negative
/// guards).
fn find_secret_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let push_if_free = |spans: &mut Vec<(usize, usize)>, start: usize, end: usize| {
        if !spans.iter().any(|&(s, e)| start < e && s < end) {
            spans.push((start, end));
        }
    };

    // 1. known vendor prefixes.
    for (prefix, min_total_len) in [
        ("sk-", 23),
        ("ghp_", 40),
        ("gho_", 40),
        ("xoxb-", 15),
        ("xoxp-", 15),
        ("xoxa-", 15),
        ("xoxs-", 15),
        ("AKIA", 20),
    ] {
        let mut i = 0;
        while let Some(rel) = text[i..].find(prefix) {
            let start = i + rel;
            let end = token_end(text, start + prefix.len());
            if end - start >= min_total_len {
                push_if_free(&mut spans, start, end);
            }
            i = start + prefix.len();
        }
    }

    // JWT shape: three dot-separated base64url segments, each >= 10 chars.
    for (start, end) in find_jwt_shapes(text) {
        push_if_free(&mut spans, start, end);
    }

    // 2. assignment-shaped: (key|token|secret|password|pwd|api_key) [:=] value
    for (start, end) in find_assignment_values(text) {
        push_if_free(&mut spans, start, end);
    }

    // 3. generic high-entropy tokens.
    for (start, end) in find_high_entropy_tokens(text) {
        push_if_free(&mut spans, start, end);
    }

    spans.sort_by_key(|&(s, _)| s);
    spans
}

fn token_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = from;
    while end < bytes.len() {
        let c = bytes[end] as char;
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn find_jwt_shapes(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let start_idx = chars[i].0;
        let seg1_end = scan_b64url(&chars, i);
        if seg1_end > i && chars.get(seg1_end).map(|&(_, c)| c) == Some('.') {
            let seg2_start = seg1_end + 1;
            let seg2_end = scan_b64url(&chars, seg2_start);
            if seg2_end > seg2_start && chars.get(seg2_end).map(|&(_, c)| c) == Some('.') {
                let seg3_start = seg2_end + 1;
                let seg3_end = scan_b64url(&chars, seg3_start);
                if seg3_end > seg3_start
                    && seg1_end - i >= 10
                    && seg2_end - seg2_start >= 10
                    && seg3_end - seg3_start >= 10
                {
                    let end_byte = chars.get(seg3_end).map(|&(b, _)| b).unwrap_or(text.len());
                    out.push((start_idx, end_byte));
                    i = seg3_end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn scan_b64url(chars: &[(usize, char)], from: usize) -> usize {
    let mut i = from;
    while i < chars.len() && (chars[i].1.is_ascii_alphanumeric() || chars[i].1 == '-' || chars[i].1 == '_') {
        i += 1;
    }
    i
}

fn find_assignment_values(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();
    for label in ["api_key", "apikey", "api-key", "key", "token", "secret", "password", "pwd"] {
        let mut i = 0;
        while let Some(rel) = lower[i..].find(label) {
            let label_start = i + rel;
            let after_label = label_start + label.len();
            let rest = &text[after_label..];
            let trimmed = rest.trim_start();
            let skipped = rest.len() - trimmed.len();
            if let Some(sep_char) = trimmed.chars().next() {
                if sep_char == ':' || sep_char == '=' {
                    let after_sep = after_label + skipped + sep_char.len_utf8();
                    let after_sep_rest = &text[after_sep..];
                    let value_trimmed = after_sep_rest.trim_start();
                    let value_start = after_sep + (after_sep_rest.len() - value_trimmed.len());
                    let value_start = value_start + if value_trimmed.starts_with(['\'', '"']) { 1 } else { 0 };
                    let end = token_end_no_quote(text, value_start);
                    if end - value_start >= 8 {
                        out.push((value_start, end));
                    }
                }
            }
            i = after_label;
        }
    }
    out
}

fn token_end_no_quote(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = from;
    while end < bytes.len() {
        let c = bytes[end] as char;
        if c.is_whitespace() || c == '\'' || c == '"' {
            break;
        }
        end += 1;
    }
    end
}

fn find_high_entropy_tokens(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (start, word) in whitespace_tokens(text) {
        if word.len() < 20 {
            continue;
        }
        if is_uuid(word) || is_pure_lowercase_hex(word) {
            continue;
        }
        // Live-verification finding (E29 follow-up): a filesystem path or
        // filename, tokenized whole by whitespace-splitting, easily clears
        // the entropy bar on mixed letters/digits/hyphens alone — and
        // paths are ubiquitous in real prompts ("docs/queue/E03-vault-
        // evolution/HANDOFF.md"). A real secret essentially never appears
        // as a path segment or bare filename, so excluding both is a
        // large false-positive reduction for near-zero miss risk.
        if word.contains('/') || looks_like_a_filename(word) {
            continue;
        }
        // Live-verification finding: a JSON/code fragment ("{"id":"s1",
        // ...,"depends_on":[]}" — a real planner-prompt example) is one
        // whitespace-delimited "word" with plenty of punctuation variety,
        // which reads as high entropy the same way a real secret does. A
        // bearer-token-shaped secret essentially never contains any of
        // these structural characters, so their presence is a strong,
        // cheap signal this is code/data, not a secret — assignment-style
        // detection (`key=value`) already covers the one case where a
        // secret legitimately sits next to such punctuation, since it
        // extracts only the value span, stopping at whitespace/quotes.
        if word.chars().any(|c| matches!(c, '{' | '}' | '[' | ']' | '"')) {
            continue;
        }
        let has_digit = word.chars().any(|c| c.is_ascii_digit());
        let has_alpha = word.chars().any(|c| c.is_ascii_alphabetic());
        if !has_digit || !has_alpha {
            continue;
        }
        if shannon_entropy(word) >= 3.5 {
            out.push((start, start + word.len()));
        }
    }
    out
}

/// True if `word` ends in a short, common file-extension shape
/// (`.md`, `.rs`, `.toml`, `.yaml`, ...) — a `.` followed by 1-5 lowercase
/// letters, ignoring trailing sentence punctuation (`notes.md:`, `a.rs),`).
fn looks_like_a_filename(word: &str) -> bool {
    let word = word.trim_end_matches([':', ',', ';', ')', '.', '!', '?']);
    match word.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_lowercase())
        }
        None => false,
    }
}

fn whitespace_tokens(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(&len, p)| p.len() == len && p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_pure_lowercase_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0u32) += 1;
    }
    let len = s.chars().count() as f64;
    counts.values().fold(0.0, |acc, &count| {
        let p = count as f64 / len;
        acc - p * p.log2()
    })
}

/// Object-safe subset of `secrets::SecretStore` this module needs,
/// defined locally so `redact.rs` doesn't have to depend on
/// `secrets.rs`'s concrete `SecretTool` type in its public signature —
/// any real `SecretStore` implementor satisfies it via the blanket impl
/// below.
pub mod single_secret_store {
    use anyhow::Result;

    pub trait SecretStoreObj {
        fn get(&self, name: &str) -> Result<Option<String>>;
        fn set(&self, name: &str, value: &str) -> Result<()>;
    }

    impl<T: crate::secrets::SecretStore> SecretStoreObj for T {
        fn get(&self, name: &str) -> Result<Option<String>> {
            crate::secrets::SecretStore::get(self, name)
        }
        fn set(&self, name: &str, value: &str) -> Result<()> {
            crate::secrets::SecretStore::set(self, name, value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// in-memory stand-in for the OS keychain so tests don't depend on a
    /// real `secret-tool`/keyring daemon being present.
    struct FakeKeychain(RefCell<HashMap<String, String>>);
    impl SecretStore for FakeKeychain {
        fn set(&self, name: &str, value: &str) -> Result<()> {
            self.0.borrow_mut().insert(name.to_string(), value.to_string());
            Ok(())
        }
        fn get(&self, name: &str) -> Result<Option<String>> {
            Ok(self.0.borrow().get(name).cloned())
        }
        fn delete(&self, name: &str) -> Result<bool> {
            Ok(self.0.borrow_mut().remove(name).is_some())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(self.0.borrow().keys().cloned().collect())
        }
    }

    fn setup() -> (Connection, FakeKeychain) {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        (conn, FakeKeychain(RefCell::new(HashMap::new())))
    }

    #[test]
    fn redacts_known_vendor_prefixes() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let (out, aliases) = scan_and_replace(
            &store,
            &keychain,
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
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "ghp_16C7e42F292c6912E7710c838347Ae178B4a and AKIAIOSFODNN7EXAMPLE and eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 3);
        assert!(!out.contains("ghp_16C7e42F292c6912E7710c838347Ae178B4a"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redacts_generic_high_entropy_assignment() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", "DATABASE_PASSWORD=Xk9mQ2vL8pR4nT7wZ1cF6").unwrap();
        assert_eq!(aliases.len(), 1);
        assert!(!out.contains("Xk9mQ2vL8pR4nT7wZ1cF6"));
    }

    /// Live-verification regression (E29 follow-up): a real orchestration
    /// prompt referencing repo paths and filenames must survive intact —
    /// found live when submitting real work to the coordinator, where
    /// `docs/queue/E03-vault-evolution/HANDOFF.md`-shaped text was
    /// getting redacted, corrupting the goal text every downstream agent
    /// (not just divisi-pool) relies on to find the right files.
    #[test]
    fn does_not_redact_repo_paths_or_filenames() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "Read docs/queue/E00-platform-foundation/HANDOFF.md and \
                     nbr-workspace/docs/queue/E03-vault-evolution/01-vault-core-ontology.md, \
                     then update EXECUTION-PLAN.md when done.";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 0, "{out}");
        assert_eq!(out, text);
    }

    /// Live regression (2026-09-24): the epic conductor writes
    /// `SPRINT FILE 02-shared-contracts.md:` — the trailing `:` hid the
    /// `.md` extension, so every conductor goal carried a redacted
    /// filename and failed once its alias expired.
    #[test]
    fn does_not_redact_filenames_followed_by_punctuation() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "SPRINT FILE 02-shared-contracts.md:\n# E31/02 (see 01-architecture-challenge-and-adrs.md), \
                     then 00-discovery-and-baseline-2.md; done";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 0, "{out}");
        assert_eq!(out, text);
    }

    /// The master passphrase is 256 random bits, so scrypt stretching adds
    /// nothing; a device-calibrated work factor made every alias cost ~1s to
    /// write and made decryption refuse ("Excessive work parameter") when
    /// the machine was busy running goals.
    #[test]
    fn aliases_use_a_fixed_low_scrypt_work_factor() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        scan_and_replace(&store, &keychain, "sess1", "DATABASE_PASSWORD=Xk9mQ2vL8pR4nT7wZ1cF6").unwrap();
        let ct: Vec<u8> = conn.query_row("SELECT ciphertext FROM redact_aliases", [], |r| r.get(0)).unwrap();
        let header = String::from_utf8_lossy(&ct[..ct.len().min(120)]).to_string();
        let log_n: u8 = header
            .lines()
            .find_map(|l| l.strip_prefix("-> scrypt "))
            .and_then(|rest| rest.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no scrypt stanza in {header}"));
        assert_eq!(log_n, ALIAS_LOG_N);
    }

    /// Goals outlive the alias TTL (they retry for hours); every successful
    /// resolve pushes the expiry out again so a live goal keeps working.
    #[test]
    fn resolve_extends_alias_expiry() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let (out, _) = scan_and_replace(&store, &keychain, "sess1", "DATABASE_PASSWORD=Xk9mQ2vL8pR4nT7wZ1cF6").unwrap();
        let soon = now_ms() + 1000;
        conn.execute("UPDATE redact_aliases SET expires_at = ?1", params![soon]).unwrap();
        resolve(&store, &keychain, &out).unwrap();
        let expires: i64 = conn.query_row("SELECT expires_at FROM redact_aliases", [], |r| r.get(0)).unwrap();
        assert!(expires > now_ms() + TTL_MS - 60_000, "expiry not extended: {expires}");
    }

    /// Live-verification regression: a real planner-prompt JSON schema
    /// example (`divisi-runtime`'s own `PLAN_INSTRUCTION`) is one
    /// whitespace-delimited "word" full of punctuation variety — high
    /// entropy for the wrong reason. Confirmed live via `divisi task run`
    /// with a JSON-example prompt before this fix.
    #[test]
    fn does_not_redact_json_schema_examples() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "Output ONLY a JSON array. Each element: \
                     {\"id\":\"s1\",\"desc\":\"...\",\"kind\":\"code|test|research|review|docs|infra\",\"effort\":\"quick|standard|deep\",\"depends_on\":[]}";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 0, "{out}");
        assert_eq!(out, text);
    }

    /// A real secret sitting right next to path-shaped text must still be
    /// caught — the path guard must not blanket-suppress detection.
    #[test]
    fn still_redacts_a_real_secret_next_to_repo_paths() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "see docs/queue/E04-sterling-agent-platform/HANDOFF.md — \
                     also use key sk-abcdEFGH1234567890abcdEFGH1234567890abcd for the API call";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 1);
        assert!(out.contains("docs/queue/E04-sterling-agent-platform/HANDOFF.md"));
        assert!(!out.contains("sk-abcdEFGH"));
    }

    #[test]
    fn does_not_redact_low_entropy_prose_or_uuids_or_git_shas() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let text = "please review commit a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 \
                     and issue id 550e8400-e29b-41d4-a716-446655440000, thanks";
        let (out, aliases) = scan_and_replace(&store, &keychain, "sess1", text).unwrap();
        assert_eq!(aliases.len(), 0);
        assert_eq!(out, text);
    }

    #[test]
    fn take_alias_value_decrypts_and_deletes_the_row() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let (_redacted, aliases) =
            scan_and_replace(&store, &keychain, "sess1", "key sk-abcdEFGH1234567890abcdEFGH1234567890abcd here").unwrap();
        let alias = &aliases[0].alias;

        let value = take_alias_value(&store, &keychain, alias).unwrap();
        assert_eq!(value, "sk-abcdEFGH1234567890abcdEFGH1234567890abcd");

        // second call fails: the row was deleted by the first.
        let err = take_alias_value(&store, &keychain, alias).unwrap_err();
        assert!(err.to_string().contains("unresolvable"));
    }

    #[test]
    fn take_alias_value_rejects_malformed_tokens() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let err = take_alias_value(&store, &keychain, "not an alias").unwrap_err();
        assert!(err.to_string().contains("not a well-formed alias token"));
    }

    #[test]
    fn resolve_round_trips_a_live_alias() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let (redacted, aliases) =
            scan_and_replace(&store, &keychain, "sess1", "key sk-abcdEFGH1234567890abcdEFGH1234567890abcd here").unwrap();
        let resolved = resolve(&store, &keychain, &redacted).unwrap();
        assert!(resolved.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
        assert_eq!(aliases.len(), 1);
    }

    #[test]
    fn resolve_passes_through_text_with_no_alias_tokens() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let resolved = resolve(&store, &keychain, "nothing redacted here").unwrap();
        assert_eq!(resolved, "nothing redacted here");
    }

    #[test]
    fn resolve_errors_on_expired_or_unknown_alias() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        let err = resolve(&store, &keychain, "value is {{REDACTED:sess1:99}}").unwrap_err();
        assert!(err.to_string().contains("REDACTED"));
    }

    #[test]
    fn resolve_finds_the_session_embedded_in_the_alias_token() {
        let (conn, keychain) = setup();
        let store = RedactStore { conn: &conn };
        // written under sess1's scope; resolve() has no external session
        // parameter to get wrong — it reads sess1 straight out of the
        // token and finds exactly the row scan_and_replace created.
        let (redacted, _) =
            scan_and_replace(&store, &keychain, "sess1", "key sk-abcdEFGH1234567890abcdEFGH1234567890abcd here").unwrap();
        let resolved = resolve(&store, &keychain, &redacted).unwrap();
        assert!(resolved.contains("sk-abcdEFGH1234567890abcdEFGH1234567890abcd"));
    }

    #[test]
    fn sweep_expired_removes_only_past_ttl_rows() {
        let (conn, _keychain) = setup();
        conn.execute(
            "INSERT INTO redact_aliases (session_id, alias, ciphertext, created_at, expires_at) VALUES ('s', '{{REDACTED:s:1}}', X'00', 0, 1)",
            [],
        )
        .unwrap();
        let removed = sweep_expired(&conn).unwrap();
        assert_eq!(removed, 1);
    }
}
