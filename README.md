# localwispr

Push-to-talk dictation for Linux Wayland, with optional CUDA acceleration.
Hold a key, speak (Polish, English, or both), and release to paste the
transcript into the focused application. Speech recognition and cleanup run
locally.

- **Speech → text:** whisper.cpp (`large-v3-turbo`, CUDA), automatic language
  detection restricted to the languages you pick
- **Cleanup:** a local LLM (Qwen2.5-3B by default) removes fillers
  (`um`, `yyy`, `eee`, `no to`, `znaczy`…) and fixes punctuation. It is told
  never to translate, and its output is discarded if it drifts too far from
  what you said
- **Overlay:** a click-through capsule with a live waveform and the transcript
  as you speak — via layer-shell or a GNOME Shell extension
- **Rust daemon** on whisper.cpp and llama.cpp; no Python, no Electron, no X11

## Desktop integration and testing

| Desktop | Overlay | Text insertion |
|---|---|---|
| GNOME (extension declares 48–51 compatibility) | Shell extension (installed by `install.sh`) | Shell extension |
| Other Wayland compositors with the required protocols | layer-shell HUD | `ext-data-control` / `wlr-data-control` clipboard + `/dev/uinput` |

Text insertion outside GNOME needs a compositor with a data-control protocol;
the overlay needs `wlr-layer-shell`. `localwispr status` shows what was
detected.

GNOME has been tested in a real session, including dictation into a GTK text
field and clipboard restoration. Outside GNOME, protocol fixtures cover
`ext-data-control` v1, `wlr-data-control` v1/v2, and both advertised together,
including clipboard restoration, cancellation, and emitted paste keys. These
are not real desktop sessions: application paste handling, focus, keyboard
adoption, terminal PRIMARY behavior, and HUD visuals on KDE Plasma, Sway,
Hyprland, niri, COSMIC, and other non-GNOME desktops remain unvalidated.

## Latency (RTX 3080, measured)

From key release to text ready, using the `transcribe` benchmark on real recordings:

| Recording | Speech model | Cleanup | Total |
|---|---|---|---|
| 11 s English | ~215 ms | ~70–85 ms | **~300 ms** |
| 12 s Polish | ~260 ms | ~85 ms | **~345 ms** |
| 45 s Polish | ~520 ms | ~100–135 ms | **~650 ms** |
| silence / noise | 12 ms (no text) | — | — |

In the daemon, language detection usually happens during the live preview,
which takes ~70 ms off the speech-model column. Models stay loaded (~4 GB VRAM).

Performance details:

- the speech model only decodes speech (Silero VAD runs once);
- live previews only encode as much audio as exists, and the final pass
  aborts any preview still running;
- the LLM reuses its cached system prompt (KV cache);
- prompt-lookup speculative decoding verifies copied transcript tokens in
  batches: 2.5–3.5× faster, with greedy output unchanged.

## Install

Build requirements: Git, Rust 1.92+, a C/C++ compiler, CMake, Clang/libclang,
pkg-config, GTK 4.14+, libadwaita 1.6+, gtk4-layer-shell, PipeWire and ALSA
development files:

```sh
# Arch
sudo pacman -S --needed git base-devel cmake clang pkgconf gtk4 libadwaita gtk4-layer-shell pipewire alsa-lib
# Fedora 41+
sudo dnf install git gcc gcc-c++ cmake clang clang-devel pkgconf-pkg-config gtk4-devel libadwaita-devel gtk4-layer-shell-devel pipewire-devel alsa-lib-devel
# Debian 13+ / Ubuntu 25.04+
sudo apt install git build-essential cmake clang libclang-dev pkg-config libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev libpipewire-0.3-dev libasound2-dev
```

GPU builds also need an NVIDIA GPU and the CUDA toolkit; `install.sh` finds
`nvcc` on `PATH`, in `/opt/cuda` or in `/usr/local/cuda` (or set `CUDACXX`).
CUDA kernels are built for the GPU in the build machine; set
`CMAKE_CUDA_ARCHITECTURES` (e.g. `"86;89"`) to build for others. On GNOME,
the optional clipboard helper also needs Mutter's development files
(`mutter` on Arch, `mutter-devel` on Fedora, `libmutter-*-dev` on Debian/Ubuntu).

```sh
git clone https://github.com/kuba001-pl/localwispr.git
cd localwispr
./install.sh               # builds with CUDA, installs to ~/.local, enables a systemd user service
./setup-permissions.sh     # once: input group, /dev/uinput, Caps Lock → inert; then log out/in
```

On GNOME, the installer pre-enables the Shell extension; GNOME only discovers
new extensions at login, so it starts working after you log out and back in
(the same logout the permission changes need).

Outside GNOME, make Caps Lock inert yourself (xkb option `caps:none`;
`setup-permissions.sh` prints the setting for KDE, Hyprland and Sway), or bind
push-to-talk to another key in Settings.

Models are downloaded automatically on first start into
`~/.local/share/localwispr/models` (resumable, SHA-256 verified). To fetch them
up front with a progress bar: `localwispr download`.

CPU-only build: `./install.sh --cpu`. Uninstall: `./install.sh --uninstall`
(keeps config and models) and `./setup-permissions.sh --undo`.

### Autostart

The installer enables a systemd user service for `graphical-session.target`.
If your session starts that target and imports the Wayland environment, no
extra autostart entry is needed. Session managers such as
[uwsm](https://github.com/Vladimir-csp/uwsm) can handle this integration.

Otherwise, use your compositor's documented autostart mechanism to run the
following shell command after the Wayland session is ready. Sway, river,
niri, and Hyprland use different configuration syntax; this is a shell command,
not a compositor configuration entry:

```sh
systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP && systemctl --user start localwispr
```

Without systemd, run `localwispr daemon` from your compositor's autostart.

## Use

| | |
|---|---|
| **Caps Lock** (hold) | dictate; release to insert |
| **Esc** while holding | cancel |
| **Super + Shift + ;** | settings (left Super and Shift) |

The microphone is opened only while you hold the key, and closed as soon as
you release it. "Keep microphone open" in Settings trades that for pre-roll
(audio from just before the key press).

Everything is configurable in the settings window: hotkeys (including mouse
buttons), microphone, speech model, languages, cleanup model and its safety
limits, paste shortcut, clipboard restore, and overlay options. Changes apply
immediately, and model switches download or load in the background. The config
lives in `$XDG_CONFIG_HOME/localwispr/config.toml` (`~/.config/…`), models in
`$XDG_DATA_HOME/localwispr/models`.

The default paste shortcut, Shift+Insert, avoids letter-layout dependence,
but paste handling depends on the application and compositor. Outside GNOME,
the Ctrl+V variants press the key in the `V` position of a US layout; on
Dvorak, AZERTY, and similar layouts, try Shift+Insert first. Some terminals
paste from PRIMARY, which may be unavailable on a compositor with only
`wlr-data-control` v1.

Clipboard restoration is enabled by default and is best effort. On the
non-GNOME data-control path, a narrow race remains: a user copy made between
the final clipboard-ownership check and the restore write can be overwritten
by the saved clipboard. To avoid this restore race, turn off **Settings →
Output → Restore clipboard** (or set `restore_clipboard = false` under
`[output]` in `config.toml`). The dictated text then remains on the clipboard.

CLI:

```sh
localwispr                        # same as `localwispr daemon`
localwispr settings               # open the settings window
localwispr status                 # what the daemon is doing
localwispr toggle                 # start/stop dictation; also `start`, `stop`, `cancel`:
                                  #   bind these in your compositor instead of evdev hotkeys if you prefer
localwispr models                 # catalog + what's installed
localwispr download [--model ID]  # fetch models up front
localwispr devices                # microphones
localwispr transcribe file.wav --repeat 3   # benchmark (--raw skips cleanup, --live replays previews)
localwispr cleanup "yyy no to jutro eee wyślę raport"
```

Logs: `journalctl --user -fu localwispr`. For timings, run
`systemctl --user edit localwispr`, add `Environment=LOCALWISPR_LOG=debug`
under `[Service]`, and restart the service. `LOCALWISPR_LLM_VERBOSE=1` shows
llama.cpp's own output.

## Architecture

```
 evdev threads ─────────keys────────┐                         ┌─► GNOME Shell extension (overlay + clipboard + paste)
 cpal/PipeWire → ring → DSP thread  │                         │
   (resample to 16 kHz, levels) ────┴─► engine (tokio) ─D-Bus─┼─► layer-shell HUD process (GTK4)
                                          │        │          └─► settings window (libadwaita)
                                          ▼        ▼
                                      whisper    localwispr-llm (llama.cpp, separate process)
                                      thread     ↑ JSON lines over stdin/stdout
```

- **Two processes for two inference engines.** whisper.cpp and llama.cpp
  each vendor their own ggml, and both cannot be linked into one binary
  (duplicate symbols, ABI mismatch). The LLM therefore runs in
  `localwispr-llm`. Side benefits: an LLM crash can't take dictation down, and
  turning cleanup off frees its VRAM completely.
- **Overlays are pure views.** The daemon broadcasts
  `org.localwispr.Daemon1` signals (`StateChanged`, `Levels` at 60 Hz,
  `PartialText`, `Notice`). It starts the layer-shell HUD itself when the
  compositor supports layer-shell, and defers to the GNOME extension when that
  is on the bus.
- **Text insertion** puts the text on CLIPBOARD and, where supported, PRIMARY,
  then sends the configured paste shortcut (Shift+Insert by default).
  With restoration enabled, it attempts to restore the previous contents
  while localwispr still owns the selection; see the restore race above.
  - On GNOME, this happens inside the Shell extension, because Mutter exposes no
    clipboard or virtual-keyboard protocol to background apps.
  - Elsewhere it uses `ext/wlr-data-control` in-process plus a `/dev/uinput`
    keyboard.

### Hyprland blur

The overlay's layer namespace is `localwispr`. Enable `blur` and ignore its
transparent pixels with a layer rule; the syntax depends on your Hyprland
version, see the [rules documentation](https://wiki.hypr.land/configuring/core/rules/).
Older hyprlang configs:

```
layerrule = blur, localwispr
layerrule = ignorezero, localwispr
```

## Troubleshooting

- **Nothing happens on Caps Lock** — `localwispr status` shows the last
  warning. Usually you're not in the `input` group yet (run
  `./setup-permissions.sh`, then log out and back in).
- **Caps Lock still toggles capitals** — GNOME: the script sets
  `caps:none`. Other desktops: set that xkb option in your compositor config.
- **Text doesn't appear on GNOME** — the extension must be enabled
  (`gnome-extensions info localwispr@localwispr.github.io`).
- **Paste doesn't work in a specific app** — try Ctrl+V or Ctrl+Shift+V in
  Settings → Output.
- **No overlay / text not inserted on another desktop** — `localwispr status`
  shows the detected `frontend` and `injection`. Insertion outside GNOME needs
  `/dev/uinput` access (`./setup-permissions.sh`) and a compositor with
  `ext-data-control` or `wlr-data-control`.
- **Service doesn't start at login** — see [Autostart](#autostart).

When reporting a bug, include your desktop and version, the output of
`localwispr status`, and `journalctl --user -u localwispr -b` (it may contain
dictated text; trim it before posting).

## Privacy

Speech, transcripts and cleanup never leave your machine. The only network
access is downloading models from Hugging Face. To insert text, localwispr
replaces the clipboard and optionally attempts to restore it (see the
limitations under [Use](#use)). Membership in the `input` group, needed for
global hotkeys, lets any program you run read every keystroke. Debug logs
contain transcripts.

## Model licenses

Models are downloaded from Hugging Face at first start; none are included in
this repository.

- Whisper (speech): MIT.
- Qwen2.5-3B-Instruct (default cleanup model): [Qwen Research
  License](https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/blob/main/LICENSE),
  non-commercial use only. Qwen2.5-1.5B and Qwen2.5-7B (selectable in
  Settings) are Apache-2.0.

## Development

```sh
cargo test --workspace                       # unit tests (DSP, resampler, guard, hotkeys, config…)
cargo test --workspace --no-default-features # without CUDA
node gnome-extension/tests/extension.test.cjs
cargo build --release --workspace
```

## License

MIT, see [LICENSE](LICENSE). Vendored whisper-rs is under the Unlicense and
whisper.cpp under MIT (see [vendor/README.md](vendor/README.md)).
