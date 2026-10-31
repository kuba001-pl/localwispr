# Local whisper-rs patches

Vendored from crates.io: whisper-rs 0.16.0 and whisper-rs-sys 0.15.0. Both Rust crates are released under the Unlicense (`LICENSE` in each directory; the sys crate ships none upstream, so a copy of the Unlicense text was added). The bundled whisper.cpp source is MIT (`whisper-rs-sys-0.15.0/whisper.cpp/LICENSE`).

- FullParams owns language, initial-prompt, and VAD-path CStrings through Arc so replacement, cloning, and dropping are safe and allocations are released. Regression tests cover replacement and final-owner destruction.
- Adds whisper_lang_auto_detect_with_state_abort and the unsafe Rust wrapper lang_detect_with_abort. Both language-detection encoder and decoder receive the abort callback, with checks between stages. CPU graph evaluation polls it internally; scheduler residual boundaries allow GPU cancellation. Callback pointers are cleared before returning. Existing entry points retain their signatures. whisper_full_with_state also uses the cancellable detector.
- whisper_lang_auto_detect_with_state_abort encodes with the full trained audio context and restores the state's previous value afterwards. exp_n_audio_ctx otherwise persists from the last whisper_full call; after a reduced-context partial decode, detection was confidently wrong (Polish speech scored en 0.92, pl 0.00), which made Whisper translate.
- The sys build recopies modified source before CMake runs and tracks the source tree. Pregenerated bindings include the additive entry point.
- `WhisperState::lang_detect` and `lang_detect_with_abort` take `&mut self`. Detection mutates native state, and `WhisperState` is `Sync`, so a shared reference allowed data races from safe code.

The application deliberately bypasses upstream set_abort_callback_safe using an explicit C callback and stack-owned data. VAD is performed separately and speech segments are joined before WhisperState::full, because that path does not preprocess built-in VAD.

The vendored wrapper is intentionally outside the application workspace. To run its private parameter regressions, copy that crate to a temporary standalone Cargo workspace, add a crates.io patch pointing whisper-rs-sys at the vendored sys crate, then run `cargo test --no-default-features --lib owned_string_regressions` there. Application regressions run with `cargo test --no-default-features --workspace`.
