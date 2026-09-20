//! The Chat tab's state and behaviour, kept free of drawing and IO so it can be tested directly.
//! It is a view of the shared conversation: events arrive from the daemon and are applied here.

use divisi_protocol::{chat_line, progress_line, ChatRole, CoordinatorEvent};

/// One thing shown in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    You(String),
    Divisi { text: String, degraded: bool },
    Confirm { approval_id: i64, text: String },
    Result(String),
    Progress(String),
    Error(String),
}

/// What the user asked to do with the line they typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    Send(String),
    Confirm { approval_id: i64, allow: bool, remember: bool },
}

/// How a row should be styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    You,
    Divisi,
    Confirm,
    Muted,
    Error,
}

#[derive(Debug, Default)]
pub struct ChatView {
    pub session_id: Option<String>,
    pub entries: Vec<Entry>,
    /// Highest event id already applied.
    pub cursor: i64,
    pub input: String,
    /// The confirmation currently waiting for a yes or no.
    pub pending: Option<i64>,
    /// A send or confirm is in flight.
    pub busy: bool,
    /// Rows scrolled up from the bottom (0 = following the newest).
    pub scroll: usize,
}

impl ChatView {
    /// Applies events from the daemon. Events at or below the cursor are ignored, so a poll that
    /// overlaps a reply never shows anything twice.
    pub fn apply(&mut self, session_id: &str, events: &[CoordinatorEvent]) {
        if self.session_id.as_deref() != Some(session_id) {
            *self = ChatView { session_id: Some(session_id.to_owned()), input: std::mem::take(&mut self.input), ..ChatView::default() };
        }
        let start = self.cursor;
        for e in events.iter().filter(|e| e.id > start) {
            self.cursor = e.id;
            if let Some(line) = chat_line(&e.kind, &e.body) {
                match line.role {
                    ChatRole::You => self.entries.push(Entry::You(line.text)),
                    ChatRole::Divisi => self.entries.push(Entry::Divisi { text: line.text, degraded: line.degraded }),
                    ChatRole::Confirm => {
                        if let Some(id) = line.approval_id {
                            self.pending = Some(id);
                            self.entries.push(Entry::Confirm { approval_id: id, text: line.text });
                        }
                    }
                    ChatRole::Result => {
                        if line.approval_id.is_some() && self.pending == line.approval_id {
                            self.pending = None;
                        }
                        self.entries.push(Entry::Result(line.text));
                    }
                }
            } else if let Some(p) = progress_line(&e.kind, &e.body) {
                self.entries.push(Entry::Progress(p));
            }
        }
    }

    pub fn type_char(&mut self, c: char) {
        if !c.is_control() && self.input.chars().count() < 2000 {
            self.input.push(c);
        }
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Takes the typed line. While a confirmation is pending, a bare yes, no or "always" answers it;
    /// anything else is a new message. `None` for an empty line or while a request is in flight.
    pub fn submit(&mut self) -> Option<Submit> {
        let line = self.input.trim().to_owned();
        if line.is_empty() || self.busy {
            return None;
        }
        self.input.clear();
        self.scroll = 0;
        let lower = line.to_lowercase();
        if let Some(id) = self.pending {
            let answer = match lower.as_str() {
                "y" | "yes" | "ok" | "approve" => Some((true, false)),
                "always" => Some((true, true)),
                "n" | "no" | "nope" | "deny" => Some((false, false)),
                _ => None,
            };
            if let Some((allow, remember)) = answer {
                return Some(Submit::Confirm { approval_id: id, allow, remember });
            }
        }
        Some(Submit::Send(line))
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.entries.push(Entry::Error(message.into()));
    }

    /// The conversation as styled, word-wrapped rows for a pane `width` columns wide.
    pub fn rows(&self, width: usize) -> Vec<(RowKind, String)> {
        let width = width.max(12);
        let mut out = Vec::new();
        for entry in &self.entries {
            let (kind, label, text) = match entry {
                Entry::You(t) => (RowKind::You, "you     ", t.clone()),
                Entry::Divisi { text, degraded } => {
                    (RowKind::Divisi, "divisi  ", if *degraded { format!("{text}  (rules only)") } else { text.clone() })
                }
                Entry::Confirm { text, .. } => (RowKind::Confirm, "?       ", format!("{text}   [y] yes  [n] no  [always]")),
                Entry::Result(t) => (RowKind::Muted, "=       ", t.clone()),
                Entry::Progress(t) => (RowKind::Muted, "·       ", t.clone()),
                Entry::Error(t) => (RowKind::Error, "error   ", t.clone()),
            };
            let body_width = width.saturating_sub(label.chars().count()).max(8);
            for (i, line) in text.lines().flat_map(|l| wrap(l, body_width)).enumerate() {
                let prefix = if i == 0 { label } else { "        " };
                out.push((kind, format!("{prefix}{line}")));
            }
        }
        out
    }
}

/// Word-wraps `text` to `width` columns; words longer than a line are split.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word: String = word.to_owned();
        while word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
        }
        if current.is_empty() {
            current = word;
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(&word);
        } else {
            lines.push(std::mem::replace(&mut current, word));
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(id: i64, kind: &str, body: serde_json::Value) -> CoordinatorEvent {
        CoordinatorEvent { id, goal_id: None, ts: "t".into(), kind: kind.into(), body: body.to_string() }
    }

    #[test]
    fn events_become_entries_and_are_never_applied_twice() {
        let mut v = ChatView::default();
        let events = vec![
            ev(1, "chat_user", json!({"text": "status", "surface": "tui"})),
            ev(2, "chat_assistant", json!({"text": "Nothing is running.", "degraded": true})),
            ev(3, "node_output", json!({"text": "noise"})),
            ev(4, "integrated", json!("done")),
        ];
        v.apply("sess_1", &events);
        v.apply("sess_1", &events);
        assert_eq!(
            v.entries,
            vec![
                Entry::You("status".into()),
                Entry::Divisi { text: "Nothing is running.".into(), degraded: true },
                Entry::Progress("done: \"done\"".into()),
            ]
        );
        assert_eq!(v.cursor, 4);
    }

    #[test]
    fn a_confirmation_is_pending_until_its_result_arrives() {
        let mut v = ChatView::default();
        v.apply("s", &[ev(1, "chat_confirm", json!({"approval_id": 7, "summary": "cancel goal_a_1"}))]);
        assert_eq!(v.pending, Some(7));
        v.apply("s", &[ev(2, "chat_result", json!({"approval_id": 8, "outcome": "approved"}))]);
        assert_eq!(v.pending, Some(7), "another approval's result does not clear it");
        v.apply("s", &[ev(3, "chat_result", json!({"approval_id": 7, "outcome": "denied"}))]);
        assert_eq!(v.pending, None);
    }

    #[test]
    fn typing_editing_and_submitting() {
        let mut v = ChatView::default();
        for c in "how is the pool".chars() {
            v.type_char(c);
        }
        v.type_char('\n');
        v.type_char('?');
        v.backspace();
        assert_eq!(v.input, "how is the pool", "control characters are ignored and backspace works");
        assert_eq!(v.submit(), Some(Submit::Send("how is the pool".into())));
        assert!(v.input.is_empty());
        assert_eq!(v.submit(), None, "an empty line sends nothing");
    }

    #[test]
    fn a_bare_yes_or_no_answers_a_pending_confirmation_but_other_text_is_a_new_message() {
        let mut v = ChatView { pending: Some(7), ..ChatView::default() };
        v.input = "yes".into();
        assert_eq!(v.submit(), Some(Submit::Confirm { approval_id: 7, allow: true, remember: false }));
        v.input = "always".into();
        assert_eq!(v.submit(), Some(Submit::Confirm { approval_id: 7, allow: true, remember: true }));
        v.input = "No".into();
        assert_eq!(v.submit(), Some(Submit::Confirm { approval_id: 7, allow: false, remember: false }));
        v.input = "yes but first tell me what is running".into();
        assert_eq!(v.submit(), Some(Submit::Send("yes but first tell me what is running".into())));
        v.pending = None;
        v.input = "yes".into();
        assert_eq!(v.submit(), Some(Submit::Send("yes".into())), "with nothing pending, yes is just text");
    }

    #[test]
    fn nothing_is_sent_while_a_request_is_in_flight() {
        let mut v = ChatView { busy: true, ..ChatView::default() };
        v.input = "status".into();
        assert_eq!(v.submit(), None);
        assert_eq!(v.input, "status", "the typed line is kept for when it is free");
    }

    #[test]
    fn switching_session_starts_a_fresh_view_but_keeps_what_you_typed() {
        let mut v = ChatView::default();
        v.apply("a", &[ev(1, "chat_user", json!({"text": "hi"}))]);
        v.input = "half typed".into();
        v.apply("b", &[ev(1, "chat_user", json!({"text": "other thread"}))]);
        assert_eq!(v.entries, vec![Entry::You("other thread".into())]);
        assert_eq!(v.input, "half typed");
    }

    #[test]
    fn wrapping_respects_width_and_splits_long_words() {
        assert_eq!(wrap("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 10), [""]);
    }

    #[test]
    fn rows_label_indent_and_style_each_entry() {
        let mut v = ChatView::default();
        v.entries = vec![
            Entry::You("cancel it".into()),
            Entry::Divisi { text: "this is a long reply that has to wrap across lines".into(), degraded: false },
            Entry::Error("daemon unreachable".into()),
        ];
        let rows = v.rows(30);
        assert_eq!(rows[0], (RowKind::You, "you     cancel it".to_owned()));
        assert!(rows[1].1.starts_with("divisi  this is a long reply"));
        assert!(rows[2].1.starts_with("        "), "continuation lines are indented under the text");
        assert!(rows.iter().all(|(_, r)| r.chars().count() <= 30), "{rows:?}");
        assert_eq!(rows.last().unwrap().0, RowKind::Error);
    }
}
