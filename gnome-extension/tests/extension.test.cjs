// Deterministic lifecycle regressions. Executes the extension with mocked Shell
// services; no connection to the user's clipboard, keyboard or desktop.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

function signalObject(properties = {}) {
    let next = 1;
    return Object.assign({
        handlers: new Map(),
        connect(signal, callback) {
            const id = next++;
            this.handlers.set(id, {signal, callback});
            return id;
        },
        disconnect(id) { this.handlers.delete(id); },
        emit(signal, ...args) {
            for (const {signal: name, callback} of [...this.handlers.values()])
                if (name === signal) callback(this, ...args);
        },
    }, properties);
}

function harness() {
    let nextSource = 1;
    let frameTime = 0;
    const timers = new Map();
    const keyEvents = [];
    const warnings = [];
    const errors = [];
    const transfers = [];
    let disposed = 0;
    const owners = new Map([
        [1, {formats: new Map([['text/plain', 'original'], ['text/html', '<b>original</b>']])}],
        [0, {formats: new Map([['text/plain', 'selection']])}],
    ]);
    let ownerChanged;
    const selection = {
        connect(_signal, callback) { ownerChanged = callback; return 1; },
        disconnect() { ownerChanged = null; },
        get_mimetypes(type) { return [...owners.get(type)?.formats.keys() ?? []]; },
        transfer_async(type, mime, size, output, cancel, callback) {
            transfers.push({type, mime, size});
            output.bytes = owners.get(type).formats.get(mime).slice(0, size);
            queueMicrotask(() => callback(selection, {cancel}));
        },
        transfer_finish(result) {
            if (result.cancel?.cancelled)
                throw new Error('Cancelled');
            return true;
        },
        set_owner(type, source) {
            owners.set(type, source);
            ownerChanged?.(selection, type, source);
        },
        unset_owner(type, source) {
            if (owners.get(type) === source) {
                owners.delete(type);
                ownerChanged?.(selection, type, null);
            }
        },
    };
    class Variant {
        constructor(_type, value) { this.value = value; }
        deepUnpack() { return this.value; }
    }
    const animationSettings = {enable_animations: true};
    const monitor = {x: 0, y: 0, width: 1280, height: 720};
    class Layout {
        constructor() { this.width = -1; this.text = ''; }
        copy() { return Object.assign(new Layout(), this); }
        set_width(width) { this.width = width; }
        set_text(text) { this.text = text; }
        set_ellipsize() {}
        set_wrap() {}
        get_line_count() { return this.width < 0 ? 1 : Math.max(1, Math.ceil(Array.from(this.text).length * 7 / this.width)); }
        get_pixel_size() { return [Array.from(this.text).length * 7, this.get_line_count() * 16]; }
    }
    class Actor {
        constructor(properties) {
            Object.assign(this, {width: 0, height: 0, text: '', opacity: 255, visible: true, easeCalls: 0}, properties);
            this.clutter_text = {get_layout: () => {
                const layout = new Layout(); layout.text = this.text; return layout;
            }};
            this.handlers = new Map();
        }
        connect(signal, callback) { this.handlers.set(signal, callback); return this.handlers.size; }
        add_child() {}
        get_theme_node() { return {get_horizontal_padding: () => 40, get_vertical_padding: () => 20}; }
        get_preferred_height(width) { return [0, Math.max(1, Math.ceil(Array.from(this.text).length * 7 / width)) * 16]; }
        set_size(width, height) { this.width = width; this.height = height; }
        set_position(x, y) { this.x = x; this.y = y; }
        set_style(style) { this.style = style; }
        remove_all_transitions() { this.completion = null; }
        ease({onComplete, duration, mode, ...values}) { this.easeCalls++; Object.assign(this, values); this.completion = onComplete; }
        show() { this.visible = true; }
        hide() { this.visible = false; }
        queue_repaint() {}
        destroy() { this.handlers.get('destroy')?.(); }
    }
    const context = vm.createContext({
        TextEncoder, TextDecoder, console: {warn(message) { warnings.push(message); }},
        Clutter: {InputDeviceType: {KEYBOARD_DEVICE: 1}, KeyState: {PRESSED: 1, RELEASED: 0},
            InputContentPurpose: {NORMAL: 0, PASSWORD: 8, TERMINAL: 10},
            InputContentHintFlags: {HIDDEN_TEXT: 64, SENSITIVE_DATA: 128},
            ActorAlign: {CENTER: 0}, AnimationMode: {EASE_IN_OUT_CUBIC: 1, EASE_IN_QUAD: 2},
            Timeline: class {
                connect() { return 1; } disconnect() {} start() { this.playing = true; }
                stop() { this.playing = false; } is_playing() { return this.playing; }
            }},
        Gio: {
            Cancellable: class {
                constructor() { this.handlers = new Map(); this.next = 1; }
                connect(fn) { const id = this.next++; this.handlers.set(id, fn); return id; }
                disconnect(id) { this.handlers.delete(id); }
                cancel() {
                    if (this.cancelled) return;
                    this.cancelled = true;
                    for (const fn of this.handlers.values()) fn();
                }
            },
            MemoryOutputStream: {new_resizable() { return {
                close() {}, get_data_size() { return Buffer.byteLength(this.bytes ?? ''); },
                steal_as_bytes() { const value = this.bytes; return {get_data() { return value; }}; },
            }; }},
            DBusError: {FAILED: 1},
        },
        GLib: {
            Variant,
            Bytes: class { constructor(value) { this.value = value; } get_data() { return this.value; } },
            PRIORITY_DEFAULT: 0, SOURCE_REMOVE: false,
            timeout_add(_priority, ms, fn) { const id = nextSource++; timers.set(id, {ms, fn}); return id; },
            Source: {remove(id) { timers.delete(id); }},
            get_monotonic_time() { frameTime += 1e6 / 60; return frameTime; },
            uuid_string_random() { return `test-token-${nextSource++}`; },
        },
        Meta: {
            SelectionType: {SELECTION_PRIMARY: 0, SELECTION_CLIPBOARD: 1, SELECTION_DND: 2},
            SelectionSourceMemory: {new(mime, bytes) { return {formats: new Map([[mime, bytes.get_data()]])}; }},
        },
        global: {
            stage: signalObject({context: {get_backend() { return {get_default_seat() { return {
                create_virtual_device() { return {
                    notify_keyval(...event) { keyEvents.push(event); }, run_dispose() { disposed++; },
                }; },
            }; }}; }}}),
            display: signalObject({get_selection() { return selection; }}),
        },
        St: {Widget: Actor, Label: Actor, DrawingArea: Actor,
            ThemeContext: {get_for_stage: () => ({scale_factor: 1})}, Settings: {get: () => animationSettings}},
        Pango: {SCALE: 1, EllipsizeMode: {NONE: 0}, WrapMode: {WORD_CHAR: 0}, Alignment: {CENTER: 0}},
        Main: {layoutManager: {monitors: [monitor], currentMonitor: monitor, addTopChrome() {}}},
        GIRepository: {}, Extension: class {},
        logError(error, message) { errors.push(`${message}: ${error}`); },
    });
    const filename = path.join(__dirname, '../localwispr@localwispr.github.io/extension.js');
    const code = fs.readFileSync(filename, 'utf8')
        .replace(/^import .*;\n/gm, '').replace('export default class', 'class');
    vm.runInContext(`${code}\nthis.api = {Lifetime, ShellService, LocalWisprExtension, Capsule, loadNativeSelection, spring};`, context);
    vm.runInContext(`nativeSelection = {create(variant) { return {formats: new Map(Object.entries(variant.value))}; }};`, context);
    const flush = () => new Promise(resolve => setImmediate(resolve));
    async function fire(ms) {
        for (const [id, timer] of [...timers]) {
            if (timer.ms === ms) { timers.delete(id); timer.fn(); }
        }
        await flush();
    }
    function invocation(sender = ':1.42') {
        return {replies: [], values: [], get_sender() { return sender; },
            return_value(value) { this.values.push(Array.from(value.value)); this.replies.push(value.value[0]); },
            return_dbus_error(name) { this.replies.push(name.split('.').pop()); },
            return_error_literal(...error) { this.replies.push(error); }};
    }
    return {...context.api, context, owners, selection, timers, keyEvents, warnings, errors, transfers, monitor, animationSettings,
        get disposed() { return disposed; }, flush, fire, invocation};
}

function contextHarness(properties = {}) {
    const h = harness();
    const inputMethod = signalObject({
        currentFocus: {}, content_purpose: 0, content_hints: 0, preedit: false,
        snapshot: ['cached text', 6, 6], requests: 0, reads: 0,
        hasPreedit() { return this.preedit; },
        getSurroundingText() { this.reads++; return this.snapshot; },
        request_surrounding() { this.requests++; },
        ...properties,
    });
    h.context.Main.inputMethod = inputMethod;
    const service = new h.ShellService(1);
    service._setDaemonOwner(':1.42');
    const reply = h.invocation();
    const request = () => service.GetCursorContextAsync([], reply);
    const respond = snapshot => {
        inputMethod.snapshot = snapshot;
        inputMethod.emit('surrounding-text-set');
    };
    const assertClean = () => {
        assert.equal(h.timers.size, 0);
        assert.equal(inputMethod.handlers.size, 0);
        assert.equal(h.context.global.display.handlers.size, 0);
        assert.equal(h.context.global.stage.handlers.size, 0);
        assert.equal(service._contextRequests.size, 0);
        assert.equal(h.keyEvents.length, 0);
        assert.equal(h.transfers.length, 0);
        assert.equal(h.errors.length, 0);
        assert.equal(h.warnings.length, 0);
        assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
        assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
    };
    return {...h, service, reply, inputMethod, request, respond, assertClean};
}

test('cursor context waits for a requested signal and returns text around the insertion point', () => {
    const h = contextHarness();
    h.inputMethod.emit('surrounding-text-set'); // An earlier response is insufficient.
    h.request();
    assert.equal(h.inputMethod.requests, 1);
    assert.equal(h.inputMethod.reads, 0);
    assert.deepEqual(h.reply.values, []);
    h.respond(['before AFTER', 7, 7]);
    assert.deepEqual(h.reply.values, [['available', 'before ', 'AFTER', false]]);
    h.respond(['unrelated', 0, 0]);
    assert.equal(h.reply.values.length, 1);
    assert.equal(h.inputMethod.reads, 1);
    h.assertClean();
    h.service.destroy();
});

test('cursor context catches a synchronous committed response after connecting the signal', () => {
    const h = contextHarness({request_surrounding() {
        this.requests++;
        this.snapshot = ['left😀right', 5, 5];
        this.emit('surrounding-text-set');
    }});
    h.request();
    assert.deepEqual(h.reply.values, [['available', 'left😀', 'right', false]]);
    h.assertClean();
    h.service.destroy();
});

test('cursor context times out after 100ms when no signal is emitted', async () => {
    const h = contextHarness();
    h.request();
    assert.deepEqual([...h.timers.values()].map(timer => timer.ms), [100]);
    await h.fire(100);
    assert.deepEqual(h.reply.values, [['timeout', '', '', false]]);
    assert.equal(h.inputMethod.reads, 0);
    h.respond(['late text', 2, 2]);
    assert.equal(h.reply.values.length, 1);
    h.assertClean();
    h.service.destroy();
});

test('cursor context excludes either selection direction and limits each side by Unicode characters', () => {
    for (const reversed of [false, true]) {
        const h = contextHarness();
        const before = '🦊'.repeat(300);
        const selected = '😀selection👩‍💻';
        const after = '𐐀'.repeat(300);
        const start = Array.from(before).length;
        const end = start + Array.from(selected).length;
        h.request();
        h.respond([before + selected + after, reversed ? end : start, reversed ? start : end]);
        assert.deepEqual(h.reply.values, [['available', '🦊'.repeat(256), '𐐀'.repeat(256), true]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('cursor context reports empty fresh input, including selection of the whole value', () => {
    for (const [snapshot, replacement] of [[['', 0, 0], false], [['😀', 0, 1], true]]) {
        const h = contextHarness();
        h.request();
        h.respond(snapshot);
        assert.deepEqual(h.reply.values, [['empty', '', '', replacement]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('cursor context rejects malformed and out-of-range character offsets', () => {
    for (const snapshot of [
        ['😀', 2, 2], ['text', -1, 0], ['text', 0, 5], ['text', 1.5, 0],
        ['text', 0, NaN], ['text', '1', 1], ['text', null, 0], [null, 0, 0],
        ['text', Infinity, 0], ['text', 0], null,
    ]) {
        const h = contextHarness();
        h.request();
        h.respond(snapshot);
        assert.deepEqual(h.reply.values, [['unavailable', '', '', false]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('cursor context bounds work for oversized surrounding documents', () => {
    const h = contextHarness();
    h.request();
    h.respond(['a'.repeat(65537), 0, 0]);
    assert.deepEqual(h.reply.values, [['unavailable', '', '', false]]);
    h.assertClean();
    h.service.destroy();
});

test('cursor context protects Clutter PASSWORD/PIN and SENSITIVE/HIDDEN_TEXT fields', () => {
    for (const fields of [{content_purpose: 8}, {content_hints: 64}, {content_hints: 128}, {content_hints: 64 | 1}]) {
        const h = contextHarness(fields);
        h.request();
        assert.deepEqual(h.reply.values, [['protected', '', '', false]]);
        assert.equal(h.inputMethod.reads, 0);
        assert.equal(h.inputMethod.requests, 0);
        h.assertClean();
        h.service.destroy();
    }
});

test('cursor context rechecks protection before reading a fresh response', () => {
    for (const notify of [false, true]) {
        const h = contextHarness();
        h.request();
        h.inputMethod.content_hints = 64;
        if (notify) h.inputMethod.emit('notify::content-hints');
        h.respond(['secret', 0, 0]);
        assert.deepEqual(h.reply.values, [['protected', '', '', false]]);
        assert.equal(h.inputMethod.reads, 0);
        h.assertClean();
        h.service.destroy();
    }
});

test('cursor context feature-detects input APIs, mapped fields and privacy enums', () => {
    const variants = [
        h => { h.context.Main.inputMethod = null; },
        h => { delete h.inputMethod.currentFocus; },
        h => { h.inputMethod.currentFocus = null; },
        ...['getSurroundingText', 'request_surrounding', 'hasPreedit', 'connect', 'disconnect', 'content_purpose', 'content_hints']
            .map(name => h => { delete h.inputMethod[name]; }),
        h => { delete h.context.Clutter.InputContentPurpose.PASSWORD; },
        h => { delete h.context.Clutter.InputContentHintFlags.SENSITIVE_DATA; },
        h => { delete h.context.Clutter.InputContentHintFlags.HIDDEN_TEXT; },
    ];
    for (const variant of variants) {
        const h = contextHarness();
        variant(h);
        h.request();
        assert.deepEqual(h.reply.values, [['unavailable', '', '', false]]);
        assert.equal(h.inputMethod.requests, 0);
        h.assertClean();
        h.service.destroy();
    }
    // Optional enum members must never accidentally match an undefined purpose.
    const h = contextHarness();
    delete h.context.Clutter.InputContentPurpose.PIN;
    h.request();
    h.respond(['safe', 0, 0]);
    assert.deepEqual(h.reply.values, [['available', '', 'safe', false]]);
    h.assertClean();
    h.service.destroy();
});

test('cursor context discards responses after focus or preedit changes', async () => {
    for (const change of [
        h => { h.inputMethod.currentFocus = {}; },
        h => { h.inputMethod.currentFocus = null; },
        h => { h.context.Main.inputMethod = {}; },
        h => { h.inputMethod.preedit = true; },
        h => { h.context.global.display.focus_window = {}; h.context.global.display.emit('notify::focus-window'); },
        h => { h.context.global.stage.get_key_focus = () => ({}); h.context.global.stage.emit('notify::key-focus'); },
    ]) {
        const h = contextHarness();
        h.request();
        change(h);
        h.respond(['private to another focus', 5, 5]);
        await h.fire(100);
        assert.deepEqual(h.reply.values, [['changed', '', '', false]]);
        assert.equal(h.inputMethod.reads, 0);
        h.assertClean();
        h.service.destroy();
    }
    const h = contextHarness({preedit: true});
    h.request();
    assert.deepEqual(h.reply.values, [['changed', '', '', false]]);
    assert.equal(h.inputMethod.requests, 0);
    h.assertClean();
    h.service.destroy();
});

test('cursor context rechecks focus on timeout without a response', async () => {
    const h = contextHarness();
    h.request();
    h.inputMethod.currentFocus = null;
    await h.fire(100);
    assert.deepEqual(h.reply.values, [['changed', '', '', false]]);
    h.assertClean();
    h.service.destroy();
});

test('cursor context discards a snapshot if focus or protection changes while reading it', () => {
    for (const change of [h => { h.inputMethod.currentFocus = {}; },
        h => { h.inputMethod.preedit = true; }, h => { h.inputMethod.content_purpose = 8; }]) {
        const h = contextHarness();
        h.inputMethod.getSurroundingText = () => { change(h); return ['sensitive', 1, 1]; };
        h.request();
        h.respond(['fresh', 1, 1]);
        assert.deepEqual(h.reply.values, [[h.inputMethod.content_purpose === 8 ? 'protected' : 'changed', '', '', false]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('a protected field that takes focus mid-read reports protected, not merely changed', () => {
    const h = contextHarness();
    h.request();
    h.inputMethod.currentFocus = {};
    h.inputMethod.content_purpose = 8;
    h.respond(['secret', 0, 0]);
    assert.deepEqual(h.reply.values, [['protected', '', '', false]]);
    assert.equal(h.inputMethod.reads, 0);
    h.assertClean();
    h.service.destroy();
});

test('cursor context requires the currently watched unique daemon owner', () => {
    for (const [owner, sender] of [[null, ':1.42'], [':1.42', ':1.99'], [':1.42', 'org.localwispr.Daemon']]) {
        const h = contextHarness();
        h.service._setDaemonOwner(owner);
        const reply = h.invocation(sender);
        h.service.GetCursorContextAsync([], reply);
        assert.deepEqual(reply.values, [['denied', '', '', false]]);
        assert.equal(h.inputMethod.requests, 0);
        h.assertClean();
        h.service.destroy();
    }
    for (const newOwner of [null, ':1.99']) {
        const h = contextHarness();
        h.request();
        h.service._setDaemonOwner(newOwner);
        h.respond(['old daemon response', 0, 0]);
        assert.deepEqual(h.reply.values, [['denied', '', '', false]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('extension daemon bus watch supplies the current unique owner and revokes it on disappearance', () => {
    const h = harness();
    const inputMethod = signalObject({currentFocus: {}, content_purpose: 0, content_hints: 0,
        hasPreedit() { return false; }, getSurroundingText() { return ['text', 0, 0]; },
        request_surrounding() {}});
    h.context.Main.inputMethod = inputMethod;
    let appeared;
    let vanished;
    Object.assign(h.context.Gio, {
        DBus: {session: {signal_subscribe() { return 1; }, signal_unsubscribe() {}}},
        DBusExportedObject: {wrapJSObject(xml, service) {
            assert.ok(xml.includes('<method name="GetCursorContext">'));
            assert.equal(typeof service.GetCursorContextAsync, 'function');
            return {export() {}, unexport() {}};
        }},
        BusType: {SESSION: 0}, BusNameOwnerFlags: {REPLACE: 1},
        DBusSignalFlags: {NONE: 0}, BusNameWatcherFlags: {NONE: 0},
        bus_own_name_on_connection() { return 1; }, bus_unown_name() {}, bus_unwatch_name() {},
        bus_watch_name(_bus, name, _flags, onAppeared, onVanished) {
            assert.equal(name, 'org.localwispr.Daemon');
            appeared = onAppeared;
            vanished = onVanished;
            return 1;
        },
    });
    const extension = new h.LocalWisprExtension();
    extension.metadata = {version: 1};
    let syncs = 0;
    extension._syncDaemon = () => { syncs++; };
    extension.enable();
    assert.equal(extension._service._daemonOwner, null);
    appeared(null, 'org.localwispr.Daemon', ':1.42');
    assert.equal(extension._service._daemonOwner, ':1.42');
    assert.equal(syncs, 1);
    const reply = h.invocation();
    extension._service.GetCursorContextAsync([], reply);
    assert.equal(h.timers.size, 1);
    vanished();
    assert.equal(extension._service._daemonOwner, null);
    assert.deepEqual(reply.values, [['denied', '', '', false]]);
    assert.equal(h.timers.size, 0);
    appeared(null, 'org.localwispr.Daemon', ':1.99');
    assert.equal(extension._service._daemonOwner, ':1.99');
    const oldCaller = h.invocation(':1.42');
    extension._service.GetCursorContextAsync([], oldCaller);
    assert.deepEqual(oldCaller.values, [['denied', '', '', false]]);
    const currentCaller = h.invocation(':1.99');
    extension._service.GetCursorContextAsync([], currentCaller);
    extension.disable();
    assert.deepEqual(currentCaller.values, [['unavailable', '', '', false]]);
    assert.equal(inputMethod.handlers.size, 0);
    assert.equal(h.context.global.display.handlers.size, 0);
    assert.equal(h.context.global.stage.handlers.size, 0);
    assert.equal(h.timers.size, 0);
});

test('cursor context teardown cancels concurrent requests exactly once and disconnects all watches', async () => {
    const h = contextHarness();
    h.request();
    const second = h.invocation();
    h.service.GetCursorContextAsync([], second);
    assert.equal(h.timers.size, 2);
    h.service.destroy();
    h.respond(['late response', 0, 0]);
    await h.fire(100);
    for (const reply of [h.reply, second])
        assert.deepEqual(reply.values, [['unavailable', '', '', false]]);
    h.assertClean();
    const disabled = h.invocation();
    h.service.GetCursorContextAsync([], disabled);
    assert.deepEqual(disabled.values, [['unavailable', '', '', false]]);
    h.assertClean();
});

test('cursor context exceptions settle once without content logs or resource leaks', () => {
    for (const method of ['connect', 'request_surrounding', 'getSurroundingText', 'hasPreedit']) {
        const h = contextHarness();
        h.inputMethod[method] = () => { throw new Error('sensitive content must not be logged'); };
        h.request();
        h.respond(['text', 0, 0]);
        assert.deepEqual(h.reply.values, [['unavailable', '', '', false]]);
        h.assertClean();
        h.service.destroy();
    }
});

test('disable during insertion delay settles the request, restores both selections, and sends no keys', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    assert.equal(h.timers.size, 1);
    service.destroy();
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.timers.size, 0);
    assert.equal(h.owners.get(1).formats.get('text/html'), '<b>original</b>');
    assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
});

test('restores all clipboard formats and independently respects PRIMARY ownership changes', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    assert.deepEqual(reply.replies, []);
    assert.ok(h.keyEvents.length > 0);
    const newPrimary = {formats: new Map([['text/plain', 'new selection']])};
    h.selection.set_owner(0, newPrimary);
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [true]);
    assert.equal(h.owners.get(1).formats.get('text/html'), '<b>original</b>');
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.owners.get(0), newPrimary);
    service.destroy();
});

test('disable after paste preserves consumption grace and cancels other lifetime waits', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], h.invocation());
    await h.flush();
    await h.fire(20);
    service.destroy();
    assert.equal(h.disposed, 1);
    assert.equal(new TextDecoder().decode(h.owners.get(1).formats.get('text/plain')), 'dictation');
    assert.deepEqual([...h.timers.values()].map(timer => timer.ms), [400]);
    await h.fire(400);
    await operation;
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.timers.size, 0);
    const life = new h.Lifetime();
    const waits = [life.wait(50), life.wait(2800), life.wait(5000)];
    life.destroy();
    assert.deepEqual(await Promise.all(waits), [false, false, false]);
    assert.equal(h.timers.size, 0);
});

test('errors during processing queue, then replace an outgoing capsule after idle', async () => {
    const h = harness();
    const extension = new h.LocalWisprExtension();
    extension._lifetime = new h.Lifetime();
    extension._state = 'processing';
    extension._stateRevision = 0;
    extension._capsule = {enabled: true, mode: 'processing', noticeGen: 0,
        setText(text) { this.text = text; }, show(mode) { this.mode = mode; }, hide() { this.mode = 'fading'; }};
    extension._onSignal('Notice', {deepUnpack: () => ['error', 'Failed transcription']});
    assert.equal(extension._capsule.mode, 'processing');
    extension._onSignal('StateChanged', {deepUnpack: () => ['idle']});
    assert.equal(extension._capsule.mode, 'notice');
    assert.equal(extension._capsule.text, 'Failed transcription');
    extension._capsule.mode = 'fading';
    extension._onSignal('Notice', {deepUnpack: () => ['error', 'Late failure']});
    assert.equal(extension._capsule.mode, 'notice');
    assert.equal(extension._capsule.text, 'Late failure');
    extension._lifetime.destroy();
    await h.flush();
});

test('bootstrap keeps an intervening state signal over the status snapshot and applies HUD configuration', async () => {
    const h = harness();
    const extension = new h.LocalWisprExtension();
    extension._lifetime = new h.Lifetime();
    extension._state = 'idle';
    extension._stateRevision = 0;
    extension._daemonGeneration = 0;
    extension._capsule = {enabled: false, mode: 'hidden', configure(config) { this.config = config; this.enabled = config[0] === 'gnome'; },
        setText() {}, show(mode) { if (this.enabled) this.mode = mode; }, hide() { this.mode = 'hidden'; }};
    let finishStatus;
    extension._call = method => method === 'GetHudConfig' ? Promise.resolve(['gnome', 4, 90, false, 0.3]) :
        new Promise(resolve => { finishStatus = resolve; });
    const sync = extension._syncDaemon();
    extension._onSignal('StateChanged', {deepUnpack: () => ['processing']});
    finishStatus([{state: 'idle'}]);
    await h.flush();
    assert.equal(extension._capsule.mode, 'processing');
    assert.equal(extension._capsule.config[1], 4);
    extension._lifetime.destroy();
    await sync;
    assert.equal(h.timers.size, 0);
});

test('without the helper, multi-format content pastes and restores the best text representation', async () => {
    const h = harness();
    vm.runInContext('nativeSelection = null;', h.context);
    h.owners.get(1).formats = new Map([
        ['text/html', '<b>original</b>'], ['STRING', 'legacy'], ['text/plain', 'plain'],
        ['UTF8_STRING', 'utf8'], ['text/plain;charset=utf-8', 'preferred'],
    ]);
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    assert.ok(h.keyEvents.length > 0);
    await h.fire(400);
    await operation;
    assert.deepEqual([...h.owners.get(1).formats], [['text/plain;charset=utf-8', 'preferred']]);
    assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
    assert.deepEqual(reply.replies, [true]);
    assert.deepEqual(h.transfers.map(({mime}) => mime), ['text/plain;charset=utf-8', 'text/plain']);
    service.destroy();
});

test('HUD configuration gates rendering, resizes bands and hides live text', () => {
    const h = harness();
    const c = new h.Capsule();
    c.setText('live words');
    c.mode = 'recording';
    c.configure(['none', 4, 100, false, 0.2]);
    assert.equal(c.mode, 'fading');
    assert.equal(c.enabled, false);
    assert.equal(c.label.visible, false);
    assert.equal(c.current.length, 4);
    assert.ok(c.current.every(value => value === 0));
    assert.equal(c.bottomMargin, 100);
    assert.ok(c.actor.style.includes('0.2'));
    c.configure(['gnome', 48, 50, true, 0.8]);
    for (let i = 0; i < 50; i++) c._tick();
    assert.equal(c.enabled, true);
    assert.equal(c.current.length, 48);
    assert.equal(c.label.visible, true);
    c.setLevels([0.1, 0.2]);
    assert.equal(c.target.length, 48);
    assert.equal(c.target[0], 0.1);
    assert.ok(c.target.slice(2).every(value => value === 0));
    assert.equal(c.actor.width, 220, 'band count does not stretch the capsule');
});

test('capsule grows smoothly, stays centered, holds width through revisions and resets next recording', () => {
    const h = harness();
    const c = new h.Capsule();
    c.configure(['gnome', 16, 64, true, 0.92]);
    c.show('recording');
    assert.equal(c.actor.width, 220);
    assert.equal(c.actor.height, 46);
    c.setText('A few words');
    assert.equal(c.targetWidth, 220);
    c.setText('A longer transcript expands smoothly as more words arrive');
    assert.equal(c.targetWidth, 420);
    assert.equal(c.actor.width, 220);
    c._tick();
    assert.ok(c.actor.width > 220 && c.actor.width < 420);
    assert.equal(c.actor.x, Math.round((1280 - c.actor.width) / 2));
    c.setText('Short revision');
    assert.equal(c.targetWidth, 420);
    const entranceCalls = c.actor.easeCalls;
    c.show('processing');
    assert.equal(c.actor.easeCalls, entranceCalls, 'processing does not restart the entrance');
    assert.equal(c.targetWidth, 420);
    for (let i = 0; i < 80; i++) c._tick();
    assert.equal(c.actor.width, 420);
    c.hide();
    const exitCalls = c.actor.easeCalls;
    c.hide();
    assert.equal(c.actor.easeCalls, exitCalls, 'repeated idle states do not restart the exit');
    const staleCompletion = c.actor.completion;
    c.setText('');
    c.show('recording');
    staleCompletion();
    assert.equal(c.mode, 'recording');
    assert.equal(c.actor.visible, true);
    assert.equal(c.actor.width, 220);
    assert.equal(c.actor.height, 46);
    c.destroy();
});

test('capsule respects narrow screens, reduced motion, hidden live text and oversized Unicode tokens', () => {
    const h = harness();
    h.monitor.width = 300;
    h.animationSettings.enable_animations = false;
    const c = new h.Capsule();
    c.configure(['gnome', 48, 64, true, 0.9]);
    c.show('recording');
    c.setText('Long words '.repeat(50));
    assert.equal(c.actor.width, 268);
    assert.ok(c.label.get_preferred_height(c.label.width)[1] <= 32);
    c.setText('😀'.repeat(500));
    assert.ok(!/[\uD800-\uDBFF]$/.test(c.label.text));
    assert.ok(c.label.get_preferred_height(c.label.width)[1] <= 32);
    c.configure(['gnome', 48, 64, false, 0.9]);
    assert.equal(c.actor.width, 220);
    assert.equal(c.actor.height, 46);
    assert.equal(c.label.visible, false);
    c.setText('A notice remains visible even with live text off', true);
    assert.equal(c.label.visible, true);
    c.destroy();
});

test('motion starts gently and preserves momentum as the transcript grows', () => {
    const h = harness();
    const c = new h.Capsule();
    c.configure(['gnome', 16, 64, true, 0.9]);
    c.show('recording');
    c.setText('A longer transcript grows smoothly across the screen');
    c._tick();
    assert.ok(c.actor.width > 220 && c.actor.width < 228, 'first frame eases into the expansion');
    for (let i = 0; i < 12; i++) c._tick();
    const velocity = c.widthVelocity;
    c.setText('A longer transcript grows smoothly across the screen with more words');
    assert.equal(c.widthVelocity, velocity, 'incoming text does not restart the motion');
    for (let i = 0; i < 70; i++) c._tick();
    const width = c.actor.width;
    const height = c.actor.height;
    c.setText('');
    assert.equal(c.actor.width, width, 'an empty partial does not collapse the pill immediately');
    assert.equal(c.actor.height, height);
    for (let i = 0; i < 80; i++) c._tick();
    assert.equal(c.actor.width, 220);
    assert.equal(c.actor.height, 46);
    c.destroy();
});

test('spring motion is stable across frame rates and retargets without restarting velocity', () => {
    const h = harness();
    const positions = [];
    for (const hz of [30, 60, 144]) {
        let width = 220, velocity = 0;
        for (let i = 0; i < hz; i++) {
            const before = width;
            [width, velocity] = h.spring(width, velocity, 420, 0.3 / hz, 12);
            assert.ok(width >= before && width <= 420);
        }
        positions.push(width);
    }
    assert.ok(Math.max(...positions) - Math.min(...positions) < 1e-8);
});

test('switching to a narrow monitor bounds an expansion already in motion', () => {
    const h = harness();
    const c = new h.Capsule();
    c.configure(['gnome', 16, 64, true, 0.9]);
    c.show('recording');
    c.setText('A long transcript expands across the available screen space');
    c.actor.width = 260;
    c.widthVelocity = 1000;
    h.monitor.width = 300;
    for (let frame = 0; frame < 30; frame++) {
        c._tick();
        assert.ok(c.actor.width <= 268, 'pill stays inside the screen margin on every frame');
        assert.ok(c.actor.x >= 16);
    }
    c.destroy();
});


test('aggregate overflow still pastes, reports false, and never restores truncated data', async () => {
    const h = harness();
    h.owners.get(1).formats = new Map([
        ['text/plain', 'a'.repeat(17 * 1024 * 1024)], ['text/html', 'b'.repeat(17 * 1024 * 1024)],
    ]);
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    assert.equal(h.transfers[0].size, 32 * 1024 * 1024 + 1);
    assert.equal(h.transfers[1].size, 15 * 1024 * 1024 + 1);
    await h.fire(20);
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [false]);
    assert.ok(h.keyEvents.length > 0);
    assert.ok(h.errors.some(message => message.includes('32 MiB')));
    assert.equal(h.owners.get(1).formats.get('text/plain;charset=utf-8').length, 9);
    assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
    service.destroy();
    assert.equal(h.timers.size, 0);
});

test('transfer timeout cancels backup, still pastes, and reports false', async () => {
    const h = harness();
    const normalTransfer = h.selection.transfer_async;
    h.selection.transfer_async = (type, mime, size, output, cancel, callback) => {
        if (type !== 1) return normalTransfer(type, mime, size, output, cancel, callback);
        cancel.connect(() => queueMicrotask(() => callback(h.selection, {cancel})));
    };
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.fire(5000);
    await h.fire(20);
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [false]);
    assert.ok(h.keyEvents.length > 0);
    assert.ok(h.errors.some(message => message.includes('backup failed')));
    service.destroy();
    assert.equal(h.timers.size, 0);
});

test('ownership loss immediately before paste sends no keys and returns an error', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    const replacement = {formats: new Map([['text/plain', 'other app']])};
    h.selection.set_owner(1, replacement);
    await h.fire(20);
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.get(1), replacement);
    assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
    service.destroy();
});

test('busy, disabled and cancellation paths each return an error exactly once and dispose the keyboard', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const first = h.invocation();
    const operation = service.InjectAsync(['text', 'ctrl-v', true, 400], first);
    const busy = h.invocation();
    await service.InjectAsync(['text', 'ctrl-v', true, 400], busy);
    assert.deepEqual(busy.replies, ['Busy']);
    service.destroy();
    await operation;
    assert.deepEqual(first.replies, ['Failed']);
    const disabled = h.invocation();
    await service.InjectAsync(['text', 'ctrl-v', true, 400], disabled);
    assert.deepEqual(disabled.replies, ['Busy']);
    assert.equal(h.disposed, 1);
    assert.equal(service._lifetime.cancellable.handlers.size, 0);
    assert.equal(h.timers.size, 0);
});

test('daemon sync retries bootstrap and later polling failures with cancellable backoff', async () => {
    const h = harness();
    const extension = new h.LocalWisprExtension();
    Object.assign(extension, {_lifetime: new h.Lifetime(), _daemonGeneration: 0, _stateRevision: 0,
        _state: 'idle', _capsule: {enabled: false, mode: 'hidden',
            configure() { this.enabled = true; }, hide() { this.mode = 'hidden'; }}, _applyState() {}});
    let fail = true;
    extension._call = method => fail ? Promise.reject(new Error('temporary failure')) :
        Promise.resolve(method === 'GetHudConfig' ? ['gnome', 16, 64, true, 0.8] : [{state: 'idle'}]);
    const sync = extension._syncDaemon();
    await h.flush();
    assert.deepEqual([...h.timers.values()].map(t => t.ms), [250]);
    fail = false;
    await h.fire(250);
    assert.equal(extension._capsule.enabled, true);
    assert.deepEqual([...h.timers.values()].map(t => t.ms), [1000]);
    fail = true;
    await h.fire(1000);
    assert.deepEqual([...h.timers.values()].map(t => t.ms), [250]);
    await h.fire(250);
    assert.deepEqual([...h.timers.values()].map(t => t.ms), [500]);
    extension._lifetime.destroy();
    await sync;
    assert.equal(h.timers.size, 0);
});

test('helper ABI is checked before importing its library', () => {
    const h = harness();
    let imports = 0;
    let abi = '0';
    const repository = {prepend_search_path() {}, prepend_library_path() {}};
    h.context.GIRepository.Repository = {dup_default() { return repository; }};
    h.context.Meta.SelectionSource = {$gtype: 1};
    h.context.GLib.file_get_contents = () => [true, new TextEncoder().encode(`${abi}\n`)];
    h.context.imports = {gi: {get LocalwisprSelection() { imports++; return {}; }}};
    h.loadNativeSelection('/extension');
    assert.equal(imports, 0);
    assert.ok(h.warnings.some(message => message.includes('unsupported helper ABI')));
    abi = '1';
    h.loadNativeSelection('/extension');
    assert.equal(imports, 1);
    h.context.GLib.file_get_contents = () => { throw new Error('missing ABI stamp'); };
    h.loadNativeSelection('/extension');
    assert.equal(imports, 1);
});

test('a helper that rejects the running Mutter falls back to text-only restoration', async () => {
    const h = harness();
    vm.runInContext('nativeSelection = {create() { return null; }};', h.context);
    h.owners.get(1).formats = new Map([['text/html', '<b>original</b>'], ['text/plain;charset=utf-8', 'preferred']]);
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    assert.ok(h.keyEvents.length > 0);
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [true]);
    assert.deepEqual([...h.owners.get(1).formats], [['text/plain;charset=utf-8', 'preferred']]);
    assert.equal(vm.runInContext('nativeSelection', h.context), null);
    assert.ok(h.warnings.some(message => message.includes('restoring text only')));
});

test('restoration failure returns false and teardown still releases resources', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const reply = h.invocation();
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    h.selection.set_owner = () => { throw new Error('restore failed'); };
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [false]);
    assert.ok(h.errors.some(message => message.includes('restoration failed')));
    service.destroy();
    assert.equal(h.disposed, 1);
    assert.equal(h.timers.size, 0);
});

test('helper build needs only GLib and stamps its ABI', () => {
    const os = require('node:os');
    const {spawnSync} = require('node:child_process');
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'localwispr-build-test-'));
    try {
        const bin = path.join(temporary, 'bin');
        fs.mkdirSync(bin);
        const scripts = {
            'pkg-config': '#!/bin/sh\ncase "$1" in --exists) [ -z "$TEST_NO_GIO" ];; *) echo -lgio-2.0;; esac\n',
            'cc': '#!/bin/sh\nexit 0\n',
            'gi-compile-repository': '#!/bin/sh\nexit 0\n',
        };
        for (const [name, script] of Object.entries(scripts))
            fs.writeFileSync(path.join(bin, name), script, {mode: 0o755});
        const env = {...process.env, PATH: `${bin}:${process.env.PATH}`, CC: path.join(bin, 'cc')};
        const output = path.join(temporary, 'out');
        fs.mkdirSync(output);
        fs.writeFileSync(path.join(output, 'selection-meta-version'), '51\n');
        const result = spawnSync('bash', [path.join(__dirname, '../build-clipboard.sh'), output], {encoding: 'utf8', env});
        assert.equal(result.status, 0, result.stderr);
        assert.equal(fs.readFileSync(path.join(output, 'selection-abi'), 'utf8').trim(), '1');
        assert.ok(!fs.existsSync(path.join(output, 'selection-meta-version')));
        const missing = spawnSync('bash', [path.join(__dirname, '../build-clipboard.sh'), output],
            {encoding: 'utf8', env: {...env, TEST_NO_GIO: '1'}});
        assert.notEqual(missing.status, 0);
        assert.match(missing.stderr, /GLib\/GIO development files/);
    } finally {
        fs.rmSync(temporary, {recursive: true, force: true});
    }
});


test('terminal context is excluded through the authoritative Clutter purpose', () => {
    for (const properties of [{content_purpose: 10}]) {
        const h = contextHarness(properties);
        h.request();
        assert.deepEqual(h.reply.values, [['terminal', '', '', false]]);
        assert.equal(h.inputMethod.requests, 0);
        h.assertClean();
        h.service.destroy();
    }
});

test('request cancellation restores clipboard and prevents pre-paste keys', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['first', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    assert.equal(h.keyEvents.length, 0);
    h.service.CancelInjectAsync(['first'], h.invocation());
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
    assert.equal(h.timers.size, 0);
    h.service.destroy();
});

test('old or unauthorized cancellation cannot revoke the next insertion', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['next', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    h.service.CancelInjectAsync(['old'], h.invocation());
    const denied = h.invocation(':1.99');
    h.service.CancelInjectAsync(['next'], denied);
    assert.deepEqual(denied.replies, ['Denied']);
    await h.fire(20);
    assert.equal(h.keyEvents.length, 4);
    await h.fire(400);
    await operation;
    assert.deepEqual(reply.replies, [true]);
    h.service.destroy();
});

test('losing daemon ownership cancels pending paste and restores both selections', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['first', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    h.service._setDaemonOwner(null);
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.timers.size, 0);
    h.service.destroy();
});

test('focus change before paste fails without inserting into the new editor', async () => {
    for (const move of [h => h.context.global.display.focus_window = {},
        h => h.inputMethod.currentFocus = {}]) {
        const h = contextHarness();
        const reply = h.invocation();
        const operation = h.service.InjectCancellableAsync(['focus', 'text', 'ctrl-v', true, 400], reply);
        await h.flush();
        move(h);
        await h.fire(20);
        await operation;
        assert.deepEqual(reply.replies, ['Failed']);
        assert.equal(h.keyEvents.length, 0);
        assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
        h.service.destroy();
    }
});

test('cancellation after paste preserves consumption grace before restoring without sending keys twice', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['done', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    assert.equal(h.keyEvents.length, 4);
    h.service.CancelInjectAsync(['done'], h.invocation());
    assert.equal(new TextDecoder().decode(h.owners.get(1).formats.get('text/plain')), 'text');
    assert.equal(reply.replies.length, 0);
    await h.fire(400);
    await operation;
    assert.equal(h.keyEvents.length, 4);
    assert.equal(reply.replies.length, 1);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.timers.size, 0);
    h.service.destroy();
});


test('focus leaving and returning before paste still cancels the request', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const initial = h.context.global.display.focus_window;
    const operation = h.service.InjectCancellableAsync(['focus-cycle', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    h.context.global.display.focus_window = {};
    h.context.global.display.emit('notify::focus-window');
    h.context.global.display.focus_window = initial;
    h.context.global.display.emit('notify::focus-window');
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.context.global.display.handlers.size, 0);
    assert.equal(h.inputMethod.handlers.size, 0);
    h.service.destroy();
});


test('request cancellation interrupts a stalled clipboard backup without pasting', async () => {
    const h = contextHarness();
    h.selection.transfer_async = (_type, _mime, _size, _output, cancel, callback) => {
        cancel.connect(() => queueMicrotask(() => callback(h.selection, {cancel})));
    };
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['stalled', 'text', 'ctrl-v', true, 400], reply);
    await h.flush();
    h.service.CancelInjectAsync(['stalled'], h.invocation());
    await operation;
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.timers.size, 0);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assert.equal(h.errors.length, 0);
    h.service.destroy();
});

test('ownership change during backup preserves the new user clipboard and prevents paste', async () => {
    const h = contextHarness();
    const normal = h.selection.transfer_async;
    const replacement = {formats: new Map([['text/plain', 'new user copy']])};
    let changed = false;
    h.selection.transfer_async = (type, mime, size, output, cancel, callback) => {
        normal(type, mime, size, output, cancel, (selection, result) => {
            if (!changed && type === 1) {
                changed = true;
                h.selection.set_owner(1, replacement);
            }
            callback(selection, result);
        });
    };
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['ownership-during-backup', 'dictation', 'ctrl-v', true, 400], reply);
    await h.flush();
    await h.fire(20);
    await h.fire(400);
    await operation;
    assert.equal(h.keyEvents.length, 0, 'ownership change during backup must fail before paste');
    assert.equal(h.owners.get(1), replacement, 'keep the newer clipboard owner');
    h.service.destroy();
});

function prepareBound(h, snapshot = ['We old tools.', 3, 6]) {
    const reply = h.invocation();
    h.service.GetInsertionContextAsync([], reply);
    h.respond(snapshot);
    assert.equal(reply.values.length, 1);
    assert.ok(['available', 'empty'].includes(reply.values[0][0]));
    const token = reply.values[0][4];
    assert.ok(token);
    return {reply, token};
}

function assertNoContextWatches(h) {
    assert.equal(h.service._handoff, null);
    assert.equal(h.inputMethod.handlers.size, 0);
    assert.equal(h.context.global.display.handlers.size, 0);
    assert.equal(h.context.global.stage.handlers.size, 0);
    assert.equal(h.timers.size, 0);
}

test('bound handoff inserts once into unchanged Unicode caret/selection ranges and releases all retained context', async () => {
    for (const [snapshot, expected] of [
        [['We old tools.', 3, 6], ['available', 'We ', ' tools.', true]],
        [['We old tools.', 6, 3], ['available', 'We ', ' tools.', true]],
        [['😀left right', 6, 6], ['available', '😀left ', 'right', false]],
        [['private selection', 0, 17], ['empty', '', '', true]],
        [['', 0, 0], ['empty', '', '', false]],
    ]) {
        const h = contextHarness();
        const {reply: contextReply, token} = prepareBound(h, snapshot);
        assert.deepEqual(contextReply.values[0].slice(0, 4), expected);
        assert.deepEqual(Object.keys(h.service._handoff.range).sort(), ['after', 'anchor', 'before', 'cursor', 'length']);
        const reply = h.invocation();
        const operation = h.service.InjectBoundAsync(['bound', token, 'new', 'ctrl-v', true, 400], reply);
        await h.flush();
        h.respond(snapshot); // Replayed identical committed context is allowed.
        await h.fire(20);
        assert.equal(h.keyEvents.length, 4);
        assert.equal(h.service._handoff, null, 'consume and release the token at key submission');
        await h.fire(400);
        await operation;
        assert.deepEqual(reply.replies, [true]);
        const repeated = h.invocation();
        await h.service.InjectBoundAsync(['replay', token, 'new', 'ctrl-v', true, 400], repeated);
        assert.deepEqual(repeated.replies, ['StaleContext']);
        assert.equal(h.keyEvents.length, 4);
        assertNoContextWatches(h);
        h.service.destroy();
    }
});

test('context-to-injection handoff rejects focus away/back and observed caret cycles', async () => {
    const transitions = [
        h => { h.context.global.display.focus_window = {}; h.context.global.display.emit('notify::focus-window'); },
        h => { const old = h.context.global.display.focus_window;
            h.context.global.display.focus_window = {}; h.context.global.display.emit('notify::focus-window');
            h.context.global.display.focus_window = old; h.context.global.display.emit('notify::focus-window'); },
        h => { let focus = {}; h.context.global.stage.get_key_focus = () => focus;
            h.context.global.stage.emit('notify::key-focus'); focus = undefined;
            h.context.global.stage.emit('notify::key-focus'); },
        h => { h.inputMethod.emit('cursor-location-changed'); },
        h => { h.respond(['We old tools.', 0, 2]); h.respond(['We old tools.', 3, 6]); },
        h => { h.inputMethod.currentFocus = {}; },
        h => { h.inputMethod.snapshot = ['We old tools.', 3, 8]; },
    ];
    for (const transition of transitions) {
        const h = contextHarness();
        const {token} = prepareBound(h);
        transition(h);
        const reply = h.invocation();
        await h.service.InjectBoundAsync(['moved', token, 'new', 'ctrl-v', true, 400], reply);
        assert.deepEqual(reply.replies, ['StaleContext']);
        assert.equal(h.keyEvents.length, 0);
        assert.equal(h.transfers.length, 0);
        h.service.destroy();
        assertNoContextWatches(h);
    }
});

test('range or bounded outside text changes during clipboard staging abort before keys, including silent changes', async () => {
    for (const change of [
        h => h.respond(['Different sentence.', 0, 19]),
        h => h.respond(['We NEW tools.', 3, 6]), // Selected text is private; outside/range unchanged, allowed below.
        h => { h.inputMethod.snapshot = ['We old things', 3, 6]; },
        h => h.inputMethod.emit('cursor-location-changed'),
        h => { h.inputMethod.preedit = true; },
        h => { h.inputMethod.content_hints = 128; },
    ]) {
        const h = contextHarness();
        const {token} = prepareBound(h);
        const reply = h.invocation();
        const operation = h.service.InjectBoundAsync(['changed', token, 'new', 'ctrl-v', true, 400], reply);
        await h.flush();
        change(h);
        await h.fire(20);
        const selectedTextOnly = h.inputMethod.snapshot[0] === 'We NEW tools.';
        assert.equal(h.keyEvents.length, selectedTextOnly ? 4 : 0);
        if (selectedTextOnly)
            await h.fire(400);
        await operation;
        assert.deepEqual(reply.replies, selectedTextOnly ? [true] : ['StaleContext']);
        assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
        assertNoContextWatches(h);
        h.service.destroy();
    }
});

test('changes while a backup is outstanding cancel the bound request and its transfer', async () => {
    const h = contextHarness();
    const {token} = prepareBound(h);
    let waiting;
    h.selection.transfer_async = (_type, _mime, _size, _output, cancel, callback) => {
        waiting = cancel;
        cancel.connect(() => queueMicrotask(() => callback(h.selection, {cancel})));
    };
    const reply = h.invocation();
    const operation = h.service.InjectBoundAsync(['backup', token, 'new', 'ctrl-v', true, 400], reply);
    await h.flush();
    h.respond(['We old tools.', 0, 13]);
    await operation;
    assert.equal(waiting.cancelled, true);
    assert.deepEqual(reply.replies, ['StaleContext']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assertNoContextWatches(h);
    h.service.destroy();
});

test('silent range changes during a backup are revalidated before installing either selection', async () => {
    const h = contextHarness();
    const {token} = prepareBound(h);
    const transfer = h.selection.transfer_async;
    h.selection.transfer_async = (...args) => {
        h.inputMethod.snapshot = ['We old tools.', 0, 3]; // No surrounding-text signal.
        transfer(...args);
    };
    let installations = 0;
    const setOwner = h.selection.set_owner;
    h.selection.set_owner = (...args) => { installations++; setOwner(...args); };
    const reply = h.invocation();
    await h.service.InjectBoundAsync(['backup-silent', token, 'new', 'ctrl-v', true, 400], reply);
    assert.deepEqual(reply.replies, ['StaleContext']);
    assert.equal(installations, 0);
    assert.equal(h.keyEvents.length, 0);
    assertNoContextWatches(h);
    h.service.destroy();
});

test('final bound revalidation rejects focus/protection changed by the surrounding getter itself', async () => {
    for (const change of [h => { h.inputMethod.content_purpose = 8; },
        h => { h.context.global.display.focus_window = {}; }]) {
        const h = contextHarness();
        const {token} = prepareBound(h);
        const reply = h.invocation();
        const operation = h.service.InjectBoundAsync(['getter', token, 'new', 'ctrl-v', true, 400], reply);
        await h.flush();
        const get = h.inputMethod.getSurroundingText.bind(h.inputMethod);
        h.inputMethod.getSurroundingText = () => { const result = get(); change(h); return result; };
        await h.fire(20);
        await operation;
        assert.deepEqual(reply.replies, ['StaleContext']);
        assert.equal(h.keyEvents.length, 0);
        assertNoContextWatches(h);
        h.service.destroy();
    }
});

test('handoffs expire, are replaced, and are revoked on owner loss without retaining private context', async () => {
    for (const invalidate of [h => h.fire(2000), h => h.service._setDaemonOwner(null), h => h.service.destroy()]) {
        const h = contextHarness();
        const {token} = prepareBound(h);
        const retained = h.service._handoff;
        await invalidate(h);
        assert.equal(retained.range, null);
        assertNoContextWatches(h);
        if (h.service._lifetime.active) h.service.destroy();
    }
    const h = contextHarness();
    const {token: old} = prepareBound(h);
    const oldHandoff = h.service._handoff;
    const {token: next} = prepareBound(h);
    assert.notEqual(old, next);
    assert.equal(oldHandoff.range, null);
    const wrong = h.invocation();
    await h.service.InjectBoundAsync(['old', old, 'new', 'ctrl-v', true, 400], wrong);
    assert.deepEqual(wrong.replies, ['StaleContext']);
    assert.equal(h.service._handoff.token, next, 'a stale token cannot revoke the newer handoff');
    h.service.destroy();
    assertNoContextWatches(h);
});

test('untrusted bound context calls cannot read, consume or invalidate the daemon handoff', async () => {
    const h = contextHarness();
    const {token} = prepareBound(h);
    const untrustedRead = h.invocation(':1.99');
    h.service.GetInsertionContextAsync([], untrustedRead);
    assert.deepEqual(untrustedRead.values, [['denied', '', '', false, '']]);
    const untrustedInject = h.invocation(':1.99');
    await h.service.InjectBoundAsync(['wrong', token, 'new', 'ctrl-v', true, 400], untrustedInject);
    assert.deepEqual(untrustedInject.replies, ['Denied']);
    assert.equal(h.service._handoff.token, token);
    assert.equal(h.transfers.length, 0);
    h.service.destroy();
    assertNoContextWatches(h);
});

test('malformed or protected insertion context returns no token and leaves no watch or text lease', () => {
    for (const [properties, snapshot] of [
        [{content_purpose: 8}, ['private', 3, 3]],
        [{}, ['oversize'.repeat(10000), 0, 0]],
        [{}, ['text', 0, 99]],
    ]) {
        const h = contextHarness(properties);
        const reply = h.invocation();
        h.service.GetInsertionContextAsync([], reply);
        h.respond(snapshot);
        assert.equal(reply.values.length, 1);
        assert.deepEqual(reply.values[0].slice(1), ['', '', false, '']);
        assertNoContextWatches(h);
        h.service.destroy();
    }
});

test('pre-key cancellation cleans every owned injected source without restore and after failed backups', async () => {
    for (const restore of [false, true]) {
        const h = contextHarness();
        if (restore) {
            const finish = h.selection.transfer_finish;
            let first = true;
            h.selection.transfer_finish = result => {
                if (first) { first = false; throw new Error('synthetic backup refusal'); }
                return finish(result);
            };
        }
        const reply = h.invocation();
        const operation = h.service.InjectCancellableAsync(['never-pasted', 'CANCELLED_SECRET', 'ctrl-v', restore, 400], reply);
        await h.flush();
        h.service.CancelInjectAsync(['never-pasted'], h.invocation());
        await operation;
        assert.equal(h.keyEvents.length, 0);
        assert.deepEqual(reply.replies, ['Failed']);
        assert.equal(h.owners.has(1), false, 'discard injected clipboard when no usable backup exists');
        if (restore)
            assert.equal(h.owners.get(0).formats.get('text/plain'), 'selection');
        else
            assert.equal(h.owners.has(0), false);
        assertNoContextWatches(h);
        h.service.destroy();
    }
});

test('pre-key cleanup preserves new owners independently even when restoration is disabled', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['new-owner', 'SECRET', 'ctrl-v', false, 400], reply);
    await h.flush();
    const copied = {formats: new Map([['text/plain', 'new copy']])};
    h.selection.set_owner(1, copied);
    h.service.CancelInjectAsync(['new-owner'], h.invocation());
    await operation;
    assert.equal(h.owners.get(1), copied);
    assert.equal(h.owners.has(0), false);
    assert.equal(h.keyEvents.length, 0);
    h.service.destroy();
});

test('failure while installing selections cleans every installed source even without backups', async () => {
    const h = contextHarness();
    const setOwner = h.selection.set_owner;
    h.selection.set_owner = (type, source) => {
        setOwner(type, source);
        if (type === 0) throw new Error('synthetic failure after taking ownership');
    };
    const reply = h.invocation();
    await h.service.InjectCancellableAsync(['partial', 'SECRET', 'ctrl-v', false, 400], reply);
    assert.deepEqual(reply.replies, ['Failed']);
    assert.equal(h.keyEvents.length, 0);
    assert.equal(h.owners.has(0), false);
    assert.equal(h.owners.has(1), false);
    h.service.destroy();
});

test('post-key cancellation, owner loss and disable preserve delayed paste consumption and newer user copies', async () => {
    for (const cancel of [h => h.service.CancelInjectAsync(['submitted'], h.invocation()),
        h => h.service._setDaemonOwner(null), h => h.service.destroy()]) {
        const h = contextHarness();
        const reply = h.invocation();
        const operation = h.service.InjectCancellableAsync(['submitted', 'DICTATED', 'ctrl-v', true, 400], reply);
        await h.flush();
        await h.fire(20);
        cancel(h);
        await h.flush();
        assert.equal(h.keyEvents.length, 4);
        assert.equal(new TextDecoder().decode(h.owners.get(1).formats.get('text/plain')), 'DICTATED',
            'the delayed application clipboard request still sees dictated text');
        assert.equal(reply.replies.length, 0);
        const copied = {formats: new Map([['text/plain', 'new copy']])};
        h.selection.set_owner(0, copied);
        await h.fire(400);
        await operation;
        assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
        assert.equal(h.owners.get(0), copied);
        assert.deepEqual(reply.replies, [true]);
        assertNoContextWatches(h);
        if (h.service._lifetime.active) h.service.destroy();
    }
});

test('committed paste retains the minimum 100ms grace when a shorter delay was requested', async () => {
    const h = contextHarness();
    const operation = h.service.InjectCancellableAsync(['minimum', 'DICTATED', 'ctrl-v', true, 1], h.invocation());
    await h.flush();
    await h.fire(20);
    h.service._setDaemonOwner(null);
    assert.deepEqual([...h.timers.values()].map(timer => timer.ms), [100]);
    await h.fire(100);
    await operation;
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    h.service.destroy();
});

test('successful restore=false paste intentionally retains text after operation and service teardown', async () => {
    const h = contextHarness();
    const reply = h.invocation();
    const operation = h.service.InjectCancellableAsync(['retain', 'DICTATED', 'ctrl-v', false, 400], reply);
    await h.fire(20);
    await operation;
    h.service.destroy();
    assert.deepEqual(reply.replies, [true]);
    assert.equal(h.keyEvents.length, 4);
    assert.equal(new TextDecoder().decode(h.owners.get(1).formats.get('text/plain')), 'DICTATED');
    assert.equal(new TextDecoder().decode(h.owners.get(0).formats.get('text/plain')), 'DICTATED');
    assertNoContextWatches(h);
});

// Retained independent final-review regressions.

test('independent: pre-key cancellation/ownerloss/disable cleans all unbacked sources for bound and unbound operations', async () => {
    let cases = 0;
    for (const bound of [false, true]) for (const backup of ['off', 'both-fail', 'primary-fail', 'empty'])
    for (const action of ['cancel', 'ownerloss', 'disable']) {
        const h = contextHarness();
        const token = bound ? prepareBound(h).token : null;
        const restore = backup !== 'off';
        if (backup === 'empty') {
            h.owners.clear();
            h.selection.get_mimetypes = () => [];
        } else if (backup !== 'off') {
            const transfer = h.selection.transfer_async;
            h.selection.transfer_async = (type, ...args) => {
                if (backup === 'both-fail' || type === 0) throw new Error('independent backup failure');
                return transfer(type, ...args);
            };
        }
        const reply = h.invocation();
        const op = bound
            ? h.service.InjectBoundAsync(['extra', token, 'NEVER_PASTED', 'ctrl-v', restore, 350], reply)
            : h.service.InjectCancellableAsync(['extra', 'NEVER_PASTED', 'ctrl-v', restore, 350], reply);
        await h.flush();
        assert.equal(h.owners.has(1), true);
        assert.equal(h.owners.has(0), true);
        if (action === 'cancel') h.service.CancelInjectAsync(['extra'], h.invocation());
        if (action === 'ownerloss') h.service._setDaemonOwner(null);
        if (action === 'disable') h.service.destroy();
        await op;
        assert.equal(h.keyEvents.length, 0);
        assert.deepEqual(reply.replies, ['Failed']);
        assert.equal(h.owners.has(0), false);
        if (backup === 'primary-fail') assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
        else assert.equal(h.owners.has(1), false);
        assertNoContextWatches(h);
        if (h.service._lifetime.active) h.service.destroy();
        cases++;
    }
    assert.equal(cases, 24);
});

test('independent: a cancelled token cannot be retried, including cancellation while backup is pending', async () => {
    for (const phase of ['backup', 'staged']) {
        const h = contextHarness(); const {token} = prepareBound(h);
        if (phase === 'backup') h.selection.transfer_async = (_t, _m, _n, _o, cancel, callback) => {
            cancel.connect(() => queueMicrotask(() => callback(h.selection, {cancel})));
        };
        const first = h.invocation();
        const op = h.service.InjectBoundAsync(['first', token, 'ADAPTED', 'ctrl-v', true, 350], first);
        await h.flush();
        h.service.CancelInjectAsync(['first'], h.invocation());
        await op;
        assert.deepEqual(first.replies, ['Failed'], 'explicit cancellation is not retryable');
        const retry = h.invocation();
        await h.service.InjectBoundAsync(['second', token, 'ADAPTED', 'ctrl-v', false, 350], retry);
        assert.deepEqual(retry.replies, ['StaleContext']);
        assert.equal(h.keyEvents.length, 0);
        assertNoContextWatches(h); h.service.destroy();
    }
});

test('independent: an unclaimed token expires, but a claimed one survives a slow clipboard backup', async () => {
    const h = contextHarness(); const {token} = prepareBound(h);
    let finish;
    const transfer = h.selection.transfer_async;
    h.selection.transfer_async = (...args) => {
        h.selection.transfer_async = transfer; // Only the first backup is slow.
        finish = () => transfer(...args);
    };
    const reply = h.invocation();
    const op = h.service.InjectBoundAsync(['slow', token, 'ADAPTED', 'ctrl-v', true, 350], reply);
    await h.flush();
    assert.ok(![...h.timers.values()].some(t => t.ms === 2000), 'claiming stops the expiry clock');
    finish(); await h.flush();
    await h.fire(20);
    assert.equal(h.keyEvents.length, 4);
    await h.fire(350); await op;
    assert.deepEqual(reply.replies, [true]);
    assertNoContextWatches(h); h.service.destroy();

    const idle = contextHarness(); const {token: unused} = prepareBound(idle);
    await idle.fire(2000);
    const late = idle.invocation();
    await idle.service.InjectBoundAsync(['late', unused, 'ADAPTED', 'ctrl-v', true, 350], late);
    assert.deepEqual(late.replies, ['StaleContext']);
    assertNoContextWatches(idle); idle.service.destroy();
});

test('any non-explicit abort of a bound request before paste is retryable StaleContext', async () => {
    for (const change of [
        h => { h.inputMethod.currentFocus = null; }, // Silent: no watched signal fires.
        h => { h.selection.set_owner(1, {formats: new Map([['text/plain', 'user copy']])}); },
    ]) {
        const h = contextHarness(); const {token} = prepareBound(h);
        const reply = h.invocation();
        const op = h.service.InjectBoundAsync(['silent', token, 'ADAPTED', 'ctrl-v', true, 350], reply);
        await h.flush();
        change(h);
        await h.fire(20); await op;
        assert.equal(h.keyEvents.length, 0);
        assert.deepEqual(reply.replies, ['StaleContext']);
        assertNoContextWatches(h); h.service.destroy();
    }
});

test('a bound handoff invalidated after its keys were queued still reports the paste', async () => {
    const h = contextHarness(); const {token} = prepareBound(h);
    const reply = h.invocation();
    const op = h.service.InjectBoundAsync(['after', token, 'ADAPTED', 'ctrl-v', true, 350], reply);
    await h.flush(); await h.fire(20);
    assert.equal(h.keyEvents.length, 4);
    h.context.global.display.focus_window = {}; h.context.global.display.emit('notify::focus-window');
    h.inputMethod.currentFocus = null;
    await h.fire(350); await op;
    assert.deepEqual(reply.replies, [true], 'never StaleContext after paste: the daemon would insert twice');
    assertNoContextWatches(h); h.service.destroy();
});

test('a Busy bound request leaves the unclaimed token on its expiry clock', async () => {
    const h = contextHarness(); const {token} = prepareBound(h);
    h.service._pending.add(h.invocation());
    const busy = h.invocation();
    await h.service.InjectBoundAsync(['busy', token, 'ADAPTED', 'ctrl-v', true, 350], busy);
    assert.deepEqual(busy.replies, ['Busy']);
    assert.ok([...h.timers.values()].some(t => t.ms === 2000));
    h.service._pending.clear();
    await h.fire(2000);
    assert.equal(h.service._handoff, null);
    assertNoContextWatches(h); h.service.destroy();
});

test('repeated identical cursor rectangles keep the handoff; a moved rectangle makes it stale', async () => {
    const rect = (x, y) => ({get_x: () => x, get_y: () => y, get_width: () => 1, get_height: () => 18});
    for (const [events, expected] of [
        [[rect(10, 20), rect(10, 20), rect(10, 20)], [true]],
        [[rect(10, 20), rect(10, 40)], ['StaleContext']],
    ]) {
        const h = contextHarness(); const {token} = prepareBound(h);
        const reply = h.invocation();
        const op = h.service.InjectBoundAsync(['rect', token, 'ADAPTED', 'ctrl-v', true, 350], reply);
        await h.flush();
        for (const r of events) h.inputMethod.emit('cursor-location-changed', r);
        await h.fire(20);
        if (expected[0] === true) await h.fire(350);
        await op;
        assert.deepEqual(reply.replies, expected);
        assert.equal(h.keyEvents.length, expected[0] === true ? 4 : 0);
        assertNoContextWatches(h); h.service.destroy();
    }
});

test('the requested clipboard restore delay is capped', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 4294967295], h.invocation());
    await h.flush();
    await h.fire(20);
    assert.deepEqual([...h.timers.values()].map(timer => timer.ms), [2000]);
    await h.fire(2000);
    await operation;
    service.destroy();
});

test('independent: consumed token cannot replay during committed grace, and cancellation cannot shorten that grace', async () => {
    const h = contextHarness(); const {token} = prepareBound(h);
    const op = h.service.InjectBoundAsync(['first', token, 'ADAPTED', 'ctrl-v', true, 350], h.invocation());
    await h.flush(); await h.fire(20);
    const retry = h.invocation();
    await h.service.InjectBoundAsync(['second', token, 'ADAPTED', 'ctrl-v', true, 350], retry);
    assert.deepEqual(retry.replies, ['StaleContext']);
    h.service.CancelInjectAsync(['first'], h.invocation());
    assert.equal(new TextDecoder().decode(h.owners.get(1).formats.get('text/plain')), 'ADAPTED');
    assert.equal(h.keyEvents.length, 4);
    assert.deepEqual([...h.timers.values()].map(t => t.ms), [350]);
    await h.fire(350); await op;
    assert.equal(h.owners.get(1).formats.get('text/plain'), 'original');
    assertNoContextWatches(h); h.service.destroy();
});

test('independent: unsupported bound signal conservatively returns unavailable without a lease or watches', () => {
    const h = contextHarness();
    const connect = h.inputMethod.connect;
    h.inputMethod.connect = function(signal, fn) {
        if (signal === 'cursor-location-changed') throw new Error('unsupported signal');
        return connect.call(this, signal, fn);
    };
    const reply = h.invocation(); h.service.GetInsertionContextAsync([], reply);
    h.respond(['text', 2, 2]);
    assert.deepEqual(reply.values, [['unavailable', '', '', false, '']]);
    assertNoContextWatches(h); h.service.destroy();
});
