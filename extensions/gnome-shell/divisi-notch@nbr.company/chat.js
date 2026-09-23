// The notch Chat tab's logic, free of GNOME Shell imports so node can test it. It mirrors
// divisi-tui's ChatView: events from `divisi chat tail --json` are applied to a small state
// object, and a typed line becomes a send or a confirmation.

/** A fresh conversation view. */
export function newChat() {
    return {session: null, cursor: 0, entries: [], pending: null, busy: false};
}

function parse(body) {
    try {
        const v = JSON.parse(body);
        return v && typeof v === 'object' ? v : null;
    } catch (_e) {
        return null;
    }
}

/** A `chat_*` event as an entry, or null for any other kind. A malformed body is shown as-is. */
export function chatEntry(kind, body) {
    const v = parse(body);
    const pick = key => (v && typeof v[key] === 'string' ? v[key] : body);
    switch (kind) {
    case 'chat_user':
        return {type: 'you', text: pick('text')};
    case 'chat_assistant':
        return {type: 'divisi', text: pick('text'), degraded: !!(v && v.degraded)};
    case 'chat_confirm':
        if (!v || typeof v.approval_id !== 'number')
            return null;
        return {type: 'confirm', text: pick('summary'), approvalId: v.approval_id, rememberOk: !!v.remember_ok};
    case 'chat_result':
        return {type: 'result', text: pick('outcome'), approvalId: v && typeof v.approval_id === 'number' ? v.approval_id : null};
    default:
        return null;
    }
}

/** A one-line note for the goal events worth reading in a conversation, or null. */
export function progressText(kind, body) {
    const labels = {integrated: 'done', blocked: 'blocked', node_failed: 'a step failed', capacity_wait: 'waiting on capacity', budget: 'budget'};
    const label = labels[kind];
    if (!label)
        return null;
    const first = String(body).split('\n').map(l => l.trim()).find(l => l) ?? '';
    const short = first.slice(0, 140);
    return short ? `${label}: ${short}` : label;
}

const MAX_REPLY_CHARS = 600;
const MAX_REPLY_LINES = 10;

/**
 * The text a row shows. A long reply is cut on a word boundary with a pointer to the TUI or
 * Zed, which show it whole; the card is too small for it.
 */
export function shownText(entry) {
    let text = entry.text;
    if (entry.type === 'divisi') {
        let cut = text.split('\n').slice(0, MAX_REPLY_LINES).join('\n');
        if (cut.length > MAX_REPLY_CHARS) {
            cut = cut.slice(0, MAX_REPLY_CHARS);
            const space = cut.search(/\s\S*$/);
            if (space > 0)
                cut = cut.slice(0, space);
        }
        if (cut.trimEnd().length < text.trimEnd().length)
            text = `${cut.trimEnd()}…\n(longer reply — open it in the TUI or Zed)`;
        if (entry.degraded)
            text += '  (rules only)';
    }
    return text;
}

/**
 * Applies events. Anything at or below the cursor is ignored, so overlapping polls never show
 * a line twice. A different session starts a fresh view.
 */
export function applyEvents(chat, session, events) {
    if (chat.session !== session) {
        chat.session = session;
        chat.cursor = 0;
        chat.entries = [];
        chat.pending = null;
    }
    const start = chat.cursor;
    let changed = false;
    for (const e of events) {
        if (!(e.id > start))
            continue;
        chat.cursor = Math.max(chat.cursor, e.id);
        const entry = chatEntry(e.kind, e.body);
        if (entry) {
            if (entry.type === 'confirm')
                chat.pending = entry.approvalId;
            if (entry.type === 'result' && entry.approvalId !== null && entry.approvalId === chat.pending)
                chat.pending = null;
            chat.entries.push(entry);
            changed = true;
            continue;
        }
        const note = progressText(e.kind, e.body);
        if (note) {
            chat.entries.push({type: 'progress', text: note});
            changed = true;
        }
    }
    return changed;
}

/**
 * What a typed line means: a message to send, or (while a confirmation is pending) a bare
 * yes, no or "always" answering it. Null for an empty line or while a request is in flight.
 */
export function submitInput(chat, input) {
    const line = input.trim();
    if (!line || chat.busy)
        return null;
    const lower = line.toLowerCase();
    if (chat.pending !== null) {
        if (['y', 'yes', 'ok', 'approve'].includes(lower))
            return {type: 'confirm', approvalId: chat.pending, allow: true, remember: false};
        if (lower === 'always')
            return {type: 'confirm', approvalId: chat.pending, allow: true, remember: true};
        if (['n', 'no', 'nope', 'deny'].includes(lower))
            return {type: 'confirm', approvalId: chat.pending, allow: false, remember: false};
    }
    return {type: 'send', text: line};
}

/** Turns `divisi chat tail --json` output (one JSON object per line) into events plus the session id. */
export function parseTail(output) {
    const events = [];
    let session = null;
    for (const line of String(output).split('\n')) {
        if (!line.trim())
            continue;
        const e = parse(line);
        if (!e || typeof e.id !== 'number')
            continue;
        if (typeof e.session === 'string')
            session = e.session;
        events.push(e);
    }
    return {session, events};
}
