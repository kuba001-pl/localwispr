#!/usr/bin/env bash
# Assembles an installable bundle from an existing `cargo build --release`.
#
#   packaging/bundle.sh --variant cuda|vulkan|cpu [options]
#
#   --channel release|source  release bundles are named after the version only;
#                             source bundles get a timestamp (default: source)
#   --target-dir DIR          cargo target directory (default: target)
#   --out DIR                 where to put the bundle (default: target/bundle)
#   --bundle-lib SONAME       copy a system library into lib/ (repeatable);
#                             binaries must be linked with RPATH $ORIGIN/../lib
#   --cuda-runtime MAJOR      record that the bundle needs CUDA MAJOR libraries
#   --archive                 also write <bundle>.tar.xz
#
# Prints the bundle directory on stdout.
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
variant=
channel=source
target_dir=$root/target
out=$root/target/bundle
cuda_runtime=
archive=0
bundle_libs=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --variant) variant=$2; shift ;;
        --channel) channel=$2; shift ;;
        --target-dir) target_dir=$2; shift ;;
        --out) out=$2; shift ;;
        --bundle-lib) bundle_libs+=("$2"); shift ;;
        --cuda-runtime) cuda_runtime=$2; shift ;;
        --archive) archive=1 ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done
[[ $variant =~ ^(cuda|vulkan|cpu)$ ]] || { echo "--variant cuda|vulkan|cpu is required" >&2; exit 2; }
[[ $channel =~ ^(release|source)$ ]] || { echo "--channel must be release or source" >&2; exit 2; }
[[ -z $cuda_runtime || $cuda_runtime =~ ^[0-9]+$ ]] || { echo "--cuda-runtime takes a major version" >&2; exit 2; }

release=$target_dir/release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
arch=$(uname -m)
name=localwispr-$version-$arch-$variant
[[ $channel == source ]] && name+=-src-$(date +%Y%m%d%H%M%S)
bundle=$out/$name
uuid=localwispr@localwispr.github.io

for b in localwispr localwispr-llm; do
    [[ -x $release/$b ]] || { echo "$release/$b is missing; run cargo build --release first" >&2; exit 1; }
done
rm -rf -- "$bundle"
mkdir -p "$bundle/bin" "$bundle/lib" "$bundle/share/localwispr" "$bundle/share/gnome-shell/extensions/$uuid"
install -m755 "$release/localwispr" "$release/localwispr-llm" "$bundle/bin/"

# llama.cpp built as shared libraries (LLAMA_BUILD_SHARED_LIBS=1): the worker
# then uses the system's (or the runtime bundle's) cuBLAS instead of a 600 MB
# static copy.
needed=$(readelf -d "$bundle/bin/localwispr-llm" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')
if grep -q '^libllama\.so' <<< "$needed"; then
    libdir=$(ls -td "$release"/build/llama-cpp-sys-2-*/out/lib 2>/dev/null | head -1)
    [[ -n $libdir ]] || { echo "llama.cpp shared libraries not found" >&2; exit 1; }
    cp -a "$libdir"/lib*.so.* "$bundle/lib/"
fi
for soname in "${bundle_libs[@]}"; do
    path=$(ldd "$bundle/bin/localwispr" | awk -v l="$soname" '$1 == l {print $3}')
    [[ -f $path ]] || { echo "$soname is not linked by localwispr" >&2; exit 1; }
    install -m644 "$path" "$bundle/lib/$soname"
done
rmdir --ignore-fail-on-non-empty "$bundle/lib"

ext=$bundle/share/gnome-shell/extensions/$uuid
cp "$root/gnome-extension/$uuid/"* "$ext/"
if ! log=$("$root/gnome-extension/build-clipboard.sh" "$ext" 2>&1); then
    [[ $channel == release ]] && { printf '%s\n' "$log" >&2; exit 1; }
    echo "!! clipboard helper not built; only text clipboard contents will be restored" >&2
    printf '%s\n' "$log" | tail -3 >&2
fi
install -m755 "$root/setup-permissions.sh" "$bundle/share/localwispr/"
install -m644 "$root/LICENSE" "$bundle/share/localwispr/"
cuda_json=null
[[ -n $cuda_runtime ]] && cuda_json="\"$cuda_runtime\""
cat > "$bundle/share/localwispr/bundle.json" <<EOF
{"version": "$version", "variant": "$variant", "channel": "$channel", "cuda_runtime": $cuda_json}
EOF

if [[ $archive -eq 1 ]]; then
    XZ_OPT=${XZ_OPT:--T0 -6} tar -C "$out" --owner=0 --group=0 --numeric-owner -cJf "$out/$name.tar.xz" "$name"
fi
printf '%s\n' "$bundle"
