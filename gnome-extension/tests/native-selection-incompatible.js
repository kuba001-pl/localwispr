// Run with gjs and the helper output directory as ARGV[0]. Meta is never
// loaded, so the helper must refuse every call (and never deadlock).
const {GLib, GIRepository} = imports.gi;
const repository = GIRepository.Repository.dup_default?.() ?? GIRepository.Repository;
repository.prepend_search_path(ARGV[0]);
repository.prepend_library_path(ARGV[0]);
const Native = imports.gi.LocalwisprSelection;
for (let i = 0; i < 3; i++) {
    if (Native.create(new GLib.Variant('a{say}', {'text/plain': new Uint8Array([104])})) !== null)
        throw new Error('Helper accepted a missing MetaSelectionSource');
}
print('Native selection: incompatible Mutter is refused repeatedly');
