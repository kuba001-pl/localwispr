#!/usr/bin/env bash
# Packs the CUDA libraries the CUDA bundle links against into
# <out>/localwispr-cuda<major>-runtime-<arch>.tar.xz (installed once, only on
# systems without CUDA <major>).
#   packaging/cuda-runtime.sh <major> <out-dir>     (uses $CUDA_PATH, default /usr/local/cuda)
set -euo pipefail
major=${1:?CUDA major version}
out=${2:?output directory}
cuda=${CUDA_PATH:-/usr/local/cuda}
arch=$(uname -m)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
dir=$work/cuda-$major
mkdir -p "$dir" "$out"
for lib in libcudart libcublas libcublasLt; do
    src=
    for d in "$cuda/lib64" "$cuda/targets/$arch-linux/lib" "$cuda/lib"; do
        [[ -e $d/$lib.so.$major ]] && { src=$d/$lib.so.$major; break; }
    done
    [[ -n $src ]] || { echo "$lib.so.$major not found under $cuda" >&2; exit 1; }
    install -m644 "$(readlink -f "$src")" "$dir/$lib.so.$major"
done
for notice in "$cuda/EULA.txt" "$cuda/LICENSE"; do
    [[ -f $notice ]] && install -m644 "$notice" "$dir/" || true
done
name=localwispr-cuda$major-runtime-$arch.tar.xz
XZ_OPT=${XZ_OPT:--T0 -6} tar -C "$work" --owner=0 --group=0 --numeric-owner -cJf "$out/$name" "cuda-$major"
printf '%s\n' "$out/$name"
