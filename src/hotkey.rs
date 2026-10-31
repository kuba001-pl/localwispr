//! Global hotkeys read straight from evdev (`/dev/input/event*`).
//!
//! Works under any compositor because it bypasses it entirely. One blocking
//! reader thread per device; a scanner thread picks up hot-plugged devices.
//! Requires read access to the event nodes (see `setup-permissions.sh`).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use evdev::{EventSummary, KeyCode};
use tokio::sync::mpsc;

use crate::inject::VIRTUAL_DEVICE_NAME;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKey {
    pub device: u64,
    pub code: u16,
    pub pressed: bool,
}

impl RawKey {
    // Linux input keycodes never reach u16::MAX. A sentinel preserves the
    // event API while carrying an atomic release of all keys for one device.
    const DISCONNECTED: u16 = u16::MAX;

    fn disconnected(device: u64) -> Self {
        Self {
            device,
            code: Self::DISCONNECTED,
            pressed: false,
        }
    }
}

/// Starts listening on all keyboards/mice. Fails only if *no* device could be
/// opened (almost always a permissions problem). Returns how many devices were
/// unreadable, so partial access (e.g. a mouse with a uaccess rule but not the
/// keyboard) can be reported instead of silently ignoring the hotkey.
pub fn spawn_listener(tx: mpsc::UnboundedSender<RawKey>) -> Result<usize> {
    let open: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let (opened, denied) = scan(&open, &tx);
    if opened == 0 {
        if denied > 0 {
            bail!(
                "no permission to read /dev/input/event* — run ./setup-permissions.sh and log in again"
            );
        }
        tracing::warn!("no keyboard found yet; waiting for one to be plugged in");
    }
    std::thread::Builder::new()
        .name("hotkey-scan".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if tx.is_closed() {
                    return;
                }
                scan(&open, &tx);
            }
        })?;
    Ok(denied)
}

fn scan(open: &Arc<Mutex<HashSet<PathBuf>>>, tx: &mpsc::UnboundedSender<RawKey>) -> (usize, usize) {
    let (mut opened, mut denied) = (0, 0);
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || open.lock().unwrap().contains(&path) {
            continue;
        }
        let dev = match evdev::Device::open(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                denied += 1;
                continue;
            }
            Err(_) => continue,
        };
        if dev.name() == Some(VIRTUAL_DEVICE_NAME) || !has_useful_keys(&dev) {
            continue;
        }
        tracing::debug!(
            "listening on {} ({})",
            path.display(),
            dev.name().unwrap_or("?")
        );
        open.lock().unwrap().insert(path.clone());
        let reader_tx = tx.clone();
        let reader_open = open.clone();
        let reader_path = path.clone();
        match std::thread::Builder::new()
            .name("hotkey-dev".into())
            .spawn(move || {
                read_device(dev, &reader_tx);
                reader_open.lock().unwrap().remove(&reader_path);
            }) {
            Ok(_) => opened += 1,
            Err(error) => {
                open.lock().unwrap().remove(&path);
                tracing::warn!(
                    "could not start input reader for {}: {error}",
                    path.display()
                );
            }
        }
    }
    (opened, denied)
}

fn has_useful_keys(dev: &evdev::Device) -> bool {
    dev.supported_keys().is_some_and(useful_keys)
}

fn useful_keys(keys: &evdev::AttributeSetRef<KeyCode>) -> bool {
    keys.iter().any(|key| {
        key.0 < KeyCode::BTN_0.0 || (KeyCode::BTN_LEFT.0..=KeyCode::BTN_TASK.0).contains(&key.0)
    })
}

static NEXT_DEVICE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn read_device(mut dev: evdev::Device, tx: &mpsc::UnboundedSender<RawKey>) {
    let device = NEXT_DEVICE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(keys) = dev.get_key_state() {
        for code in keys.iter() {
            let _ = tx.send(RawKey {
                device,
                code: code.0,
                pressed: true,
            });
        }
    }
    loop {
        let events = match dev.fetch_events() {
            Ok(ev) => ev,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                let _ = tx.send(RawKey::disconnected(device));
                return;
            }
        };
        for ev in events {
            if let EventSummary::Key(_, code, value) = ev.destructure() {
                // value 2 = autorepeat; irrelevant for chords.
                if value != 0 && value != 1 {
                    continue;
                }
                if tx
                    .send(RawKey {
                        device,
                        code: code.0,
                        pressed: value == 1,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- matching --

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord(Vec<u16>);

impl Chord {
    pub fn parse(names: &[String]) -> Result<Self> {
        let mut codes = Vec::with_capacity(names.len());
        for n in names {
            match n.parse::<KeyCode>() {
                Ok(k) => codes.push(k.0),
                Err(_) => bail!("unknown key name `{n}`"),
            }
        }
        if codes.is_empty() {
            bail!("empty hotkey");
        }
        // Left/right modifiers are interchangeable inside multi-key chords.
        if codes.len() > 1 {
            for c in &mut codes {
                *c = normalize_modifier(*c);
            }
        }
        Ok(Self(codes))
    }

    fn is_held(&self, pressed: &BTreeSet<u16>, normalized: &BTreeSet<u16>) -> bool {
        let set = if self.0.len() > 1 {
            normalized
        } else {
            pressed
        };
        self.0.iter().all(|c| set.contains(c))
    }

    fn involves(&self, code: u16) -> bool {
        let n = normalize_modifier(code);
        self.0
            .iter()
            .any(|&c| c == code || (self.0.len() > 1 && c == n))
    }
}

fn normalize_modifier(code: u16) -> u16 {
    match KeyCode(code) {
        KeyCode::KEY_RIGHTSHIFT => KeyCode::KEY_LEFTSHIFT.0,
        KeyCode::KEY_RIGHTCTRL => KeyCode::KEY_LEFTCTRL.0,
        KeyCode::KEY_RIGHTMETA => KeyCode::KEY_LEFTMETA.0,
        _ => code,
    }
}

fn is_modifier(code: u16) -> bool {
    matches!(
        KeyCode(code),
        KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    PttDown,
    PttUp,
    OpenSettings,
    Cancel,
    /// Result of a capture request: key names in press order (modifiers first).
    Captured(Vec<String>),
}

pub struct Matcher {
    ptt: Chord,
    settings: Option<Chord>,
    cancel: Option<Chord>,
    devices: BTreeSet<(u64, u16)>,
    pressed: BTreeSet<u16>,
    normalized: BTreeSet<u16>,
    modifiers_held: Arc<AtomicBool>,
    ptt_active: bool,
    capture: Option<Vec<u16>>,
}

impl Matcher {
    pub fn new(cfg: &crate::config::Hotkeys) -> Result<Self> {
        Ok(Self {
            ptt: Chord::parse(&cfg.push_to_talk)?,
            settings: Chord::parse(&cfg.open_settings).ok(),
            cancel: Chord::parse(&cfg.cancel).ok(),
            devices: BTreeSet::new(),
            pressed: BTreeSet::new(),
            normalized: BTreeSet::new(),
            modifiers_held: Arc::default(),
            ptt_active: false,
            capture: None,
        })
    }

    /// Swap in new bindings, keeping the physical key state.
    pub fn reconfigure(&mut self, cfg: &crate::config::Hotkeys) -> Result<()> {
        let mut next = Self::new(cfg)?;
        next.devices = std::mem::take(&mut self.devices);
        next.pressed = std::mem::take(&mut self.pressed);
        next.normalized = std::mem::take(&mut self.normalized);
        next.modifiers_held = self.modifiers_held.clone();
        next.ptt_active = next.ptt.is_held(&next.pressed, &next.normalized);
        next.capture = self.capture.take();
        *self = next;
        Ok(())
    }

    pub fn start_capture(&mut self) {
        self.capture = Some(Vec::new());
    }

    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    pub fn modifiers_held(&self) -> Arc<AtomicBool> {
        self.modifiers_held.clone()
    }

    pub fn on_key(&mut self, key: RawKey) -> Option<Action> {
        if key.code == RawKey::DISCONNECTED {
            self.devices.retain(|(device, _)| *device != key.device);
        } else if key.pressed {
            if !self.devices.insert((key.device, key.code)) {
                return None;
            }
        } else {
            self.devices.remove(&(key.device, key.code));
        }
        self.pressed = self.devices.iter().map(|(_, code)| *code).collect();
        self.normalized = self
            .pressed
            .iter()
            .map(|code| normalize_modifier(*code))
            .collect();
        self.modifiers_held.store(
            self.pressed.iter().any(|&code| is_modifier(code)),
            Ordering::Release,
        );

        if let Some(cap) = self.capture.as_mut() {
            if key.pressed && !cap.contains(&key.code) {
                cap.push(key.code);
            } else if !key.pressed && !cap.is_empty() && self.pressed.is_empty() {
                let mut keys = self.capture.take().unwrap();
                keys.sort_by_key(|&c| !is_modifier(c)); // stable: modifiers first
                return Some(Action::Captured(
                    keys.into_iter()
                        .map(|c| format!("{:?}", KeyCode(c)))
                        .collect(),
                ));
            }
            return None;
        }

        let held = self.ptt.is_held(&self.pressed, &self.normalized);
        if held != self.ptt_active {
            self.ptt_active = held;
            return Some(if held { Action::PttDown } else { Action::PttUp });
        }
        if key.pressed {
            for (chord, action) in [
                (&self.settings, Action::OpenSettings),
                (&self.cancel, Action::Cancel),
            ] {
                if let Some(ch) = chord
                    && ch.involves(key.code)
                    && ch.is_held(&self.pressed, &self.normalized)
                {
                    return Some(action);
                }
            }
        }
        None
    }
}

/// Waits (up to `timeout`) for physically held Ctrl/Alt/Super/Shift to be
/// released so they don't combine with the paste chord. Pastes anyway after
/// the timeout: a missed key release (e.g. a device grabbed by another
/// program) must not block every later dictation. Fails only on cancellation.
pub async fn wait_for_modifiers(
    held: &AtomicBool,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        anyhow::ensure!(!cancelled.load(Ordering::Acquire), "dictation cancelled");
        if !held.load(Ordering::Acquire) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            if !timeout.is_zero() {
                tracing::warn!("modifier keys still reported as held; pasting anyway");
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Human-friendly label for an evdev key name (`KEY_CAPSLOCK` → `Caps Lock`).
pub fn pretty_key(name: &str) -> String {
    let base = name.trim_start_matches("KEY_").trim_start_matches("BTN_");
    let special = match base {
        "CAPSLOCK" => "Caps Lock",
        "LEFTMETA" | "RIGHTMETA" => "Super",
        "LEFTSHIFT" => "Shift",
        "RIGHTSHIFT" => "Right Shift",
        "LEFTCTRL" => "Ctrl",
        "RIGHTCTRL" => "Right Ctrl",
        "LEFTALT" => "Alt",
        "RIGHTALT" => "AltGr",
        "SEMICOLON" => ";",
        "APOSTROPHE" => "'",
        "GRAVE" => "`",
        "COMMA" => ",",
        "DOT" => ".",
        "SLASH" => "/",
        "BACKSLASH" => "\\",
        "LEFTBRACE" => "[",
        "RIGHTBRACE" => "]",
        "MINUS" => "-",
        "EQUAL" => "=",
        "ESC" => "Esc",
        "SPACE" => "Space",
        "SIDE" => "Mouse Back",
        "EXTRA" => "Mouse Forward",
        "MIDDLE" => "Middle Click",
        "SCROLLLOCK" => "Scroll Lock",
        "PAUSE" => "Pause",
        "INSERT" => "Insert",
        "COMPOSE" => "Menu",
        _ => "",
    };
    if !special.is_empty() {
        return special.to_owned();
    }
    let mut out = String::with_capacity(base.len());
    for (i, ch) in base.chars().enumerate() {
        if i == 0 {
            out.push(ch)
        } else {
            out.extend(ch.to_lowercase())
        }
    }
    out
}

pub fn pretty_chord(names: &[String]) -> String {
    names
        .iter()
        .map(|n| pretty_key(n))
        .collect::<Vec<_>>()
        .join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Hotkeys;

    fn k(code: KeyCode, pressed: bool) -> RawKey {
        RawKey {
            device: 0,
            code: code.0,
            pressed,
        }
    }

    #[test]
    fn keyboard_and_plain_mouse_are_useful_but_buttonless_devices_are_not() {
        for key in [
            KeyCode::KEY_A,
            KeyCode::KEY_RIGHTMETA,
            KeyCode::KEY_F13,
            KeyCode::BTN_MIDDLE,
            KeyCode::BTN_SIDE,
            KeyCode::BTN_EXTRA,
        ] {
            let mut keys = evdev::AttributeSet::new();
            keys.insert(key);
            assert!(useful_keys(&keys), "{key:?}");
        }
        assert!(!useful_keys(&evdev::AttributeSet::new()));
        let mut keys = evdev::AttributeSet::new();
        keys.insert(KeyCode::BTN_TOUCH);
        assert!(!useful_keys(&keys));
    }

    #[test]
    fn physical_modifiers_survive_reconfigure_and_other_device_releases() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        let held = m.modifiers_held();
        m.on_key(k(KeyCode::KEY_CAPSLOCK, true));
        assert!(!held.load(Ordering::Acquire));
        for code in [
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_RIGHTCTRL,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_RIGHTALT,
            KeyCode::KEY_LEFTMETA,
            KeyCode::KEY_RIGHTMETA,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_RIGHTSHIFT,
        ] {
            for device in [1, 2] {
                m.on_key(RawKey {
                    device,
                    code: code.0,
                    pressed: true,
                });
            }
            assert!(held.load(Ordering::Acquire));
            m.reconfigure(&Hotkeys::default()).unwrap();
            m.on_key(RawKey::disconnected(1));
            assert!(held.load(Ordering::Acquire));
            m.on_key(RawKey::disconnected(2));
            assert!(!held.load(Ordering::Acquire));
        }
    }

    #[tokio::test]
    async fn modifier_wait_observes_release_cancellation_and_timeout() {
        let held = AtomicBool::new(true);
        let cancelled = AtomicBool::new(false);
        let release = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            held.store(false, Ordering::Release);
        };
        let (result, ()) = tokio::join!(
            wait_for_modifiers(&held, &cancelled, Duration::from_secs(1)),
            release
        );
        result.unwrap();
        // A modifier that never reports its release does not block pasting.
        held.store(true, Ordering::Release);
        wait_for_modifiers(&held, &cancelled, Duration::from_millis(30))
            .await
            .unwrap();
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancelled.store(true, Ordering::Release);
        };
        let (result, ()) = tokio::join!(
            wait_for_modifiers(&held, &cancelled, Duration::from_secs(1)),
            cancel
        );
        assert_eq!(result.unwrap_err().to_string(), "dictation cancelled");
        held.store(false, Ordering::Release);
        assert!(
            wait_for_modifiers(&held, &cancelled, Duration::ZERO)
                .await
                .is_err()
        );
    }

    #[test]
    fn ptt_single_key() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        assert_eq!(
            m.on_key(k(KeyCode::KEY_CAPSLOCK, true)),
            Some(Action::PttDown)
        );
        assert_eq!(m.on_key(k(KeyCode::KEY_A, true)), None);
        assert_eq!(
            m.on_key(k(KeyCode::KEY_CAPSLOCK, false)),
            Some(Action::PttUp)
        );
        assert_eq!(m.on_key(k(KeyCode::KEY_A, false)), None);
    }

    #[test]
    fn settings_chord_with_either_modifier_side() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        m.on_key(k(KeyCode::KEY_RIGHTMETA, true));
        m.on_key(k(KeyCode::KEY_RIGHTSHIFT, true));
        assert_eq!(
            m.on_key(k(KeyCode::KEY_SEMICOLON, true)),
            Some(Action::OpenSettings)
        );
        // Holding the chord does not re-fire on unrelated keys.
        assert_eq!(m.on_key(k(KeyCode::KEY_A, true)), None);
    }

    #[test]
    fn single_key_ptt_is_side_exact() {
        let cfg = Hotkeys {
            push_to_talk: vec!["KEY_RIGHTCTRL".into()],
            ..Hotkeys::default()
        };
        let mut m = Matcher::new(&cfg).unwrap();
        assert_eq!(m.on_key(k(KeyCode::KEY_LEFTCTRL, true)), None);
        assert_eq!(
            m.on_key(k(KeyCode::KEY_RIGHTCTRL, true)),
            Some(Action::PttDown)
        );
    }

    #[test]
    fn multi_key_ptt_releases_on_any_key() {
        let cfg = Hotkeys {
            push_to_talk: vec!["KEY_LEFTCTRL".into(), "KEY_SPACE".into()],
            ..Hotkeys::default()
        };
        let mut m = Matcher::new(&cfg).unwrap();
        assert_eq!(m.on_key(k(KeyCode::KEY_RIGHTCTRL, true)), None);
        assert_eq!(m.on_key(k(KeyCode::KEY_SPACE, true)), Some(Action::PttDown));
        assert_eq!(
            m.on_key(k(KeyCode::KEY_RIGHTCTRL, false)),
            Some(Action::PttUp)
        );
    }

    #[test]
    fn capture_orders_modifiers_first() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        m.start_capture();
        assert_eq!(m.on_key(k(KeyCode::KEY_F9, true)), None);
        assert_eq!(m.on_key(k(KeyCode::KEY_LEFTCTRL, true)), None);
        assert_eq!(m.on_key(k(KeyCode::KEY_F9, false)), None);
        assert_eq!(
            m.on_key(k(KeyCode::KEY_LEFTCTRL, false)),
            Some(Action::Captured(vec![
                "KEY_LEFTCTRL".into(),
                "KEY_F9".into()
            ]))
        );
        assert!(m.capture.is_none());
        // PTT not triggered during capture.
        assert_eq!(
            m.on_key(k(KeyCode::KEY_CAPSLOCK, true)),
            Some(Action::PttDown)
        );
    }

    #[test]
    fn same_key_on_two_devices_stays_held_until_last_release() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        let key = |device, pressed| RawKey {
            device,
            code: KeyCode::KEY_CAPSLOCK.0,
            pressed,
        };
        assert_eq!(m.on_key(key(1, true)), Some(Action::PttDown));
        assert_eq!(m.on_key(key(2, true)), None);
        assert_eq!(m.on_key(key(2, false)), None);
        assert_eq!(m.on_key(key(1, false)), Some(Action::PttUp));
    }

    #[test]
    fn disconnect_clears_only_its_device_and_ends_its_hold() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        m.on_key(RawKey {
            device: 1,
            code: KeyCode::KEY_CAPSLOCK.0,
            pressed: true,
        });
        m.on_key(RawKey {
            device: 2,
            code: KeyCode::KEY_A.0,
            pressed: true,
        });
        assert_eq!(m.on_key(RawKey::disconnected(1)), Some(Action::PttUp));
        assert_eq!(m.pressed, BTreeSet::from([KeyCode::KEY_A.0]));
        assert_eq!(m.on_key(RawKey::disconnected(1)), None);
        assert_eq!(m.on_key(RawKey::disconnected(2)), None);
        assert!(m.pressed.is_empty());
    }

    #[test]
    fn disconnect_does_not_release_another_keyboard_ptt() {
        let mut m = Matcher::new(&Hotkeys::default()).unwrap();
        for device in [1, 2] {
            m.on_key(RawKey {
                device,
                code: KeyCode::KEY_CAPSLOCK.0,
                pressed: true,
            });
        }
        assert_eq!(m.on_key(RawKey::disconnected(1)), None);
        assert_eq!(m.on_key(RawKey::disconnected(2)), Some(Action::PttUp));
    }

    #[test]
    fn pretty() {
        assert_eq!(
            pretty_chord(&Hotkeys::default().open_settings),
            "Super + Shift + ;"
        );
        assert_eq!(pretty_key("KEY_F9"), "F9");
        assert_eq!(pretty_key("KEY_CAPSLOCK"), "Caps Lock");
    }
}
