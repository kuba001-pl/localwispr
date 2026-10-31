// Deterministic lifecycle regressions. Executes the extension with mocked Shell
// services; no connection to the user's clipboard, keyboard or desktop.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

function harness() {
    let nextSource = 1;
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
    const context = vm.createContext({
        TextEncoder, TextDecoder, console: {warn(message) { warnings.push(message); }},
        Clutter: {InputDeviceType: {KEYBOARD_DEVICE: 1}, KeyState: {PRESSED: 1, RELEASED: 0}},
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
            get_monotonic_time() { return 1; },
        },
        Meta: {
            SelectionType: {SELECTION_PRIMARY: 0, SELECTION_CLIPBOARD: 1, SELECTION_DND: 2},
            SelectionSourceMemory: {new(mime, bytes) { return {formats: new Map([[mime, bytes.get_data()]])}; }},
        },
        global: {
            stage: {context: {get_backend() { return {get_default_seat() { return {
                create_virtual_device() { return {
                    notify_keyval(...event) { keyEvents.push(event); }, run_dispose() { disposed++; },
                }; },
            }; }}; }}},
            display: {get_selection() { return selection; }, disconnect() {}},
        },
        St: {}, Pango: {}, Main: {}, GIRepository: {}, Extension: class {},
        logError(error, message) { errors.push(`${message}: ${error}`); },
    });
    const filename = path.join(__dirname, '../localwispr@localwispr.github.io/extension.js');
    const code = fs.readFileSync(filename, 'utf8')
        .replace(/^import .*;\n/gm, '').replace('export default class', 'class');
    vm.runInContext(`${code}\nthis.api = {Lifetime, ShellService, LocalWisprExtension, Capsule, loadNativeSelection};`, context);
    vm.runInContext(`nativeSelection = {create(variant) { return {formats: new Map(Object.entries(variant.value))}; }};`, context);
    const flush = () => new Promise(resolve => setImmediate(resolve));
    async function fire(ms) {
        for (const [id, timer] of [...timers]) {
            if (timer.ms === ms) { timers.delete(id); timer.fn(); }
        }
        await flush();
    }
    function invocation() {
        return {replies: [], return_value(value) { this.replies.push(value.value[0]); },
            return_dbus_error(name) { this.replies.push(name.split('.').pop()); },
            return_error_literal(...error) { this.replies.push(error); }};
    }
    return {...context.api, context, owners, selection, timers, keyEvents, warnings, errors, transfers,
        get disposed() { return disposed; }, flush, fire, invocation};
}

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

test('disable after paste cancels delayed restoration and notice/settings waits', async () => {
    const h = harness();
    const service = new h.ShellService(1);
    const operation = service.InjectAsync(['dictation', 'ctrl-v', true, 400], h.invocation());
    await h.flush();
    await h.fire(20);
    service.destroy();
    await operation;
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
    const c = Object.create(h.Capsule.prototype);
    Object.assign(c, {current: Array(16).fill(0.8), target: Array(16).fill(0.9),
        mode: 'recording', label: {text: 'live words', visible: true},
        actor: {set_style(value) { this.style = value; }},
        _reposition() {}, hide() { this.mode = 'hidden'; }});
    c.configure(['none', 4, 100, false, 0.2]);
    assert.equal(c.mode, 'hidden');
    assert.equal(c.enabled, false);
    assert.equal(c.label.visible, false);
    assert.equal(c.current.length, 4);
    assert.ok(c.current.every(value => value === 0));
    assert.equal(c.bottomMargin, 100);
    assert.ok(c.actor.style.includes('0.2'));
    c.configure(['gnome', 48, 50, true, 0.8]);
    assert.equal(c.enabled, true);
    assert.equal(c.current.length, 48);
    assert.equal(c.label.visible, true);
    c.setLevels([0.1, 0.2]);
    assert.equal(c.target.length, 48);
    assert.equal(c.target[0], 0.1);
    assert.ok(c.target.slice(2).every(value => value === 0));
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
    let version = '19';
    const repository = {get_version() { return '51'; }, prepend_search_path() {}, prepend_library_path() {}};
    h.context.GIRepository.Repository = {dup_default() { return repository; }};
    h.context.GLib.file_get_contents = () => [true, new TextEncoder().encode(version)];
    h.context.imports = {gi: {get LocalwisprSelection() { imports++; return {}; }}};
    h.loadNativeSelection('/extension');
    assert.equal(imports, 0);
    assert.ok(h.warnings.some(message => message.includes('does not match loaded Meta')));
    version = '51';
    h.loadNativeSelection('/extension');
    assert.equal(imports, 1);
    h.context.GLib.file_get_contents = () => { throw new Error('missing ABI stamp'); };
    h.loadNativeSelection('/extension');
    assert.equal(imports, 1);
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

test('helper build prefers matching headers, supports override, and announces fallback', () => {
    const os = require('node:os');
    const {spawnSync} = require('node:child_process');
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'localwispr-build-test-'));
    try {
        const bin = path.join(temporary, 'bin');
        fs.mkdirSync(bin);
        const scripts = {
            'gnome-shell': '#!/bin/sh\necho "GNOME Shell 51.1"\n',
            'pkg-config': `#!/bin/sh
case "$1" in
    --exists) case " $TEST_PACKAGES " in *" \${2#libmutter-} "*) exit 0;; *) exit 1;; esac;;
    --list-all) for v in $TEST_PACKAGES; do echo "libmutter-$v Meta"; done;;
    --variable=libdir|--variable=girdir) echo /nonexistent;;
esac
`,
            'cc': '#!/bin/sh\nexit 0\n',
            'gi-compile-repository': '#!/bin/sh\nexit 0\n',
        };
        for (const [name, script] of Object.entries(scripts))
            fs.writeFileSync(path.join(bin, name), script, {mode: 0o755});
        for (const [packages, override, expected, fallback] of [
            ['19 51', '', '51', false], ['19 50', '', '19', false], ['18 50', '', '50', true], ['18 51', '18', '18', false],
        ]) {
            const output = path.join(temporary, `out-${expected}`);
            const result = spawnSync('bash', [path.join(__dirname, '../build-clipboard.sh'), output], {
                encoding: 'utf8', env: {...process.env, PATH: `${bin}:${process.env.PATH}`,
                    CC: path.join(bin, 'cc'), MUTTER_VERSION: override, TEST_PACKAGES: packages},
            });
            assert.equal(result.status, 0, result.stderr);
            assert.equal(fs.readFileSync(path.join(output, 'selection-meta-version'), 'utf8').trim(), expected);
            assert.equal(result.stderr.includes('falling back'), fallback);
        }
        const invalid = spawnSync('bash', [path.join(__dirname, '../build-clipboard.sh'), temporary], {
            encoding: 'utf8', env: {...process.env, PATH: `${bin}:${process.env.PATH}`,
                MUTTER_VERSION: '99', TEST_PACKAGES: '51'},
        });
        assert.notEqual(invalid.status, 0);
        assert.match(invalid.stderr, /libmutter-99 is not installed/);
    } finally {
        fs.rmSync(temporary, {recursive: true, force: true});
    }
});
