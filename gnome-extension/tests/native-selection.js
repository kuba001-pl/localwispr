// Run with gjs and the helper output directory as ARGV[0]. No Shell required.
const {Gio, GLib, Meta, GIRepository} = imports.gi;
const repository = GIRepository.Repository.dup_default?.() ?? GIRepository.Repository;
repository.prepend_search_path(ARGV[0]);
repository.prepend_library_path(ARGV[0]);
// The helper finds its parent type by name, as the extension ensures in the Shell.
void Meta.SelectionSource.$gtype;
const Native = imports.gi.LocalwisprSelection;
const expected = {
    'text/html': new TextEncoder().encode('<b>hello</b>'),
    'text/plain': new TextEncoder().encode('hello'),
    'text/uri-list': new TextEncoder().encode('file:///tmp/example\r\n'),
    'image/png': new Uint8Array([137, 80, 78, 71, 0, 255]),
    'application/x-empty': new Uint8Array(),
};
const source = Native.create(new GLib.Variant('a{say}', expected));
if (!(source instanceof Meta.SelectionSource))
    throw new Error('Wrong source type');
const mimes = source.get_mimetypes();
if (mimes.length !== Object.keys(expected).length)
    throw new Error('Missing MIME types');
const loop = new GLib.MainLoop(null, false);
let pending = mimes.length + 2;
let failure = null;
function done(error) {
    failure ??= error;
    if (!--pending)
        loop.quit();
}
for (const mime of mimes) {
    source.read_async(mime, null, (s, result) => {
        try {
            const stream = s.read_finish(result);
            const bytes = stream.read_bytes(1024, null).get_data();
            if (bytes.length !== expected[mime].length || bytes.some((byte, i) => byte !== expected[mime][i]))
                throw new Error(`Corrupt ${mime}`);
            stream.close(null);
            done(null);
        } catch (error) {
            done(error);
        }
    });
}
source.read_async('unsupported/type', null, (s, result) => {
    try {
        s.read_finish(result);
        done(new Error('Unknown MIME unexpectedly succeeded'));
    } catch {
        done(null);
    }
});
const cancelled = new Gio.Cancellable();
cancelled.cancel();
source.read_async('text/plain', cancelled, (s, result) => {
    try {
        s.read_finish(result);
        done(new Error('Cancelled read unexpectedly succeeded'));
    } catch {
        done(null);
    }
});
loop.run();
if (failure)
    throw failure;
print('Native selection: all formats, binary/empty bytes, unsupported MIME and cancellation passed');
