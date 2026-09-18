// SingleCLI notch: a right-edge, vertically centered overlay drawn as shell
// chrome (not a window, so tiling extensions never see it). Data comes from
// `single-notch --snapshot`, which reuses the Rust aggregation.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const TEAL = '#2EC4B6';
const AMBER = '#E9A319';
const RED = '#E85D4C';
const MUTED = '#8A8A90';
const TEXT = '#E8E8EA';

const W_HIDDEN = 6;
const W_PEEK = 210;
const W_CARD = 340;
const H_HIDDEN = 64;
const H_PEEK = 40;
const PAD = 12;
const ANIM_MS = 180;

const POLL_VISIBLE_S = 2;
const POLL_HIDDEN_S = 8;
const LEAVE_PEEK_MS = 700;
const LEAVE_CARD_MS = 2500;
const NOTABLE_PEEK_MS = 3500;

const toneColor = tone => ({healthy: TEAL, amber: AMBER, degraded: RED}[tone] ?? MUTED);

export default class SingleNotch extends Extension {
    enable() {
        this._state = 'hidden';
        this._snapshot = null;
        this._raw = '';
        this._offline = false;
        this._lastFetch = 0;
        this._fetching = false;
        this._timers = new Set();
        this._leaveTimer = 0;

        this._root = new St.BoxLayout({
            vertical: true,
            reactive: true,
            track_hover: true,
            clip_to_allocation: true,
            style: this._rootStyle(MUTED),
            opacity: 90,
        });
        this._content = new St.BoxLayout({vertical: true, x_expand: true, y_expand: true});
        this._root.add_child(this._content);

        this._tip = new St.Label({
            visible: false,
            reactive: false,
            style: `background-color: rgba(13,13,15,0.95); color: ${TEXT}; border-radius: 8px; ` +
                'padding: 6px 10px; font-size: 12px; max-width: 300px;',
        });
        this._tip.clutter_text.line_wrap = true;

        Main.layoutManager.addChrome(this._root, {affectsStruts: false, trackFullscreen: true});
        Main.layoutManager.addChrome(this._tip, {affectsStruts: false, trackFullscreen: true});

        this._hoverId = this._root.connect('notify::hover', () => this._onHover());
        this._pressId = this._root.connect('button-press-event', () => {
            this._onClick();
            return Clutter.EVENT_STOP;
        });
        this._monitorsId = Main.layoutManager.connect('monitors-changed', () => this._place(false));

        this._render();
        this._place(false);
        this._fetch();
        this._pollId = this._addTimer(1, () => {
            this._tickPoll();
            return GLib.SOURCE_CONTINUE;
        });
    }

    disable() {
        for (const id of this._timers)
            GLib.source_remove(id);
        this._timers = null;
        if (this._monitorsId)
            Main.layoutManager.disconnect(this._monitorsId);
        this._cancellable?.cancel();
        this._root?.remove_all_transitions();
        if (this._root) {
            Main.layoutManager.removeChrome(this._root);
            this._root.destroy();
        }
        if (this._tip) {
            Main.layoutManager.removeChrome(this._tip);
            this._tip.destroy();
        }
        this._root = this._tip = this._content = this._snapshot = null;
    }

    _addTimer(seconds, fn) {
        const id = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, seconds, () => {
            const keep = fn();
            if (keep === GLib.SOURCE_REMOVE)
                this._timers?.delete(id);
            return keep;
        });
        this._timers.add(id);
        return id;
    }

    _addTimerMs(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._timers?.delete(id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._timers.add(id);
        return id;
    }

    _clearTimer(id) {
        if (id && this._timers?.delete(id))
            GLib.source_remove(id);
    }

    _rootStyle(accent) {
        return 'background-color: rgba(13,13,15,0.88); border-radius: 14px 0 0 14px; ' +
            `border: 1px solid rgba(255,255,255,0.08); border-right-width: 0; border-left-color: ${accent};`;
    }

    // ---- data -----------------------------------------------------------

    _tickPoll() {
        const interval = this._state === 'hidden' ? POLL_HIDDEN_S : POLL_VISIBLE_S;
        if (GLib.get_monotonic_time() / 1e6 - this._lastFetch >= interval)
            this._fetch();
    }

    _fetch() {
        if (this._fetching)
            return;
        this._fetching = true;
        this._lastFetch = GLib.get_monotonic_time() / 1e6;
        const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'single-notch']);
        const bin = GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE) ? local : 'single-notch';
        this._cancellable = new Gio.Cancellable();
        try {
            const proc = Gio.Subprocess.new([bin, '--snapshot'],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE);
            proc.communicate_utf8_async(null, this._cancellable, (p, res) => {
                this._fetching = false;
                try {
                    const [, out] = p.communicate_utf8_finish(res);
                    if (!p.get_successful())
                        throw new Error('snapshot failed');
                    this._apply(out.trim(), JSON.parse(out));
                } catch (e) {
                    if (this._root)
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
        this._render();
    }

    _apply(raw, snap) {
        const prev = this._snapshot;
        this._offline = false;
        if (raw === this._raw)
            return;
        this._raw = raw;
        this._snapshot = snap;
        const notable = prev && (prev.tone !== snap.tone || snap.benches.length > prev.benches.length);
        this._render();
        if (notable && this._state === 'hidden') {
            this._setState('peek');
            this._scheduleLeave(NOTABLE_PEEK_MS);
        }
    }

    // ---- interaction ------------------------------------------------------

    _onHover() {
        if (!this._root)
            return;
        if (this._root.hover) {
            this._clearTimer(this._leaveTimer);
            if (this._state === 'hidden')
                this._setState('peek');
        } else {
            this._hideTip();
            this._scheduleLeave(this._state === 'card' ? LEAVE_CARD_MS : LEAVE_PEEK_MS);
        }
    }

    _onClick() {
        this._hideTip();
        this._setState(this._state === 'card' ? 'peek' : 'card');
    }

    _scheduleLeave(ms) {
        this._clearTimer(this._leaveTimer);
        this._leaveTimer = this._addTimerMs(ms, () => {
            if (this._root && !this._root.hover)
                this._setState('hidden');
        });
    }

    _setState(state) {
        if (this._state === state)
            return;
        this._state = state;
        this._render();
        this._place(true);
        if (state !== 'hidden')
            this._fetch();
    }

    _attachTip(actor, text) {
        actor.reactive = true;
        actor.track_hover = true;
        actor.connect('notify::hover', () => {
            if (actor.hover)
                this._showTip(actor, text);
            else
                this._hideTip();
        });
    }

    _showTip(actor, text) {
        if (!this._tip || !this._root)
            return;
        this._tip.text = text;
        this._tip.show();
        const [, y] = actor.get_transformed_position();
        const [, tipH] = this._tip.get_preferred_height(-1);
        const [, tipW] = this._tip.get_preferred_width(-1);
        const mon = Main.layoutManager.primaryMonitor;
        const ty = Math.max(mon.y + 4, Math.min(y + actor.height / 2 - tipH / 2, mon.y + mon.height - tipH - 4));
        this._tip.set_position(Math.round(this._root.x - tipW - 8), Math.round(ty));
    }

    _hideTip() {
        this._tip?.hide();
    }

    // ---- layout -------------------------------------------------------------

    _targetSize() {
        const mon = Main.layoutManager.primaryMonitor;
        if (this._state === 'hidden')
            return [W_HIDDEN, H_HIDDEN];
        if (this._state === 'peek')
            return [W_PEEK, H_PEEK];
        const [, natH] = this._content.get_preferred_height(W_CARD - PAD * 2);
        return [W_CARD, Math.min(natH + PAD * 2, Math.round(mon.height * 0.8))];
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
            opacity: this._state === 'hidden' ? this._restingOpacity() : 255,
        };
        this._root.remove_all_transitions();
        if (animate)
            this._root.ease({...props, duration: ANIM_MS, mode: Clutter.AnimationMode.EASE_OUT_CUBIC});
        else
            this._root.set({...props});
    }

    _restingOpacity() {
        const tone = this._snapshot?.tone;
        return this._offline || tone === 'degraded' || tone === 'amber' ? 230 : 90;
    }

    // ---- rendering ----------------------------------------------------------

    _dot(color, size = 8) {
        return new St.Widget({
            width: size, height: size,
            y_align: Clutter.ActorAlign.CENTER,
            style: `background-color: ${color}; border-radius: ${size / 2}px;`,
        });
    }

    _label(text, {color = TEXT, size = 12, bold = false, expand = false} = {}) {
        const l = new St.Label({
            text,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: expand,
            style: `color: ${color}; font-size: ${size}px;${bold ? ' font-weight: bold;' : ''}`,
        });
        l.clutter_text.ellipsize = 3; // Pango.EllipsizeMode.END
        return l;
    }

    _row(children, tip) {
        const row = new St.BoxLayout({style: 'spacing: 8px; padding: 2px 0;', x_expand: true});
        for (const c of children)
            row.add_child(c);
        if (tip)
            this._attachTip(row, tip);
        return row;
    }

    _summaryText() {
        const s = this._snapshot;
        if (this._offline || !s)
            return 'daemon unreachable';
        const pct = Math.round(s.healthy_ratio * 100);
        const bench = s.benches.length ? ` · ${s.benches.length} benched` : '';
        return `${s.total_keys} keys · ${pct}%${bench}`;
    }

    _summaryTip() {
        const s = this._snapshot;
        if (this._offline || !s)
            return 'single-runtimed is not answering. Start it with `single daemon restart`.';
        return `Pool ${s.tone}: healthy ratio ${s.healthy_ratio.toFixed(2)}, ` +
            `${s.provider_count} providers, ${s.total_keys} keys, ${s.benches.length} benched.`;
    }

    _render() {
        if (!this._root)
            return;
        const s = this._snapshot;
        const color = this._offline || !s ? MUTED : toneColor(s.tone);
        this._root.style = this._rootStyle(color);
        this._content.destroy_all_children();
        this._content.style = `padding: ${this._state === 'hidden' ? 0 : PAD}px; spacing: 4px;`;
        this._hideTip();

        if (this._state === 'hidden')
            return;

        this._content.add_child(this._row(
            [this._dot(color, 10), this._label(this._summaryText(), {bold: true, expand: true})],
            this._summaryTip()));
        if (this._state === 'peek' || !s)
            return;

        const section = title => this._content.add_child(
            this._label(title, {color: MUTED, size: 11, bold: true}));

        section('PROVIDERS');
        const ranked = [...s.providers].sort((a, b) =>
            Number(b.cooldown !== 'clear') - Number(a.cooldown !== 'clear'));
        for (const p of ranked.slice(0, 8)) {
            const benched = p.cooldown !== 'clear';
            this._content.add_child(this._row([
                this._dot(benched ? AMBER : TEAL, 6),
                this._label(p.platform, {expand: true}),
                this._label(benched ? p.cooldown : `×${p.key_count}`, {color: benched ? AMBER : MUTED}),
            ], `${p.platform}: ${p.key_count} key(s), cooldown ${p.cooldown}, headroom ${p.headroom}.`));
        }
        if (ranked.length > 8)
            this._content.add_child(this._label(`+${ranked.length - 8} more`, {color: MUTED, size: 11}));

        if (s.benches.length) {
            section('BENCHED');
            for (const b of s.benches.slice(0, 5)) {
                this._content.add_child(this._row([
                    this._label(`${b.platform}/${b.model}`, {expand: true}),
                    this._label(`${b.remaining_secs}s`, {color: AMBER}),
                ], `Key ${b.key_id} on ${b.platform} (${b.model}) is benched for ${b.remaining_secs}s. Source: ${b.provenance}.`));
            }
        }

        if (s.activity.length) {
            section('ACTIVITY');
            for (const g of s.activity.slice(0, 5)) {
                this._content.add_child(this._row([
                    this._label(g.text, {expand: true}),
                    this._label(g.status, {color: MUTED}),
                ], `Goal ${g.id} (${g.status}): ${g.text}`));
            }
        }

        if (s.agents.length) {
            section('AGENTS');
            const PER_ROW = 16;
            for (let i = 0; i < s.agents.length; i += PER_ROW) {
                const dots = new St.BoxLayout({style: 'spacing: 6px;', x_expand: true});
                for (const a of s.agents.slice(i, i + PER_ROW)) {
                    const ok = a.state === 'authenticated';
                    const d = this._dot(ok ? TEAL : (a.state === 'not_authenticated' ? RED : MUTED), 10);
                    this._attachTip(d, `${a.name}: ${a.state.replaceAll('_', ' ')}`);
                    dots.add_child(d);
                }
                this._content.add_child(dots);
            }
        }
    }
}
