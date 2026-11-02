// localwispr GNOME Shell companion.
//
// A thin view + actuator for the Rust daemon:
//  * draws the dictation capsule from the daemon's D-Bus signals
//    (StateChanged / Levels / PartialText / Notice), and
//  * exports org.localwispr.Shell so the daemon can insert text: Mutter has
//    no clipboard or virtual-keyboard protocol for background clients, so
//    clipboard + paste keystroke happen here, inside the compositor.
//
// No logic lives here; everything else is in the daemon.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GIRepository from 'gi://GIRepository';
import Meta from 'gi://Meta';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const DAEMON_NAME = 'org.localwispr.Daemon';
const DAEMON_PATH = '/org/localwispr/Daemon';
const DAEMON_IFACE = 'org.localwispr.Daemon1';
const SHELL_NAME = 'org.localwispr.Shell';
const SHELL_PATH = '/org/localwispr/Shell';
const SETTINGS_APP_ID = 'org.localwispr.Settings';

const SHELL_XML = `
<node>
  <interface name="org.localwispr.Shell1">
    <method name="Inject">
      <arg type="s" direction="in" name="text"/>
      <arg type="s" direction="in" name="chord"/>
      <arg type="b" direction="in" name="restore"/>
      <arg type="u" direction="in" name="restore_delay_ms"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="Version">
      <arg type="s" direction="out" name="version"/>
    </method>
  </interface>
</node>`;

const BAR_W = 4;
const BAR_GAP = 4;
const BARS_H = 26;
const BAR_MIN = 4;
const BOTTOM_MARGIN = 64;
const CAPSULE_W = 220;
const CAPSULE_MAX_W = 420;
const WAVEFORM_W = 180;
const TEXT_LINES = 2;
// Exact critically damped motion: starts gently and keeps velocity when the
// transcript changes the destination, without frame-rate-dependent stepping.
function spring(value, velocity, target, dt, frequency) {
    const offset = value - target;
    const step = (velocity + frequency * offset) * dt;
    const decay = Math.exp(-frequency * dt);
    const next = target + (offset + step) * decay;
    const speed = (velocity - frequency * step) * decay;
    return Math.abs(next - target) < 0.08 && Math.abs(speed) < 1 ? [target, 0] : [next, speed];
}
const SNAPSHOT_LIMIT = 32 * 1024 * 1024;
const TRANSFER_TIMEOUT_MS = 5000;
const TEXT_MIMES = ['text/plain;charset=utf-8', 'UTF8_STRING', 'text/plain', 'STRING'];

function bestMime(mimes) {
    return TEXT_MIMES.find(mime => mimes.includes(mime)) ?? mimes[0];
}

const CHORDS = {
    'shift-insert': [Clutter.KEY_Shift_L, Clutter.KEY_Insert],
    'ctrl-v': [Clutter.KEY_Control_L, Clutter.KEY_v],
    'ctrl-shift-v': [Clutter.KEY_Control_L, Clutter.KEY_Shift_L, Clutter.KEY_v],
};

// Every delayed action belongs to one enable/service lifetime. Cancellation
// settles waits as well as removing their GLib sources.
class Lifetime {
    constructor() {
        this.active = true;
        this.cancellable = new Gio.Cancellable();
        this.sources = new Map();
    }

    wait(ms) {
        if (!this.active)
            return Promise.resolve(false);
        return new Promise(resolve => {
            const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
                this.sources.delete(id);
                resolve(this.active);
                return GLib.SOURCE_REMOVE;
            });
            this.sources.set(id, resolve);
        });
    }

    destroy() {
        this.active = false;
        this.cancellable.cancel();
        for (const [id, resolve] of this.sources) {
            GLib.Source.remove(id);
            resolve(false);
        }
        this.sources.clear();
    }
}

// GJS cannot override SelectionSource.read_async (a callback vfunc).
// The optional native helper supplies all saved MIME representations.
let nativeSelection = null;
function savedSelection(contents) {
    if (contents.length === 1 || !nativeSelection) {
        const mime = bestMime(contents.map(([type]) => type));
        return Meta.SelectionSourceMemory.new(mime, contents.find(([type]) => type === mime)[1]);
    }
    const values = Object.fromEntries(contents.map(([mime, bytes]) => [mime, bytes.get_data()]));
    const source = nativeSelection.create(new GLib.Variant('a{say}', values));
    if (source)
        return source;
    // The helper refuses to run on a Mutter whose ABI it does not match.
    console.warn('localwispr: clipboard helper incompatible with this GNOME; restoring text only');
    nativeSelection = null;
    return savedSelection(contents);
}

// Inject returns whether the clipboard was preserved; a D-Bus error means the
// text was not inserted.
function settle(invocation, pasted, preserved, failure) {
    if (pasted)
        invocation.return_value(new GLib.Variant('(b)', [preserved]));
    else
        invocation.return_dbus_error('org.localwispr.Shell1.Error.Failed', failure);
}

function loadNativeSelection(path) {
    nativeSelection = null;
    try {
        const repository = GIRepository.Repository.dup_default?.() ?? GIRepository.Repository.get_default();
        const [, bytes] = GLib.file_get_contents(`${path}/selection-abi`);
        const abi = new TextDecoder().decode(bytes).trim();
        if (abi !== '1')
            throw new Error(`unsupported helper ABI ${abi}`);
        // The helper looks up MetaSelectionSource by name; make sure it is registered.
        void Meta.SelectionSource.$gtype;
        // Do not import (and dlopen) the helper until its ABI has been checked.
        const paths = GIRepository.Repository.dup_default ? repository : GIRepository.Repository;
        paths.prepend_search_path(path);
        paths.prepend_library_path(path);
        nativeSelection = imports.gi.LocalwisprSelection;
    } catch (error) {
        console.warn(`localwispr: clipboard helper unavailable: ${error}`);
    }
}

class Capsule {
    constructor() {
        this.mode = 'hidden';
        this.target = new Array(16).fill(0);
        this.current = new Array(16).fill(0);
        this.time = 0;
        this.lastFrame = 0;
        this.noticeGen = 0;
        this.bottomMargin = BOTTOM_MARGIN;
        this.showLiveText = true;
        this.enabled = false;
        this.rawText = '';
        this.isNotice = false;
        this.targetWidth = CAPSULE_W;
        this.targetHeight = BARS_H + 20;
        this.widthVelocity = 0;
        this.heightVelocity = 0;

        // Explicit child geometry lets the capsule resize without exposing
        // extra wrapped lines halfway through an expansion.
        this.actor = new St.Widget({
            style_class: 'lw-capsule',
            reactive: false,
            visible: false,
            opacity: 0,
            width: CAPSULE_W,
            height: BARS_H + 20,
        });
        this.label = new St.Label({
            style_class: 'lw-live',
            x_align: Clutter.ActorAlign.CENTER,
            visible: false,
            clip_to_allocation: true,
        });
        const ct = this.label.clutter_text;
        ct.ellipsize = Pango.EllipsizeMode.NONE;
        ct.line_wrap = true;
        ct.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
        ct.line_alignment = Pango.Alignment.CENTER;
        this.area = new St.DrawingArea({height: BARS_H});
        this.area.connect('repaint', area => this._draw(area));
        this.actor.add_child(this.label);
        this.actor.add_child(this.area);

        Main.layoutManager.addTopChrome(this.actor, {
            trackFullscreen: false,
        });
        // Height changes are notified during allocation; moving the actor
        // there makes Clutter warn, so reposition before the next redraw.
        this._laterId = 0;
        const queueLayout = () => {
            this._laterId ||= global.compositor.get_laters().add(Meta.LaterType.BEFORE_REDRAW, () => {
                this._laterId = 0;
                this._layout();
                this._reposition();
                return GLib.SOURCE_REMOVE;
            });
        };
        this.actor.connect('notify::height', queueLayout);
        this.actor.connect('notify::width', queueLayout);

        this.timeline = new Clutter.Timeline({
            actor: this.area,
            duration: 1000,
            repeat_count: -1,
        });
        this._frameId = this.timeline.connect('new-frame', () => this._tick());
        // The Shell destroys actors at exit without disabling extensions.
        this.actor.connect('destroy', () => this.timeline.stop());
    }

    destroy() {
        if (this._laterId)
            global.compositor.get_laters().remove(this._laterId);
        this._laterId = 0;
        this.timeline.stop();
        this.timeline.disconnect(this._frameId);
        this.actor.remove_all_transitions();
        this.actor.destroy();
    }

    _monitor() {
        const win = global.display.focus_window;
        const idx = win ? win.get_monitor() : -1;
        return Main.layoutManager.monitors[idx] ?? Main.layoutManager.currentMonitor ??
            Main.layoutManager.primaryMonitor;
    }

    _reposition() {
        const m = this._monitor();
        if (!m)
            return;
        const x = Math.round(m.x + (m.width - this.actor.width) / 2);
        const y = Math.round(m.y + m.height - this.actor.height - this.bottomMargin);
        this.actor.set_position(x, y);
    }

    _scale() {
        return St.ThemeContext.get_for_stage(global.stage).scale_factor;
    }

    _animationsEnabled() {
        return St.Settings.get().enable_animations;
    }

    _updateSize(reset = false) {
        const scale = this._scale();
        const monitor = this._monitor();
        this._monitorWidth = monitor?.width;
        this._sizeScale = scale;
        const maxWidth = Math.max(1, Math.min(CAPSULE_MAX_W * scale,
            monitor ? monitor.width - 32 * scale : CAPSULE_MAX_W * scale));
        this._maxWidth = maxWidth;
        const minWidth = Math.min(CAPSULE_W * scale, maxWidth);
        const node = this.actor.get_theme_node();
        const layout = this.label.clutter_text.get_layout().copy();
        layout.set_width(-1);
        layout.set_text(this.rawText, -1);
        const [textWidth] = layout.get_pixel_size();
        const visible = this.rawText.length > 0 && (this.isNotice || this.showLiveText);
        const requested = visible ? textWidth + node.get_horizontal_padding() : minWidth;
        const width = Math.max(minWidth, Math.min(maxWidth, requested));
        // Preview revisions can remove words; keep a steady silhouette until
        // the next recording, while still respecting a smaller monitor.
        this.targetWidth = reset || !visible ? width : Math.min(maxWidth, Math.max(this.targetWidth, width));
        if (reset || this.mode === 'hidden' || !this._animationsEnabled()) {
            this.actor.width = this.targetWidth;
            this.widthVelocity = 0;
        } else if (this.actor.width > maxWidth) {
            this.actor.width = maxWidth;
            this.widthVelocity = 0;
        }
        this._layout();
        if (reset || this.mode === 'hidden' || !this._animationsEnabled()) {
            this.actor.height = this.targetHeight;
            this.heightVelocity = 0;
            this._layout();
        }
        this._reposition();
    }

    _layout() {
        const scale = this._scale();
        const node = this.actor.get_theme_node();
        const padX = node.get_horizontal_padding() / 2;
        const padY = node.get_vertical_padding() / 2;
        const width = Math.max(1, Math.floor(this.actor.width - 2 * padX));
        const barsHeight = BARS_H * scale;
        const visible = this.rawText.length > 0 && (this.isNotice || this.showLiveText);
        if (this._renderedText !== this.rawText || this._renderedWidth !== width ||
            this._renderedScale !== scale || this._renderedNotice !== this.isNotice) {
            this._renderedText = this.rawText;
            this._renderedWidth = width;
            this._renderedScale = scale;
            this._renderedNotice = this.isNotice;
            const tail = this._tail(this.rawText, width);
            if (this.label.text !== tail)
                this.label.text = tail;
            // Query Pango directly: an actor's preferred height includes any
            // explicit height from the preceding animation frame.
            const layout = this.label.clutter_text.get_layout().copy();
            layout.set_width(width * Pango.SCALE);
            layout.set_text(this.label.text, -1);
            this._textHeight = layout.get_pixel_size()[1];
            layout.set_width(-1);
            layout.set_text('Ag', -1);
            this._lineHeight = layout.get_pixel_size()[1];
        }
        // Keep two lines of room once text arrives so line wrapping doesn't
        // make the capsule bounce as the width eases outward.
        const gap = 6 * scale;
        this.targetHeight = 2 * padY + barsHeight + (visible ? 2 * this._lineHeight + gap : 0);
        const available = Math.max(0, this.actor.height - 2 * padY - barsHeight - gap);
        this.label.set_position(padX, padY);
        this.label.set_size(width, Math.min(available, this._textHeight));
        this.label.visible = visible && available > 0;
        this.area.set_position(padX, this.actor.height - padY - barsHeight);
        this.area.set_size(width, barsHeight);
    }

    configure([frontend, bands, margin, liveText, opacity]) {
        this.enabled = frontend === 'auto' || frontend === 'gnome';
        this.bottomMargin = margin;
        this.showLiveText = liveText;
        bands = Math.max(4, Math.min(48, bands));
        if (this.current.length !== bands) {
            this.current = new Array(bands).fill(0);
            this.target = new Array(bands).fill(0);
        }
        this.actor.set_style(`background-color: rgba(0, 0, 0, ${Math.max(0, Math.min(1, opacity))});`);
        this._updateSize();
        if (!this.enabled)
            this.hide();
    }

    show(mode) {
        if (!this.enabled)
            return;
        const entering = this.mode === 'hidden' || this.mode === 'fading';
        if (entering) {
            this.current.fill(0);
            this.target.fill(0);
            this.lastFrame = 0;
            this.actor.remove_all_transitions();
        }
        this.mode = mode;
        this.actor.show();
        this._updateSize();
        this._reposition();
        if (entering) {
            if (this.actor.opacity === 0)
                this.actor.translation_y = this._animationsEnabled() ? 6 * this._scale() : 0;
            this.actor.ease({opacity: 255, translation_y: 0, duration: 260,
                mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC});
        }
        if (!this.timeline.is_playing())
            this.timeline.start();
    }

    hide() {
        if (this.mode === 'hidden' || this.mode === 'fading')
            return;
        this.mode = 'fading';
        this.actor.ease({
            opacity: 0,
            translation_y: this._animationsEnabled() ? 3 * this._scale() : 0,
            duration: 70, // the daemon pastes 80 ms after going idle
            mode: Clutter.AnimationMode.EASE_IN_QUAD,
            onComplete: () => {
                if (this.mode !== 'fading')
                    return;
                this.mode = 'hidden';
                this.actor.hide();
                this.timeline.stop();
            },
        });
    }

    setText(text, notice = false) {
        this.rawText = text.replace(/\s+/g, ' ').trim();
        this.isNotice = notice;
        this.label.style_class = notice ? 'lw-notice' : 'lw-live';
        this._updateSize(this.mode === 'hidden' || this.mode === 'fading' ||
            !this.rawText && this.mode !== 'recording');
    }

    // The newest words that fit in TEXT_LINES lines, cut at a word boundary
    // ("… ask what you can do for your country.") so the line never jumps
    // mid-word as the preview grows.
    _tail(text, width) {
        text = text.replace(/\s+/g, ' ').trim();
        if (!text || width <= 0)
            return text;
        const layout = this.label.clutter_text.get_layout().copy();
        layout.set_ellipsize(Pango.EllipsizeMode.NONE);
        layout.set_wrap(Pango.WrapMode.WORD_CHAR);
        layout.set_width(width * Pango.SCALE);
        const fits = t => {
            layout.set_text(t, -1);
            return layout.get_line_count() <= TEXT_LINES;
        };
        const words = text.split(' ');
        if (fits(text))
            return text;
        // An unbroken URL or identifier can itself exceed two lines.
        // Trim by Unicode code point so the viewport stays bounded.
        if (words.length < 2 || !fits(`… ${words.at(-1)}`)) {
            const chars = Array.from(words.at(-1));
            let lo = 0, hi = chars.length;
            while (lo < hi) {
                const mid = (lo + hi) >> 1;
                if (fits(`… ${chars.slice(mid).join('')}`))
                    hi = mid;
                else
                    lo = mid + 1;
            }
            return `… ${chars.slice(lo).join('')}`;
        }
        let lo = 1, hi = words.length - 1; // smallest start index that fits
        while (lo < hi) {
            const mid = (lo + hi) >> 1;
            if (fits(`… ${words.slice(mid).join(' ')}`))
                hi = mid;
            else
                lo = mid + 1;
        }
        return `… ${words.slice(lo).join(' ')}`;
    }

    setLevels(bands) {
        this.target.fill(0);
        for (let i = 0; i < Math.min(bands.length, this.target.length); i++)
            this.target[i] = bands[i];
    }

    _tick() {
        const now = GLib.get_monotonic_time();
        const dt = this.lastFrame ? Math.min((now - this.lastFrame) / 1e6, 0.1) : 1 / 60;
        this.lastFrame = now;
        const animate = this._animationsEnabled();
        if (animate)
            this.time += dt;
        for (let i = 0; i < this.current.length; i++) {
            const rate = this.target[i] > this.current[i] ? 20 : 12;
            const k = 1 - Math.exp(-dt * rate);
            this.current[i] += (this.target[i] - this.current[i]) * k;
        }
        if (this._monitorWidth !== this._monitor()?.width || this._sizeScale !== this._scale())
            this._updateSize();
        let width, height;
        [width, this.widthVelocity] = animate ?
            spring(this.actor.width, this.widthVelocity, this.targetWidth, dt, 12) : [this.targetWidth, 0];
        if (width > this._maxWidth) {
            width = this._maxWidth;
            this.widthVelocity = 0;
        }
        this.actor.width = width;
        this._layout();
        [height, this.heightVelocity] = animate ?
            spring(this.actor.height, this.heightVelocity, this.targetHeight, dt, 14) : [this.targetHeight, 0];
        this.actor.height = height;
        this._layout();
        this._reposition();
        this.area.queue_repaint();
    }

    _draw(area) {
        const cr = area.get_context();
        const [w, h] = area.get_surface_size();
        const n = this.current.length;
        const scale = this._scale();
        const step = Math.min((BAR_W + BAR_GAP) * scale,
            Math.min(WAVEFORM_W * scale, w) / (n * 2));
        const barWidth = Math.min(BAR_W * scale, step * 0.56);
        const total = n * 2 * step - (step - barWidth);
        const x0 = (w - total) / 2;
        const mid = h / 2;
        for (let slot = 0; slot < n * 2; slot++) {
            const band = slot < n ? n - 1 - slot : slot - n;
            let level, alpha;
            if (this.mode === 'processing') {
                const wave = 0.5 + 0.5 * Math.sin(this.time * 7 - slot * 0.45);
                level = 0.08 + 0.18 * wave;
                alpha = 0.35 + 0.55 * wave;
            } else if (this.mode === 'notice') {
                level = 0;
                alpha = 0.35;
            } else {
                level = Math.max(0, Math.min(1, this.current[band]));
                alpha = 0.92;
            }
            const bh = BAR_MIN * scale + level * (h - BAR_MIN * scale);
            const x = x0 + slot * step;
            const y = mid - bh / 2;
            const r = barWidth / 2;
            cr.newSubPath();
            cr.arc(x + r, y + r, r, Math.PI, 0);
            cr.arc(x + r, y + bh - r, r, 0, Math.PI);
            cr.closePath();
            cr.setSourceRGBA(1, 1, 1, alpha);
            cr.fill();
        }
        cr.$dispose();
    }
}

class ShellService {
    constructor(version) {
        this._version = String(version);
        const seat = global.stage.context.get_backend().get_default_seat();
        this._keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._selection = global.display.get_selection();
        this._owners = new Map();
        this._ownerWatch = this._selection.connect('owner-changed', (_s, type, source) => {
            this._owners.set(type, source);
        });
        this._lifetime = new Lifetime();
        this._generation = 0;
        this._restore = [];
        this._pending = new Set();
        this._pasted = new Set();
    }

    Version() {
        return this._version;
    }

    destroy() {
        ++this._generation;
        this._lifetime.destroy();
        // Restore synchronously while our sources still own the selections.
        this._restoreOwned();
        this._selection.disconnect(this._ownerWatch);
        this._ownerWatch = 0;
        this._owners.clear();
        for (const invocation of this._pending)
            settle(invocation, this._pasted.has(invocation), false, 'Extension disabled');
        this._pending.clear();
        this._pasted.clear();
        this._keyboard.run_dispose();
        this._keyboard = null;
    }

    _valid(generation) {
        return this._lifetime.active && generation === this._generation;
    }

    async _save(type, generation) {
        const original = this._owners.get(type);
        const contents = [];
        let remaining = SNAPSHOT_LIMIT;
        let mimes = this._selection.get_mimetypes(type) ?? [];
        if (!nativeSelection && mimes.length)
            mimes = [bestMime(mimes)];
        for (const mime of mimes) {
            const output = Gio.MemoryOutputStream.new_resizable();
            const transfer = new Lifetime();
            const cancelled = this._lifetime.cancellable.connect(() => transfer.destroy());
            transfer.wait(TRANSFER_TIMEOUT_MS).then(active => {
                if (active)
                    transfer.cancellable.cancel();
            });
            try {
                await new Promise((resolve, reject) => {
                    // One extra byte detects overflow without restoring truncated data.
                    this._selection.transfer_async(type, mime, remaining + 1, output,
                        transfer.cancellable, (selection, result) => {
                            try {
                                if (!selection.transfer_finish(result))
                                    throw new Error('Clipboard transfer failed');
                                resolve();
                            } catch (error) {
                                reject(error);
                            }
                        });
                });
                if (!this._valid(generation))
                    throw new Error('Insertion cancelled');
                if (this._owners.get(type) !== original)
                    throw new Error('Clipboard changed while saving');
                if (output.get_data_size() > remaining)
                    throw new Error('Clipboard snapshot exceeds 32 MiB');
                remaining -= output.get_data_size();
                output.close(null);
                contents.push([mime, output.steal_as_bytes()]);
            } finally {
                this._lifetime.cancellable.disconnect(cancelled);
                transfer.destroy();
                output.close(null);
            }
        }
        return contents.length ? savedSelection(contents) : null;
    }

    _restoreOwned() {
        const snapshots = this._restore;
        this._restore = [];
        let ok = true;
        for (const {type, injected, saved} of snapshots) {
            if (this._owners.get(type) !== injected)
                continue;
            try {
                if (saved)
                    this._selection.set_owner(type, saved);
                else
                    this._selection.unset_owner(type, injected);
            } catch (error) {
                logError(error, `localwispr: selection ${type} restoration failed`);
                ok = false;
            }
        }
        return ok;
    }

    _press(keyvals) {
        let t = GLib.get_monotonic_time();
        for (const kv of keyvals)
            this._keyboard.notify_keyval(t++, kv, Clutter.KeyState.PRESSED);
        for (const kv of [...keyvals].reverse())
            this._keyboard.notify_keyval(t++, kv, Clutter.KeyState.RELEASED);
    }

    async InjectAsync(params, invocation) {
        const [text, chord, restore, delayMs] = params;
        if (!this._lifetime.active || this._pending.size) {
            invocation.return_dbus_error('org.localwispr.Shell1.Error.Busy', 'Another insertion is in progress');
            return;
        }
        const generation = ++this._generation;
        this._pending.add(invocation);
        let ok = false;
        let failure = 'Insertion cancelled';
        try {
            let backedUp = this._restoreOwned();
            const types = [Meta.SelectionType.SELECTION_CLIPBOARD, Meta.SelectionType.SELECTION_PRIMARY];
            const snapshots = [];
            for (const type of types) {
                let saved = null;
                let restorable = restore;
                try {
                    saved = restore ? await this._save(type, generation) : null;
                } catch (error) {
                    if (!this._valid(generation))
                        return;
                    logError(error, `localwispr: selection ${type} backup failed; skipping restoration`);
                    backedUp = false;
                    restorable = false;
                }
                if (!this._valid(generation))
                    return;
                snapshots.push({type, saved, restorable, owner: this._owners.get(type)});
            }
            // Both selections are saved before either is overwritten.
            if (snapshots.some(({type, owner}) => this._owners.get(type) !== owner))
                throw new Error('Clipboard changed while saving');
            const installed = [];
            for (const {type, saved, restorable} of snapshots) {
                const bytes = new GLib.Bytes(new TextEncoder().encode(text));
                const formats = [['text/plain;charset=utf-8', bytes]];
                if (nativeSelection)
                    formats.push(['text/plain', bytes], ['UTF8_STRING', bytes]);
                const injected = savedSelection(formats);
                this._selection.set_owner(type, injected);
                installed.push({type, injected});
                if (restorable)
                    this._restore.push({type, injected, saved});
            }
            // Clipboard propagation is separate from the daemon's HUD dissolve wait.
            if (!await this._lifetime.wait(20) || !this._valid(generation))
                return;
            if (installed.some(({type, injected}) => this._owners.get(type) !== injected))
                throw new Error('Clipboard ownership lost before paste');
            this._press(CHORDS[chord] ?? CHORDS['shift-insert']);
            this._pasted.add(invocation);
            if (restore) {
                if (!await this._lifetime.wait(Math.max(100, delayMs)) || !this._valid(generation))
                    return;
                backedUp = this._restoreOwned() && backedUp;
            }
            ok = backedUp;
        } catch (e) {
            failure = e.message;
            if (this._valid(generation)) {
                logError(e, 'localwispr: inject failed');
                this._restoreOwned();
            }
        } finally {
            if (this._pending.delete(invocation))
                settle(invocation, this._pasted.has(invocation), ok, failure);
            this._pasted.delete(invocation);
        }
    }
}

export default class LocalWisprExtension extends Extension {
    enable() {
        loadNativeSelection(this.path);
        this._lifetime = new Lifetime();
        this._state = 'idle';
        this._stateRevision = 0;
        this._daemonGeneration = 0;
        this._pendingNotice = null;
        this._capsule = new Capsule();
        this._service = new ShellService(this.metadata.version ?? 1);
        this._exported = Gio.DBusExportedObject.wrapJSObject(SHELL_XML, this._service);
        this._exported.export(Gio.DBus.session, SHELL_PATH);
        this._ownerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, SHELL_NAME, Gio.BusNameOwnerFlags.REPLACE, null, null);

        this._signalId = Gio.DBus.session.signal_subscribe(
            DAEMON_NAME, DAEMON_IFACE, null, DAEMON_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_conn, _sender, _path, _iface, signal, params) => this._onSignal(signal, params));

        // If the daemon dies mid-dictation, don't leave the capsule on screen.
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION, DAEMON_NAME, Gio.BusNameWatcherFlags.NONE,
            () => this._syncDaemon(), () => {
                ++this._daemonGeneration;
                this._state = 'idle';
                this._pendingNotice = null;
                this._capsule?.hide();
            });
    }

    disable() {
        this._lifetime?.destroy();
        ++this._daemonGeneration;
        this._service?.destroy();
        if (this._signalId) {
            Gio.DBus.session.signal_unsubscribe(this._signalId);
            this._signalId = 0;
        }
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        this._exported?.unexport();
        this._exported = null;
        this._service = null;
        this._capsule?.destroy();
        this._capsule = null;
        if (this._settingsWatch) {
            global.display.disconnect(this._settingsWatch);
            this._settingsWatch = 0;
        }
    }

    _call(method) {
        const lifetime = this._lifetime;
        return new Promise((resolve, reject) => {
            Gio.DBus.session.call(DAEMON_NAME, DAEMON_PATH, DAEMON_IFACE, method,
                null, null, Gio.DBusCallFlags.NO_AUTO_START, 3000, lifetime.cancellable,
                (conn, result) => {
                    try {
                        const value = conn.call_finish(result).deepUnpack();
                        resolve(value.length === 1 && Array.isArray(value[0]) ? value[0] : value);
                    } catch (error) {
                        reject(error);
                    }
                });
        });
    }

    async _syncDaemon() {
        const lifetime = this._lifetime;
        const generation = ++this._daemonGeneration;
        const valid = () => lifetime.active && generation === this._daemonGeneration;
        let bootstrap = true;
        let recovering = false;
        let backoff = 250;
        while (valid()) {
            try {
                const revision = this._stateRevision;
                const [config, statusReply] = await Promise.all([
                    this._call('GetHudConfig'), bootstrap ? this._call('GetStatus') : null,
                ]);
                if (!valid())
                    return;
                const wasEnabled = this._capsule.enabled;
                this._capsule.configure(config);
                // A state signal received during the snapshot wins over the reply.
                if (bootstrap && revision === this._stateRevision)
                    this._state = statusReply[0]?.state ?? 'idle';
                if (bootstrap || recovering || !wasEnabled && this._capsule.enabled)
                    this._applyState();
                bootstrap = false;
                recovering = false;
                backoff = 250;
                if (!await lifetime.wait(1000))
                    return;
            } catch (error) {
                if (!valid())
                    return;
                console.warn(`localwispr: daemon sync failed; retrying: ${error}`);
                this._capsule?.hide();
                recovering = true;
                if (!await lifetime.wait(backoff))
                    return;
                backoff = Math.min(backoff * 2, 5000);
            }
        }
    }

    _applyState() {
        const c = this._capsule;
        if (!c)
            return;
        if (this._state === 'recording') {
            if (c.mode !== 'recording')
                c.setText('');
            c.show('recording');
        } else if (this._state === 'processing') {
            c.show('processing');
        } else if (this._pendingNotice) {
            const message = this._pendingNotice;
            this._pendingNotice = null;
            this._showNotice(message);
        } else if (c.mode !== 'notice') {
            c.hide();
        }
    }

    async _showNotice(message) {
        const c = this._capsule;
        const lifetime = this._lifetime;
        if (!c || !c.enabled)
            return;
        c.setText(message, true);
        c.show('notice');
        const gen = ++c.noticeGen;
        if (await lifetime.wait(2800) && this._capsule === c && c.mode === 'notice' && c.noticeGen === gen)
            c.hide();
    }

    _onSignal(signal, params) {
        const c = this._capsule;
        if (!c || !this._lifetime.active)
            return;
        switch (signal) {
        case 'StateChanged':
            ++this._stateRevision;
            [this._state] = params.deepUnpack();
            this._applyState();
            break;
        case 'Levels':
            if (c.mode === 'recording')
                c.setLevels(params.deepUnpack()[0]);
            break;
        case 'PartialText':
            if (c.mode === 'recording' || c.mode === 'processing')
                c.setText(params.deepUnpack()[0]);
            break;
        case 'Notice': {
            const [level, message] = params.deepUnpack();
            if (level === 'info')
                break;
            if (this._state === 'recording' || this._state === 'processing')
                this._pendingNotice = message;
            else
                this._showNotice(message);
            break;
        }
        case 'SettingsRequested':
            this._raiseSettings();
            break;
        }
    }

    // Focus-stealing prevention would leave a daemon-launched window behind;
    // the Shell is allowed to activate it.
    _raiseSettings() {
        const lifetime = this._lifetime;
        if (!lifetime.active)
            return;
        const isSettings = w => w && (w.get_gtk_application_id?.() === SETTINGS_APP_ID ||
            w.get_wm_class?.() === SETTINGS_APP_ID);
        const existing = global.get_window_actors().map(a => a.meta_window).find(isSettings);
        if (existing) {
            Main.activateWindow(existing);
            return;
        }
        if (this._settingsWatch)
            global.display.disconnect(this._settingsWatch);
        this._settingsWatch = global.display.connect('window-created', (_d, win) => {
            // Application id is set shortly after creation.
            lifetime.wait(50).then(active => {
                if (active && isSettings(win)) {
                    Main.activateWindow(win);
                    if (this._settingsWatch) {
                        global.display.disconnect(this._settingsWatch);
                        this._settingsWatch = 0;
                    }
                }
            });
        });
        lifetime.wait(5000).then(active => {
            if (active && this._settingsWatch) {
                global.display.disconnect(this._settingsWatch);
                this._settingsWatch = 0;
            }
        });
    }
}
