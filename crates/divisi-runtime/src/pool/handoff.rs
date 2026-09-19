//! Context handoff on a model switch — spec §6.7. Memory-only (never
//! disk/logs, per the spec's explicit privacy note), TTL 3h.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(3 * 60 * 60);

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub messages_summary: String,
    pub last_provider_model: (String, String),
}

pub struct HandoffStore {
    entries: Mutex<HashMap<String, (SessionEntry, Instant)>>,
}

impl Default for HandoffStore {
    fn default() -> Self {
        HandoffStore { entries: Mutex::new(HashMap::new()) }
    }
}

impl HandoffStore {
    fn get_live(&self, key: &str, now: Instant) -> Option<SessionEntry> {
        let mut map = self.entries.lock().unwrap();
        match map.get(key) {
            Some((_, inserted_at)) if now.duration_since(*inserted_at) > TTL => {
                map.remove(key);
                None
            }
            Some((entry, _)) => Some(entry.clone()),
            None => None,
        }
    }

    fn put(&self, key: &str, entry: SessionEntry, at: Instant) {
        self.entries.lock().unwrap().insert(key.to_string(), (entry, at));
    }
}

/// Explicit id if given (coordinator node id, ACP session id), else the
/// SHA-1 hex of the first user message.
pub fn session_key(explicit: Option<&str>, first_user_message: &str) -> String {
    match explicit {
        Some(id) => id.to_string(),
        None => sha1_hex(first_user_message.as_bytes()),
    }
}

/// Reuses `pool::client::ChatMessage` (Phase 4) now that it exists — this
/// module predates `client.rs` and originally defined its own minimal
/// stand-in, but there's no reason to keep two structurally-identical
/// message types once the real one landed.
pub use crate::pool::client::ChatMessage;

const HANDOFF_MARKER: &str = "SingleCLI context handoff:";

/// Prepends the spec §6.7 handoff system message when — and only when —
/// an entry exists for `session_key`, the provider/model actually
/// changed, and no handoff message is already present. Returns whether it
/// injected. Updates the stored `last_provider_model` regardless (so the
/// next call's "did it change" check is against the model that's about to
/// run, not stale state).
pub fn inject(store: &HandoffStore, key: &str, new_provider: &str, new_model: &str, messages: &mut Vec<ChatMessage>) -> bool {
    let now = Instant::now();
    let existing = store.get_live(key, now);

    let already_present = messages.iter().any(|m| m.content.starts_with(HANDOFF_MARKER));

    let should_inject = match &existing {
        Some(entry) => {
            let (old_p, old_m) = &entry.last_provider_model;
            (old_p.as_str(), old_m.as_str()) != (new_provider, new_model) && !already_present
        }
        None => false,
    };

    if should_inject {
        if let Some(entry) = &existing {
            let (old_p, old_m) = &entry.last_provider_model;
            let body = format!(
                "{HANDOFF_MARKER}\n\
                 You are taking over an ongoing task from another model ({old_p} {old_m} -> {new_provider} {new_model}).\n\
                 Continue using the context already in this request. Do not restart, re-ask\n\
                 answered setup questions, or discard prior tool results. The user's latest\n\
                 message is the highest-priority instruction.\n\
                 Recent summary: {}",
                entry.messages_summary
            );
            messages.insert(0, ChatMessage { role: "system".to_string(), content: body });
        }
    }

    let summary = existing.map(|e| e.messages_summary).unwrap_or_default();
    store.put(key, SessionEntry { messages_summary: summary, last_provider_model: (new_provider.to_string(), new_model.to_string()) }, now);

    should_inject
}

pub fn record_summary(store: &HandoffStore, key: &str, summary: String, provider: &str, model: &str) {
    store.put(key, SessionEntry { messages_summary: summary, last_provider_model: (provider.to_string(), model.to_string()) }, Instant::now());
}

// --- hand-rolled SHA-1 (spec-allowed: stable-key hash, not security-sensitive) ---

fn sha1_hex(data: &[u8]) -> String {
    let digest = sha1(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_known_vector() {
        // sha1("abc") is a well-known test vector.
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn injects_on_model_switch() {
        let store = HandoffStore::default();
        let key = "sess1";
        record_summary(&store, key, "did X and Y".to_string(), "groq", "llama-3");

        let mut messages = vec![ChatMessage { role: "user".to_string(), content: "continue".to_string() }];
        let injected = inject(&store, key, "cerebras", "llama-3", &mut messages);
        assert!(injected);
        assert!(messages[0].content.starts_with(HANDOFF_MARKER));
        assert!(messages[0].content.contains("groq"));
        assert!(messages[0].content.contains("cerebras"));
    }

    #[test]
    fn does_not_inject_on_first_request() {
        let store = HandoffStore::default();
        let mut messages = vec![ChatMessage { role: "user".to_string(), content: "hello".to_string() }];
        let injected = inject(&store, "new-session", "groq", "llama-3", &mut messages);
        assert!(!injected);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn does_not_inject_on_same_model_continuation() {
        let store = HandoffStore::default();
        let key = "sess2";
        record_summary(&store, key, "summary".to_string(), "groq", "llama-3");
        let mut messages = vec![ChatMessage { role: "user".to_string(), content: "continue".to_string() }];
        let injected = inject(&store, key, "groq", "llama-3", &mut messages);
        assert!(!injected);
    }

    #[test]
    fn does_not_inject_twice_if_already_present() {
        let store = HandoffStore::default();
        let key = "sess3";
        record_summary(&store, key, "summary".to_string(), "groq", "llama-3");
        let mut messages =
            vec![ChatMessage { role: "system".to_string(), content: format!("{HANDOFF_MARKER} already here") }, ChatMessage { role: "user".to_string(), content: "hi".to_string() }];
        let injected = inject(&store, key, "cerebras", "llama-3", &mut messages);
        assert!(!injected);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn session_key_uses_explicit_id_when_given() {
        assert_eq!(session_key(Some("node-42"), "whatever"), "node-42");
    }

    #[test]
    fn session_key_falls_back_to_sha1_of_first_message() {
        assert_eq!(session_key(None, "abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn entry_expires_after_ttl() {
        let store = HandoffStore::default();
        let key = "sess4";
        let stale_time = Instant::now() - TTL - Duration::from_secs(1);
        store.put(key, SessionEntry { messages_summary: "old".to_string(), last_provider_model: ("groq".to_string(), "llama-3".to_string()) }, stale_time);

        let mut messages = vec![ChatMessage { role: "user".to_string(), content: "continue".to_string() }];
        let injected = inject(&store, key, "cerebras", "llama-3", &mut messages);
        assert!(!injected, "expired entry should be treated as absent");
    }
}
