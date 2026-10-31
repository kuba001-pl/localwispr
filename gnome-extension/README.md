# Companion extension development

The extension uses Mutter's selection API to restore CLIPBOARD and PRIMARY
independently. With the helper, every advertised MIME representation is read
before either selection is replaced, and restoration occurs only while
localwispr still owns that selection. Disabling cancels outstanding paste,
restore, notice, settings, and configuration tasks. Snapshots are limited to 32 MiB per selection, with a
five-second timeout per transfer. A failed backup still allows insertion, skips
restoration of that selection, logs the failure, and returns false from Inject.

GJS cannot implement `Meta.SelectionSource.read_async` (callback vfuncs are not
supported). `native/selection-source.c` provides that one source implementation.
Build it against the development package for the target Mutter version:

```sh
gnome-extension/build-clipboard.sh
```

This writes `liblocalwispr-selection.so` and `LocalwisprSelection-1.0.typelib` into
`localwispr@localwispr.github.io/`, plus the `selection-meta-version` ABI stamp.
Package all three beside `extension.js`. The build script does not install or
enable anything. The project installer builds the helper directly into the
installed extension directory. Rebuild when Mutter's ABI changes.
The build prefers the Mutter API matching the installed Shell (the Shell major
version on recent releases, or major minus 32 on older ones), falling back to
the highest installed development package with a message.
Set `MUTTER_VERSION=51` (for example) to choose an explicit Meta API version. The
extension checks the stamp against the loaded Meta namespace before importing
the helper and skips an incompatible or unstamped helper.

Without the helper, restoration keeps one representation, preferring
`text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`, then `STRING`, otherwise
the first advertised type. extensions.gnome.org forbids bundled binary
libraries: an EGO upload must exclude the native helper (`.so`, `.typelib`, and
ABI stamp). The extension works without it, with this text-only restoration.

Run the deterministic Shell mocks without touching the desktop:

```sh
node --test gnome-extension/tests/extension.test.cjs
```

Build and test the real native source in an isolated output directory (GNOME 51
paths shown; adjust for the target Mutter version):

```sh
gnome-extension/build-clipboard.sh /tmp/localwispr-clipboard-test
GI_TYPELIB_PATH=/usr/lib/mutter-51 LD_LIBRARY_PATH=/usr/lib/mutter-51 \
  gjs gnome-extension/tests/native-selection.js /tmp/localwispr-clipboard-test
```

The GJS test reads source streams directly; it never claims the desktop
clipboard or sends keyboard input.
