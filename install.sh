#!/usr/bin/env bash
# localwispr installer. Installs a prebuilt release for the current user:
#
#   curl -fsSL https://github.com/kuba001-pl/localwispr/releases/latest/download/install.sh | bash
#
# Options (after `bash -s --` when piped):
#   --variant cuda|vulkan  GPU backend (default: cuda with an NVIDIA driver >= 580, else vulkan)
#   --version X.Y.Z        install this release instead of the latest
#   --from-source          build this checkout instead (add --vulkan or --cpu; default CUDA)
#   --no-service           don't install the systemd user service
#   --no-permissions       skip the one-time permission setup (needs sudo)
#   --uninstall            remove localwispr (keeps config and models)
#
# Later updates: `localwispr update` (or the button in Settings).

set -euo pipefail

REPO=kuba001-pl/localwispr
VARIANT=${LOCALWISPR_VARIANT:-}
VERSION=${LOCALWISPR_VERSION:-}
SOURCE=0
SOURCE_FEATURES=cuda
SERVICE=1
PERMISSIONS=1
UNINSTALL=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --variant) VARIANT=${2:-}; shift ;;
        --version) VERSION=${2#v}; shift ;;
        --from-source) SOURCE=1 ;;
        --vulkan) SOURCE_FEATURES=vulkan ;;
        --cpu) SOURCE_FEATURES=cpu ;;
        --no-service) SERVICE=0 ;;
        --no-permissions) PERMISSIONS=0 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" 2>/dev/null || true; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m!!\033[0m %s\n' "$*" >&2; exit 1; }

[[ $EUID -ne 0 ]] || die "Run the installer as your normal user, not root."
[[ $(uname -s) == Linux ]] || die "localwispr runs on Linux only."

DATA=${XDG_DATA_HOME:-$HOME/.local/share}
APP=$DATA/localwispr/app
BIN=${XDG_BIN_HOME:-$HOME/.local/bin}
INSTALL_ARGS=()
[[ $SERVICE -eq 1 ]] || INSTALL_ARGS+=(--no-service)

# --------------------------------------------------------------- uninstall --
if [[ $UNINSTALL -eq 1 ]]; then
    if [[ -x $BIN/localwispr ]] && "$BIN/localwispr" uninstall --help >/dev/null 2>&1; then
        exec "$BIN/localwispr" uninstall
    fi
    # Installs made by the old build-only installer.
    systemctl --user disable --now localwispr.service 2>/dev/null || true
    command -v gnome-extensions >/dev/null && gnome-extensions disable localwispr@localwispr.github.io 2>/dev/null || true
    rm -f "$BIN/localwispr" "$BIN/localwispr-llm" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/localwispr.service" \
        "$DATA/applications/org.localwispr.Settings.desktop"
    rm -rf "$DATA/gnome-shell/extensions/localwispr@localwispr.github.io"
    systemctl --user daemon-reload 2>/dev/null || true
    say "Uninstalled. Config and models were kept."
    exit 0
fi

# Asks on the terminal even when the script itself arrives on stdin.
ask() { # $1 = question; default yes
    local answer
    [[ -r /dev/tty ]] || return 1
    printf '%s [Y/n] ' "$1" > /dev/tty
    read -r answer < /dev/tty || return 1
    [[ -z $answer || $answer =~ ^[Yy] ]]
}

distro() {
    local id='' like=''
    if [[ -r /etc/os-release ]]; then
        id=$(. /etc/os-release; echo "${ID:-}")
        like=$(. /etc/os-release; echo "${ID_LIKE:-}")
    fi
    case " $id $like " in
        *" arch "*) echo arch ;;
        *" fedora "*|*" rhel "*) echo fedora ;;
        *" opensuse"*|*" suse "*) echo suse ;;
        *" debian "*|*" ubuntu "*) echo debian ;;
        *) echo other ;;
    esac
}

# Distribution package that provides a missing runtime library.
package_for() { # $1 = soname
    case "$(distro):$1" in
        arch:libgtk-4*) echo gtk4 ;;               fedora:libgtk-4*) echo gtk4 ;;
        arch:libadwaita*) echo libadwaita ;;       fedora:libadwaita*) echo libadwaita ;;
        arch:libpipewire*) echo pipewire ;;        fedora:libpipewire*) echo pipewire-libs ;;
        arch:libasound*) echo alsa-lib ;;          fedora:libasound*) echo alsa-lib ;;
        arch:libvulkan*) echo vulkan-icd-loader ;; fedora:libvulkan*) echo vulkan-loader ;;
        debian:libgtk-4*) echo libgtk-4-1 ;;       suse:libgtk-4*) echo libgtk-4-1 ;;
        debian:libadwaita*) echo libadwaita-1-0 ;; suse:libadwaita*) echo libadwaita-1-0 ;;
        debian:libpipewire*) echo libpipewire-0.3-0t64 ;; suse:libpipewire*) echo libpipewire-0_3-0 ;;
        debian:libasound*) echo libasound2t64 ;;   suse:libasound*) echo libasound2 ;;
        debian:libvulkan*) echo libvulkan1 ;;      suse:libvulkan*) echo libvulkan1 ;;
        *:libcuda.so*) echo "the NVIDIA driver" ;;
        *) echo "$1" ;;
    esac
}

# Fails with install hints when the bundle's binaries cannot be loaded.
check_libraries() { # $1 = bundle dir, $2 = extra library dir
    local missing
    missing=$(for b in "$1"/bin/*; do LD_LIBRARY_PATH=$2${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH} ldd "$b" 2>/dev/null; done \
        | awk '/not found/ {print $1}' | sort -u)
    [[ -z $missing ]] && return 0
    warn "Missing system libraries: $(echo $missing)"
    local packages
    packages=$(for l in $missing; do package_for "$l"; done | sort -u | tr '\n' ' ')
    case "$(distro)" in
        arch) warn "Install them with: sudo pacman -S --needed $packages" ;;
        fedora) warn "Install them with: sudo dnf install $packages" ;;
        debian) warn "Install them with: sudo apt install $packages" ;;
        suse) warn "Install them with: sudo zypper install $packages" ;;
        *) warn "Install these packages: $packages" ;;
    esac
    warn "GNOME 47+ or another desktop with GTK 4.14+ and libadwaita 1.6+ is required."
    return 1
}

setup_permissions() { # $1 = setup-permissions.sh
    [[ $PERMISSIONS -eq 1 ]] || return 0
    if [[ -e /etc/udev/rules.d/70-localwispr.rules ]] && id -nG "$(id -un)" | tr ' ' '\n' | grep -qx input; then
        return 0
    fi
    echo
    say "One-time setup: global hotkeys need read access to keyboards (the input group),"
    say "pasting outside GNOME needs /dev/uinput, and Caps Lock becomes push-to-talk."
    if ask "Run it now? It uses sudo"; then
        # Keep a copy so it can be undone after uninstalling.
        bash "$1" < /dev/tty || warn "Permission setup failed; rerun: bash $1"
    else
        warn "Skipped. Run it later with: bash $1"
    fi
}

# ------------------------------------------------------------- from source --
if [[ $SOURCE -eq 1 ]]; then
    cd "$(dirname "${BASH_SOURCE[0]}")"
    [[ -f Cargo.toml && -f packaging/bundle.sh ]] || die "--from-source must be run from a localwispr checkout"
    command -v cargo >/dev/null || die "Rust is required: https://rustup.rs"
    features=(--no-default-features)
    case $SOURCE_FEATURES in
        cuda)
            features=()
            if [[ -z ${CUDACXX:-} ]]; then
                # Distributions put the CUDA toolkit in different places.
                nvcc=$(command -v nvcc || true)
                for dir in /opt/cuda /usr/local/cuda; do
                    [[ -n $nvcc ]] || [[ ! -x $dir/bin/nvcc ]] || nvcc=$dir/bin/nvcc
                done
                [[ -n $nvcc ]] || die "CUDA toolkit (nvcc) not found. Install it, set CUDACXX, or use --vulkan / --cpu"
                nvcc=$(readlink -f "$nvcc")
                export CUDACXX=$nvcc
                export CUDA_PATH=${CUDA_PATH:-$(dirname "$(dirname "$nvcc")")}
            fi
            # llama.cpp as shared libraries: the LLM worker uses the system's
            # cuBLAS instead of embedding a 600 MB static copy.
            export LLAMA_BUILD_SHARED_LIBS=1
            ;;
        vulkan) features+=(--features vulkan) ;;
    esac
    # Private libraries live next to the binaries.
    export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,--disable-new-dtags,-rpath,\$ORIGIN/../lib"
    say "Building ($SOURCE_FEATURES)…"
    cargo build --release --locked --workspace "${features[@]}"
    # CARGO_TARGET_DIR or a cargo config may move the build output.
    target_dir=$(cargo metadata --no-deps --format-version 1 \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    bundle=$(packaging/bundle.sh --variant "$SOURCE_FEATURES" --channel source --target-dir "${target_dir:-target}")
    check_libraries "$bundle" "" || exit 1
    "$bundle/bin/localwispr" install "${INSTALL_ARGS[@]}"
    rm -rf "$bundle"
    setup_permissions "$APP/current/share/localwispr/setup-permissions.sh"
    exit 0
fi

# ---------------------------------------------------------------- prebuilt --
for tool in curl tar xz sha256sum; do
    command -v $tool >/dev/null || die "$tool is required"
done
arch=$(uname -m)
[[ $arch == x86_64 ]] || die "Prebuilt releases are x86_64 only; build from source on $arch"

if [[ -z $VARIANT ]]; then
    driver=
    if command -v nvidia-smi >/dev/null; then
        driver=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 || true)
    fi
    if [[ -z $driver && -r /proc/driver/nvidia/version ]]; then
        driver=$(sed -nE 's/.*Kernel Module( for [a-z0-9_]+)? +([0-9.]+).*/\2/p' /proc/driver/nvidia/version | head -1)
    fi
    # CUDA 13 needs driver 580+ and a Turing (compute 7.5) or newer GPU.
    cc=$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | sort -V | head -1 || true)
    if [[ -n $driver ]]; then
        if [[ $cc =~ ^[0-9]+\.[0-9]+$ ]] && [[ $(printf '%s\n' 7.5 "$cc" | sort -V | head -1) != 7.5 ]]; then
            warn "This NVIDIA GPU (compute capability $cc) is too old for CUDA 13; using Vulkan instead"
            VARIANT=vulkan
        elif [[ ${driver%%.*} =~ ^[0-9]+$ ]] && (( ${driver%%.*} >= 580 )); then
            VARIANT=cuda
        else
            warn "NVIDIA driver $driver is older than 580 (needed for CUDA 13); using Vulkan instead"
            VARIANT=vulkan
        fi
    else
        VARIANT=vulkan
    fi
fi
[[ $VARIANT =~ ^(cuda|vulkan)$ ]] || die "--variant must be cuda or vulkan"

if [[ -z $VERSION ]]; then
    [[ -z ${LOCALWISPR_RELEASE_BASE:-} ]] || die "LOCALWISPR_RELEASE_BASE needs --version"
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
        || die "Could not reach GitHub"
    VERSION=${url##*/tag/v}
    [[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "No published release found"
fi
# LOCALWISPR_RELEASE_BASE points at a mirror (tests use file://).
base=${LOCALWISPR_RELEASE_BASE:-https://github.com/$REPO/releases/download}/v$VERSION
name=localwispr-$VERSION-$arch-$VARIANT
say "Installing localwispr $VERSION ($VARIANT)"

mkdir -p "$APP"
staging=$(mktemp -d "$APP/.incoming-XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
curl -fsSL "$base/SHA256SUMS" -o "$staging/SHA256SUMS" || die "Release v$VERSION not found"

fetch() { # $1 = asset
    grep -qE "^[0-9a-f]{64}  \*?$1\$" "$staging/SHA256SUMS" || die "Release v$VERSION has no $1"
    curl -fL --progress-bar "$base/$1" -o "$staging/$1" || die "Downloading $1 failed"
    (cd "$staging" && grep -E "^[0-9a-f]{64}  \*?$1\$" SHA256SUMS | sha256sum -c --status) \
        || die "Checksum mismatch for $1"
    tar -xJf "$staging/$1" -C "$staging" --no-same-owner
    rm -f "$staging/$1"
}

fetch "$name.tar.xz"
bundle=$staging/$name
cuda=$(sed -nE 's/.*"cuda_runtime": *"([0-9]+)".*/\1/p' "$bundle/share/localwispr/bundle.json")
runtime=
if [[ -n $cuda ]]; then
    runtime=$APP/runtime/cuda-$cuda
    have_system_cuda() {
        local cache
        cache=$( (/sbin/ldconfig -p || /usr/sbin/ldconfig -p || ldconfig -p) 2>/dev/null)
        for lib in libcudart libcublas libcublasLt; do
            grep -q "^[[:space:]]*$lib\.so\.$cuda " <<< "$cache" || return 1
        done
    }
    if [[ ! -e $runtime/libcublasLt.so.$cuda ]] && ! have_system_cuda; then
        say "CUDA $cuda libraries not found on this system; downloading them once (~400 MB)"
        fetch "localwispr-cuda$cuda-runtime-$arch.tar.xz"
        mkdir -p "$APP/runtime"
        rm -rf "$runtime"
        mv "$staging/cuda-$cuda" "$runtime"
    fi
fi

check_libraries "$bundle" "$runtime" || exit 1
LD_LIBRARY_PATH=$runtime${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH} "$bundle/bin/localwispr" --version >/dev/null \
    || die "The downloaded build does not run on this system"
"$bundle/bin/localwispr" install "${INSTALL_ARGS[@]}"
setup_permissions "$APP/current/share/localwispr/setup-permissions.sh"

cat <<EOF

Done. Models download automatically on first start (progress: journalctl --user -fu localwispr).
  • Dictate: hold Caps Lock      • Settings: Super+Shift+;  or  localwispr settings
  • Update:  localwispr update   • Uninstall: localwispr uninstall
EOF
