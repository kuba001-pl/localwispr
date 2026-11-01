#!/usr/bin/env bash
# Installs, reinstalls and uninstalls release bundles from a local directory
# with install.sh, in a throwaway HOME. No GPU, systemd or GNOME needed.
#   packaging/test-install.sh <dist-dir with *.tar.xz and SHA256SUMS> <version>
set -euo pipefail
dist=$(cd -- "${1:?dist dir}" && pwd)
version=${2:?version}
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
home=$(mktemp -d)
mirror=$(mktemp -d)
trap 'rm -rf -- "$home" "$mirror"' EXIT
# Same layout as GitHub: <base>/v<version>/<asset>
ln -s "$dist" "$mirror/v$version"
run() { env -i HOME="$home" USER="$(id -un)" PATH="$home/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    LOCALWISPR_RELEASE_BASE="file://$mirror" "$@"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
arch=$(uname -m)
app=$home/.local/share/localwispr/app

if [[ -f $dist/localwispr-$version-$arch-vulkan.tar.xz ]]; then
    echo "--- vulkan: install"
    run bash "$root/install.sh" --version "$version" --variant vulkan --no-service --no-permissions < /dev/null
    run localwispr --version | grep -q "^localwispr $version (vulkan, release build)" || fail "wrong version"
    run localwispr models >/dev/null || fail "localwispr models"
    [[ -L $home/.local/share/gnome-shell/extensions/localwispr@localwispr.github.io ]] || fail "extension link"
    [[ -f $home/.local/share/gnome-shell/extensions/localwispr@localwispr.github.io/liblocalwispr-selection.so ]] \
        || fail "clipboard helper missing"
    grep -q "Exec=\"$home/.local/bin/localwispr\" settings" "$home/.local/share/applications/org.localwispr.Settings.desktop" \
        || fail "desktop entry"
    echo "--- vulkan: reinstall over itself"
    run bash "$root/install.sh" --version "$version" --variant vulkan --no-service --no-permissions < /dev/null
    # The installed copy may be in use, so the reinstall gets its own name
    # and the first one stays as the rollback target.
    [[ $(ls "$app/versions" | tr '\n' ' ') == "localwispr-$version-$arch-vulkan localwispr-$version-$arch-vulkan~2 " ]] \
        || fail "versions: $(ls "$app/versions")"
    [[ $(readlink "$app/current") == "versions/localwispr-$version-$arch-vulkan~2" ]] || fail "current: $(readlink "$app/current")"
    ! ls -a "$app" | grep -q '^\.incoming' || fail "staging left behind"
fi

if [[ -f $dist/localwispr-$version-$arch-cuda.tar.xz ]]; then
    echo "--- cuda: libraries resolve except the driver"
    work=$(mktemp -d)
    tar -xJf "$dist/localwispr-$version-$arch-cuda.tar.xz" -C "$work"
    tar -xJf "$dist"/localwispr-cuda*-runtime-"$arch".tar.xz -C "$work"
    bundle=$work/localwispr-$version-$arch-cuda
    grep -q '"cuda_runtime": "[0-9]*"' "$bundle/share/localwispr/bundle.json" || fail "cuda bundle.json"
    missing=$(for b in "$bundle"/bin/*; do LD_LIBRARY_PATH=$(echo "$work"/cuda-*) ldd "$b"; done \
        | awk '/not found/ {print $1}' | sort -u)
    [[ $missing == libcuda.so.1 || -z $missing ]] || fail "unresolved: $missing"
    rm -rf "$work"
fi

if [[ -x $home/.local/bin/localwispr ]]; then
    echo "--- uninstall"
    run localwispr uninstall
    [[ ! -e $app && ! -e $home/.local/bin/localwispr ]] || fail "uninstall left files"
fi
echo "install tests passed"
