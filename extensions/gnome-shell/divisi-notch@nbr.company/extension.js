// divisi notch: a right-edge, vertically centered overlay drawn as shell
// chrome (not a window, so tiling extensions never see it). Data comes from
// `divisi-notch --snapshot`, which reuses the Rust aggregation.
//
// States: hidden (faint sliver) -> peek (hover) -> card (click, tabbed).
// Dismissal never relies on enter/leave events of individual rows (they are
// destroyed on every refresh): a pointer watcher decides what the pointer is
// over, and stage-level press/Esc handlers dismiss on click-outside.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Pango from 'gi://Pango';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import Cairo from 'cairo';
import * as Chat from './chat.js';

// divisi brand tokens (graphite / bone / signal / go / caution / fault). The names below keep
// their old role (TEAL = ok, AMBER = warn, RED = error, BLUE = neutral info) so call sites stay put.
const GRAPHITE = '#16181d';
const TEAL = '#3ddc97';
const AMBER = '#ffd23f';
const RED = '#ff2d55';
const BLUE = '#f2f2f0';
const MUTED = '#7d818a';
const TEXT = '#f2f2f0';
const SURFACE = 'rgba(242,242,240,0.06)';

const W_HIDDEN = 6;
const W_CARD = 440;
const H_HIDDEN = 72;
const PAD = 14;
const ANIM_MS = 200;
const FADE_MS = 140;

const POLL_VISIBLE_S = 2;
const POLL_HIDDEN_S = 8;
const WATCH_MS = 90;
const BREATH_MS = 900;
const LEAVE_PEEK_MS = 600;
const LEAVE_CARD_MS = 1300;
const NOTABLE_PEEK_MS = 3800;
const SIGNAL = [0xff / 255, 0x5a / 255, 0x1f / 255];
const MARK_SIZE = 20;

const TABS = [['overview', 'Overview'], ['goals', 'Goals'], ['chat', 'Chat'], ['pool', 'Pool'], ['agents', 'Agents'], ['usage', 'Usage']];
const SIGNAL_CSS = '#ff5a1f';
// A keyboard grab must never be able to trap the desktop: with no typing for this long it lets go.
const CHAT_IDLE_MS = 45000;
const CHAT_GUARD_MS = 100;

// [label, colour] per provider auth_state / agent class (see divisi_core::auth_class).
const PROVIDER_STATES = [
    ['authed', 'AUTHED', TEAL],
    ['no_auth_needed', 'NO AUTH NEEDED', BLUE],
    ['unverified', 'UNVERIFIED', AMBER],
    ['invalid', 'KEY REJECTED', RED],
    ['disabled', 'DISABLED', MUTED],
    ['blocked', 'SIGNUP BLOCKED', MUTED],
    ['no_key', 'NO KEY', MUTED],
];
const AGENT_CLASSES = [
    ['authed', 'AUTHED', TEAL],
    ['needs_login', 'NEEDS LOGIN', RED],
    ['unverified', 'CAN\'T VERIFY LOGIN', AMBER],
    ['no_auth_needed', 'NO AUTH NEEDED', BLUE],
    ['not_installed', 'NOT INSTALLED', MUTED],
];

const toneColor = tone => ({healthy: TEAL, amber: AMBER, degraded: RED}[tone] ?? MUTED);
const statusColor = st => ({
    running: TEAL, completed: TEAL, authenticated: TEAL,
    waiting_on_capacity: AMBER, created: AMBER,
    queued: BLUE,
    blocked: RED, failed: RED, not_authenticated: RED,
}[st] ?? MUTED);
const TONE_WORD = {healthy: 'Healthy', amber: 'Attention', degraded: 'Degraded'};

function fmtDuration(secs) {
    secs = Math.max(0, Math.round(secs));
    if (secs < 60)
        return `${secs}s`;
    if (secs < 3600)
        return `${Math.floor(secs / 60)}m ${secs % 60}s`;
    return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

function relTime(iso) {
    const t = Date.parse(iso);
    if (Number.isNaN(t))
        return '';
    const d = (Date.now() - t) / 1000;
    if (d < 0)
        return `in ${fmtDuration(-d)}`;
    if (d < 45)
        return 'now';
    return `${fmtDuration(d).split(' ')[0]} ago`;
}

function fmtNum(n) {
    if (n >= 1e6)
        return `${(n / 1e6).toFixed(1)}M`;
    if (n >= 1e3)
        return `${(n / 1e3).toFixed(1)}K`;
    return String(n);
}

function shortVersion(v) {
    if (!v)
        return '';
    const m = v.match(/\d+(?:\.\d+)+[\w.+-]*/);
    const out = m ? m[0] : v.split('\n')[0];
    return out.length > 14 ? `${out.slice(0, 13)}…` : out;
}

export default class SingleNotch extends Extension {
    enable() {
        this._state = 'hidden';
        this._tab = 'overview';
        this._snapshot = null;
        this._raw = '';
        this._offline = false;
        this._lastFetch = 0;
        this._fetching = false;
        this._timers = new Set();
        this._leaveTimer = 0;
        this._watchId = 0;
        this._tipTargets = [];
        this._tipActive = null;
        this._pulseDots = [];
        this._breathHigh = false;
        this._scroll = null;
        this._scrollPos = 0;
        this._cancellable = new Gio.Cancellable();
        this._frames = null;
        this._markArea = null;
        this._markRunning = false;
        this._mark = {phase: 'idle', t0: 0, exitAt: 0};
        this._chat = Chat.newChat();
        this._chatEntry = null;
        this._chatGrab = null;
        this._chatFocused = false;
        this._chatRefocus = false;
        this._chatFetching = false;
        this._chatStick = false;
        this._chatIdleMs = CHAT_IDLE_MS;
        this._chatLastActive = 0;
        this._chatGuardId = 0;
        this._addTimer(1500, () => {
            if (this._root && this._state === 'card' && this._tab === 'chat')
                this._chatFetch();
            return GLib.SOURCE_CONTINUE;
        });
        try {
            const [, bytes] = Gio.File.new_for_path(`${this.path}/mark-frames.json`).load_contents(null);
            this._frames = JSON.parse(new TextDecoder().decode(bytes));
        } catch (e) {
            console.warn(`divisi notch: mark-frames.json unavailable (${e.message}); using a static mark`);
        }
        this._addTimer(33, () => {
            if (this._markArea?.mapped && (this._markRunning || this._mark.phase !== 'idle'))
                this._markArea.queue_repaint();
            return GLib.SOURCE_CONTINUE;
        });

        this._root = new St.BoxLayout({
            vertical: true,
            reactive: true,
            track_hover: true,
            clip_to_allocation: true,
            style: this._rootStyle(MUTED),
        });
        this._bar = new St.Widget({x_expand: true, y_expand: true, style: `background-color: ${MUTED};`});
        this._content = new St.BoxLayout({vertical: true, x_expand: true, y_expand: true});
        this._root.add_child(this._bar);
        this._root.add_child(this._content);

        // The layout manager rewrites `visible` on every tracked chrome actor
        // whenever focus/overview state changes, which would silently undo
        // hide(); the tip is hidden with opacity + parking off-screen instead.
        this._tipShown = false;
        this._tip = new St.Label({
            opacity: 0,
            reactive: false,
            style: 'background-color: rgba(22,24,29,0.97); color: ' + TEXT + '; border-radius: 9px; ' +
                'border: 1px solid #2c2f37; padding: 7px 11px; font-size: 12px; ' +
                'max-width: 320px;',
        });
        this._tip.clutter_text.line_wrap = true;
        this._tip.set_position(-4000, -4000);

        Main.layoutManager.addChrome(this._root, {affectsStruts: false, trackFullscreen: true});
        Main.layoutManager.addChrome(this._tip, {affectsStruts: false, trackFullscreen: true});

        this._hoverId = this._root.connect('notify::hover', () => {
            if (this._root?.hover && this._state === 'hidden')
                this._setState('peek');
        });
        this._pressId = this._root.connect('button-press-event', () => {
            if (this._state === 'peek')
                this._openTab(this._tab);
            return Clutter.EVENT_STOP;
        });
        this._stageId = global.stage.connect('captured-event', (_s, event) => this._onStageEvent(event));
        this._monitorsId = Main.layoutManager.connect('monitors-changed', () => this._place(false));

        this._render(false);
        this._place(false);
        this._fetch();
        this._addTimer(1000, () => {
            this._tickPoll();
            return GLib.SOURCE_CONTINUE;
        });
        this._addTimer(BREATH_MS, () => {
            this._breathe();
            return GLib.SOURCE_CONTINUE;
        });
    }

    disable() {
        for (const id of this._timers)
            GLib.source_remove(id);
        this._timers = null;
        this._cancellable?.cancel();
        if (this._stageId)
            global.stage.disconnect(this._stageId);
        if (this._monitorsId)
            Main.layoutManager.disconnect(this._monitorsId);
        for (const actor of [this._root, this._tip]) {
            if (!actor)
                continue;
            actor.remove_all_transitions();
            Main.layoutManager.removeChrome(actor);
            actor.destroy();
        }
        this._root = this._tip = this._bar = this._content = this._scroll = null;
        this._snapshot = null;
        this._tipTargets = [];
        this._pulseDots = [];
        this._releaseChatFocus();
        this._chatEntry = null;
        this._chat = null;
        this._frames = this._markArea = null;
        this._markRunning = false;
        this._mark = null;
    }

    // ---- timers -------------------------------------------------------------

    _addTimer(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            const keep = fn();
            if (keep === GLib.SOURCE_REMOVE)
                this._timers?.delete(id);
            return keep;
        });
        this._timers.add(id);
        return id;
    }

    _clearTimer(id) {
        if (id && this._timers?.delete(id))
            GLib.source_remove(id);
    }

    _rootStyle(accent) {
        return 'background-color: rgba(22,24,29,0.96); border-radius: 16px 0 0 16px; ' +
            'border: 1px solid #2c2f37; border-right-width: 0; ' +
            `border-left: 2px solid ${accent};`;
    }

    // ---- data -----------------------------------------------------------------

    _tickPoll() {
        const interval = this._state === 'hidden' ? POLL_HIDDEN_S : POLL_VISIBLE_S;
        if (GLib.get_monotonic_time() / 1e6 - this._lastFetch >= interval)
            this._fetch();
    }

    _fetch() {
        if (this._fetching || !this._root)
            return;
        this._fetching = true;
        this._lastFetch = GLib.get_monotonic_time() / 1e6;
        const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'divisi-notch']);
        const bin = GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE) ? local : 'divisi-notch';
        try {
            const proc = Gio.Subprocess.new([bin, '--snapshot'],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE);
            proc.communicate_utf8_async(null, this._cancellable, (p, res) => {
                this._fetching = false;
                if (!this._root)
                    return;
                try {
                    const [, out] = p.communicate_utf8_finish(res);
                    if (!p.get_successful())
                        throw new Error('snapshot failed');
                    this._apply(out.trim(), JSON.parse(out));
                } catch (e) {
                    this._applyOffline();
                }
            });
        } catch (e) {
            this._fetching = false;
            this._applyOffline();
        }
    }

    _applyOffline() {
        if (this._offline)
            return;
        this._offline = true;
        this._snapshot = null;
        this._raw = '';
        this._render(false);
        this._place(this._state !== 'hidden');
    }

    _apply(raw, snap) {
        const prev = this._snapshot;
        const wasOffline = this._offline;
        this._offline = false;
        if (raw === this._raw && !wasOffline)
            return;
        this._raw = raw;
        this._snapshot = snap;
        this._render(false);
        this._place(this._state !== 'hidden');
        if (prev && this._isNotable(prev, snap) && this._state === 'hidden') {
            this._setState('peek');
            this._cancelLeave();
            this._scheduleLeave(NOTABLE_PEEK_MS);
        }
    }

    _isNotable(a, b) {
        const da = a.detail, db = b.detail;
        return a.tone !== b.tone ||
            b.benches.length > a.benches.length ||
            db.goals_blocked > da.goals_blocked ||
            db.goals_running !== da.goals_running;
    }

    // ---- interaction ------------------------------------------------------------

    _contains(actor, x, y, slack = 0) {
        if (!actor?.mapped)
            return false;
        const [ax, ay] = actor.get_transformed_position();
        const [w, h] = actor.get_transformed_size();
        return x >= ax - slack && x <= ax + w + slack && y >= ay - slack && y <= ay + h + slack;
    }

    _onStageEvent(event) {
        if (!this._root || this._state === 'hidden')
            return Clutter.EVENT_PROPAGATE;
        const type = event.type();
        if (type === Clutter.EventType.BUTTON_PRESS) {
            const [x, y] = event.get_coords();
            if (!this._contains(this._root, x, y))
                this._setState('hidden');
        } else if (type === Clutter.EventType.KEY_PRESS && event.get_key_symbol() === Clutter.KEY_Escape) {
            this._setState('hidden');
        }
        return Clutter.EVENT_PROPAGATE;
    }

    // Runs while visible. Single source of truth for "is the pointer on us"
    // and "which item is it over", independent of widget lifetimes.
    _watch() {
        if (!this._root || this._state === 'hidden')
            return GLib.SOURCE_REMOVE;
        const [x, y] = global.get_pointer();
        if (this._contains(this._root, x, y, 1)) {
            this._cancelLeave();
            this._updateTip(x, y);
        } else {
            this._hideTip();
            this._scheduleLeave(this._state === 'card' ? LEAVE_CARD_MS : LEAVE_PEEK_MS);
        }
        return GLib.SOURCE_CONTINUE;
    }

    _startWatch() {
        if (!this._watchId)
            this._watchId = this._addTimer(WATCH_MS, () => {
                const keep = this._watch();
                if (keep === GLib.SOURCE_REMOVE)
                    this._watchId = 0;
                return keep;
            });
    }

    _scheduleLeave(ms) {
        if (this._leaveTimer)
            return;
        this._leaveTimer = this._addTimer(ms, () => {
            this._leaveTimer = 0;
            if (this._chatFocused)
                return GLib.SOURCE_REMOVE;
            if (this._root && this._state !== 'hidden') {
                const [x, y] = global.get_pointer();
                if (!this._contains(this._root, x, y, 1))
                    this._setState('hidden');
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    _cancelLeave() {
        this._clearTimer(this._leaveTimer);
        this._leaveTimer = 0;
    }

    _setState(state, {fade = true} = {}) {
        if (!this._root || this._state === state)
            return;
        this._state = state;
        if (state !== 'card')
            this._releaseChatFocus();
        this._hideTip();
        this._cancelLeave();
        this._render(fade);
        this._place(true);
        if (state === 'hidden') {
            this._clearTimer(this._watchId);
            this._watchId = 0;
        } else {
            this._startWatch();
            this._fetch();
        }
        this._breathe(true);
    }

    _selectTab(id) {
        if (this._tab === id)
            return;
        this._tab = id;
        this._scrollPos = 0;
        this._hideTip();
        this._render(true);
        this._place(true);
    }

    // ---- tooltips -----------------------------------------------------------------

    _tipFor(actor, text, clip = null) {
        this._tipTargets.push({actor, text, clip});
    }

    _updateTip(x, y) {
        let hit = null;
        for (const t of this._tipTargets) {
            if (t.actor.mapped && this._contains(t.actor, x, y) && (!t.clip || this._contains(t.clip, x, y)))
                hit = t;
        }
        if (!hit) {
            this._hideTip();
            return;
        }
        if (this._tipActive?.text === hit.text && this._tipShown) {
            this._tipActive = hit;
            return;
        }
        this._tipActive = hit;
        this._tip.text = hit.text;
        const [, ay] = hit.actor.get_transformed_position();
        const [, tipH] = this._tip.get_preferred_height(-1);
        const [, tipW] = this._tip.get_preferred_width(-1);
        const mon = Main.layoutManager.primaryMonitor;
        const [rx] = this._root.get_transformed_position();
        const ty = Math.max(mon.y + 6, Math.min(ay + hit.actor.height / 2 - tipH / 2, mon.y + mon.height - tipH - 6));
        this._tip.set_position(Math.round(rx - tipW - 10), Math.round(ty));
        if (!this._tipShown) {
            this._tipShown = true;
            this._tip.remove_all_transitions();
            this._tip.ease({opacity: 255, duration: 110, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        }
    }

    _hideTip() {
        this._tipActive = null;
        this._tipShown = false;
        if (!this._tip)
            return;
        this._tip.remove_all_transitions();
        this._tip.set({opacity: 0});
        this._tip.set_position(-4000, -4000);
    }

    // ---- layout ---------------------------------------------------------------------

    _cardWidth() {
        return W_CARD;
    }

    _targetSize() {
        const mon = Main.layoutManager.primaryMonitor;
        if (this._state === 'hidden')
            return [W_HIDDEN, H_HIDDEN];
        if (this._state === 'peek') {
            const [, pw] = this._content.get_preferred_width(-1);
            const [, ph] = this._content.get_preferred_height(pw);
            return [Math.ceil(pw) + 2, Math.ceil(ph) + 2];
        }
        const maxH = Math.round(mon.height * 0.78);
        const [, natH] = this._content.get_preferred_height(W_CARD);
        return [W_CARD, Math.min(natH, maxH)];
    }

    _place(animate) {
        if (!this._root)
            return;
        const mon = Main.layoutManager.primaryMonitor;
        const [w, h] = this._targetSize();
        const props = {
            x: mon.x + mon.width - w,
            y: mon.y + Math.round((mon.height - h) / 2),
            width: w,
            height: h,
        };
        this._root.remove_all_transitions();
        if (animate)
            this._root.ease({...props, duration: ANIM_MS, mode: Clutter.AnimationMode.EASE_OUT_CUBIC});
        else
            this._root.set({...props});
    }

    // ---- breathing indicators ------------------------------------------------------------

    _attention() {
        const s = this._snapshot;
        return this._offline || !s || s.tone !== 'healthy' || s.detail.goals_running > 0 || s.detail.goals_blocked > 0;
    }

    _breathe(now = false) {
        if (!this._root)
            return;
        if (!now)
            this._breathHigh = !this._breathHigh;
        const active = this._attention();
        if (this._state === 'hidden') {
            const rest = active ? 0.55 : 0.28;
            const target = active ? (this._breathHigh ? 255 : Math.round(255 * 0.35)) : Math.round(255 * rest);
            this._bar.ease({opacity: target, duration: BREATH_MS - 100, mode: Clutter.AnimationMode.EASE_IN_OUT_SINE});
        } else {
            this._bar.opacity = 255;
        }
        for (const dot of this._pulseDots) {
            if (dot.mapped)
                dot.ease({opacity: this._breathHigh ? 255 : 110, duration: BREATH_MS - 100, mode: Clutter.AnimationMode.EASE_IN_OUT_SINE});
        }
    }

    // ---- widgets ---------------------------------------------------------------------------

    // ---- chat --------------------------------------------------------------------

    // Redraw the chat and resize the card to fit, as a snapshot refresh does.
    _redrawChat() {
        if (!this._root || this._state !== 'card' || this._tab !== 'chat')
            return;
        this._render(false);
        this._place(true);
    }

    _divisiBin() {
        const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'divisi']);
        return GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE) ? local : 'divisi';
    }

    _chatRun(args, done) {
        try {
            const proc = Gio.Subprocess.new([this._divisiBin(), ...args],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
            proc.communicate_utf8_async(null, this._cancellable, (p, res) => {
                if (!this._root)
                    return;
                try {
                    const [, out, err] = p.communicate_utf8_finish(res);
                    done(p.get_successful(), out ?? '', err ?? '');
                } catch (e) {
                    done(false, '', String(e.message ?? e));
                }
            });
        } catch (e) {
            done(false, '', String(e.message ?? e));
        }
    }

    // Pulls conversation events newer than the cursor and redraws if there are any.
    _chatFetch() {
        if (this._chatFetching || !this._root)
            return;
        this._chatFetching = true;
        this._chatRun(['chat', 'tail', '--json', '--after', String(this._chat.cursor)], (ok, out) => {
            this._chatFetching = false;
            if (!ok || !this._root)
                return;
            const {session, events} = Chat.parseTail(out);
            if (session && Chat.applyEvents(this._chat, session, events)) {
                this._chatStick = true;
                this._redrawChat();
            }
        });
    }

    _chatSubmit() {
        const action = Chat.submitInput(this._chat, this._chatEntry.get_text());
        if (!action)
            return;
        this._chatEntry.set_text('');
        this._chat.busy = true;
        const args = action.type === 'send'
            ? ['chat', 'send', '--surface', 'notch', '--json', '--', action.text]
            : ['chat', 'confirm', String(action.approvalId), action.allow ? '--allow' : '--deny', ...(action.remember ? ['--remember'] : [])];
        this._chatStick = true;
        this._redrawChat();
        this._chatRun(args, (ok, _out, err) => {
            this._chat.busy = false;
            if (!ok) {
                const first = (err || 'that did not go through').trim().split('\n')[0];
                this._chat.entries.push({type: 'error', text: first.slice(0, 200)});
            }
            this._chatFetch();
            this._redrawChat();
        });
    }

    _answer(approvalId, allow, remember) {
        this._chatEntry?.set_text('');
        this._chat.busy = true;
        this._chatStick = true;
        this._redrawChat();
        this._chatRun(['chat', 'confirm', String(approvalId), allow ? '--allow' : '--deny', ...(remember ? ['--remember'] : [])], (ok, _out, err) => {
            this._chat.busy = false;
            if (!ok)
                this._chat.entries.push({type: 'error', text: (err || 'that did not go through').trim().split('\n')[0].slice(0, 200)});
            this._chatFetch();
            this._redrawChat();
        });
    }

    // A modal grab gives the entry the keyboard while the card is open; it is released
    // whenever the card collapses so the desktop is never left without keyboard focus.
    _focusChat() {
        if (!this._chatEntry?.mapped)
            return;
        if (!this._chatFocused) {
            const grab = Main.pushModal(this._chatEntry);
            if (!grab)
                return;
            this._chatGrab = grab;
            this._chatFocused = true;
            this._startChatGuard();
        }
        this._touchChat();
        this._chatEntry.clutter_text.grab_key_focus();
    }

    _touchChat() {
        this._chatLastActive = GLib.get_monotonic_time() / 1000;
    }

    // While the grab is held the stage-level handlers that normally close the card never see input, so
    // the grab has to police itself: a button press outside the card, or a long silence, lets go.
    _startChatGuard() {
        if (this._chatGuardId)
            return;
        this._chatGuardId = this._addTimer(CHAT_GUARD_MS, () => {
            if (!this._chatFocused || !this._root) {
                this._chatGuardId = 0;
                return GLib.SOURCE_REMOVE;
            }
            const [x, y, mods] = global.get_pointer();
            const pressed = (mods & Clutter.ModifierType.BUTTON1_MASK) !== 0;
            if (pressed && !this._contains(this._root, x, y)) {
                this._chatGuardId = 0;
                this._leaveChat(true);
                return GLib.SOURCE_REMOVE;
            }
            if (GLib.get_monotonic_time() / 1000 - this._chatLastActive > this._chatIdleMs) {
                this._chatGuardId = 0;
                this._leaveChat(false);
                return GLib.SOURCE_REMOVE;
            }
            return GLib.SOURCE_CONTINUE;
        });
    }

    // Lets go of the keyboard; with `collapse` the card closes as well.
    _leaveChat(collapse) {
        this._releaseChatFocus();
        if (collapse)
            this._setState('hidden');
        else if (this._state === 'card')
            this._redrawChat();
    }

    _releaseChatFocus() {
        if (this._chatGrab) {
            try {
                Main.popModal(this._chatGrab);
            } catch (e) {
                console.warn(`divisi notch: releasing the chat grab failed (${e.message})`);
            }
        }
        this._chatGrab = null;
        this._chatFocused = false;
        if (this._chatGuardId) {
            this._clearTimer?.(this._chatGuardId);
            this._chatGuardId = 0;
        }
    }

    _ensureChatEntry() {
        if (!this._chatEntry) {
            this._chatEntry = new St.Entry({
                can_focus: true, x_expand: true,
                style: `color: ${TEXT}; caret-color: ${TEXT}; background-color: rgba(242,242,240,0.07); border-radius: 8px; padding: 5px 8px; font-size: 12px;`,
            });
            this._chatEntry.clutter_text.connect('activate', () => this._chatSubmit());
            this._chatEntry.clutter_text.connect('key-press-event', (_actor, event) => {
                const sym = event.get_key_symbol();
                if (sym === Clutter.KEY_Escape) {
                    this._leaveChat(true);
                    return Clutter.EVENT_STOP;
                }
                if (sym === Clutter.KEY_Tab || sym === Clutter.KEY_ISO_Left_Tab) {
                    this._leaveChat(false);
                    return Clutter.EVENT_STOP;
                }
                this._touchChat();
                return Clutter.EVENT_PROPAGATE;
            });
            this._chatEntry.clutter_text.connect('key-focus-out', () => {
                if (this._chatFocused && this._chatEntry && !this._chatEntry.clutter_text.has_key_focus())
                    GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                        if (this._chatFocused && !this._chatEntry?.clutter_text.has_key_focus())
                            this._leaveChat(false);
                        return GLib.SOURCE_REMOVE;
                    });
            });
            this._chatEntry.connect('button-press-event', () => {
                this._focusChat();
                return Clutter.EVENT_STOP;
            });
        }
        this._chatEntry.hint_text = this._chat.pending !== null ? 'yes / no / always' : 'say something…';
        return this._chatEntry;
    }

    _chatRow(entry) {
        const styleFor = {
            you: [BLUE, 'you'], divisi: [TEXT, 'divisi'], confirm: [AMBER, '?'],
            result: [MUTED, '='], progress: [MUTED, '·'], error: [RED, 'error'],
        }[entry.type] ?? [TEXT, ''];
        const row = new St.BoxLayout({x_expand: true, style: 'spacing: 8px; padding: 2px 0;'});
        const tag = new St.Label({text: styleFor[1], y_align: Clutter.ActorAlign.START, style: `color: ${MUTED}; font-size: 10px; min-width: 40px; padding-top: 2px;`});
        const body = new St.Label({text: Chat.shownText(entry), x_expand: true, style: `color: ${styleFor[0]}; font-size: 12px;`});
        body.clutter_text.line_wrap = true;
        body.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
        body.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
        row.add_child(tag);
        const col = new St.BoxLayout({vertical: true, x_expand: true, style: 'spacing: 4px;'});
        col.add_child(body);
        if (entry.type === 'confirm' && entry.approvalId === this._chat.pending) {
            const buttons = new St.BoxLayout({style: 'spacing: 6px;'});
            const mk = (label, allow, remember) => {
                const b = new St.Button({label, reactive: true, can_focus: true, style: `color: ${TEXT}; background-color: rgba(242,242,240,0.10); border-radius: 7px; padding: 3px 10px; font-size: 11px;`});
                b.connect('clicked', () => this._answer(entry.approvalId, allow, remember));
                return b;
            };
            buttons.add_child(mk('Yes', true, false));
            buttons.add_child(mk('No', false, false));
            if (entry.rememberOk)
                buttons.add_child(mk('Always', true, true));
            col.add_child(buttons);
        }
        row.add_child(col);
        return row;
    }

    _tabChat(body) {
        if (this._offline) {
            body.add_child(this._empty('The daemon is not answering, so there is nothing to talk to. Run `divisi daemon restart`.'));
            return;
        }
        if (!this._chat.entries.length) {
            body.add_child(this._empty('Say what you want in plain language.\nhow are things? · how much have I used? · add tests for the parser · cancel goal_…\nRisky actions ask you first.'));
            this._chatFetch();
        }
        for (const entry of this._chat.entries.slice(-40))
            body.add_child(this._chatRow(entry));
        if (this._chat.busy)
            body.add_child(this._label('thinking…', {color: MUTED, size: 11}));
        const input = new St.BoxLayout({x_expand: true, style: 'spacing: 6px; padding: 8px 0 0 0;'});
        input.add_child(this._label('/', {color: SIGNAL_CSS, bold: true, size: 14}));
        input.add_child(this._ensureChatEntry());
        body.add_child(input);
        if (this._chatRefocus || this._chatFocused) {
            this._chatRefocus = false;
            GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                if (this._root && this._state === 'card' && this._tab === 'chat')
                    this._focusChat();
                return GLib.SOURCE_REMOVE;
            });
        }
        if (this._chatStick) {
            this._chatStick = false;
            GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                const adj = this._vadj();
                if (adj)
                    adj.value = Math.max(0, adj.upper - adj.page_size);
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    // At rest the mark is the obelus. While work runs it is a spinning slash: `enter` plays
    // once (the dots collapse as the bar spins into the slash), `spin` loops, and when work
    // ends the current turn finishes, then `exit` plays once and the obelus is back.
    _markFrame() {
        const f = this._frames;
        if (!f)
            return {a: 0, l: 32, o: 14, r: 4.6};
        if (!St.Settings.get().enable_animations)
            return this._markRunning ? f.spin[0] : f.rest;
        const now = GLib.get_monotonic_time() / 1e6;
        const m = this._mark;
        if (this._markRunning && (m.phase === 'idle' || m.phase === 'exit')) {
            m.phase = 'enter';
            m.t0 = now;
        }
        if (m.phase === 'enter') {
            const i = Math.floor((now - m.t0) * f.fps);
            if (i < f.enter.length)
                return f.enter[i];
            m.phase = 'spin';
            m.t0 = now;
            m.exitAt = 0;
        }
        if (m.phase === 'spin') {
            const dur = f.spin.length / f.fps;
            const el = now - m.t0;
            if (!this._markRunning && !m.exitAt)
                m.exitAt = m.t0 + Math.ceil(el / dur) * dur;
            if (m.exitAt && now >= m.exitAt) {
                m.phase = 'exit';
                m.t0 = m.exitAt;
                m.exitAt = 0;
            } else {
                return f.spin[Math.floor((el % dur) * f.fps) % f.spin.length];
            }
        }
        if (m.phase === 'exit') {
            const i = Math.floor((now - m.t0) * f.fps);
            if (i < f.exit.length)
                return f.exit[i];
            m.phase = 'idle';
        }
        return f.rest;
    }

    _markWidget(running) {
        this._markRunning = running;
        const area = new St.DrawingArea({width: MARK_SIZE, height: MARK_SIZE, y_align: Clutter.ActorAlign.CENTER});
        area.connect('repaint', () => {
            const cr = area.get_context();
            const [w, h] = area.get_surface_size();
            const fr = this._markFrame();
            cr.scale(Math.min(w, h) / 48, Math.min(w, h) / 48);
            cr.setSourceRGBA(SIGNAL[0], SIGNAL[1], SIGNAL[2], 1);
            cr.save();
            cr.translate(24, 24);
            cr.rotate(fr.a * Math.PI / 180);
            cr.rectangle(-fr.l / 2, -3, fr.l, 6);
            cr.fill();
            cr.restore();
            if (fr.r > 0.05) {
                cr.arc(24, 24 - fr.o, fr.r, 0, 2 * Math.PI);
                cr.fill();
                cr.arc(24, 24 + fr.o, fr.r, 0, 2 * Math.PI);
                cr.fill();
            }
            cr.$dispose();
        });
        area.connect('destroy', () => {
            if (this._markArea === area)
                this._markArea = null;
        });
        this._markArea = area;
        return area;
    }

    _dot(color, size = 8, pulse = false) {
        const d = new St.Widget({
            width: size, height: size,
            y_align: Clutter.ActorAlign.CENTER,
            style: `background-color: ${color}; border-radius: ${size / 2}px;`,
        });
        if (pulse)
            this._pulseDots.push(d);
        return d;
    }

    _label(text, {color = TEXT, size = 12, bold = false, expand = false, align = null, spacing = 0, ellipsize = true} = {}) {
        const l = new St.Label({
            text: String(text),
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: expand,
            style: `color: ${color}; font-size: ${size}px;${bold ? ' font-weight: 700;' : ''}` +
                (spacing ? ` letter-spacing: ${spacing}px;` : ''),
        });
        if (align)
            l.x_align = align;
        l.clutter_text.ellipsize = ellipsize ? Pango.EllipsizeMode.END : Pango.EllipsizeMode.NONE;
        return l;
    }

    _row(children, tip, {gap = 8, pad = '3px 0'} = {}) {
        const row = new St.BoxLayout({x_expand: true, style: `spacing: ${gap}px; padding: ${pad};`});
        for (const c of children)
            row.add_child(c);
        if (tip)
            this._tipFor(row, tip, this._scroll);
        return row;
    }

    _ratioBar(ratio, color, width) {
        const track = new St.BoxLayout({
            width, height: 4,
            style: 'background-color: rgba(242,242,240,0.09); border-radius: 2px;',
        });
        track.add_child(new St.Widget({
            width: Math.max(0, Math.min(width, Math.round(width * ratio))), height: 4,
            style: `background-color: ${color}; border-radius: 2px;`,
        }));
        return track;
    }

    _section(title, count = null) {
        const row = new St.BoxLayout({x_expand: true, style: 'spacing: 6px; padding: 8px 0 2px 0;'});
        row.add_child(this._label(title, {color: MUTED, size: 10, bold: true, spacing: 0.9, ellipsize: false}));
        if (count !== null)
            row.add_child(this._label(count, {color: MUTED, size: 10, ellipsize: false}));
        return row;
    }

    _tile(caption, value, sub, color = TEXT) {
        const t = new St.BoxLayout({
            vertical: true, x_expand: true,
            style: `background-color: ${SURFACE}; border-radius: 11px; padding: 9px 11px; spacing: 1px;`,
        });
        t.add_child(this._label(caption, {color: MUTED, size: 9, bold: true, spacing: 0.9, ellipsize: false}));
        t.add_child(this._label(value, {color, size: 19, bold: true}));
        t.add_child(this._label(sub, {color: MUTED, size: 10}));
        return t;
    }

    _empty(text) {
        return this._label(text, {color: MUTED, size: 12});
    }

    // ---- rendering --------------------------------------------------------------------------

    _pct() {
        return Math.round((this._snapshot?.healthy_ratio ?? 0) * 100);
    }

    _summaryTip() {
        const s = this._snapshot;
        if (this._offline || !s)
            return 'divisid is not answering. Start it with `divisi daemon restart`.';
        return `Pool ${s.tone}: healthy ratio ${s.healthy_ratio.toFixed(2)}, ${s.provider_count} providers, ` +
            `${s.total_keys} keys, ${s.benches.length} benched. Goals: ${s.detail.goals_running} running, ` +
            `${s.detail.goals_queued} queued, ${s.detail.goals_waiting} waiting, ${s.detail.goals_blocked} blocked.`;
    }

    _render(fade) {
        if (!this._root)
            return;
        const adj = this._vadj();
        if (adj)
            this._scrollPos = adj.value;
        this._tipTargets = [];
        this._pulseDots = [];
        this._scroll = null;
        this._hideTip();

        const s = this._snapshot;
        const color = this._offline || !s ? MUTED : toneColor(s.tone);
        this._root.style = this._rootStyle(color);
        this._bar.style = `background-color: ${color}; border-radius: 3px 0 0 3px;`;
        this._bar.visible = this._state === 'hidden';
        // The chat entry outlives the rebuild so half-typed text survives a refresh.
        if (this._chatEntry?.get_parent()) {
            if (this._chatFocused) {
                this._releaseChatFocus();
                this._chatRefocus = true;
            }
            this._chatEntry.get_parent().remove_child(this._chatEntry);
        }
        this._content.destroy_all_children();
        this._content.visible = this._state !== 'hidden';
        this._content.style = `padding: ${this._state === 'peek' ? '5px 5px 5px 7px' : `${PAD}px`}; spacing: 6px;`;

        if (this._state === 'hidden')
            return;

        const running = !!s && s.detail.goals_running > 0;
        if (this._state === 'peek') {
            this._content.add_child(this._peekStrip(s, color, running));
        } else {
            this._renderCard(s, color, running);
        }

        if (fade) {
            this._content.opacity = 0;
            this._content.ease({opacity: 255, duration: FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        }
    }

    _renderCard(s, color, running) {
        const innerW = W_CARD - PAD * 2 - 2;
        const header = new St.BoxLayout({reactive: true, x_expand: true, style: 'spacing: 9px; padding: 0 0 2px 0;'});
        header.add_child(this._markWidget(running));
        header.add_child(this._label('divisi', {bold: true, size: 14, expand: true}));
        header.add_child(this._label(this._offline ? 'offline' : !s ? 'loading…' : `${TONE_WORD[s.tone] ?? s.tone} · ${this._pct()}%`,
            {color, bold: true, size: 12}));
        header.connect('button-press-event', () => {
            this._setState('peek');
            return Clutter.EVENT_STOP;
        });
        this._tipFor(header, `${this._summaryTip()}\nClick to collapse. Esc or a click elsewhere hides the notch.`);
        this._content.add_child(header);
        this._content.add_child(this._ratioBar(s?.healthy_ratio ?? 0, color, innerW));

        this._content.add_child(this._tabStrip(s));

        const body = new St.BoxLayout({vertical: true, x_expand: true, style: 'spacing: 2px; padding: 2px 12px 2px 0;'});
        const mon = Main.layoutManager.primaryMonitor;
        this._scroll = new St.ScrollView({
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
            x_expand: true,
        });
        this._scroll.add_child(body);
        this._content.add_child(this._scroll);

        if (this._offline || !s) {
            body.add_child(this._empty(this._offline ? 'The daemon is not answering. Run `divisi daemon restart`.' : 'Loading…'));
        } else {
            ({overview: () => this._tabOverview(body, s), goals: () => this._tabGoals(body, s),
                pool: () => this._tabPool(body, s, innerW), agents: () => this._tabAgents(body, s),
                usage: () => this._tabUsage(body, s, innerW), chat: () => this._tabChat(body)})[this._tab]();
        }

        const maxBody = Math.round(mon.height * 0.78) - 150;
        const [, bodyH] = body.get_preferred_height(innerW);
        this._scroll.height = Math.min(Math.max(bodyH, 30), Math.max(maxBody, 160));
        const saved = this._scrollPos;
        if (saved > 0)
            GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
                const a = this._vadj();
                if (a)
                    a.value = saved;
                return GLib.SOURCE_REMOVE;
            });
    }

    _openTab(id) {
        this._tab = id;
        this._scrollPos = 0;
        if (this._state === 'card') {
            this._hideTip();
            this._render(true);
            this._place(true);
        } else {
            this._setState('card');
        }
    }

    _peekStrip(s, color, running) {
        const style = (bg) => `spacing: 8px; padding: 5px 9px; border-radius: 10px; background-color: ${bg};`;
        const strip = new St.BoxLayout({vertical: true, style: 'spacing: 2px;'});
        const chip = (id, children, tip) => {
            // Marks the section a background click would reopen.
            const rest = id === this._tab ? 'rgba(242,242,240,0.05)' : 'transparent';
            const c = new St.BoxLayout({reactive: true, track_hover: true, x_expand: true, style: style(rest)});
            children.forEach(ch => c.add_child(ch));
            c.connect('notify::hover', () => {
                if (c.get_stage())
                    c.style = style(c.hover ? 'rgba(242,242,240,0.11)' : rest);
            });
            c.connect('button-press-event', () => {
                this._openTab(id);
                return Clutter.EVENT_STOP;
            });
            this._tipFor(c, tip);
            strip.add_child(c);
        };
        const row = (name, badge, badgeColor) => [
            this._label(name, {size: 12, expand: true, ellipsize: false}),
            this._label(badge, {color: badgeColor, size: 11, ellipsize: false}),
        ];

        const head = this._offline ? 'offline' : !s ? 'loading…' : `${this._pct()}%`;
        const word = s && !this._offline ? (TONE_WORD[s.tone] ?? '') : '';
        chip('overview', [
            this._dot(color, 8, running),
            this._label(head, {bold: true, size: 12, ellipsize: false}),
            this._label(word, {color: MUTED, size: 11, expand: true, ellipsize: false}),
        ], `${this._summaryTip()}\nClick for the overview.`);
        if (!s || this._offline)
            return strip;

        const d = s.detail;
        const benched = s.benches.length;
        const signed = d.agent_rows.filter(a => a.detected).length;
        chip('goals', row('Goals', d.goals.length, d.goals_blocked ? RED : d.goals_running ? TEAL : MUTED),
            `${d.goals_running} running, ${d.goals_queued} queued, ${d.goals_waiting} waiting, ${d.goals_blocked} blocked. Click to open Goals.`);
        chip('pool', row('Pool', benched ? `${benched} benched` : s.provider_count, benched ? AMBER : MUTED),
            `${s.provider_count} providers, ${s.total_keys} keys, ${benched} benched. Click to open Pool.`);
        chip('agents', row('Agents', signed, MUTED),
            `${signed} of ${d.agent_rows.length} agents installed. Click to open Agents.`);
        const runs = d.agent_usage.reduce((n, u) => n + u.runs_24h, 0);
        chip('usage', row('Usage', `${runs} today`, runs ? TEAL : MUTED),
            `${runs} agent runs in the last 24h. Click to open Usage.`);
        return strip;
    }

    _vadj() {
        return this._scroll?.vadjustment ?? this._scroll?.vscroll?.adjustment ?? null;
    }

    _tabStrip(s) {
        const d = s?.detail;
        const badges = {
            overview: null,
            goals: d ? (d.goals.length || null) : null,
            pool: s ? (s.benches.length ? `${s.benches.length} benched` : s.provider_count) : null,
            agents: d ? d.agent_rows.filter(a => a.detected).length : null,
            usage: d ? d.agent_usage.reduce((n, u) => n + u.runs_24h, 0) : null,
        };
        const strip = new St.BoxLayout({x_expand: true, style: 'spacing: 4px; padding: 6px 0 2px 0;'});
        for (const [id, name] of TABS) {
            const active = this._tab === id;
            const tab = new St.BoxLayout({
                reactive: true, track_hover: true,
                style: `spacing: 5px; padding: 4px 10px; border-radius: 9px; background-color: ${active ? 'rgba(242,242,240,0.11)' : 'transparent'};`,
            });
            tab.add_child(this._label(name, {color: active ? TEXT : MUTED, size: 12, bold: active}));
            if (badges[id] !== null && badges[id] !== undefined) {
                const alert = id === 'pool' && s.benches.length;
                tab.add_child(this._label(badges[id], {color: alert ? AMBER : MUTED, size: 10}));
            }
            tab.connect('notify::hover', () => {
                if (this._tab !== id && tab.get_stage())
                    tab.style = `spacing: 5px; padding: 4px 10px; border-radius: 9px; background-color: ${tab.hover ? 'rgba(242,242,240,0.06)' : 'transparent'};`;
            });
            tab.connect('button-press-event', () => {
                this._selectTab(id);
                return Clutter.EVENT_STOP;
            });
            strip.add_child(tab);
        }
        return strip;
    }

    // ---- tabs ----------------------------------------------------------------------------------

    _goalRow(g, withNote = true) {
        const rows = new St.BoxLayout({vertical: true, x_expand: true});
        const line = new St.BoxLayout({x_expand: true, style: 'spacing: 8px;'});
        line.add_child(this._dot(statusColor(g.status), 7, g.status === 'running'));
        line.add_child(this._label(g.text, {expand: true}));
        line.add_child(this._label(`${g.dispatches}/${g.max_dispatches}`, {color: MUTED, size: 10}));
        rows.add_child(line);
        const bits = [];
        if (withNote && g.note)
            bits.push(g.note);
        if (withNote && g.eta)
            bits.push(`retry ${relTime(g.eta)}`);
        if (bits.length) {
            const note = this._label(bits.join(' · '), {color: statusColor(g.status), size: 10});
            note.style += ' padding-left: 15px;';
            rows.add_child(note);
        }
        this._tipFor(rows, `${g.text}\n${g.status.replaceAll('_', ' ')} · ${g.dispatches}/${g.max_dispatches} dispatches` +
            (g.note ? `\n${g.note}` : '') + (g.eta ? `\nretry ${relTime(g.eta)}` : '') + `\n${g.id}`, this._scroll);
        rows.style = 'padding: 3px 0;';
        return rows;
    }

    _tabOverview(body, s) {
        const d = s.detail;
        const pairs = new St.BoxLayout({x_expand: true, style: 'spacing: 8px;'});
        const kt = this._keyTotals(d);
        pairs.add_child(this._tile('KEYS', kt.keys, `${kt.keyed} providers · ${kt.multi} multi-key`));
        pairs.add_child(this._tile('HEALTH', `${this._pct()}%`, TONE_WORD[s.tone] ?? s.tone, toneColor(s.tone)));
        body.add_child(pairs);
        const pairs2 = new St.BoxLayout({x_expand: true, style: 'spacing: 8px; padding-top: 8px;'});
        pairs2.add_child(this._tile('GOALS', `${d.goals_running}/${d.max_parallel}`,
            `${d.goals_queued} queued · ${d.goals_waiting} waiting · ${d.goals_blocked} blocked`,
            d.goals_running ? TEAL : TEXT));
        const soonest = s.benches.length ? Math.min(...s.benches.map(b => b.remaining_secs)) : null;
        pairs2.add_child(this._tile('BENCHED', s.benches.length,
            soonest === null ? 'all providers clear' : `next clears in ${fmtDuration(soonest)}`,
            s.benches.length ? AMBER : TEXT));
        body.add_child(pairs2);

        body.add_child(this._section('CREDENTIALS'));
        const pc = this._countBy(d.providers_full, p => p.auth_state);
        const ac = this._countBy(d.agent_rows, a => a.class);
        body.add_child(this._summaryLine('Providers', PROVIDER_STATES, pc));
        body.add_child(this._summaryLine('Agents', AGENT_CLASSES, ac));

        body.add_child(this._section('RUNNING NOW', d.goals_running));
        const live = d.goals.filter(g => g.status === 'running').slice(0, 3);
        if (live.length)
            live.forEach(g => body.add_child(this._goalRow(g)));
        else
            body.add_child(this._empty('No goals running'));

        if (d.recent_tasks.length) {
            body.add_child(this._section('RECENT TASKS'));
            for (const t of d.recent_tasks.slice(0, 5)) {
                body.add_child(this._row([
                    this._dot(statusColor(t.status), 7),
                    this._label(t.agent, {color: MUTED, size: 11}),
                    this._label(t.description, {expand: true}),
                    this._label(relTime(t.updated_at), {color: MUTED, size: 10}),
                ], `#${t.id} ${t.agent} · ${t.status}\n${t.description}`));
            }
        }
    }

    _tabGoals(body, s) {
        const d = s.detail;
        const groups = [
            ['running', 'RUNNING'], ['waiting_on_capacity', 'WAITING ON CAPACITY'],
            ['queued', 'QUEUED'], ['blocked', 'BLOCKED'],
        ];
        let any = false;
        for (const [status, title] of groups) {
            const goals = d.goals.filter(g => g.status === status);
            if (!goals.length)
                continue;
            any = true;
            body.add_child(this._section(title, goals.length));
            goals.forEach(g => body.add_child(this._goalRow(g)));
        }
        if (!any)
            body.add_child(this._empty('No goals in flight'));
        body.add_child(this._section('CAPACITY'));
        body.add_child(this._label(`${d.goals_running} of ${d.max_parallel} parallel slots in use`, {color: MUTED, size: 11}));
    }

    _keyTotals(d) {
        return {
            keys: d.providers_full.reduce((n, p) => n + p.key_count, 0),
            keyed: d.providers_full.filter(p => p.key_count > 0).length,
            multi: d.providers_full.filter(p => p.key_count > 1).length,
        };
    }

    _countBy(list, fn) {
        const m = {};
        for (const x of list)
            m[fn(x)] = (m[fn(x)] ?? 0) + 1;
        return m;
    }

    _summaryLine(title, states, counts) {
        const SHORT = {
            authed: 'authed', no_auth_needed: 'no auth', unverified: 'unverified', invalid: 'rejected',
            disabled: 'disabled', blocked: 'blocked', no_key: 'no key', needs_login: 'login needed', not_installed: 'missing',
        };
        const items = states.filter(([id]) => counts[id]).map(([id, , color]) => [`${counts[id]} ${SHORT[id]}`, color]);
        const box = new St.BoxLayout({vertical: true, x_expand: true, style: 'padding: 1px 0;'});
        const PER_ROW = 4;
        for (let i = 0; i < Math.max(items.length, 1); i += PER_ROW) {
            const line = new St.BoxLayout({x_expand: true, style: 'spacing: 10px;'});
            if (title) {
                const head = this._label(i === 0 ? title : '', {color: MUTED, size: 11, ellipsize: false});
                head.width = 62;
                line.add_child(head);
            }
            for (const [text, color] of items.slice(i, i + PER_ROW))
                line.add_child(this._label(text, {color, size: 11, ellipsize: false}));
            box.add_child(line);
        }
        return box;
    }

    _note(text) {
        const l = new St.Label({text, x_expand: true, style: `color: ${MUTED}; font-size: 10px; padding-top: 4px;`});
        l.clutter_text.line_wrap = true;
        l.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
        return l;
    }

    _providerTip(p) {
        const lines = [`${p.platform}: ${p.auth_kind === 'keyless' ? 'needs no key' : PROVIDER_STATES.find(x => x[0] === p.auth_state)?.[1].toLowerCase()}`];
        if (p.key_count)
            lines.push(`${p.key_count} key${p.key_count > 1 ? 's' : ''}: ${p.keys_valid} valid, ${p.keys_invalid} rejected, ` +
                `${p.keys_unvalidated} not yet validated, ${p.keys_disabled} disabled` +
                (p.key_count > 1 ? '. The pool rotates across them.' : '.'));
        if (p.auth_state === 'unverified')
            lines.push(p.can_validate ? 'Run `divisi provider validate` to check it.' : 'This provider has no validation endpoint, so a key can only be confirmed by a real successful call.');
        if (p.reason)
            lines.push(p.reason);
        lines.push(p.metered ? `${p.requests_today} requests today of ${p.rpd_limit ?? '?'} per day (counted locally).`
            : `${p.requests_today} requests today (counted locally); no published daily limit.`);
        return lines.join('\n');
    }

    _tabPool(body, s) {
        const d = s.detail;
        body.add_child(this._row([
            this._label(`${this._pct()}% healthy`, {bold: true, color: toneColor(s.tone)}),
            this._label(`${d.providers_full.length} providers · ${this._keyTotals(d).keys} keys`, {color: MUTED, size: 11, expand: true, align: Clutter.ActorAlign.END}),
        ], this._summaryTip()));

        if (s.benches.length) {
            body.add_child(this._section('BENCHED', s.benches.length));
            for (const b of s.benches) {
                body.add_child(this._row([
                    this._dot(AMBER, 7, true),
                    this._label(`${b.platform}/${b.model}`, {expand: true}),
                    this._label(fmtDuration(b.remaining_secs), {color: AMBER, size: 11}),
                ], `Key ${b.key_id} on ${b.platform} (${b.model}) is benched for ${fmtDuration(b.remaining_secs)}.\nSource: ${b.provenance}.`));
            }
        }

        for (const [id, title, color] of PROVIDER_STATES) {
            const group = d.providers_full.filter(p => p.auth_state === id)
                .sort((a, b) => b.key_count - a.key_count || a.platform.localeCompare(b.platform));
            if (!group.length)
                continue;
            body.add_child(this._section(title, group.length));
            for (const p of group) {
                const kids = [this._dot(color, 6), this._label(p.platform, {expand: true})];
                if (p.key_count > 1)
                    kids.push(this._chip(`${p.key_count} keys`, BLUE));
                if (id === 'authed')
                    kids.push(this._label(`${p.keys_valid}/${p.key_count} valid`, {color: MUTED, size: 10, ellipsize: false}));
                else if (id === 'unverified')
                    kids.push(this._label(p.can_validate ? 'run validate' : 'no probe', {color: MUTED, size: 10, ellipsize: false}));
                else if (id === 'blocked' || id === 'disabled')
                    kids.push(this._label(p.reason ? p.reason.slice(0, 26) : '', {color: MUTED, size: 10}));
                else if (id === 'no_key')
                    kids.push(this._label('add a key', {color: MUTED, size: 10, ellipsize: false}));
                else if (id === 'no_auth_needed')
                    kids.push(this._label(p.key_count ? `${p.key_count} key${p.key_count > 1 ? 's' : ''} optional` : 'no key', {color: MUTED, size: 10, ellipsize: false}));
                body.add_child(this._row(kids, this._providerTip(p), {pad: '2px 0'}));
            }
        }
    }

    _chip(text, color) {
        const l = this._label(text, {color, size: 10, bold: true, ellipsize: false});
        l.style += ` background-color: rgba(242,242,240,0.07); border-radius: 6px; padding: 0 6px;`;
        return l;
    }

    _tabAgents(body, s) {
        const d = s.detail;
        const slotOf = new Map(d.slots.map(sl => [sl.agent, sl]));
        const counts = this._countBy(d.agent_rows, a => a.class);
        body.add_child(this._summaryLine('', AGENT_CLASSES, counts));
        for (const [id, title, color] of AGENT_CLASSES) {
            const group = d.agent_rows.filter(a => a.class === id).sort((a, b) => a.name.localeCompare(b.name));
            if (!group.length)
                continue;
            body.add_child(this._section(title, group.length));
            for (const a of group) {
                const slot = slotOf.get(a.name);
                const right = slot?.rate_limited ? 'rate-limited' : slot?.running ? `${slot.running}${slot.cap ? `/${slot.cap}` : ''} running` : '';
                body.add_child(this._row([
                    this._dot(id === 'not_installed' ? 'rgba(125,129,138,0.45)' : color, 8, !!slot?.running),
                    this._label(a.name, {expand: true, color: id === 'not_installed' ? MUTED : TEXT}),
                    this._label(shortVersion(a.version), {color: MUTED, size: 10, ellipsize: false}),
                    this._label(right, {color: slot?.rate_limited ? AMBER : TEAL, size: 10, ellipsize: false}),
                ], `${a.name}${a.version ? ` ${a.version}` : ''}: ${title.toLowerCase()}.\n${a.why}.` +
                    (id === 'unverified' ? '\nsingle can only detect logins for a few agents (claude, codex, cursor, copilot, agy, grok, codebuff).' : ''),
                {pad: '2px 0'}));
            }
        }
        if (!counts.no_auth_needed)
            body.add_child(this._note('No registered agent is confirmed to run without a login.'));
    }

    _tabUsage(body, s, innerW) {
        const d = s.detail;
        const runs24 = d.agent_usage.reduce((n, u) => n + u.runs_24h, 0);
        const runs7 = d.agent_usage.reduce((n, u) => n + u.runs_7d, 0);
        const tok = d.agent_usage.reduce((n, u) => n + u.prompt_tokens_7d + u.completion_tokens_7d, 0);
        const est = d.agent_usage.reduce((n, u) => n + u.estimated_runs_7d, 0);
        const reqToday = d.providers_full.reduce((n, p) => n + p.requests_today, 0);

        const t1 = new St.BoxLayout({x_expand: true, style: 'spacing: 8px;'});
        t1.add_child(this._tile('AGENT RUNS', String(runs24), `today · ${runs7} in 7 days`));
        t1.add_child(this._tile('TOKENS 7D', fmtNum(tok), est ? `~estimated for ${est} of ${runs7} runs` : 'as reported'));
        body.add_child(t1);
        const t2 = new St.BoxLayout({x_expand: true, style: 'spacing: 8px; padding-top: 8px;'});
        t2.add_child(this._tile('POOL REQUESTS', String(reqToday), 'today, counted by single'));
        const metered = d.providers_full.filter(p => p.metered).length;
        t2.add_child(this._tile('LIMITS KNOWN', `${metered}/${d.providers_full.length}`, 'providers with a daily cap'));
        body.add_child(t2);

        body.add_child(this._section('PROVIDERS TODAY'));
        const active = d.providers_full.filter(p => p.requests_today > 0 || p.metered)
            .sort((a, b) => b.requests_today - a.requests_today || a.platform.localeCompare(b.platform));
        const barW = 90;
        for (const p of active) {
            const limitBits = [p.rpd_limit && `${p.rpd_limit}/day`, p.rpm_limit && `${p.rpm_limit}/min`,
                p.tpd_limit && `${fmtNum(p.tpd_limit)} tok/day`, p.tpm_limit && `${fmtNum(p.tpm_limit)} tok/min`].filter(Boolean);
            const kids = [this._label(p.platform, {expand: true})];
            if (p.rpd_limit) {
                const frac = Math.min(1, p.requests_today / p.rpd_limit);
                kids.push(this._ratioBar(frac, frac > 0.8 ? AMBER : TEAL, barW));
                kids.push(this._label(`${p.requests_today}/${p.rpd_limit}`, {color: MUTED, size: 10, ellipsize: false}));
            } else if (p.requests_today > 0 || !p.tpd_limit) {
                kids.push(this._label(`${p.requests_today} req`, {color: MUTED, size: 10, ellipsize: false}));
                kids.push(this._chip('unmetered', MUTED));
            } else {
                kids.push(this._label(`${fmtNum(p.tpd_limit)} tok/day cap`, {color: MUTED, size: 10, ellipsize: false}));
            }
            body.add_child(this._row(kids,
                `${p.platform}: ${p.requests_today} requests today across ${p.key_count || 0} key(s), counted by single's own ledger.\n` +
                (limitBits.length ? `Published limits: ${limitBits.join(', ')}.` : 'No published limit is recorded for this provider, so remaining quota is unknown.') +
                (p.key_count > 1 ? '\nWith several keys the real ceiling is higher than one key\'s limit.' : ''), {pad: '2px 0'}));
        }
        const idle = d.providers_full.length - active.length;
        if (idle > 0)
            body.add_child(this._label(`${idle} providers idle today`, {color: MUTED, size: 10}));

        body.add_child(this._section('AGENTS · 7 DAYS'));
        const agents = [...d.agent_usage].filter(u => u.runs_7d > 0).sort((a, b) => b.runs_7d - a.runs_7d);
        if (!agents.length)
            body.add_child(this._empty('No agent runs in the last 7 days'));
        for (const u of agents.slice(0, 12)) {
            const t = u.prompt_tokens_7d + u.completion_tokens_7d;
            const guess = u.estimated_runs_7d > 0 && u.estimated_runs_7d === u.runs_7d;
            body.add_child(this._row([
                this._label(u.agent, {expand: true}),
                this._label(`${u.runs_7d} runs`, {color: MUTED, size: 10, ellipsize: false}),
                this._label(`${guess ? '~' : ''}${fmtNum(t)} tok`, {color: guess ? AMBER : MUTED, size: 10, ellipsize: false}),
                u.rate_limited_7d ? this._label(`${u.rate_limited_7d} limited`, {color: AMBER, size: 10, ellipsize: false}) : this._label('', {size: 10}),
            ], `${u.agent}: ${u.runs_24h} runs today, ${u.runs_7d} in 7 days (${u.runs_total} total).\n` +
                `Tokens: ${fmtNum(u.prompt_tokens_7d)} in, ${fmtNum(u.completion_tokens_7d)} out; ` +
                `${u.estimated_runs_7d} of ${u.runs_7d} runs were estimated (chars/4), the rest reported by the agent.` +
                (u.discarded_token_rows ? `\n${u.discarded_token_rows} runs with implausible token counts were left out.` : '') +
                (u.rate_limited_7d ? `\n${u.rate_limited_7d} runs hit a rate limit.` : ''), {pad: '2px 0'}));
        }
        body.add_child(this._note('single does not know any agent subscription limits, so this shows what ran, not what is left. Token counts marked ~ are estimates.'));
    }
}
