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
- **Context-aware recognition:** nearby accessible text supplies local Whisper
  vocabulary hints for names, jargon, and technical terms
- **Smarter insertion:** match boundary spacing, common sentence-starter casing,
  and punctuation to the text around the cursor

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
- live previews (~70–180 ms each on an RTX 3080) use the same full encoder
  context as the final pass, and the LLM polishes them too (~50–90 ms), so
  the overlay shows the text that will be inserted. On release, previews in
  flight are aborted; when the final transcript has the same words as the
  last preview (you paused before releasing), its polish is reused and the
  LLM is skipped. Turn off "Polish the live preview" to save GPU time;
- the LLM reuses its cached system prompt (KV cache);
- prompt-lookup speculative decoding verifies copied transcript tokens in
  batches: 2.5–3.5× faster, with greedy output unchanged.

## Install

One command, no build tools needed (x86_64; Debian 13, Ubuntu 25.04,
Fedora 42, Arch, openSUSE Tumbleweed or newer):

```sh
curl -fsSL https://github.com/kuba001-pl/localwispr/releases/latest/download/install.sh | bash
```

The installer:

- picks the build: **CUDA** with an NVIDIA driver 580 or newer, otherwise
  **Vulkan** (AMD, Intel, NVIDIA; falls back to the CPU without a GPU);
- downloads it from GitHub Releases, checks its SHA-256, and installs it into
  `~/.local` (no root);
- on CUDA systems without CUDA 13 libraries, downloads them once (~400 MB) into
  localwispr's own folder. Updates don't fetch them again;
- installs the systemd user service, the settings launcher, and the GNOME Shell
  extension;
- offers the one-time permission setup (sudo): `input` group for global
  hotkeys, `/dev/uinput` for pasting outside GNOME, and Caps Lock as
  push-to-talk. Log out and back in afterwards. GNOME only loads new
  extensions at login.

It needs GTK 4.14+, libadwaita 1.6+, PipeWire and ALSA libraries, which
current desktops already have. Anything missing is listed together with the
package to install. Options go after `bash -s --`, e.g.
`… | bash -s -- --variant vulkan` (`--variant cuda|vulkan`, `--version X.Y.Z`,
`--no-service`, `--no-permissions`).

Models are downloaded automatically on first start into
`~/.local/share/localwispr/models` (resumable, SHA-256 verified). To fetch them
up front with a progress bar: `localwispr download`.

Outside GNOME, make Caps Lock inert yourself (xkb option `caps:none`; the
permission setup prints the setting for KDE, Hyprland and Sway), or bind
push-to-talk to another key in Settings.

### Updating

```sh
localwispr update            # latest release; restarts the service
localwispr update --check    # only check
localwispr update --rollback # back to the previous version
```

Or use **Settings → Models → Updates**. By default the daemon asks GitHub
once a day whether a new release exists. That is one request with nothing
sent. You get a desktop notification, and nothing installs until you ask.
Turn the check off in the same place (`[updates] check = false`).

Versions are installed side by side under `~/.local/share/localwispr/app`
and switched atomically, and the previous one is kept for `--rollback`. When
the GNOME extension changes, the update tells you to log out and back in.
Until then, the old extension keeps working with the new daemon.

### Uninstall

```sh
localwispr uninstall                     # keeps config and models
localwispr uninstall --undo-permissions  # also reverts the permission setup
```

### Building from source

Needed for other architectures, CPU-only builds, or development. Build
requirements: Git, Rust 1.92+, a C/C++ compiler, CMake, Clang/libclang,
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

```sh
git clone https://github.com/kuba001-pl/localwispr.git
cd localwispr
./install.sh --from-source           # CUDA (needs the CUDA toolkit)
./install.sh --from-source --vulkan  # Vulkan (needs Vulkan headers, glslc, SPIRV-Headers)
./install.sh --from-source --cpu     # CPU only
```

`install.sh` finds `nvcc` on `PATH`, in `/opt/cuda` or in `/usr/local/cuda`
(or set `CUDACXX`). CUDA kernels are built for the GPU in the build machine.
Set `CMAKE_CUDA_ARCHITECTURES` (e.g. `"86;89"`) to build for others. A source
install is laid out like a release, so `localwispr update` can later replace it
with a prebuilt release; it asks first.

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

### Context-aware dictation

Two independent switches in **Settings → Output** are enabled by default:

- **Context-aware recognition** captures nearby text when recording starts and
  supplies bounded vocabulary hints to Whisper, for live previews and the final
  transcript. Whisper's tokenizer enforces the model's prompt budget, reserving
  its bilingual hint and prioritizing nearby terms and your configured vocabulary
  prompt over distant terms. Hints can help recognition; they do not guarantee a
  particular spelling.
- **Smarter text insertion** requests cursor context again before pasting. It adds
  spaces between words, avoids extra spaces beside existing whitespace or closing
  punctuation, and suppresses redundant final periods before sentence
  continuations. Sentence starts capitalize lowercase words; common English and
  Polish starters can become lowercase mid-sentence. Likely multiword names,
  acronyms, and mixed-case identifiers keep their casing.

These use shared desktop interfaces, with no application-name rules. On GNOME,
the companion extension first requests the compositor's committed surrounding
text through its input method; AT-SPI accessibility provides a second route for
recognition and for insertion on other desktops. GNOME insertion uses an
extension-issued token to keep the formatting snapshot bound to the focused
field and selection through clipboard preparation. Only a small window around
the focused editable field is returned. Requests exclude the
validated selection and reject protected/password fields. Focus and selection
are rechecked, and detected changes discard the snapshot; concurrent edits can
still race because context inspection and clipboard paste are not atomic.
On Wayland, the compositor can replay its most recently committed text when
queried, so the response is not proof of a new application-level snapshot.
Context stays in memory locally, is not saved to configuration or
sent to the cleanup model, and expires with the dictation. Context reads have a
short timeout and never use select/copy keystrokes or change global accessibility
settings. Focus lookup uses the application's Collection interface when available,
with a bounded tree walk as fallback, so it need not inspect every node in large
documents. You can disable recognition hints with `[transcription]
context_aware = false` and insertion adaptation with `[output] cursor_aware = false`.

After dictating, `localwispr status` reports `recognition_context` and
`insertion_context`, plus their `_source` fields. `available` means usable
context, `empty-field` means the editor was readable but supplied no surrounding
text, and fixed failure labels such as `no-active-window`, `no-focused-editor`,
`terminal-field`, `timeout`, or `changed-before-paste` (the field changed twice
before paste, so the text went in unadapted) explain the fallback. These labels never include document contents.
This avoids mistaking ordinary dictation for working cursor awareness.

The compositor route requires the updated GNOME extension to be loaded; older
extensions continue to paste normally. Recognition can still use AT-SPI, while
GNOME cursor formatting falls back to plain insertion with `unbound-extension`
status until the new handoff methods are loaded. Installing updated Shell
extension code takes effect after a fresh login: until you log out and back in
after upgrading, the bound handoff and its pre-paste protection recheck are not
active. If the cursor or field changes between the context read and the paste,
the daemon re-reads context and retries; if that fails again it inserts the text
unadapted instead of discarding it. A retry never pastes into a field that has
meanwhile become protected or a terminal (for example a password prompt that took
focus); that dictation is reported as not inserted. Neither route can inspect an
editor that exposes no surrounding-text or accessibility interface. Terminals,
inaccessible browser/editor fields, ambiguous selections, and unavailable
desktop services fall back to ordinary
dictation. Inside words and recognizable URLs, paths, and email addresses,
insertion preserves literal transcript text without adding separator spaces.
Clipboard paste still follows application behavior; context inspection and paste
are separate operations, so keep the intended field focused until insertion.
The updated GNOME extension cancels pending paste when the daemon cancels that
request or disappears, and rejects focus changes detected while preparing paste.
Cancellation after the paste keys have been sent cannot undo inserted text;
clipboard restoration still waits through the configured consumption delay so a
queued paste does not receive the previous clipboard. Cancelling before any keys
restores available backups and clears injected sources that have no backup,
provided a newer user copy has not taken ownership.
Older extensions still support ordinary dictation but lack remote cancellation;
load the updated extension at the next login to enable that guarantee.

Vocabulary extraction prioritizes names and identifiers over common prose.
Cleanup preserves paragraph boundaries, indentation, code spans, recognized
assignment syntax, and bracketed arguments. Its default guard checks names, numeric values,
negation anchors, and content-word retention before accepting model rewrites.
It can decline a useful spelling repair rather than accept an unsupported
word substitution; explicit custom rewrite instructions keep their existing
behavior. These checks do not establish arbitrary semantic equivalence.
Formatting preserves code fences, inline code, and recognizable unfenced syntax. It adjusts the inserted
fragment; it does not remove a period already in the destination to merge a
previous dictation into the current sentence. Single-token selections inside a
word preserve the word join. Without the selected text, replacing a word fragment
and replacing selected separator spaces can be ambiguous; the formatter cannot
always distinguish them. Recognition skips digital silence even when VAD is off,
and repeated decoder sentences are collapsed without reflowing paragraphs/code.

Reproducible quality checks live in `benchmarks/`: the frozen recognition corpus
has 270 unique synthetic sentences, 42 company/product names, misleading-context
and ordinary-word controls, and separate development/held-out sentence sets.
A public 73-clip, one-speaker natural-speech negative-control set complements it.
The reports retain regressions alongside gains; neither corpus establishes
compatibility with every app or accuracy for every speaker.

For a local smoke test, try a GTK text field with `We use Kubernetes and
PostgreSQL.`, place the cursor in nearby prose, and dictate those terms. Then
test inserting `the new API` between `We use ` and ` daily.`, replacing a selected
phrase, dictating after a sentence-ending period, and inserting before a comma
or closing bracket. Compare with each switch disabled. Desktop/app behavior
requires real-session testing; automated fixtures do not establish compatibility
with every editor.

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
  `bash ~/.local/share/localwispr/app/current/share/localwispr/setup-permissions.sh`,
  then log out and back in).
- **Caps Lock still toggles capitals** — GNOME: the script sets
  `caps:none`. Other desktops: set that xkb option in your compositor config.
- **Text doesn't appear on GNOME** — the extension must be enabled
  (`gnome-extensions info localwispr@localwispr.github.io`).
- **Paste doesn't work in a specific app** — try Ctrl+V or Ctrl+Shift+V in
  Settings → Output.
- **No overlay / text not inserted on another desktop** — `localwispr status`
  shows the detected `frontend` and `injection`. Insertion outside GNOME needs
  `/dev/uinput` access (the permission setup above) and a compositor with
  `ext-data-control` or `wlr-data-control`.
- **Service doesn't start at login** — see [Autostart](#autostart).

When reporting a bug, include your desktop and version, the output of
`localwispr status`, and `journalctl --user -u localwispr -b` (it may contain
dictated text; trim it before posting).

## Privacy

Speech, transcripts and cleanup never leave your machine. There is no
localwispr server. Network access is limited to downloading models from
Hugging Face and releases from GitHub, plus the daily update check (one
request to github.com that sends nothing; it can be turned off in Settings). To insert text, localwispr
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

For the context-recognition and insertion changes, see the [verification report](benchmarks/verification/REPORT.md), including benchmark results, known limitations, and local acceptance steps.

```sh
cargo test --workspace                       # unit tests (DSP, resampler, guard, hotkeys, config…)
cargo test --workspace --no-default-features # without CUDA
node gnome-extension/tests/extension.test.cjs
cargo build --release --workspace
bash packaging/test-install.sh dist 0.2.0    # installer test against locally built bundles
```

Releases: bump `version` in `Cargo.toml`, then push a `vX.Y.Z` tag.
`.github/workflows/release.yml` builds the CUDA and Vulkan bundles on Debian 13,
installs them on several distributions, and publishes them with `install.sh`
and `SHA256SUMS`.

## License

MIT, see [LICENSE](LICENSE). Vendored whisper-rs is under the Unlicense and
whisper.cpp under MIT (see [vendor/README.md](vendor/README.md)).
