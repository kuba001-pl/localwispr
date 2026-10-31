//! D-Bus contract between the daemon and its frontends.
//!
//! The daemon owns `org.localwispr.Daemon` and broadcasts HUD signals
//! (state, levels, live text). Any frontend — the layer-shell HUD process or
//! the GNOME Shell extension — just subscribes; the daemon never needs to
//! know how they draw. The GNOME extension additionally exports
//! `org.localwispr.Shell` for in-compositor clipboard + paste.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use zbus::object_server::SignalEmitter;
use zbus::{interface, proxy};

pub const DAEMON_NAME: &str = "org.localwispr.Daemon";
pub const DAEMON_PATH: &str = "/org/localwispr/Daemon";
pub const SHELL_NAME: &str = "org.localwispr.Shell";

/// Requests from D-Bus clients to the engine.
pub enum Request {
    Reload,
    OpenSettings,
    Start,
    Stop,
    Toggle,
    Cancel,
    CaptureHotkey(oneshot::Sender<Vec<String>>),
    CancelCapture,
    Download(String),
}

pub type Status = std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>;

pub struct DaemonIface {
    pub tx: mpsc::UnboundedSender<Request>,
    pub status: Status,
    /// zbus dispatches methods on its own executor, outside Tokio; anything
    /// needing the Tokio timer or blocking pool must run on this handle.
    pub rt: tokio::runtime::Handle,
}

#[interface(name = "org.localwispr.Daemon1")]
impl DaemonIface {
    /// Re-read config.toml and apply changes (models, mic, hotkeys…).
    async fn reload_config(&self) {
        let _ = self.tx.send(Request::Reload);
    }

    async fn open_settings(&self) {
        let _ = self.tx.send(Request::OpenSettings);
    }

    /// Manual control, e.g. for compositor keybindings instead of evdev.
    async fn start(&self) {
        let _ = self.tx.send(Request::Start);
    }

    async fn stop(&self) {
        let _ = self.tx.send(Request::Stop);
    }

    async fn toggle(&self) {
        let _ = self.tx.send(Request::Toggle);
    }

    async fn cancel(&self) {
        let _ = self.tx.send(Request::Cancel);
    }

    /// Waits for the user to press and release a key combination and returns
    /// its evdev key names. Empty on timeout.
    async fn capture_hotkey(&self, timeout_ms: u32) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Request::CaptureHotkey(tx));
        let timeout = Duration::from_millis(timeout_ms.clamp(1000, 20_000) as u64);
        // The timer must be created on the Tokio runtime, not just awaited there.
        match self
            .rt
            .spawn(async move { tokio::time::timeout(timeout, rx).await })
            .await
        {
            Ok(Ok(Ok(keys))) => keys,
            _ => {
                let _ = self.tx.send(Request::CancelCapture);
                Vec::new()
            }
        }
    }

    async fn download_model(&self, id: String) {
        let _ = self.tx.send(Request::Download(id));
    }

    async fn get_status(&self) -> HashMap<String, String> {
        self.status.lock().unwrap().clone()
    }

    /// Overlay appearance for frontends: (frontend, bands, bottom_margin,
    /// show_live_text, opacity). frontend is "auto" | "layer-shell" | "gnome" | "none";
    /// a frontend must stay hidden unless it is the selected one (or "auto").
    async fn get_hud_config(&self) -> (String, u32, u32, bool, f64) {
        let h = crate::config::Config::load_or_default().hud;
        let fe = match h.frontend {
            crate::config::FrontendChoice::Auto => "auto",
            crate::config::FrontendChoice::LayerShell => "layer-shell",
            crate::config::FrontendChoice::Gnome => "gnome",
            crate::config::FrontendChoice::None => "none",
        };
        (
            fe.to_owned(),
            h.bands,
            h.bottom_margin,
            h.show_live_text,
            h.opacity,
        )
    }

    /// (id, name, is_default) for every capture device.
    async fn list_microphones(&self) -> Vec<(String, String, bool)> {
        self.rt
            .spawn_blocking(|| {
                crate::audio::list_input_devices()
                    .into_iter()
                    .map(|d| (d.id, d.name, d.is_default))
                    .collect()
            })
            .await
            .unwrap_or_default()
    }

    /// "loading" | "idle" | "recording" | "processing"
    #[zbus(signal)]
    pub async fn state_changed(emitter: &SignalEmitter<'_>, state: &str) -> zbus::Result<()>;

    /// Visualiser band levels in 0..1, ~60 Hz while recording.
    #[zbus(signal)]
    pub async fn levels(emitter: &SignalEmitter<'_>, bands: Vec<f64>) -> zbus::Result<()>;

    /// Live (unpolished) transcript of the current recording.
    #[zbus(signal)]
    pub async fn partial_text(emitter: &SignalEmitter<'_>, text: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn download_progress(
        emitter: &SignalEmitter<'_>,
        model: &str,
        done: u64,
        total: u64,
    ) -> zbus::Result<()>;

    /// Emitted after a download is verified and installed (ok=true) or has
    /// definitively failed (ok=false, error set).
    #[zbus(signal)]
    pub async fn download_finished(
        emitter: &SignalEmitter<'_>,
        model: &str,
        ok: bool,
        error: &str,
    ) -> zbus::Result<()>;

    /// level: "info" | "warning" | "error"
    #[zbus(signal)]
    pub async fn notice(
        emitter: &SignalEmitter<'_>,
        level: &str,
        message: &str,
    ) -> zbus::Result<()>;

    /// Lets the GNOME extension raise the settings window (focus-stealing prevention).
    #[zbus(signal)]
    pub async fn settings_requested(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.localwispr.Daemon1",
    default_service = "org.localwispr.Daemon",
    default_path = "/org/localwispr/Daemon"
)]
pub trait Daemon {
    fn reload_config(&self) -> zbus::Result<()>;
    fn open_settings(&self) -> zbus::Result<()>;
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
    fn cancel(&self) -> zbus::Result<()>;
    fn capture_hotkey(&self, timeout_ms: u32) -> zbus::Result<Vec<String>>;
    fn download_model(&self, id: &str) -> zbus::Result<()>;
    fn get_status(&self) -> zbus::Result<HashMap<String, String>>;
    fn get_hud_config(&self) -> zbus::Result<(String, u32, u32, bool, f64)>;
    fn list_microphones(&self) -> zbus::Result<Vec<(String, String, bool)>>;

    #[zbus(signal)]
    fn state_changed(&self, state: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn levels(&self, bands: Vec<f64>) -> zbus::Result<()>;
    #[zbus(signal)]
    fn partial_text(&self, text: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn download_progress(&self, model: &str, done: u64, total: u64) -> zbus::Result<()>;
    #[zbus(signal)]
    fn download_finished(&self, model: &str, ok: bool, error: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notice(&self, level: &str, message: &str) -> zbus::Result<()>;
}

/// Exported by the GNOME Shell extension.
#[proxy(
    interface = "org.localwispr.Shell1",
    default_service = "org.localwispr.Shell",
    default_path = "/org/localwispr/Shell"
)]
pub trait Shell {
    /// Sets CLIPBOARD+PRIMARY to `text`, presses `chord` with a Clutter
    /// virtual keyboard, and (optionally) restores the previous clipboard
    /// after `restore_delay_ms`.
    fn inject(
        &self,
        text: &str,
        chord: &str,
        restore: bool,
        restore_delay_ms: u32,
    ) -> zbus::Result<bool>;
    fn version(&self) -> zbus::Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal executor with no Tokio context, like zbus's.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        struct Unpark(std::thread::Thread);
        impl std::task::Wake for Unpark {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::sync::Arc::new(Unpark(std::thread::current())).into();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut cx) {
                return out;
            }
            std::thread::park();
        }
    }

    #[test]
    fn capture_hotkey_runs_outside_tokio() {
        // zbus calls methods on its own executor thread, without a Tokio context.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let iface = DaemonIface {
            tx,
            status: Status::default(),
            rt: rt.handle().clone(),
        };
        rt.spawn(async move {
            if let Some(Request::CaptureHotkey(reply)) = rx.recv().await {
                let _ = reply.send(vec!["KEY_ESC".into()]);
            }
        });
        let keys = std::thread::spawn(move || block_on(iface.capture_hotkey(1000)))
            .join()
            .unwrap();
        assert_eq!(keys, ["KEY_ESC"]);
    }
}
