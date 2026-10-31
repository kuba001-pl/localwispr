#!/usr/bin/env bash
# Builds localwispr and installs it for the current user:
#   ~/.local/bin/{localwispr,localwispr-llm}
#   ~/.local/share/gnome-shell/extensions/localwispr@localwispr.github.io
#   ~/.local/share/applications/org.localwispr.Settings.desktop
#   ~/.config/systemd/user/localwispr.service (enabled)
#
#   ./install.sh               CUDA build (default)
#   ./install.sh --cpu         CPU-only build
#   ./install.sh --no-build    install already-built binaries from target/release
#   ./install.sh --no-service  don't install the systemd user service
#   ./install.sh --uninstall   remove everything above (keeps config and models)

set -euo pipefail
cd "$(dirname "$0")"

FEATURES=()
BUILD=1
SERVICE=1
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --cpu) FEATURES=(--no-default-features) ;;
        --no-build) BUILD=0 ;;
        --no-service) SERVICE=0 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*"; }

BIN=$HOME/.local/bin
EXT_UUID=localwispr@localwispr.github.io
EXT_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$EXT_UUID
UNIT_DIR=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
APP_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/applications
is_gnome() { [[ ":${XDG_CURRENT_DESKTOP:-}:" == *:GNOME:* ]]; }
has_systemd() { command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; }

if [[ $UNINSTALL -eq 1 ]]; then
    if has_systemd; then
        systemctl --user disable --now localwispr.service 2>/dev/null || true
    fi
    command -v gnome-extensions >/dev/null && gnome-extensions disable "$EXT_UUID" 2>/dev/null || true
    rm -f "$BIN/localwispr" "$BIN/localwispr-llm" "$UNIT_DIR/localwispr.service" \
        "$APP_DIR/org.localwispr.Settings.desktop"
    rm -rf "$EXT_DIR"
    has_systemd && systemctl --user daemon-reload
    say "Uninstalled. Config (~/.config/localwispr) and models (~/.local/share/localwispr) were kept."
    say "Undo the permission changes with ./setup-permissions.sh --undo"
    exit 0
fi

if [[ $BUILD -eq 1 ]]; then
    command -v cargo >/dev/null || { echo "Rust is required: https://rustup.rs" >&2; exit 1; }
    if [[ ${#FEATURES[@]} -eq 0 && -z "${CUDACXX:-}" ]]; then
        # Distributions put the CUDA toolkit in different places.
        nvcc=$(command -v nvcc || true)
        for dir in /opt/cuda /usr/local/cuda; do
            [[ -n $nvcc ]] || [[ ! -x $dir/bin/nvcc ]] || nvcc=$dir/bin/nvcc
        done
        if [[ -z $nvcc ]]; then
            echo "CUDA toolkit (nvcc) not found. Install it, set CUDACXX, or use ./install.sh --cpu" >&2
            exit 1
        fi
        nvcc=$(readlink -f "$nvcc")
        export CUDACXX=$nvcc
        export CUDA_PATH=${CUDA_PATH:-$(dirname "$(dirname "$nvcc")")}
    fi
    say "Building (release${FEATURES:+, CPU only})…"
    cargo build --release --workspace "${FEATURES[@]}"
fi

say "Installing binaries to $BIN"
mkdir -p "$BIN"
# Replace atomically so a running daemon keeps its old inode.
for b in localwispr localwispr-llm; do
    install -m755 "target/release/$b" "$BIN/.$b.new"
    mv -f "$BIN/.$b.new" "$BIN/$b"
done

say "Installing GNOME Shell extension"
# Start clean so a helper built for an older GNOME is never left behind.
rm -rf "$EXT_DIR"
mkdir -p "$EXT_DIR"
cp -f gnome-extension/$EXT_UUID/* "$EXT_DIR/"
# Native helper for restoring every clipboard format (GJS cannot implement a
# Meta.SelectionSource). Without it only the text of the clipboard is restored.
if is_gnome; then
    helper_dir=$(mktemp -d)
    if log=$(gnome-extension/build-clipboard.sh "$helper_dir" 2>&1); then
        cp -f "$helper_dir"/* "$EXT_DIR/"
        say "Built clipboard helper for Mutter $(cat "$helper_dir/selection-meta-version" 2>/dev/null || echo '?')"
    else
        warn "Clipboard helper not built (needs Mutter development files); only text clipboard contents will be restored"
        printf '%s\n' "$log" | tail -5 >&2
    fi
    rm -rf "$helper_dir"
fi

say "Installing desktop entry"
mkdir -p "$APP_DIR"
cat > "$APP_DIR/org.localwispr.Settings.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=localwispr Settings
Comment=Local voice dictation
Exec="$BIN/localwispr" settings
Icon=audio-input-microphone
Categories=Utility;Settings;
StartupNotify=true
EOF

if [[ $SERVICE -eq 1 ]] && ! has_systemd; then
    warn "No systemd user session; start '$BIN/localwispr daemon' from your compositor's autostart instead"
    SERVICE=0
fi
if [[ $SERVICE -eq 1 ]]; then
    say "Installing systemd user service"
    mkdir -p "$UNIT_DIR"
    template=$(<packaging/localwispr.service)
    printf '%s\n' "${template//@BIN@/$BIN}" > "$UNIT_DIR/localwispr.service"
    systemctl --user daemon-reload
    systemctl --user enable localwispr.service >/dev/null
    # The daemon needs the compositor's environment; desktops that start
    # graphical-session.target import it themselves.
    systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP 2>/dev/null || true
    if ! systemctl --user restart localwispr.service; then
        warn "The service failed to start; see: journalctl --user -u localwispr"
    fi
    if ! systemctl --user is-active --quiet graphical-session.target; then
        warn "Your session does not start graphical-session.target, so the service won't start at login."
        warn "Add this to your compositor's autostart (see README → Autostart):"
        warn "  systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP && systemctl --user start localwispr"
    fi
fi

if is_gnome; then
    if gnome-extensions enable "$EXT_UUID" 2>/dev/null; then
        say "GNOME extension enabled"
    else
        # GNOME only discovers new extensions at login. Pre-enabling it makes
        # the Shell load it on its own at the next login.
        enabled=$(gsettings get org.gnome.shell enabled-extensions)
        if [[ $enabled != *"'$EXT_UUID'"* ]]; then
            enabled=${enabled#@as }
            if [[ $enabled == '[]' ]]; then
                enabled="['$EXT_UUID']"
            else
                enabled="${enabled%]}, '$EXT_UUID']"
            fi
            gsettings set org.gnome.shell enabled-extensions "$enabled"
        fi
        say "GNOME extension will load at your next login (log out and back in)"
    fi
fi

cat <<EOF

Installed. Next steps:
  • First time only: ./setup-permissions.sh (input group, uinput, Caps Lock) then log out/in
  • Models download automatically on first start; progress: journalctl --user -fu localwispr
  • Settings: Super+Shift+;  or  localwispr settings
EOF
