// SingleCLI notch: a right-edge, vertically centered overlay drawn as shell
// chrome (not a window, so tiling extensions never see it). Data comes from
// `single-notch --snapshot`, which reuses the Rust aggregation.
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

const TEAL = '#2EC4B6';
const AMBER = '#E9A319';
const RED = '#E85D4C';
const BLUE = '#7FA8D9';
const MUTED = '#8A8A90';
const TEXT = '#ECECEE';
const SURFACE = 'rgba(255,255,255,0.05)';

const W_HIDDEN = 6;
const W_CARD = 392;
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

const TABS = [['overview', 'Overview'], ['goals', 'Goals'], ['pool', 'Pool'], ['agents', 'Agents']];

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
            style: 'background-color: rgba(14,15,18,0.96); color: ' + TEXT + '; border-radius: 9px; ' +
                'border: 1px solid rgba(255,255,255,0.10); padding: 7px 11px; font-size: 12px; ' +
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
        return 'background-color: rgba(16,17,20,0.94); border-radius: 16px 0 0 16px; ' +
            'border: 1px solid rgba(255,255,255,0.09); border-right-width: 0; ' +
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
        const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'single-notch']);
        const bin = GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE) ? local : 'single-notch';
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
            style: 'background-color: rgba(255,255,255,0.09); border-radius: 2px;',
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
            return 'single-runtimed is not answering. Start it with `single daemon restart`.';
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
        header.add_child(this._dot(color, 11, running));
        header.add_child(this._label('SingleCLI', {bold: true, size: 14, expand: true}));
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
            body.add_child(this._empty(this._offline ? 'The daemon is not answering. Run `single daemon restart`.' : 'Loading…'));
        } else {
            ({overview: () => this._tabOverview(body, s), goals: () => this._tabGoals(body, s),
                pool: () => this._tabPool(body, s, innerW), agents: () => this._tabAgents(body, s)})[this._tab]();
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
            const rest = id === this._tab ? 'rgba(255,255,255,0.05)' : 'transparent';
            const c = new St.BoxLayout({reactive: true, track_hover: true, x_expand: true, style: style(rest)});
            children.forEach(ch => c.add_child(ch));
            c.connect('notify::hover', () => {
                if (c.get_stage())
                    c.style = style(c.hover ? 'rgba(255,255,255,0.11)' : rest);
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
        };
        const strip = new St.BoxLayout({x_expand: true, style: 'spacing: 4px; padding: 6px 0 2px 0;'});
        for (const [id, name] of TABS) {
            const active = this._tab === id;
            const tab = new St.BoxLayout({
                reactive: true, track_hover: true,
                style: `spacing: 5px; padding: 4px 10px; border-radius: 9px; background-color: ${active ? 'rgba(255,255,255,0.11)' : 'transparent'};`,
            });
            tab.add_child(this._label(name, {color: active ? TEXT : MUTED, size: 12, bold: active}));
            if (badges[id] !== null && badges[id] !== undefined) {
                const alert = id === 'pool' && s.benches.length;
                tab.add_child(this._label(badges[id], {color: alert ? AMBER : MUTED, size: 10}));
            }
            tab.connect('notify::hover', () => {
                if (this._tab !== id && tab.get_stage())
                    tab.style = `spacing: 5px; padding: 4px 10px; border-radius: 9px; background-color: ${tab.hover ? 'rgba(255,255,255,0.06)' : 'transparent'};`;
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
        pairs.add_child(this._tile('KEYS', s.total_keys, `${s.provider_count} providers`));
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

    _tabPool(body, s, innerW) {
        body.add_child(this._row([
            this._label(`${this._pct()}% healthy`, {bold: true, color: toneColor(s.tone)}),
            this._label(`${s.total_keys} keys · ${s.provider_count} providers`, {color: MUTED, size: 11, expand: true, align: Clutter.ActorAlign.END}),
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
        if (s.detail.problem_keys.length) {
            body.add_child(this._section('KEY ISSUES', s.detail.problem_keys.length));
            for (const k of s.detail.problem_keys) {
                body.add_child(this._row([
                    this._dot(RED, 7),
                    this._label(k.platform),
                    this._label(k.reason, {color: RED, size: 11, expand: true}),
                ], `${k.platform}: ${k.reason}`));
            }
        }
        body.add_child(this._section('PROVIDERS', s.providers.length));
        const ranked = [...s.providers].sort((a, b) => Number(b.cooldown !== 'clear') - Number(a.cooldown !== 'clear'));
        for (const p of ranked) {
            const benched = p.cooldown !== 'clear';
            body.add_child(this._row([
                this._dot(benched ? AMBER : TEAL, 6),
                this._label(p.platform, {expand: true}),
                this._label(benched ? p.cooldown : p.headroom.replace('unbounded/unknown', 'unmetered'),
                    {color: benched ? AMBER : MUTED, size: 10}),
                this._label(`×${p.key_count}`, {color: MUTED, size: 10}),
            ], `${p.platform}: ${p.key_count} key(s), cooldown ${p.cooldown}, headroom ${p.headroom}.`, {pad: '2px 0'}));
        }
        if (s.detail.keys_unkeyed)
            body.add_child(this._label(`${s.detail.keys_unkeyed} providers registered without a key`, {color: MUTED, size: 10}));
    }

    _tabAgents(body, s) {
        const authOf = new Map(s.agents.map(a => [a.name, a.state]));
        const slotOf = new Map(s.detail.slots.map(sl => [sl.agent, sl]));
        const rows = s.detail.agent_rows.map(a => ({...a, auth: authOf.get(a.name) ?? 'unsupported', slot: slotOf.get(a.name)}));
        const rank = r => (r.auth === 'authenticated' ? 0 : r.detected ? 1 : 2);
        rows.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
        const authed = rows.filter(r => r.auth === 'authenticated').length;
        const detected = rows.filter(r => r.detected).length;
        body.add_child(this._label(`${authed} signed in · ${detected} installed · ${rows.length} known`, {color: MUTED, size: 11}));
        for (const r of rows) {
            const color = !r.detected ? 'rgba(138,138,144,0.45)' : statusColor(r.auth === 'unsupported' ? 'x' : r.auth);
            const right = r.slot?.rate_limited ? 'rate-limited'
                : r.slot && r.slot.running ? `${r.slot.running}${r.slot.cap ? `/${r.slot.cap}` : ''} running`
                : !r.detected ? 'not installed'
                : r.auth === 'authenticated' ? 'signed in'
                : r.auth === 'not_authenticated' ? 'sign-in needed' : 'installed';
            const rightColor = r.slot?.rate_limited ? AMBER : r.slot?.running ? TEAL : MUTED;
            body.add_child(this._row([
                this._dot(color, 8, !!r.slot?.running),
                this._label(r.name, {expand: true, color: r.detected ? TEXT : MUTED}),
                this._label(shortVersion(r.version), {color: MUTED, size: 10, ellipsize: false}),
                this._label(right, {color: rightColor, size: 10, ellipsize: false}),
            ], `${r.name}${r.version ? ` ${r.version}` : ''}: ${r.detected ? 'installed' : 'not installed'}, ` +
                `${r.auth.replaceAll('_', ' ')}${r.slot ? `, ${r.slot.running} running` : ''}${r.slot?.rate_limited ? ', rate-limited' : ''}`,
                {pad: '2px 0'}));
        }
    }
}
