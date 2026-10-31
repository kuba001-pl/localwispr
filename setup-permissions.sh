#!/usr/bin/env bash
# localwispr: one-time system setup.
#
#  * read access to /dev/input/event*  (global push-to-talk via evdev)
#  * write access to /dev/uinput       (paste keystroke on non-GNOME desktops)
#  * makes Caps Lock inert in the keyboard layout (xkb `caps:none`) so it can
#    be used as push-to-talk without toggling capitals
#
# Security note: membership in the `input` group lets any program you run read
# every keystroke. That is inherent to compositor-independent global hotkeys.
#
# Usage: ./setup-permissions.sh            (asks for sudo where needed)
#        ./setup-permissions.sh --no-caps  (leave Caps Lock alone)
#        ./setup-permissions.sh --undo

set -euo pipefail

RULES=/etc/udev/rules.d/70-localwispr.rules
MODULES=/etc/modules-load.d/localwispr.conf
SAVED_CAPS=${XDG_STATE_HOME:-$HOME/.local/state}/localwispr/xkb-caps-options
USER_NAME=${SUDO_USER:-$USER}
CAPS=1
UNDO=0
for arg in "$@"; do
    case "$arg" in
        --no-caps) CAPS=0 ;;
        --undo) UNDO=1 ;;
        -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*"; }

if [[ $EUID -eq 0 ]]; then
    echo "Run as your normal user; the script calls sudo itself." >&2
    exit 1
fi

xkb_get() { gsettings get org.gnome.desktop.input-sources xkb-options 2>/dev/null || echo "@as []"; }

# Edits the GNOME xkb option list without clobbering the user's other options.
xkb_edit() { # $1 = add|remove
    command -v gsettings >/dev/null || return 1
    gsettings list-keys org.gnome.desktop.input-sources >/dev/null 2>&1 || return 1
    local cur token new='[' separator='' replaced=() token_pattern="^'([a-zA-Z0-9_:.-]+)'"
    cur=$(xkb_get)
    cur=${cur#@as }
    [[ $cur == \[*\] ]] || return 1
    cur=${cur:1:${#cur}-2}
    while [[ -n $cur ]]; do
        cur="${cur#"${cur%%[![:space:]]*}"}"
        [[ $cur =~ $token_pattern ]] || return 1
        token=${BASH_REMATCH[1]}
        cur=${cur:${#BASH_REMATCH[0]}}
        if [[ $token != caps:* ]]; then
            new+="$separator'$token'"
            separator=', '
        elif [[ $1 == add && $token != caps:none ]]; then
            replaced+=("$token")
        fi
        cur="${cur#"${cur%%[![:space:]]*}"}"
        [[ -z $cur ]] && break
        [[ $cur == ,* ]] || return 1
        cur=${cur:1}
    done
    if [[ $1 == add ]]; then
        new+="$separator'caps:none'"
        # Remember a Caps Lock option this replaces (e.g. caps:escape) for --undo.
        if [[ ${#replaced[@]} -gt 0 ]]; then
            mkdir -p "$(dirname "$SAVED_CAPS")"
            printf '%s\n' "${replaced[@]}" > "$SAVED_CAPS"
        fi
    elif [[ -f $SAVED_CAPS ]]; then
        while read -r token; do
            [[ $token =~ ^caps:[a-zA-Z0-9_]+$ ]] && new+="$separator'$token'" && separator=', '
        done < "$SAVED_CAPS"
        rm -f "$SAVED_CAPS"
    fi
    new+=']'
    gsettings set org.gnome.desktop.input-sources xkb-options "$new"
}

if [[ $UNDO -eq 1 ]]; then
    say "Removing udev rule and module config"
    sudo rm -f "$RULES" "$MODULES"
    sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=misc --subsystem-match=input || true
    if [[ $CAPS -eq 1 ]] && xkb_edit remove; then say "Restored Caps Lock (GNOME)"; fi
    warn "Remove yourself from the input group manually if you want: sudo gpasswd -d $USER_NAME input"
    exit 0
fi

say "Loading the uinput kernel module (now and at boot)"
sudo groupadd -f input
echo uinput | sudo tee "$MODULES" >/dev/null
sudo modprobe uinput

say "Installing udev rule $RULES"
sudo tee "$RULES" >/dev/null <<'EOF'
# localwispr — virtual keyboard for the paste keystroke.
# uaccess grants the logged-in seat user access without any group membership.
KERNEL=="uinput", SUBSYSTEM=="misc", MODE="0660", GROUP="input", TAG+="uaccess", OPTIONS+="static_node=uinput"
# Global hotkeys require input-group access to physical input events.
KERNEL=="event*", SUBSYSTEM=="input", MODE="0660", GROUP="input"
EOF
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput || true
sudo udevadm trigger --subsystem-match=input || true

if id -nG "$USER_NAME" | tr ' ' '\n' | grep -qx input; then
    say "$USER_NAME is already in the input group"
    NEED_RELOGIN=0
else
    say "Adding $USER_NAME to the input group (keyboard reading for hotkeys)"
    sudo usermod -aG input "$USER_NAME"
    NEED_RELOGIN=1
fi

if [[ $CAPS -eq 1 ]]; then
    case "${XDG_CURRENT_DESKTOP:-}" in
        *GNOME*)
            if xkb_edit add; then
                say "Caps Lock no longer toggles capitals (GNOME xkb-options: $(xkb_get))"
            fi
            ;;
        *KDE*)
            warn "KDE: System Settings → Keyboard → Key Bindings → Caps Lock behavior → 'Caps Lock is disabled'"
            ;;
        *Hyprland*)
            warn "Hyprland: add 'kb_options = caps:none' to the input section of hyprland.conf"
            ;;
        *sway*|*Sway*)
            warn "Sway: add 'input type:keyboard xkb_options caps:none' to your sway config"
            ;;
        *)
            warn "Set the xkb option 'caps:none' in your compositor to make Caps Lock inert"
            ;;
    esac
fi

echo
say "Done."
if [[ $NEED_RELOGIN -eq 1 ]]; then
    warn "Log out and back in so the new group membership takes effect."
fi
