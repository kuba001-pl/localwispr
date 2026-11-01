#!/usr/bin/env bash
# Builds the optional clipboard helper (no Mutter headers needed; the helper
# checks the running Mutter's ABI itself). Does not install anything.
#   build-clipboard.sh [output-dir]
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")" && pwd)
output=${1:-"$root/localwispr@localwispr.github.io"}
pkg-config --exists gio-2.0 || { echo 'GLib/GIO development files are required.' >&2; exit 1; }
compiler=$(command -v gi-compile-repository || command -v g-ir-compiler) \
    || { echo 'gi-compile-repository or g-ir-compiler is required.' >&2; exit 1; }
mkdir -p -- "$output"
rm -f -- "$output/selection-abi" "$output/selection-meta-version"
# pkg-config flags are intentionally split into compiler arguments.
read -r -a cflags <<< "$(pkg-config --cflags gio-2.0)"
read -r -a libs <<< "$(pkg-config --libs gio-2.0)"
"${CC:-cc}" -shared -fPIC -Wall -Wextra -Werror -O2 -Wl,--no-undefined -Wl,--as-needed \
    "${cflags[@]}" "$root/native/selection-source.c" "${libs[@]}" \
    -o "$output/liblocalwispr-selection.so"
"$compiler" --output="$output/LocalwisprSelection-1.0.typelib" "$root/native/LocalwisprSelection-1.0.gir"
# Read by the extension before it loads the helper.
printf '1\n' > "$output/selection-abi"
