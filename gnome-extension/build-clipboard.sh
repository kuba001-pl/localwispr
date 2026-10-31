#!/usr/bin/env bash
# Build only. Does not install or enable any extension.
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")" && pwd)
output=${1:-"$root/localwispr@localwispr.github.io"}
if [[ -n "${MUTTER_VERSION:-}" ]]; then
    if [[ ! "$MUTTER_VERSION" =~ ^[0-9]+$ ]]; then
        echo 'MUTTER_VERSION must be a numeric Meta API version.' >&2
        exit 1
    fi
    package="libmutter-$MUTTER_VERSION"
    pkg-config --exists "$package" || { echo "$package is not installed." >&2; exit 1; }
else
    shell_version=$(gnome-shell --version 2>/dev/null | sed -nE 's/.* ([0-9]+)(\..*)?$/\1/p' || true)
    # Mutter's API version is the Shell version on recent releases and the
    # Shell version minus 32 on older ones; try both.
    package=
    for candidate in "${shell_version:-0}" "$(( ${shell_version:-32} - 32 ))"; do
        if [[ -n "$shell_version" ]] && pkg-config --exists "libmutter-$candidate"; then
            package="libmutter-$candidate"
            break
        fi
    done
    if [[ -z "$package" ]]; then
        package=$(pkg-config --list-all | awk '$1 ~ /^libmutter-[0-9]+$/ {print $1}' | sort -V | tail -1)
        echo "Matching Mutter headers unavailable; falling back to ${package:-no installed libmutter}." >&2
    fi
fi
if [[ -z "$package" ]]; then
    echo 'Mutter development headers are required for clipboard restoration.' >&2
    exit 1
fi
version=${package#libmutter-}
libdir=$(pkg-config --variable=libdir "$package")
girdir="${libdir}/mutter-${version}"
if [[ ! -f "$girdir/Meta-${version}.gir" ]]; then
    girdir=$(pkg-config --variable=girdir "$package")
fi
mkdir -p -- "$output"
rm -f -- "$output/selection-meta-version"
# pkg-config flags are intentionally split into compiler arguments.
read -r -a cflags <<< "$(pkg-config --cflags "$package")"
read -r -a libs <<< "$(pkg-config --libs "$package")"
"${CC:-cc}" -shared -fPIC -Wall -Wextra -Werror -O2 "${cflags[@]}" \
    "$root/native/selection-source.c" "${libs[@]}" \
    -Wl,-rpath,"${libdir}/mutter-${version}" -o "$output/liblocalwispr-selection.so"
temporary=$(mktemp -d)
gir="$temporary/LocalwisprSelection-1.0.gir"
trap 'rm -rf -- "$temporary"' EXIT
sed "s/@META_VERSION@/${version}/g" "$root/native/LocalwisprSelection-1.0.gir.in" > "$gir"
compiler=$(command -v gi-compile-repository || command -v g-ir-compiler)
"$compiler" --includedir="$girdir" --output="$output/LocalwisprSelection-1.0.typelib" "$gir"
printf '%s\n' "$version" > "$output/selection-meta-version"
