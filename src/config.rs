//! Persistent user configuration (`~/.config/localwispr/config.toml`).
//!
//! Every field has a default so partial or older config files keep working.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "localwispr";

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join(APP_ID)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join(APP_ID)
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub hotkeys: Hotkeys,
    pub audio: Audio,
    pub transcription: Transcription,
    pub cleanup: Cleanup,
    pub output: Output,
    pub hud: Hud,
    pub updates: Updates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    /// Once a day, ask GitHub whether a newer release exists (one request to
    /// github.com, nothing is sent). Updates are never installed unasked.
    pub check: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self { check: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PttMode {
    /// Record while the key is held.
    Hold,
    /// Press once to start, again to stop.
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// evdev key names, all of which must be held (e.g. `["KEY_CAPSLOCK"]`).
    pub push_to_talk: Vec<String>,
    pub mode: PttMode,
    pub open_settings: Vec<String>,
    /// Discards the current recording.
    pub cancel: Vec<String>,
    /// Presses shorter than this are treated as accidental and ignored.
    pub min_hold_ms: u32,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            push_to_talk: vec!["KEY_CAPSLOCK".into()],
            mode: PttMode::Hold,
            open_settings: vec![
                "KEY_LEFTMETA".into(),
                "KEY_LEFTSHIFT".into(),
                "KEY_SEMICOLON".into(),
            ],
            cancel: vec!["KEY_ESC".into()],
            min_hold_ms: 250,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// cpal device id (`"pipewire:…"` / `"alsa:…"`); empty = system default.
    pub device: String,
    /// Keep the microphone stream open between dictations so the first
    /// syllable is never clipped (pre-roll). Off by default: the mic is only
    /// opened while the dictation key is held.
    pub keep_warm: bool,
    pub preroll_ms: u32,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            device: String::new(),
            keep_warm: false,
            preroll_ms: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Accuracy {
    /// Greedy decoding, no temperature fallback. Lowest latency.
    Fast,
    /// Greedy decoding with temperature fallback on low-confidence output.
    Balanced,
    /// Beam search (5 beams). Slowest, most accurate.
    Best,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcription {
    /// Model id from the catalog, or an absolute path to a ggml `.bin` file.
    pub model: String,
    /// Empty = detect any language. One entry = force that language.
    /// Several entries = detect, but only choose among these.
    pub languages: Vec<String>,
    pub live_preview: bool,
    pub preview_interval_ms: u32,
    /// Silero VAD: drops silence/noise before decoding (prevents hallucinations).
    pub vad: bool,
    pub accuracy: Accuracy,
    /// Vocabulary/context hint passed to Whisper (names, jargon…).
    pub initial_prompt: String,
    pub use_gpu: bool,
    pub threads: u32,
}

impl Default for Transcription {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo".into(),
            languages: vec!["pl".into(), "en".into()],
            live_preview: true,
            preview_interval_ms: 400,
            vad: true,
            accuracy: Accuracy::Balanced,
            initial_prompt: String::new(),
            use_gpu: true,
            threads: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cleanup {
    /// Polish the transcript with a local LLM. Off = raw Whisper output.
    pub enabled: bool,
    /// Model id from the catalog, or an absolute path to a `.gguf` file.
    pub model: String,
    /// Polish live previews too, so the overlay shows the text that will be
    /// inserted. Costs one LLM pass per preview while dictating; when the
    /// final transcript matches the last preview, its polish is reused and
    /// insertion skips the LLM.
    pub live_preview: bool,
    /// Rule-based removal of unambiguous fillers ("um", "yyy"…). Applied in
    /// raw mode and whenever the LLM output is rejected.
    pub remove_fillers: bool,
    /// Reject LLM output whose word count drifts more than this fraction
    /// from the input (guards against the model "answering" the text).
    pub max_length_drift: f32,
    /// Reject LLM output that introduces more than this fraction of words
    /// not present in the input.
    pub max_new_words: f32,
    /// Utterances with fewer words skip the LLM (nothing to polish).
    pub skip_below_words: u32,
    /// Extra instructions appended to the system prompt.
    pub custom_instructions: String,
    pub gpu_layers: u32,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "qwen2.5-3b".into(),
            live_preview: true,
            remove_fillers: true,
            max_length_drift: 0.4,
            max_new_words: 0.1,
            skip_below_words: 3,
            custom_instructions: String::new(),
            gpu_layers: 999,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PasteChord {
    /// Works in GTK/Qt apps, browsers, Electron and terminals.
    ShiftInsert,
    CtrlV,
    CtrlShiftV,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Output {
    pub paste_chord: PasteChord,
    pub restore_clipboard: bool,
    /// How long to wait after pasting before the previous clipboard is restored.
    pub restore_delay_ms: u32,
    /// Append a trailing space so consecutive dictations join naturally.
    pub append_space: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            paste_chord: PasteChord::ShiftInsert,
            restore_clipboard: true,
            restore_delay_ms: 400,
            append_space: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontendChoice {
    /// GNOME extension if present, else layer-shell if supported, else none.
    Auto,
    LayerShell,
    Gnome,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hud {
    pub frontend: FrontendChoice,
    /// Number of frequency bands (bars are mirrored around the centre).
    pub bands: u32,
    pub bottom_margin: u32,
    pub show_live_text: bool,
    /// Capsule background opacity, 0–1.
    pub opacity: f64,
}

impl Default for Hud {
    fn default() -> Self {
        Self {
            frontend: FrontendChoice::Auto,
            bands: 16,
            bottom_margin: 64,
            show_live_text: true,
            opacity: 0.92,
        }
    }
}

impl Config {
    /// Loads the config, creating it with defaults on first run. A malformed
    /// file is reported and replaced by defaults in memory (never overwritten).
    pub fn load() -> Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut cfg: Config = toml::from_str(&text)
                    .with_context(|| format!("invalid config file {}", path.display()))?;
                cfg.sanitize();
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save()?;
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::error!("{e:#}; using defaults");
            Config::default()
        })
    }

    /// Atomic write (temp file + rename) so readers never see a partial file.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        std::fs::create_dir_all(config_dir())?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn sanitize(&mut self) {
        let t = &mut self.transcription;
        // whisper.cpp takes C strings; an embedded NUL would panic the worker.
        t.initial_prompt.retain(|c| c != '\0');
        t.languages.iter_mut().for_each(|l| l.retain(|c| c != '\0'));
        t.preview_interval_ms = t.preview_interval_ms.clamp(150, 5000);
        t.threads = t.threads.clamp(1, 64);
        t.languages.retain(|l| !l.trim().is_empty() && l != "auto");
        for l in &mut t.languages {
            *l = l.trim().to_lowercase();
        }
        let c = &mut self.cleanup;
        c.max_length_drift = c.max_length_drift.clamp(0.05, 5.0);
        c.max_new_words = c.max_new_words.clamp(0.0, 1.0);
        let h = &mut self.hud;
        h.bands = h.bands.clamp(4, 48);
        h.opacity = h.opacity.clamp(0.0, 1.0);
        if self.hotkeys.push_to_talk.is_empty() {
            self.hotkeys.push_to_talk = Hotkeys::default().push_to_talk;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), cfg);

        let partial: Config = toml::from_str("[cleanup]\nenabled = false\n").unwrap();
        assert!(!partial.cleanup.enabled);
        assert_eq!(partial.transcription, Transcription::default());
    }

    #[test]
    fn sanitize_languages() {
        let mut cfg = Config::default();
        cfg.transcription.languages = vec![" PL ".into(), "auto".into(), "".into()];
        cfg.sanitize();
        assert_eq!(cfg.transcription.languages, vec!["pl"]);
    }
}
