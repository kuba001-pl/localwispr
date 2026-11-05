//! The dictation engine: one async task owning all state, driven by hotkeys,
//! D-Bus requests and worker results.
//!
//! ```text
//! Loading ──models ready──► Idle ──PTT down──► Recording ──PTT up──► Processing ──► Idle
//!                                 ◄──── cancel / too short ─────┘
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};
use zbus::object_server::SignalEmitter;

use crate::audio::{self, AudioHandle, SAMPLE_RATE};
use crate::cleanup::{self, LlmClient, Outcome, Pass};
use crate::config::{Config, FrontendChoice, PttMode, Transcription};
use crate::dbus::{self, DaemonIface, Request, Status};
use crate::desktop::Capabilities;
use crate::hotkey::{self, Action, Matcher, RawKey};
use crate::inject::{Injector, Route};
use crate::insertion::{self, SurroundingText};
use crate::models::{self, ModelKind, ModelRef};
use crate::stt::{SttWorker, Transcript};

/// Partial previews look at most this far back (Whisper's window is 30 s).
const PREVIEW_WINDOW: usize = 28 * SAMPLE_RATE as usize;
const CONTEXT_TIMEOUT: Duration = Duration::from_millis(300);

/// One recording-start snapshot, shared by previews and the final pass. The
/// deadline starts when queued, rather than when the final pass asks for it.
struct ContextCapture {
    task: tokio::task::JoinHandle<()>,
    result: oneshot::Receiver<Option<SurroundingText>>,
    deadline: tokio::time::Instant,
    snapshot: Option<SurroundingText>,
    complete: bool,
}

impl ContextCapture {
    fn spawn(
        read: impl std::future::Future<Output = Option<SurroundingText>> + Send + 'static,
    ) -> Self {
        let deadline = tokio::time::Instant::now() + CONTEXT_TIMEOUT;
        let (reply, result) = oneshot::channel();
        let task = tokio::spawn(async move {
            let context = if tokio::time::Instant::now() >= deadline {
                None
            } else {
                tokio::time::timeout_at(deadline, read).await.ok().flatten()
            };
            let _ = reply.send(context);
        });
        Self {
            task,
            result,
            deadline,
            snapshot: None,
            complete: false,
        }
    }

    fn snapshot(&mut self) -> Option<&SurroundingText> {
        if !self.complete {
            match self.result.try_recv() {
                Ok(context) => {
                    self.snapshot = context;
                    self.complete = true;
                }
                Err(oneshot::error::TryRecvError::Closed) => self.complete = true,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        self.snapshot.as_ref()
    }

    async fn finish(mut self) -> Option<SurroundingText> {
        self.snapshot();
        if self.complete {
            return self.snapshot.take();
        }
        tokio::time::timeout_at(self.deadline, &mut self.result)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
    }
}

impl Drop for ContextCapture {
    fn drop(&mut self) {
        // JoinHandle::drop would detach a read from a cancelled utterance.
        self.task.abort();
    }
}

fn transcription_with_context(
    configured: &Transcription,
    context: Option<&SurroundingText>,
) -> Transcription {
    let mut settings = configured.clone();
    settings.initial_prompt = insertion::recognition_prompt(
        &configured.initial_prompt,
        context.filter(|_| configured.context_aware),
    );
    settings
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadKey {
    path: PathBuf,
    settings: LoadSettings,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadSettings {
    Speech { use_gpu: bool },
    Cleanup { gpu_layers: u32 },
}

struct Processing {
    task: tokio::task::JoinHandle<()>,
    cancelled: Arc<AtomicBool>,
    /// Only a client actually lent to this task counts as loaded.
    llm_key: Option<LoadKey>,
}

enum Event {
    ContextRead {
        utterance: u64,
        phase: &'static str,
        status: &'static str,
        source: &'static str,
    },
    Processed {
        utterance: u64,
        llm_key: Option<LoadKey>,
        llm: Option<LlmClient>,
        result: Result<()>,
        worker_gone: bool,
    },
    Key(RawKey),
    Dbus(Request),
    PartialDone {
        utterance: u64,
        result: Result<Transcript>,
    },
    PreviewPolished {
        utterance: u64,
        raw: String,
        text: String,
        outcome: Outcome,
    },
    SttLoaded(LoadKey, Result<SttWorker>),
    SttUnloaded,
    LlmLoaded(LoadKey, Result<LlmClient>),
    DownloadDone(&'static str, Result<PathBuf>),
    ShellOwnerChanged,
    HudExited(u64),
    HudRetry(u64),
    Notice(&'static str, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Recording { since: Instant },
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frontend {
    Gnome,
    LayerShell,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InjectionBackend {
    Gnome,
    DataControl,
}

fn injection_backend(
    shell: &AtomicBool,
    data_control: bool,
    uinput: bool,
) -> Option<InjectionBackend> {
    if shell.load(Ordering::Acquire) {
        Some(InjectionBackend::Gnome)
    } else if data_control && uinput {
        Some(InjectionBackend::DataControl)
    } else {
        None
    }
}

/// Formats `text` for the destination caret and inserts it. A bound GNOME
/// insertion fails without pasting when its target changed after the context
/// read: re-read and re-format once, then insert unadapted rather than drop
/// the dictation. A retry never pastes into a field that has meanwhile become
/// protected or a terminal (a password prompt that grabbed focus, say).
#[allow(clippy::too_many_arguments)]
async fn insert_adapted(
    injector: &mut Injector,
    base: &Route,
    conn: &zbus::Connection,
    text: &str,
    output: &crate::config::Output,
    input: (&AtomicBool, &Arc<AtomicBool>),
    events: &mpsc::UnboundedSender<Event>,
    utterance: u64,
) -> Result<()> {
    const BOUND_ATTEMPTS: u32 = 2;
    let cancelled = input.1;
    let mut attempt = 0;
    loop {
        let fallback = attempt >= BOUND_ATTEMPTS;
        let mut route = match base {
            Route::Gnome(shell) | Route::GnomeBound(shell, _) => Route::Gnome(shell.clone()),
            Route::DataControl => Route::DataControl,
        };
        // This is deliberately a fresh read, after the HUD disappears and
        // modifiers are released: the caret may have moved since recording.
        let context = if output.cursor_aware {
            let report = if let Route::Gnome(shell) = &route {
                let (report, token) =
                    crate::cursor_context::read_gnome_insertion_report(shell).await;
                // The fallback still reads, for the checks below, but inserts
                // unbound: the target keeps changing under us.
                if let Some(token) = token.filter(|_| !fallback) {
                    route = Route::GnomeBound(shell.clone(), token);
                }
                report
            } else {
                crate::cursor_context::read_insertion_report(conn).await
            };
            let _ = events.send(Event::ContextRead {
                utterance,
                phase: "insertion",
                status: if fallback && report.context.is_some() {
                    "changed-before-paste"
                } else {
                    report.status
                },
                source: report.source,
            });
            // After a stale attempt the target is unknown. Paste again only
            // into a field just confirmed readable and unprotected; "changed",
            // "timeout" or "protected" could be a prompt that took focus.
            if attempt > 0 && report.context.is_none() {
                if !fallback && !matches!(report.status, "protected-field" | "terminal-field") {
                    attempt += 1;
                    continue;
                }
                anyhow::bail!(
                    "the focused field changed before paste and could not be confirmed safe \
                     ({}); the dictation was not inserted",
                    report.status
                );
            }
            report.context.filter(|_| !fallback)
        } else {
            None
        };
        anyhow::ensure!(!cancelled.load(Ordering::Acquire), "dictation cancelled");
        let formatted = insertion::format(text, context.as_ref(), output.append_space);
        let result = injector
            .inject(&formatted, &route, output, input, |message| {
                let _ = events.send(Event::Notice("warning", message));
            })
            .await;
        match result {
            Err(e)
                if !fallback
                    && crate::inject::is_stale_context(&e)
                    && !cancelled.load(Ordering::Acquire) =>
            {
                attempt += 1;
                tracing::debug!("insertion target changed before paste; retry {attempt}");
            }
            result => return result,
        }
    }
}

fn missing_injection_message(gnome: bool, data_control: bool, uinput: bool) -> &'static str {
    if gnome {
        "Cannot insert dictation: enable the localwispr GNOME Shell extension and log out/in"
    } else if !data_control {
        "Cannot insert dictation: this compositor does not support ext/wlr-data-control; use a compositor that supports data-control"
    } else if !uinput {
        "Cannot insert dictation: /dev/uinput is unavailable — run ./setup-permissions.sh and log out/in"
    } else {
        "Cannot insert dictation: no injection backend is available"
    }
}

/// LLM polishing of live previews, one at a time.
#[derive(Default)]
struct LivePolish {
    in_flight: bool,
    /// Newest raw preview, waiting for the polish in flight.
    queued: Option<String>,
    /// Raw preview most recently polished (or being polished).
    last_raw: Option<String>,
    /// Newest preview the LLM polished successfully: (raw, polished).
    polished: Option<(String, String)>,
}

struct HudProcess {
    kill: Option<oneshot::Sender<()>>,
    started: Instant,
}

impl Drop for HudProcess {
    fn drop(&mut self) {
        if let Some(k) = self.kill.take() {
            let _ = k.send(());
        }
    }
}

pub struct Engine {
    cfg: Config,
    caps: Capabilities,
    conn: zbus::Connection,
    emitter: SignalEmitter<'static>,
    events: mpsc::UnboundedSender<Event>,
    status: Status,

    matcher: Matcher,
    audio: Option<AudioHandle>,
    levels_tx: mpsc::UnboundedSender<Vec<f32>>,
    stt: Option<Arc<SttWorker>>,
    llm: Option<LlmClient>,
    injector: Arc<tokio::sync::Mutex<Injector>>,
    processing: Option<Processing>,
    model_generation: u64,
    loaded_stt: Option<LoadKey>,
    loaded_llm: Option<LoadKey>,

    phase: Phase,
    utterance: u64,
    partial_in_flight: bool,
    last_partial_samples: usize,
    live_polish: LivePolish,
    recognition_context: Option<ContextCapture>,
    capture_reply: Option<oneshot::Sender<Vec<String>>>,

    loading_stt: Option<LoadKey>,
    unloading_stt: bool,
    /// A model that failed to load is not retried until the config changes.
    failed_stt: Option<LoadKey>,
    loading_llm: Option<LoadKey>,
    failed_llm: Option<LoadKey>,
    downloading: HashSet<&'static str>,

    frontend: Frontend,
    shell_present: Arc<AtomicBool>,
    hud: Option<HudProcess>,
    hud_restarts: Vec<Instant>,
    hud_generation: u64,
    hud_retry_pending: bool,
    hud_retry_delay: Duration,
}

pub async fn run() -> Result<()> {
    let cfg = Config::load_or_default();
    let caps = tokio::task::spawn_blocking(Capabilities::probe).await??;
    tracing::info!(
        "desktop: {} (layer-shell: {}, data-control: {})",
        if caps.desktop.is_empty() {
            "unknown"
        } else {
            &caps.desktop
        },
        caps.layer_shell,
        caps.data_control
    );

    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();
    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
    let status: Status = Arc::default();

    let conn = zbus::connection::Builder::session()
        .serve_at(
            dbus::DAEMON_PATH,
            DaemonIface {
                tx: req_tx,
                status: status.clone(),
                rt: tokio::runtime::Handle::current(),
            },
        )
        .build()
        .await
        .context("connecting to the session bus")?;
    let reply = conn
        .request_name_with_flags(dbus::DAEMON_NAME, zbus::fdo::RequestNameFlags::DO_NOT_QUEUE)
        .await;
    if !matches!(
        reply,
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner)
    ) {
        anyhow::bail!("localwispr is already running");
    }
    {
        let events = events_tx.clone();
        tokio::spawn(crate::update::check_daily(move |version| {
            let _ = events.send(Event::Notice(
                "info",
                format!(
                    "localwispr {version} is available: Settings → Models → Updates, \
                     or run `localwispr update`"
                ),
            ));
        }));
    }
    // Forward D-Bus requests into the single event stream.
    {
        let ev = events_tx.clone();
        tokio::spawn(async move {
            while let Some(r) = req_rx.recv().await {
                if ev.send(Event::Dbus(r)).is_err() {
                    break;
                }
            }
        });
    }
    // Hotkeys.
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<RawKey>();
    let hotkey_problem = match hotkey::spawn_listener(key_tx) {
        Ok(0) => None,
        Ok(n) => Some(format!(
            "{n} input devices are not readable, so hotkeys on them are ignored — run ./setup-permissions.sh and log in again"
        )),
        Err(e) => Some(format!("{e:#}")),
    };
    {
        let ev = events_tx.clone();
        tokio::spawn(async move {
            while let Some(k) = key_rx.recv().await {
                if ev.send(Event::Key(k)).is_err() {
                    break;
                }
            }
        });
    }

    let (levels_tx, mut levels_rx) = mpsc::unbounded_channel::<Vec<f32>>();
    let mut engine = Engine::new(cfg, caps, conn, events_tx, status, levels_tx)?;

    if let Some(msg) = hotkey_problem {
        engine.notice("warning", &msg).await;
    }
    watch_shell_extension(
        &engine.conn,
        engine.events.clone(),
        engine.shell_present.clone(),
    )
    .await;
    engine.route_frontend().await;
    engine.warn_missing_frontend().await;
    engine.warn_missing_capabilities().await;
    engine.open_audio();
    engine.ensure_models().await;
    engine.publish_status();

    let mut preview = tokio::time::interval(Duration::from_millis(
        engine.cfg.transcription.preview_interval_ms as u64,
    ));
    preview.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            Some(ev) = events_rx.recv() => {
                let interval_before = engine.cfg.transcription.preview_interval_ms;
                engine.handle(ev).await;
                if engine.cfg.transcription.preview_interval_ms != interval_before {
                    preview = tokio::time::interval(Duration::from_millis(
                        engine.cfg.transcription.preview_interval_ms as u64,
                    ));
                    preview.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                }
            }
            Some(bands) = levels_rx.recv() => {
                // Coalesce: only the newest frame matters.
                let mut bands = bands;
                while let Ok(b) = levels_rx.try_recv() { bands = b; }
                if matches!(engine.phase, Phase::Recording { .. }) && engine.frontend != Frontend::None {
                    let b: Vec<f64> = bands.iter().map(|&v| v as f64).collect();
                    let _ = DaemonIface::levels(&engine.emitter, b).await;
                }
            }
            _ = preview.tick() => engine.maybe_preview().await,
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
    }
    tracing::info!("shutting down");
    let _ = DaemonIface::state_changed(&engine.emitter, "idle").await;
    Ok(())
}

async fn watch_shell_extension(
    conn: &zbus::Connection,
    events: mpsc::UnboundedSender<Event>,
    present: Arc<AtomicBool>,
) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return;
    };
    let Ok(mut stream) = dbus
        .receive_name_owner_changed_with_args(&[(0, dbus::SHELL_NAME)])
        .await
    else {
        return;
    };
    if let Ok(name) = zbus::names::BusName::try_from(dbus::SHELL_NAME) {
        present.store(
            dbus.name_has_owner(name).await.unwrap_or(false),
            Ordering::Release,
        );
    }
    tokio::spawn(async move {
        use futures_util::StreamExt;
        while let Some(signal) = stream.next().await {
            if let Ok(args) = signal.args() {
                present.store(args.new_owner().is_some(), Ordering::Release);
            }
            if events.send(Event::ShellOwnerChanged).is_err() {
                break;
            }
        }
    });
}

fn preview_has_new_audio(window_samples: usize, captured: usize, previous: usize) -> bool {
    window_samples >= SAMPLE_RATE as usize / 2 && captured != previous
}

fn restore_cleanup_client<T>(
    current: &mut Option<T>,
    loaded_key: &mut Option<LoadKey>,
    returned_key: Option<LoadKey>,
    returned: Option<T>,
) {
    // A load can finish during transcription when no client was lent. Only
    // restore the client actually borrowed by this utterance.
    if let Some(key) = returned_key
        && loaded_key.as_ref() == Some(&key)
        && current.is_none()
    {
        *current = returned;
        if current.is_none() {
            *loaded_key = None;
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Dropping a JoinHandle alone detaches it. Shutdown must also prevent
        // a processing task from inserting its eventual result.
        self.abort_processing();
        self.recognition_context = None;
        if let Some(audio) = &self.audio {
            audio.send(audio::Cmd::Cancel);
        }
        if let Some(stt) = &self.stt {
            stt.abort_pending();
        }
    }
}

impl Engine {
    fn new(
        cfg: Config,
        caps: Capabilities,
        conn: zbus::Connection,
        events: mpsc::UnboundedSender<Event>,
        status: Status,
        levels_tx: mpsc::UnboundedSender<Vec<f32>>,
    ) -> Result<Self> {
        let matcher = Matcher::new(&cfg.hotkeys).unwrap_or_else(|e| {
            tracing::error!("invalid hotkeys in config ({e:#}); using defaults");
            Matcher::new(&Default::default()).expect("default hotkeys are valid")
        });
        let injector = Injector::new(caps.data_control);
        let emitter = SignalEmitter::new(&conn, dbus::DAEMON_PATH)?.into_owned();

        Ok(Self {
            cfg,
            caps,
            conn,
            emitter,
            events,
            status,
            matcher,
            audio: None,
            levels_tx,
            stt: None,
            llm: None,
            injector: Arc::new(tokio::sync::Mutex::new(injector)),
            processing: None,
            model_generation: 0,
            loaded_stt: None,
            loaded_llm: None,
            phase: Phase::Idle,
            utterance: 0,
            partial_in_flight: false,
            last_partial_samples: 0,
            live_polish: LivePolish::default(),
            recognition_context: None,
            capture_reply: None,
            loading_stt: None,
            unloading_stt: false,
            failed_stt: None,
            loading_llm: None,
            failed_llm: None,
            downloading: HashSet::new(),
            frontend: Frontend::None,
            shell_present: Arc::default(),
            hud: None,
            hud_restarts: Vec::new(),
            hud_generation: 0,
            hud_retry_pending: false,
            hud_retry_delay: Duration::from_secs(30),
        })
    }

    async fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => {
                if let Some(action) = self.matcher.on_key(k) {
                    self.on_action(action).await;
                }
            }
            Event::Dbus(r) => self.on_request(r).await,
            Event::PartialDone { utterance, result } => {
                if utterance != self.utterance {
                    return;
                }
                self.partial_in_flight = false;
                if utterance == self.utterance
                    && matches!(self.phase, Phase::Recording { .. })
                    && let Ok(t) = result
                    && !t.text.is_empty()
                {
                    tracing::debug!("preview ({} ms, {:.1}s): {}", t.ms, t.audio_secs, t.text);
                    self.show_preview(t.text).await;
                }
            }
            Event::PreviewPolished {
                utterance,
                raw,
                text,
                outcome,
            } => {
                if utterance != self.utterance {
                    return;
                }
                self.live_polish.in_flight = false;
                if !matches!(self.phase, Phase::Recording { .. }) {
                    return;
                }
                tracing::debug!("preview polished ({outcome:?}): {text}");
                if outcome == Outcome::Polished {
                    self.live_polish.polished = Some((raw, text.clone()));
                }
                let _ = DaemonIface::partial_text(&self.emitter, &text).await;
                if let Some(next) = self.live_polish.queued.take() {
                    self.start_live_polish(next);
                }
            }
            Event::Processed {
                utterance,
                llm_key,
                llm,
                result,
                worker_gone,
            } => {
                if utterance != self.utterance || self.phase != Phase::Processing {
                    return;
                }
                self.processing = None;
                restore_cleanup_client(&mut self.llm, &mut self.loaded_llm, llm_key, llm);
                if worker_gone {
                    self.unload_stt();
                }
                self.finish_idle().await;
                if let Err(e) = result {
                    self.notice("error", &format!("dictation failed: {e:#}"))
                        .await;
                }
                self.ensure_models().await;
            }
            Event::SttLoaded(path, res) => {
                if self.loading_stt.as_ref() != Some(&path) {
                    tokio::task::spawn_blocking(move || drop(res));
                    return;
                }
                if path.generation != self.model_generation {
                    self.loading_stt = None;
                    if let Ok(worker) = res {
                        // Release obsolete GPU allocations before loading the
                        // replacement, without joining on the event loop.
                        self.unloading_stt = true;
                        let events = self.events.clone();
                        tokio::task::spawn_blocking(move || {
                            drop(worker);
                            let _ = events.send(Event::SttUnloaded);
                        });
                    } else {
                        self.ensure_models().await;
                    }
                    return;
                }
                self.loading_stt = None;
                if res.is_err() {
                    self.failed_stt = Some(path.clone());
                }
                match res {
                    Ok(w) => {
                        self.loaded_stt = Some(path);
                        self.stt = Some(Arc::new(w));
                        if self.phase == Phase::Idle {
                            let _ = DaemonIface::state_changed(&self.emitter, "idle").await;
                        }
                    }
                    Err(e) => self.notice("error", &format!("{e:#}")).await,
                }
                self.ensure_models().await;
            }
            Event::SttUnloaded => {
                self.unloading_stt = false;
                self.ensure_models().await;
            }
            Event::LlmLoaded(path, res) => {
                if self.loading_llm.as_ref() != Some(&path) {
                    return;
                }
                if path.generation != self.model_generation {
                    self.loading_llm = None;
                    self.ensure_models().await;
                    return;
                }
                self.loading_llm = None;
                match res {
                    Ok(c) => {
                        self.loaded_llm = Some(path);
                        self.llm = Some(c);
                    }
                    Err(e) => {
                        self.failed_llm = Some(path);
                        self.notice("warning", &format!("cleanup model unavailable: {e:#}"))
                            .await;
                        // Don't retry in a tight loop; next reload will try again.
                        self.publish_status();
                        return;
                    }
                }
                self.ensure_models().await;
            }
            Event::DownloadDone(id, res) => {
                self.downloading.remove(id);
                let err = res
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_default();
                let _ = DaemonIface::download_finished(&self.emitter, id, res.is_ok(), &err).await;
                if let Err(e) = res {
                    self.notice("error", &format!("download of {id} failed: {e:#}"))
                        .await;
                    self.publish_status();
                    return;
                }
                self.ensure_models().await;
            }
            Event::ShellOwnerChanged => {
                tracing::info!(
                    "GNOME Shell extension {}",
                    if self.shell_present.load(Ordering::Acquire) {
                        "connected"
                    } else {
                        "gone"
                    }
                );
                self.route_frontend().await;
            }
            Event::HudExited(generation) => {
                if generation != self.hud_generation {
                    return;
                }
                let stable = self
                    .hud
                    .take()
                    .is_some_and(|h| h.started.elapsed() >= Duration::from_secs(60));
                if stable {
                    self.hud_restarts.clear();
                    self.hud_retry_delay = Duration::from_secs(30);
                }
                let now = Instant::now();
                self.hud_restarts
                    .retain(|t| now - *t < Duration::from_secs(60));
                if self.frontend == Frontend::LayerShell {
                    self.hud_restarts.push(now);
                    if self.hud_restarts.len() < 3 {
                        tracing::warn!("HUD exited; restarting");
                        self.spawn_hud();
                    } else {
                        self.schedule_hud_retry();
                    }
                }
            }
            Event::HudRetry(generation) => {
                if generation == self.hud_generation && self.hud_retry_pending {
                    self.hud_retry_pending = false;
                    if self.frontend == Frontend::LayerShell {
                        self.spawn_hud();
                    }
                }
            }
            Event::ContextRead {
                utterance,
                phase,
                status,
                source,
            } => {
                if utterance == self.utterance {
                    let mut current = self.status.lock().unwrap();
                    current.insert(format!("{phase}_context"), status.into());
                    current.insert(format!("{phase}_context_source"), source.into());
                }
            }
            Event::Notice(level, message) => self.notice(level, &message).await,
        }
        self.publish_status();
    }

    async fn on_action(&mut self, action: Action) {
        match action {
            Action::Captured(keys) => {
                if let Some(reply) = self.capture_reply.take() {
                    let _ = reply.send(keys);
                }
            }
            Action::PttDown => match (self.cfg.hotkeys.mode, self.phase) {
                (PttMode::Toggle, Phase::Recording { .. }) => self.finish_recording().await,
                (_, Phase::Idle) => self.start_recording().await,
                _ => {}
            },
            Action::PttUp => {
                if self.cfg.hotkeys.mode == PttMode::Hold
                    && matches!(self.phase, Phase::Recording { .. })
                {
                    self.finish_recording().await;
                }
            }
            Action::Cancel => {
                if self.phase != Phase::Idle {
                    self.cancel_recording().await;
                    self.ensure_models().await;
                }
            }
            Action::OpenSettings => self.open_settings().await,
        }
    }

    async fn on_request(&mut self, r: Request) {
        match r {
            Request::Reload => self.reload().await,
            Request::OpenSettings => self.open_settings().await,
            Request::Start => {
                if self.phase == Phase::Idle {
                    self.start_recording().await;
                }
            }
            Request::Stop => {
                if matches!(self.phase, Phase::Recording { .. }) {
                    self.finish_recording().await;
                }
            }
            Request::Toggle => match self.phase {
                Phase::Idle => self.start_recording().await,
                Phase::Recording { .. } => self.finish_recording().await,
                Phase::Processing => {}
            },
            Request::Cancel => {
                if self.phase != Phase::Idle {
                    self.cancel_recording().await;
                    self.ensure_models().await;
                }
            }
            Request::CaptureHotkey(reply) => {
                if matches!(self.phase, Phase::Recording { .. }) {
                    self.cancel_recording().await;
                }
                self.matcher.start_capture();
                self.capture_reply = Some(reply);
            }
            Request::CancelCapture => {
                self.matcher.cancel_capture();
                self.capture_reply = None;
            }
            Request::Download(id) => match models::find(&id) {
                Some(spec) if !spec.is_installed() => self.start_download(spec),
                Some(_) => {}
                None => {
                    self.notice("warning", &format!("unknown model `{id}`"))
                        .await
                }
            },
        }
    }

    // ------------------------------------------------------------ dictation --

    async fn start_recording(&mut self) {
        let Some(stt) = self.stt.clone() else {
            let msg = if self.loading_stt.is_some() || !self.downloading.is_empty() {
                "Speech model is still loading…"
            } else {
                "No speech model loaded — check settings"
            };
            self.notice("warning", msg).await;
            return;
        };
        if self.audio.is_none() {
            self.open_audio();
        }
        let Some(audio) = &self.audio else {
            self.notice("error", "Microphone unavailable").await;
            return;
        };
        audio.send(audio::Cmd::Start);
        // Start the microphone first; accessibility must never delay input or
        // hold the engine's event loop while an application responds.
        self.utterance += 1;
        let utterance = self.utterance;
        {
            let mut status = self.status.lock().unwrap();
            status.insert(
                "recognition_context".into(),
                if self.cfg.transcription.context_aware {
                    "pending"
                } else {
                    "disabled"
                }
                .into(),
            );
            status.insert(
                "insertion_context".into(),
                if self.cfg.output.cursor_aware {
                    "pending"
                } else {
                    "disabled"
                }
                .into(),
            );
            status.remove("recognition_context_source");
            status.remove("insertion_context_source");
        }
        self.recognition_context = self.cfg.transcription.context_aware.then(|| {
            let conn = self.conn.clone();
            let events = self.events.clone();
            ContextCapture::spawn(async move {
                let report = crate::cursor_context::read_report(&conn).await;
                let _ = events.send(Event::ContextRead {
                    utterance,
                    phase: "recognition",
                    status: report.status,
                    source: report.source,
                });
                report.context
            })
        });
        stt.new_utterance();
        self.last_partial_samples = 0;
        self.partial_in_flight = false;
        self.live_polish = LivePolish::default();
        self.phase = Phase::Recording {
            since: Instant::now(),
        };
        let _ = DaemonIface::state_changed(&self.emitter, "recording").await;
    }

    async fn cancel_recording(&mut self) {
        self.utterance += 1;
        self.partial_in_flight = false;
        self.recognition_context = None;
        self.cancel_live_polish().await;
        if let Some(a) = &self.audio {
            a.send(audio::Cmd::Cancel);
        }
        if let Some(task) = self.abort_processing() {
            // Wait for the task to drop its borrowed cleanup client before
            // loading a replacement. No detached task may later inject text.
            let _ = task.await;
        }
        // A blocking chord that committed before cancellation must finish
        // before idle acknowledges cancellation; queued jobs recheck the flag.
        self.injector.lock().await.wait_for_keyboard_idle().await;
        if let Some(s) = &self.stt {
            // Join cancellation first: the task must not submit a final job
            // just after this abort, for example while Stop is being flushed.
            s.abort_pending();
        }
        self.phase = Phase::Idle;
        let _ = DaemonIface::state_changed(&self.emitter, "idle").await;
    }

    fn abort_processing(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let processing = self.processing.take()?;
        processing.cancelled.store(true, Ordering::Release);
        if processing.llm_key.as_ref() == self.loaded_llm.as_ref() && self.llm.is_none() {
            self.loaded_llm = None;
        }
        processing.task.abort();
        Some(processing.task)
    }

    fn unload_stt(&mut self) {
        self.loaded_stt = None;
        if let Some(worker) = self.stt.take() {
            worker.abort_pending();
            // Joining the worker can wait for a GPU operation. Keep consuming
            // input while its model is being released.
            self.unloading_stt = true;
            let events = self.events.clone();
            tokio::task::spawn_blocking(move || {
                drop(worker);
                let _ = events.send(Event::SttUnloaded);
            });
        }
    }

    async fn maybe_preview(&mut self) {
        if !self.cfg.transcription.live_preview
            || self.partial_in_flight
            || !matches!(self.phase, Phase::Recording { .. })
            || self.frontend == Frontend::None
            || !self.cfg.hud.show_live_text
        {
            return;
        }
        let (Some(stt), Some(audio)) = (self.stt.clone(), self.audio.as_ref()) else {
            return;
        };
        let (snap, captured) = audio.snapshot(PREVIEW_WINDOW).await;
        if !preview_has_new_audio(snap.len(), captured, self.last_partial_samples) {
            return;
        }
        self.last_partial_samples = captured;
        self.partial_in_flight = true;
        let context = self
            .recognition_context
            .as_mut()
            .and_then(ContextCapture::snapshot);
        let settings = transcription_with_context(&self.cfg.transcription, context);
        let rx = stt.submit(snap, true, &settings, None);
        let ev = self.events.clone();
        let utterance = self.utterance;
        tokio::spawn(async move {
            let result = rx
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("stt worker gone")));
            let _ = ev.send(Event::PartialDone { utterance, result });
        });
    }

    /// Shows a raw preview, polished first when the LLM is used for previews.
    async fn show_preview(&mut self, raw: String) {
        let c = &self.cfg.cleanup;
        if c.enabled && c.live_preview && self.llm.is_some() {
            if self.live_polish.last_raw.as_deref() == Some(raw.as_str()) {
                return; // unchanged since the last polish
            }
            if self.live_polish.in_flight {
                self.live_polish.queued = Some(raw);
            } else {
                self.start_live_polish(raw);
            }
        } else {
            // What insertion falls back to without the LLM.
            let text = cleanup::fallback(&raw, c);
            let _ = DaemonIface::partial_text(&self.emitter, &text).await;
        }
    }

    fn start_live_polish(&mut self, raw: String) {
        if self.live_polish.last_raw.as_deref() == Some(raw.as_str()) {
            return;
        }
        let Some(llm) = self.llm.as_ref().map(LlmClient::shared) else {
            return;
        };
        self.live_polish.in_flight = true;
        self.live_polish.last_raw = Some(raw.clone());
        let cfg = self.cfg.cleanup.clone();
        let events = self.events.clone();
        let utterance = self.utterance;
        tokio::spawn(async move {
            let (text, outcome) = cleanup::polish(Some(&llm), &raw, &cfg, Pass::Preview).await;
            drop(llm);
            let _ = events.send(Event::PreviewPolished {
                utterance,
                raw,
                text,
                outcome,
            });
        });
    }

    /// Stops a preview polish so it never delays the final one, and returns
    /// the newest successful preview polish.
    async fn cancel_live_polish(&mut self) -> Option<(String, String)> {
        let live = std::mem::take(&mut self.live_polish);
        if live.in_flight
            && let Some(llm) = &self.llm
        {
            llm.cancel_pending().await;
        }
        live.polished
    }

    async fn finish_recording(&mut self) {
        let Phase::Recording { since } = self.phase else {
            return;
        };
        let held = since.elapsed();
        if held < Duration::from_millis(self.cfg.hotkeys.min_hold_ms as u64) {
            tracing::debug!("ignoring {} ms tap", held.as_millis());
            self.cancel_recording().await;
            return;
        }
        let t_release = Instant::now();
        let preview_polish = self.cancel_live_polish().await;
        self.phase = Phase::Processing;
        let _ = DaemonIface::state_changed(&self.emitter, "processing").await;

        // Stop capture even when the speech worker has disappeared.
        let samples = self.audio.as_ref().map(AudioHandle::stop);
        let Some(stt) = self.stt.clone() else {
            self.finish_idle().await;
            return;
        };
        let vad = self
            .cfg
            .transcription
            .vad
            .then(|| {
                models::find(models::VAD_ID)
                    .filter(|m| m.is_installed())
                    .map(|m| m.path())
            })
            .flatten();
        let mut llm = self.llm.take();
        let llm_key = llm.as_ref().and(self.loaded_llm.clone());
        let cfg = self.cfg.clone();
        let recognition_context = self.recognition_context.take();
        let conn = self.conn.clone();
        let shell = self.shell_present.clone();
        let modifiers = self.matcher.modifiers_held();
        let data_control = self.caps.data_control;
        let is_gnome = self.caps.is_gnome();
        let visible_frontend = self.frontend != Frontend::None;
        let show_final_text =
            visible_frontend && self.cfg.hud.show_live_text && self.cfg.transcription.live_preview;
        let emitter = self.emitter.clone();
        let injector = self.injector.clone();
        let events = self.events.clone();
        let utterance = self.utterance;
        let task_llm_key = llm_key.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = cancelled.clone();
        let task = tokio::spawn(async move {
            let mut worker_gone = false;
            let result = async {
                let samples = match samples {
                    Some(rx) => rx.await.unwrap_or_default(),
                    None => Vec::new(),
                };
                anyhow::ensure!(
                    !task_cancelled.load(Ordering::Acquire),
                    "dictation cancelled"
                );
                let context = match recognition_context {
                    Some(capture) => capture.finish().await,
                    None => None,
                };
                anyhow::ensure!(
                    !task_cancelled.load(Ordering::Acquire),
                    "dictation cancelled"
                );
                let settings = transcription_with_context(&cfg.transcription, context.as_ref());
                let rx = stt.submit(samples, false, &settings, vad);
                drop(stt);
                let transcript = match rx.await {
                    Ok(r) => r?,
                    Err(_) => {
                        worker_gone = true;
                        anyhow::bail!("speech worker crashed");
                    }
                };
                if transcript.text.trim().is_empty() {
                    return Ok(());
                }
                anyhow::ensure!(
                    !task_cancelled.load(Ordering::Acquire),
                    "dictation cancelled"
                );
                let reused =
                    cleanup::reuse_preview_polish(preview_polish, &transcript.text, &cfg.cleanup);
                let (text, outcome) = match reused {
                    Some(polished) => {
                        tracing::debug!(
                            "final transcript matches the last preview; reusing its polish"
                        );
                        (polished, Outcome::Polished)
                    }
                    None => {
                        cleanup::polish(llm.as_ref(), &transcript.text, &cfg.cleanup, Pass::Final)
                            .await
                    }
                };
                if let Outcome::Failed(e) = outcome {
                    tracing::warn!("cleanup skipped: {e}");
                    if !llm.as_ref().is_some_and(|l| l.is_alive()) {
                        llm = None;
                    }
                }
                // Show the cleaned words before dismissal. Cursor-dependent
                // punctuation and spacing are finalized only once focus is
                // back in the destination, without reopening the overlay.
                anyhow::ensure!(
                    !task_cancelled.load(Ordering::Acquire),
                    "dictation cancelled"
                );
                if show_final_text {
                    let _ = DaemonIface::partial_text(&emitter, &text).await;
                }
                let mut injector = injector.lock().await;
                // Both frontends dissolve within 70 ms. Keep consuming input
                // as Processing until insertion has completed.
                let _ = DaemonIface::state_changed(&emitter, "idle").await;
                if visible_frontend {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                hotkey::wait_for_modifiers(
                    &modifiers,
                    &task_cancelled,
                    Duration::from_millis(1500),
                )
                .await?;
                let route = match injection_backend(&shell, data_control, injector.has_uinput()) {
                    Some(InjectionBackend::Gnome) => {
                        Some(Route::Gnome(dbus::ShellProxy::new(&conn).await?))
                    }
                    Some(InjectionBackend::DataControl) => Some(Route::DataControl),
                    None => None,
                };
                anyhow::ensure!(
                    !task_cancelled.load(Ordering::Acquire),
                    "dictation cancelled"
                );
                match route {
                    Some(base) => {
                        insert_adapted(
                            &mut injector,
                            &base,
                            &conn,
                            &text,
                            &cfg.output,
                            (&modifiers, &task_cancelled),
                            &events,
                            utterance,
                        )
                        .await?;
                    }
                    None => anyhow::bail!(
                        "{}",
                        missing_injection_message(is_gnome, data_control, injector.has_uinput())
                    ),
                }
                tracing::debug!(
                    "dictation completed in {} ms",
                    t_release.elapsed().as_millis()
                );
                Ok(())
            }
            .await;
            let _ = events.send(Event::Processed {
                utterance,
                llm_key: task_llm_key,
                llm,
                result,
                worker_gone,
            });
        });
        self.processing = Some(Processing {
            task,
            cancelled,
            llm_key,
        });
    }

    async fn finish_idle(&mut self) {
        self.recognition_context = None;
        self.phase = Phase::Idle;
        let _ = DaemonIface::state_changed(&self.emitter, "idle").await;
    }

    // ------------------------------------------------------------- settings --

    async fn open_settings(&mut self) {
        // In its own scope, the window survives service restarts (updates).
        let argv = crate::update::own_scope_argv(&["settings"]);
        match std::process::Command::new(&argv[0])
            .args(&argv[1..])
            // Command inherits XDG_ACTIVATION_TOKEN when the caller/session
            // provides one; otherwise the compositor decides whether to focus.
            .stdin(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Reap in the background; GTK's single-instance logic makes a
                // second launch just raise the existing window.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                let _ = DaemonIface::settings_requested(&self.emitter).await;
            }
            Err(e) => {
                self.notice("error", &format!("cannot open settings: {e}"))
                    .await
            }
        }
    }

    async fn reload(&mut self) {
        let new = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                self.notice("error", &format!("{e:#}")).await;
                return;
            }
        };
        self.apply_config(new).await;
    }

    async fn apply_config(&mut self, new: Config) {
        let reloads_models = self.cfg.transcription.model != new.transcription.model
            || self.cfg.transcription.use_gpu != new.transcription.use_gpu
            || self.cfg.cleanup.model != new.cleanup.model
            || self.cfg.cleanup.gpu_layers != new.cleanup.gpu_layers
            || self.cfg.cleanup.enabled != new.cleanup.enabled;
        // Input plumbing (hotkeys, capture, band count) is rebuilt below.
        let rebuilds_input = self.cfg.hotkeys != new.hotkeys
            || self.cfg.audio != new.audio
            || self.cfg.hud.bands != new.hud.bands;
        let cancel = match self.phase {
            Phase::Idle => false,
            // Processing runs on its own configuration copy, already holds its
            // samples, and reads only the shared modifier state, which a
            // hotkey reconfiguration keeps. Only unloading its worker breaks it.
            Phase::Processing => reloads_models,
            // Output settings are read after recording ends; they never cancel.
            Phase::Recording { .. } => {
                rebuilds_input
                    || self.cfg.transcription != new.transcription
                    || self.cfg.cleanup != new.cleanup
            }
        };
        if cancel {
            self.cancel_recording().await;
        }
        let old = std::mem::replace(&mut self.cfg, new);
        if reloads_models {
            self.model_generation += 1;
            self.unload_stt();
            self.llm = None;
            self.loaded_llm = None;
        }
        self.failed_stt = None;
        self.failed_llm = None;
        tracing::info!("configuration reloaded");

        if old.hotkeys != self.cfg.hotkeys
            && let Err(e) = self.matcher.reconfigure(&self.cfg.hotkeys)
        {
            self.notice("error", &format!("invalid hotkey: {e:#}"))
                .await;
        }
        if old.audio != self.cfg.audio || old.hud.bands != self.cfg.hud.bands {
            self.audio = None; // joins the old capture thread
            self.open_audio();
        }
        if old.hud.frontend != self.cfg.hud.frontend {
            self.route_frontend().await;
        }
        self.ensure_models().await;
    }

    // --------------------------------------------------------------- models --

    fn start_download(&mut self, spec: &'static models::ModelSpec) {
        if !self.downloading.insert(spec.id) {
            return;
        }
        let emitter = self.emitter.clone();
        let ev = self.events.clone();
        tokio::spawn(async move {
            let last = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
            let rt = tokio::runtime::Handle::current();
            let progress = |done: u64, total: u64| {
                let mut l = last.lock().unwrap();
                if l.elapsed() >= Duration::from_millis(250) || done == total {
                    *l = Instant::now();
                    let e = emitter.clone();
                    rt.spawn(async move {
                        let _ = DaemonIface::download_progress(&e, spec.id, done, total).await;
                    });
                }
            };
            let res = models::download_with_terminal_progress(spec, &progress).await;
            let _ = ev.send(Event::DownloadDone(spec.id, res));
        });
    }

    /// Converges loaded models towards the config: downloads what is missing,
    /// loads what is not loaded, unloads what is no longer wanted.
    async fn ensure_models(&mut self) {
        // VAD first — tiny, and used per job so it never forces a reload.
        if self.cfg.transcription.vad
            && let Some(vad) = models::find(models::VAD_ID)
            && !vad.is_installed()
        {
            self.start_download(vad);
        }

        let t = &self.cfg.transcription;
        match ModelRef::resolve(&t.model, ModelKind::Whisper) {
            Ok(m) => {
                let path = m.path();
                let key = LoadKey {
                    path: path.clone(),
                    settings: LoadSettings::Speech { use_gpu: t.use_gpu },
                    generation: self.model_generation,
                };
                let loaded = self.stt.is_some() && self.loaded_stt.as_ref() == Some(&key);
                if !loaded
                    && !self.unloading_stt
                    && self.loading_stt.as_ref() != Some(&key)
                    && self.failed_stt.as_ref() != Some(&key)
                {
                    if m.is_installed() {
                        if self.loading_stt.is_none() {
                            self.unload_stt();
                            self.loading_stt = Some(key.clone());
                            let _ = DaemonIface::state_changed(&self.emitter, "loading").await;
                            let ev = self.events.clone();
                            let use_gpu = self.cfg.transcription.use_gpu;
                            tokio::task::spawn_blocking(move || {
                                let res = SttWorker::spawn(path.clone(), use_gpu);
                                let _ = ev.send(Event::SttLoaded(key, res));
                            });
                        }
                    } else if let ModelRef::Catalog(spec) = m {
                        self.start_download(spec);
                    } else {
                        self.notice(
                            "error",
                            &format!("model file not found: {}", path.display()),
                        )
                        .await;
                    }
                }
            }
            Err(e) => self.notice("error", &format!("{e:#}")).await,
        }

        let c = &self.cfg.cleanup;
        if !c.enabled {
            self.llm = None;
            self.loaded_llm = None;
            return;
        }
        match ModelRef::resolve(&c.model, ModelKind::Llm) {
            Ok(m) => {
                let path = m.path();
                let key = LoadKey {
                    path: path.clone(),
                    settings: LoadSettings::Cleanup {
                        gpu_layers: c.gpu_layers,
                    },
                    generation: self.model_generation,
                };
                let borrowed =
                    self.processing.as_ref().and_then(|p| p.llm_key.as_ref()) == Some(&key);
                let loaded =
                    (self.llm.is_some() || borrowed) && self.loaded_llm.as_ref() == Some(&key);
                if !loaded && self.loading_llm.is_none() && self.failed_llm.as_ref() != Some(&key) {
                    if m.is_installed() {
                        self.llm = None;
                        self.loading_llm = Some(key.clone());
                        let ev = self.events.clone();
                        let gpu_layers = c.gpu_layers;
                        tokio::spawn(async move {
                            let res = LlmClient::spawn(&path, gpu_layers).await;
                            let _ = ev.send(Event::LlmLoaded(key, res));
                        });
                    } else if let ModelRef::Catalog(spec) = m {
                        self.start_download(spec);
                    } else {
                        self.notice(
                            "error",
                            &format!("model file not found: {}", path.display()),
                        )
                        .await;
                    }
                }
            }
            Err(e) => self.notice("error", &format!("{e:#}")).await,
        }
    }

    // ------------------------------------------------------------- plumbing --

    fn open_audio(&mut self) {
        match AudioHandle::spawn(
            self.cfg.audio.clone(),
            self.cfg.hud.bands as usize,
            self.levels_tx.clone(),
        ) {
            Ok(a) => self.audio = Some(a),
            Err(e) => tracing::error!("microphone: {e:#}"),
        }
    }

    async fn route_frontend(&mut self) {
        let want = match self.cfg.hud.frontend {
            FrontendChoice::Auto if self.shell_present.load(Ordering::Acquire) => Frontend::Gnome,
            FrontendChoice::Auto if self.caps.layer_shell => Frontend::LayerShell,
            FrontendChoice::Auto => Frontend::None,
            FrontendChoice::Gnome => Frontend::Gnome,
            FrontendChoice::LayerShell => Frontend::LayerShell,
            FrontendChoice::None => Frontend::None,
        };
        if want == self.frontend
            && (want != Frontend::LayerShell || self.hud.is_some() || self.hud_retry_pending)
        {
            return;
        }
        if want != self.frontend {
            self.hud_generation += 1;
            self.hud_retry_pending = false;
        }
        self.frontend = want;
        tracing::info!("HUD frontend: {want:?}");
        match want {
            Frontend::LayerShell => self.spawn_hud(),
            _ => self.hud = None,
        }
        self.warn_missing_frontend().await;
    }

    async fn warn_missing_frontend(&self) {
        if !self.shell_present.load(Ordering::Acquire)
            && matches!(
                self.cfg.hud.frontend,
                FrontendChoice::Auto | FrontendChoice::Gnome
            )
            && self.caps.is_gnome()
        {
            self.notice(
                "warning",
                "GNOME detected but the localwispr Shell extension is not active — run `gnome-extensions enable localwispr@localwispr.github.io` and log out/in",
            )
            .await;
        }
    }

    async fn warn_missing_capabilities(&self) {
        if self.caps.is_gnome() {
            return;
        }
        if !self.caps.layer_shell {
            self.notice("warning", "No dictation HUD: this compositor does not support wlr-layer-shell; use a compositor with layer-shell support for the overlay").await;
        }
        let uinput = self.injector.lock().await.has_uinput();
        if !self.caps.data_control {
            self.notice("warning", missing_injection_message(false, false, uinput))
                .await;
        } else if !uinput {
            self.notice("warning", missing_injection_message(false, true, false))
                .await;
        }
    }

    fn schedule_hud_retry(&mut self) {
        if self.hud_retry_pending {
            return;
        }
        self.hud_retry_pending = true;
        let delay = self.hud_retry_delay;
        self.hud_retry_delay = (delay * 2).min(Duration::from_secs(300));
        let message = format!(
            "HUD unavailable after repeated failures; retrying in {} seconds",
            delay.as_secs()
        );
        let _ = self.events.send(Event::Notice("warning", message));
        let ev = self.events.clone();
        let generation = self.hud_generation;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = ev.send(Event::HudRetry(generation));
        });
    }

    fn active_frontend(&self) -> Frontend {
        match self.frontend {
            Frontend::Gnome if self.shell_present.load(Ordering::Acquire) => Frontend::Gnome,
            Frontend::LayerShell if self.hud.is_some() => Frontend::LayerShell,
            _ => Frontend::None,
        }
    }

    fn spawn_hud(&mut self) {
        self.hud_generation += 1;
        let generation = self.hud_generation;
        let exe = std::env::current_exe().unwrap_or_else(|_| "localwispr".into());
        let mut child = match tokio::process::Command::new(exe)
            .arg("hud")
            // The HUD exits when this pipe closes, so it never outlives a
            // crashed daemon (whose restart starts a new HUD).
            .env(crate::hud::PARENT_PIPE_ENV, "1")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("cannot start HUD: {e}");
                self.schedule_hud_retry();
                return;
            }
        };
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let ev = self.events.clone();
        tokio::spawn(async move {
            let _parent_pipe = child.stdin.take();
            tokio::select! {
                _ = child.wait() => { let _ = ev.send(Event::HudExited(generation)); }
                _ = kill_rx => { let _ = child.kill().await; }
            }
        });
        self.hud = Some(HudProcess {
            kill: Some(kill_tx),
            started: Instant::now(),
        });
    }

    async fn notice(&self, level: &str, msg: &str) {
        match level {
            "error" => tracing::error!("{msg}"),
            "warning" => tracing::warn!("{msg}"),
            _ => tracing::info!("{msg}"),
        }
        let _ = DaemonIface::notice(&self.emitter, level, msg).await;
        self.status
            .lock()
            .unwrap()
            .insert("last_notice".into(), format!("{level}: {msg}"));
    }

    fn publish_status(&self) {
        let mut s = self.status.lock().unwrap();
        // Nothing is read while idle: an empty, failed or cancelled utterance
        // must not leave a read reported as still pending.
        let idle = self.phase == Phase::Idle;
        for (phase, enabled) in [
            ("recognition", self.cfg.transcription.context_aware),
            ("insertion", self.cfg.output.cursor_aware),
        ] {
            let key = format!("{phase}_context");
            if enabled {
                if s.get(&key)
                    .is_none_or(|value| value == "disabled" || (idle && value == "pending"))
                {
                    s.insert(key, "not-captured".into());
                }
            } else {
                s.insert(key, "disabled".into());
                s.remove(&format!("{phase}_context_source"));
            }
        }
        let phase = match self.phase {
            Phase::Idle if self.stt.is_none() => "loading",
            Phase::Idle => "idle",
            Phase::Recording { .. } => "recording",
            Phase::Processing => "processing",
        };
        let path_name = |p: &PathBuf| {
            p.file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut set = |k: &str, v: String| {
            s.insert(k.to_owned(), v);
        };
        set("state", phase.into());
        set("version", env!("CARGO_PKG_VERSION").into());
        set(
            "stt_model",
            self.stt
                .as_ref()
                .map(|w| path_name(&w.model_path))
                .unwrap_or_default(),
        );
        set(
            "llm_model",
            self.llm
                .as_ref()
                .map(|l| path_name(&l.model_path))
                .unwrap_or_default(),
        );
        set("stt_loading", self.loading_stt.is_some().to_string());
        set("llm_loading", self.loading_llm.is_some().to_string());
        set(
            "downloading",
            self.downloading
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .join(","),
        );
        set(
            "frontend",
            format!("{:?}", self.active_frontend()).to_lowercase(),
        );
        set("hud_retry_pending", self.hud_retry_pending.to_string());
        // The capture thread always exists; the stream is opened only while
        // recording unless "keep microphone open" is on.
        set(
            "microphone_open",
            (self.audio.is_some()
                && (self.cfg.audio.keep_warm || matches!(self.phase, Phase::Recording { .. })))
            .to_string(),
        );
        set(
            "injection",
            if self.shell_present.load(Ordering::Acquire) {
                "gnome-shell"
            } else if self.caps.data_control
                && self.injector.try_lock().is_ok_and(|i| i.has_uinput())
            {
                "data-control+uinput"
            } else {
                "unavailable"
            }
            .into(),
        );
    }
}

/// Used by `localwispr download`: fetch everything the config needs, with
/// terminal progress bars.
pub async fn provision(cfg: &Config) -> Result<()> {
    let mut wanted: Vec<&'static models::ModelSpec> = Vec::new();
    if let Ok(ModelRef::Catalog(s)) =
        ModelRef::resolve(&cfg.transcription.model, ModelKind::Whisper)
    {
        wanted.push(s);
    }
    if cfg.transcription.vad
        && let Some(s) = models::find(models::VAD_ID)
    {
        wanted.push(s);
    }
    if cfg.cleanup.enabled
        && let Ok(ModelRef::Catalog(s)) = ModelRef::resolve(&cfg.cleanup.model, ModelKind::Llm)
    {
        wanted.push(s);
    }
    for spec in wanted {
        if spec.is_installed() {
            eprintln!("✓ {} already installed", spec.label);
            continue;
        }
        models::download_with_terminal_progress(spec, &|_, _| {}).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn surrounding(before: &str) -> SurroundingText {
        SurroundingText {
            before: before.into(),
            after: String::new(),
            is_replacement: false,
        }
    }

    #[test]
    fn recognition_settings_preserve_configuration_and_respect_opt_out() {
        let mut configured = Transcription {
            initial_prompt: "Manual vocabulary: Kubernetes.".into(),
            context_aware: true,
            ..Transcription::default()
        };
        let original = configured.clone();
        let context = surrounding("We use PostgreSQL and Tokio in localwispr.");
        let settings = transcription_with_context(&configured, Some(&context));
        assert_eq!(configured, original);
        assert_eq!(
            settings.initial_prompt,
            insertion::recognition_prompt(&configured.initial_prompt, Some(&context))
        );
        assert!(settings.initial_prompt.contains(&configured.initial_prompt));
        assert_eq!(
            transcription_with_context(&configured, None).initial_prompt,
            configured.initial_prompt
        );
        configured.context_aware = false;
        assert_eq!(
            transcription_with_context(&configured, Some(&context)),
            configured
        );
    }

    #[tokio::test]
    async fn context_preview_never_waits_and_final_reuses_the_same_snapshot() {
        let (reply, result) = oneshot::channel();
        let mut capture = ContextCapture::spawn(async move { result.await.ok() });
        assert_eq!(capture.snapshot(), None);
        let expected = surrounding("Tokio and PostgreSQL");
        reply.send(expected.clone()).unwrap();
        (&mut capture.task).await.unwrap();
        assert_eq!(capture.snapshot(), Some(&expected));
        assert_eq!(capture.snapshot(), Some(&expected));
        assert_eq!(capture.finish().await, Some(expected));
    }

    #[tokio::test]
    async fn final_context_can_finish_a_capture_that_was_unavailable_to_preview() {
        let (reply, result) = oneshot::channel();
        let mut capture = ContextCapture::spawn(async move { result.await.ok() });
        assert_eq!(capture.snapshot(), None);
        let expected = surrounding("Whisper");
        reply.send(expected.clone()).unwrap();
        assert_eq!(capture.finish().await, Some(expected));
    }

    #[tokio::test]
    async fn final_context_wait_uses_the_recording_start_deadline() {
        let (dropped, done) = oneshot::channel();
        let guard = NotifyDrop(Some(dropped));
        let capture = ContextCapture::spawn(async move {
            let _guard = guard;
            std::future::pending().await
        });
        // Most of the read budget has elapsed before release. A fresh 250 ms
        // timeout here would fail this remaining-budget bound.
        tokio::time::sleep(CONTEXT_TIMEOUT - Duration::from_millis(50)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(150), capture.finish())
                .await
                .unwrap(),
            None
        );
        tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_a_capture_cancels_its_read_and_cannot_reuse_its_snapshot() {
        let (dropped, done) = oneshot::channel();
        let guard = NotifyDrop(Some(dropped));
        let capture = ContextCapture::spawn(async move {
            let _guard = guard;
            std::future::pending().await
        });
        drop(capture);
        tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap();
        let expected = surrounding("New utterance vocabulary");
        let mut next = ContextCapture::spawn({
            let expected = expected.clone();
            async move { Some(expected) }
        });
        (&mut next.task).await.unwrap();
        assert_eq!(next.finish().await, Some(expected));
    }

    #[test]
    fn insertion_uses_current_extension_state_and_available_fallback() {
        let shell = AtomicBool::new(false);
        assert_eq!(
            injection_backend(&shell, true, true),
            Some(InjectionBackend::DataControl)
        );
        assert_eq!(injection_backend(&shell, true, false), None);
        assert_eq!(injection_backend(&shell, false, true), None);
        shell.store(true, Ordering::Release);
        assert_eq!(
            injection_backend(&shell, false, false),
            Some(InjectionBackend::Gnome)
        );
        shell.store(false, Ordering::Release);
        assert_eq!(
            injection_backend(&shell, true, true),
            Some(InjectionBackend::DataControl)
        );
        assert!(missing_injection_message(true, false, false).contains("GNOME"));
        assert!(missing_injection_message(false, false, false).contains("data-control"));
        assert!(missing_injection_message(false, true, false).contains("setup-permissions.sh"));
    }

    #[tokio::test]
    async fn owner_watcher_updates_insertion_state_without_engine_dispatch() {
        let (engine, _bus) = engine().await;
        let (events, mut rx) = mpsc::unbounded_channel();
        let present = Arc::new(AtomicBool::new(true));
        watch_shell_extension(&engine.conn, events, present.clone()).await;
        assert!(!present.load(Ordering::Acquire));
        engine.conn.request_name(dbus::SHELL_NAME).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(present.load(Ordering::Acquire));
        engine.conn.release_name(dbus::SHELL_NAME).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!present.load(Ordering::Acquire));
    }

    struct TestShell;

    #[zbus::interface(name = "org.localwispr.Shell1")]
    impl TestShell {
        // Mirrors the extension: an error when nothing was pasted, otherwise
        // whether the clipboard was preserved.
        fn inject(
            &self,
            text: &str,
            chord: &str,
            restore: bool,
            restore_delay_ms: u32,
        ) -> zbus::fdo::Result<bool> {
            let _ = (chord, restore, restore_delay_ms);
            match text {
                "busy" => Err(zbus::fdo::Error::Failed(
                    "Another insertion is in progress".into(),
                )),
                _ => Ok(false),
            }
        }
    }

    #[derive(Debug, zbus::DBusError)]
    #[zbus(prefix = "org.localwispr.Shell1.Error")]
    enum ShellError {
        #[zbus(error)]
        ZBus(zbus::Error),
        StaleContext(String),
    }

    /// Bound insertions go stale `stale` times before any paste succeeds.
    struct StaleShell {
        stale: std::sync::Mutex<u32>,
        /// Set (as a user cancel would) when a stale reply is sent.
        cancel: Option<Arc<AtomicBool>>,
        /// Context status reported by reads after the first.
        retry_status: &'static str,
        pasted: std::sync::Mutex<Vec<(String, bool)>>,
        tokens: std::sync::Mutex<u32>,
    }

    struct StaleShellRef(Arc<StaleShell>);

    #[zbus::interface(name = "org.localwispr.Shell1")]
    impl StaleShellRef {
        fn get_insertion_context(&self) -> (String, String, String, bool, String) {
            let mut tokens = self.0.tokens.lock().unwrap();
            *tokens += 1;
            let status = if *tokens == 1 {
                "available"
            } else {
                self.0.retry_status
            };
            if status != "available" {
                return (
                    status.into(),
                    String::new(),
                    String::new(),
                    false,
                    String::new(),
                );
            }
            (
                status.into(),
                "Done.".into(),
                String::new(),
                false,
                format!("t{tokens}"),
            )
        }
        fn inject_bound(
            &self,
            _request_id: &str,
            _token: &str,
            text: &str,
            _chord: &str,
            _restore: bool,
            _restore_delay_ms: u32,
        ) -> Result<bool, ShellError> {
            let mut stale = self.0.stale.lock().unwrap();
            if *stale > 0 {
                *stale -= 1;
                if let Some(cancel) = &self.0.cancel {
                    cancel.store(true, Ordering::Release);
                }
                return Err(ShellError::StaleContext("Insertion context changed".into()));
            }
            self.0.pasted.lock().unwrap().push((text.into(), true));
            Ok(true)
        }
        fn inject_cancellable(
            &self,
            _request_id: &str,
            text: &str,
            _chord: &str,
            _restore: bool,
            _restore_delay_ms: u32,
        ) -> bool {
            self.0.pasted.lock().unwrap().push((text.into(), false));
            true
        }
        fn cancel_inject(&self, _request_id: &str) {}
    }

    #[tokio::test]
    async fn stale_insertion_context_retries_then_falls_back_instead_of_dropping_text() {
        // (stale replies, expected (text, bound) paste)
        for (stale, expected) in [
            (0, (" Hello there ", true)),
            (1, (" Hello there ", true)),
            (2, ("hello there ", false)),
            (9, ("hello there ", false)),
        ] {
            let (engine, _bus) = engine().await;
            let shell = Arc::new(StaleShell {
                stale: std::sync::Mutex::new(stale),
                cancel: None,
                retry_status: "available",
                pasted: std::sync::Mutex::default(),
                tokens: std::sync::Mutex::default(),
            });
            engine
                .conn
                .object_server()
                .at("/org/localwispr/Shell", StaleShellRef(shell.clone()))
                .await
                .unwrap();
            engine.conn.request_name(dbus::SHELL_NAME).await.unwrap();
            let route = Route::Gnome(dbus::ShellProxy::new(&engine.conn).await.unwrap());
            let mut output = engine.cfg.output.clone();
            output.cursor_aware = true;
            output.append_space = true;
            let held = AtomicBool::new(false);
            let cancelled = Arc::new(AtomicBool::new(false));
            let (events, _rx) = mpsc::unbounded_channel();
            insert_adapted(
                &mut Injector::new(false),
                &route,
                &engine.conn,
                "hello there",
                &output,
                (&held, &cancelled),
                &events,
                1,
            )
            .await
            .unwrap();
            let pasted = shell.pasted.lock().unwrap().clone();
            assert_eq!(
                pasted,
                vec![(expected.0.to_owned(), expected.1)],
                "stale={stale}"
            );
            // The fallback still reads, to refuse a field turned protected.
            assert_eq!(*shell.tokens.lock().unwrap(), stale.min(2) + 1);
        }
    }

    #[tokio::test]
    async fn a_retry_pastes_only_into_a_field_confirmed_readable_and_unprotected() {
        // "changed"/"timeout": a prompt taking focus mid-read is not
        // confirmed safe either, even after the extra read.
        for retry_status in ["protected", "terminal", "changed", "timeout"] {
            let (engine, _bus) = engine().await;
            let shell = Arc::new(StaleShell {
                stale: std::sync::Mutex::new(9),
                cancel: None,
                retry_status,
                pasted: std::sync::Mutex::default(),
                tokens: std::sync::Mutex::default(),
            });
            engine
                .conn
                .object_server()
                .at("/org/localwispr/Shell", StaleShellRef(shell.clone()))
                .await
                .unwrap();
            engine.conn.request_name(dbus::SHELL_NAME).await.unwrap();
            let route = Route::Gnome(dbus::ShellProxy::new(&engine.conn).await.unwrap());
            let mut output = engine.cfg.output.clone();
            output.cursor_aware = true;
            let held = AtomicBool::new(false);
            let cancelled = Arc::new(AtomicBool::new(false));
            let (events, _rx) = mpsc::unbounded_channel();
            let error = insert_adapted(
                &mut Injector::new(false),
                &route,
                &engine.conn,
                "hello",
                &output,
                (&held, &cancelled),
                &events,
                1,
            )
            .await
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("not inserted"),
                "{retry_status}"
            );
            assert!(shell.pasted.lock().unwrap().is_empty(), "{retry_status}");
        }
    }

    #[tokio::test]
    async fn cancellation_during_a_stale_attempt_stops_retries() {
        let (engine, _bus) = engine().await;
        let cancelled = Arc::new(AtomicBool::new(false));
        let shell = Arc::new(StaleShell {
            stale: std::sync::Mutex::new(9),
            cancel: Some(cancelled.clone()),
            retry_status: "available",
            pasted: std::sync::Mutex::default(),
            tokens: std::sync::Mutex::default(),
        });
        engine
            .conn
            .object_server()
            .at("/org/localwispr/Shell", StaleShellRef(shell.clone()))
            .await
            .unwrap();
        engine.conn.request_name(dbus::SHELL_NAME).await.unwrap();
        let route = Route::Gnome(dbus::ShellProxy::new(&engine.conn).await.unwrap());
        let held = AtomicBool::new(false);
        let (events, _rx) = mpsc::unbounded_channel();
        let result = insert_adapted(
            &mut Injector::new(false),
            &route,
            &engine.conn,
            "hello",
            &engine.cfg.output,
            (&held, &cancelled),
            &events,
            1,
        )
        .await;
        assert!(result.is_err());
        assert!(shell.pasted.lock().unwrap().is_empty());
        // No second context read: a cancelled dictation never retries.
        assert_eq!(*shell.tokens.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn settings_saved_while_processing_cancel_only_when_the_task_breaks() {
        let (mut engine, _bus) = engine().await;
        type Change = fn(&mut Config);
        let changes: [(Change, bool); 7] = [
            (|c| c.output.append_space = !c.output.append_space, false),
            (|c| c.output.cursor_aware = !c.output.cursor_aware, false),
            (
                |c| c.cleanup.custom_instructions.push_str(" Be terse."),
                false,
            ),
            (|c| c.transcription.languages.push("de".into()), false),
            (|c| c.hotkeys.push_to_talk = vec!["KEY_F13".into()], false),
            (|c| c.hud.bands = 12, false),
            (|c| c.transcription.use_gpu = !c.transcription.use_gpu, true),
        ];
        for (change, cancels) in changes {
            engine.phase = Phase::Processing;
            let utterance = engine.utterance;
            let mut new = engine.cfg.clone();
            change(&mut new);
            engine.apply_config(new).await;
            assert_eq!(engine.phase == Phase::Idle, cancels);
            assert_eq!(engine.utterance != utterance, cancels);
        }
    }

    #[tokio::test]
    async fn extension_failure_is_an_error_and_unpreserved_clipboard_a_warning() {
        let (engine, _bus) = engine().await;
        engine
            .conn
            .object_server()
            .at("/org/localwispr/Shell", TestShell)
            .await
            .unwrap();
        engine.conn.request_name(dbus::SHELL_NAME).await.unwrap();
        let route = Route::Gnome(dbus::ShellProxy::new(&engine.conn).await.unwrap());
        let held = AtomicBool::new(false);
        let cancelled = Arc::new(AtomicBool::new(false));
        let err = Injector::new(false)
            .inject(
                "busy",
                &route,
                &engine.cfg.output,
                (&held, &cancelled),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Another insertion"));
        let warnings = std::sync::Mutex::new(Vec::new());
        Injector::new(false)
            .inject(
                "hello",
                &route,
                &engine.cfg.output,
                (&held, &cancelled),
                |w| warnings.lock().unwrap().push(w),
            )
            .await
            .unwrap();
        assert_eq!(
            warnings.lock().unwrap().len(),
            usize::from(engine.cfg.output.restore_clipboard)
        );
    }

    #[tokio::test]
    async fn hud_crashes_schedule_retries_and_status_tracks_absence() {
        let (mut engine, _bus) = engine().await;
        engine.frontend = Frontend::LayerShell;
        engine.hud = Some(HudProcess {
            kill: None,
            started: Instant::now(),
        });
        engine.hud_restarts = vec![Instant::now(); 2];
        engine.handle(Event::HudExited(0)).await;
        assert!(engine.hud.is_none());
        assert!(engine.hud_retry_pending);
        assert_eq!(engine.hud_retry_delay, Duration::from_secs(60));
        assert_eq!(
            engine.status.lock().unwrap().get("frontend").unwrap(),
            "none"
        );
        for expected in [120, 240, 300, 300] {
            engine.hud_retry_pending = false;
            engine.schedule_hud_retry();
            assert_eq!(engine.hud_retry_delay, Duration::from_secs(expected));
        }
        engine.cfg.hud.frontend = FrontendChoice::None;
        engine.route_frontend().await;
        engine.handle(Event::HudRetry(0)).await;
        assert!(!engine.hud_retry_pending);
        assert!(engine.hud.is_none());
        assert_eq!(engine.active_frontend(), Frontend::None);
    }

    // A private bus keeps tests independent of the user's session and cannot
    // call the desktop's extension or change any desktop settings.
    async fn engine() -> (Engine, tokio::process::Child) {
        let mut bus = tokio::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("daemon regressions require dbus-daemon");
        let mut address = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(bus.stdout.take().unwrap()).read_line(&mut address),
        )
        .await
        .unwrap()
        .unwrap();
        let conn = zbus::connection::Builder::address(address.trim())
            .build()
            .await
            .unwrap();
        let mut cfg = Config::default();
        cfg.transcription.model = "/nonexistent/localwispr-test-model.bin".into();
        cfg.transcription.vad = false;
        cfg.cleanup.enabled = false;
        let (events, _) = mpsc::unbounded_channel();
        let (levels, _) = mpsc::unbounded_channel();
        let engine = Engine::new(
            cfg,
            Capabilities::default(),
            conn,
            events,
            Arc::default(),
            levels,
        )
        .unwrap();
        (engine, bus)
    }

    #[tokio::test]
    async fn context_status_reports_current_capture_and_ignores_cancelled_utterances() {
        let (mut engine, _bus) = engine().await;
        engine.utterance = 7;
        engine
            .handle(Event::ContextRead {
                utterance: 7,
                phase: "recognition",
                status: "available",
                source: "gnome-input-method",
            })
            .await;
        assert_eq!(
            engine.status.lock().unwrap()["recognition_context"],
            "available"
        );
        assert_eq!(
            engine.status.lock().unwrap()["recognition_context_source"],
            "gnome-input-method"
        );
        engine
            .handle(Event::ContextRead {
                utterance: 6,
                phase: "recognition",
                status: "no-active-window",
                source: "at-spi",
            })
            .await;
        assert_eq!(
            engine.status.lock().unwrap()["recognition_context"],
            "available"
        );
        engine.cfg.transcription.context_aware = false;
        engine.publish_status();
        assert_eq!(
            engine.status.lock().unwrap()["recognition_context"],
            "disabled"
        );
        assert!(
            !engine
                .status
                .lock()
                .unwrap()
                .contains_key("recognition_context_source")
        );
    }

    fn llm_key(generation: u64, gpu_layers: u32) -> LoadKey {
        LoadKey {
            path: "/nonexistent/localwispr-test-model.gguf".into(),
            settings: LoadSettings::Cleanup { gpu_layers },
            generation,
        }
    }

    #[test]
    fn preview_progress_continues_beyond_twenty_eight_seconds() {
        let thirty_seconds = 30 * SAMPLE_RATE as usize;
        assert!(preview_has_new_audio(PREVIEW_WINDOW, thirty_seconds, 0));
        assert!(preview_has_new_audio(
            PREVIEW_WINDOW,
            thirty_seconds + 1600,
            thirty_seconds
        ));
        assert!(!preview_has_new_audio(
            PREVIEW_WINDOW,
            thirty_seconds,
            thirty_seconds
        ));
        assert!(!preview_has_new_audio(100, 100, 0));
    }

    #[test]
    fn completion_preserves_clients_loaded_during_transcription_and_releases_failed_clients() {
        let key = llm_key(1, 99);
        let mut current = Some("new client");
        let mut loaded = Some(key.clone());
        restore_cleanup_client(&mut current, &mut loaded, None, None);
        assert_eq!(current, Some("new client"));
        assert_eq!(loaded, Some(key.clone()));

        restore_cleanup_client(
            &mut current,
            &mut loaded,
            Some(llm_key(0, 0)),
            Some("stale client"),
        );
        assert_eq!(current, Some("new client"));
        current = None; // The current client was borrowed by processing.
        restore_cleanup_client(
            &mut current,
            &mut loaded,
            Some(llm_key(0, 0)),
            Some("stale client"),
        );
        assert_eq!(current, None);
        assert_eq!(loaded, Some(key.clone()));
        restore_cleanup_client(
            &mut current,
            &mut loaded,
            Some(key.clone()),
            Some("returned client"),
        );
        assert_eq!(current, Some("returned client"));
        current = None;
        restore_cleanup_client(&mut current, &mut loaded, Some(key), None);
        assert_eq!(current, None);
        assert_eq!(loaded, None); // ensure_models must now load a replacement.
    }

    #[tokio::test]
    async fn reconfiguration_cancels_recording_before_rebinding_or_unloading() {
        let (mut engine, _bus) = engine().await;
        let (audio, commands) = AudioHandle::test_handle();
        engine.audio = Some(audio);
        for change in 0..6 {
            engine.phase = Phase::Recording {
                since: Instant::now(),
            };
            let (dropped, done) = oneshot::channel();
            let guard = NotifyDrop(Some(dropped));
            engine.recognition_context = Some(ContextCapture::spawn(async move {
                let _guard = guard;
                std::future::pending().await
            }));
            let utterance = engine.utterance;
            let mut new = engine.cfg.clone();
            match change {
                0 => new.hotkeys.push_to_talk = vec!["KEY_F13".into()],
                1 => new.hotkeys.mode = PttMode::Toggle,
                2 => new.transcription.use_gpu = !new.transcription.use_gpu,
                3 => new.transcription.model = "/nonexistent/replacement-model.bin".into(),
                4 => new.transcription.context_aware = !new.transcription.context_aware,
                _ => new.cleanup.enabled = !new.cleanup.enabled,
            }
            engine.apply_config(new).await;
            assert_eq!(engine.phase, Phase::Idle);
            assert_eq!(engine.utterance, utterance + 1);
            assert!(matches!(commands.try_recv(), Ok(audio::Cmd::Cancel)));
            assert!(engine.recognition_context.is_none());
            tokio::time::timeout(Duration::from_secs(1), done)
                .await
                .unwrap()
                .unwrap();
        }
        // Output settings are read after recording ends; they never cancel it.
        engine.phase = Phase::Recording {
            since: Instant::now(),
        };
        let utterance = engine.utterance;
        let mut new = engine.cfg.clone();
        new.output.cursor_aware = !new.output.cursor_aware;
        new.output.append_space = !new.output.append_space;
        engine.apply_config(new).await;
        assert!(matches!(engine.phase, Phase::Recording { .. }));
        assert_eq!(engine.utterance, utterance);
        assert!(commands.try_recv().is_err());
        let (reply, _) = oneshot::channel();
        engine.on_request(Request::CaptureHotkey(reply)).await;
        assert_eq!(engine.phase, Phase::Idle);
        assert!(matches!(commands.try_recv(), Ok(audio::Cmd::Cancel)));
    }

    #[tokio::test]
    async fn finishing_without_a_speech_worker_still_stops_capture() {
        let (mut engine, _bus) = engine().await;
        let (audio, commands) = AudioHandle::test_handle();
        engine.audio = Some(audio);
        engine.phase = Phase::Recording {
            since: Instant::now() - Duration::from_secs(1),
        };
        engine.recognition_context = Some(ContextCapture::spawn(std::future::pending()));
        // Withhold the capture reply: stopping a missing-worker recording must
        // still queue Stop, without blocking the event loop for the buffer.
        tokio::time::timeout(Duration::from_secs(1), engine.finish_recording())
            .await
            .unwrap();
        assert!(matches!(commands.try_recv(), Ok(audio::Cmd::Stop(_))));
        assert_eq!(engine.phase, Phase::Idle);
        assert!(engine.processing.is_none());
        assert!(engine.recognition_context.is_none());
    }

    struct NotifyDrop(Option<oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }

    #[tokio::test]
    async fn cancellation_joins_processing_and_invalidates_its_borrowed_client() {
        let (mut engine, _bus) = engine().await;
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let guard = NotifyDrop(Some(dropped_tx));
        let (context_dropped, context_done) = oneshot::channel();
        let context_guard = NotifyDrop(Some(context_dropped));
        let capture = ContextCapture::spawn(async move {
            let _guard = context_guard;
            std::future::pending().await
        });
        let key = llm_key(0, 999);
        let cancelled = Arc::new(AtomicBool::new(false));
        engine.loaded_llm = Some(key.clone());
        engine.phase = Phase::Processing;
        engine.processing = Some(Processing {
            cancelled: cancelled.clone(),
            task: tokio::spawn(async move {
                let _guard = guard;
                let _capture = capture;
                std::future::pending::<()>().await;
            }),
            llm_key: Some(key),
        });
        engine.cancel_recording().await;
        assert!(dropped_rx.await.is_ok());
        tokio::time::timeout(Duration::from_secs(1), context_done)
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(engine.phase, Phase::Idle);
        assert!(engine.processing.is_none());
        assert!(engine.loaded_llm.is_none());

        // Results queued before cancellation cannot affect a newer capture.
        engine.phase = Phase::Recording {
            since: Instant::now(),
        };
        engine.partial_in_flight = true;
        engine
            .handle(Event::Processed {
                utterance: engine.utterance - 1,
                llm_key: None,
                llm: None,
                result: Err(anyhow::anyhow!("stale failure")),
                worker_gone: true,
            })
            .await;
        engine
            .handle(Event::PartialDone {
                utterance: engine.utterance - 1,
                result: Err(anyhow::anyhow!("stale preview")),
            })
            .await;
        assert!(matches!(engine.phase, Phase::Recording { .. }));
        assert!(engine.partial_in_flight);
        assert!(!engine.status.lock().unwrap().contains_key("last_notice"));
    }

    #[tokio::test]
    async fn shutdown_aborts_processing_instead_of_detaching_it() {
        let (mut engine, _bus) = engine().await;
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let guard = NotifyDrop(Some(dropped_tx));
        engine.processing = Some(Processing {
            cancelled: Arc::default(),
            llm_key: None,
            task: tokio::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            }),
        });
        drop(engine);
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn processing_consumes_ptt_and_manual_requests_without_starting_capture() {
        let (mut engine, _bus) = engine().await;
        let (audio, commands) = AudioHandle::test_handle();
        engine.audio = Some(audio);
        engine.phase = Phase::Processing;
        engine.cfg.hotkeys.mode = PttMode::Toggle;
        for pressed in [true, false, true, false] {
            engine
                .handle(Event::Key(RawKey {
                    device: 0,
                    code: evdev::KeyCode::KEY_CAPSLOCK.0,
                    pressed,
                }))
                .await;
            assert_eq!(engine.phase, Phase::Processing);
        }
        for request in [Request::Start, Request::Toggle, Request::Stop] {
            tokio::time::timeout(Duration::from_secs(1), engine.handle(Event::Dbus(request)))
                .await
                .unwrap();
            assert_eq!(engine.phase, Phase::Processing);
        }
        assert!(commands.try_recv().is_err());
        assert!(!engine.status.lock().unwrap().contains_key("last_notice"));
    }

    #[tokio::test]
    async fn processing_without_a_borrowed_client_does_not_block_cleanup_recovery() {
        let (mut engine, _bus) = engine().await;
        engine.cfg.cleanup.enabled = true;
        let key = llm_key(0, engine.cfg.cleanup.gpu_layers);
        engine.cfg.cleanup.model = key.path.to_string_lossy().into_owned();
        engine.loaded_llm = Some(key);
        engine.phase = Phase::Processing;
        engine.processing = Some(Processing {
            cancelled: Arc::default(),
            task: tokio::spawn(std::future::pending()),
            llm_key: None,
        });
        engine.ensure_models().await;
        // An absent client must be retried even while processing. This fixture
        // uses a missing file so the attempted recovery cannot spawn a worker.
        assert!(
            engine.status.lock().unwrap()["last_notice"].contains("localwispr-test-model.gguf")
        );
    }

    #[tokio::test]
    async fn stale_loads_cannot_replace_the_current_generation_or_gpu_settings() {
        let (mut engine, _bus) = engine().await;
        let old_stt = LoadKey {
            path: engine.cfg.transcription.model.clone().into(),
            settings: LoadSettings::Speech { use_gpu: true },
            generation: 0,
        };
        engine.loading_stt = Some(old_stt.clone());
        let mut cfg = engine.cfg.clone();
        cfg.transcription.use_gpu = false;
        cfg.cleanup.gpu_layers = 0;
        engine.apply_config(cfg).await;
        assert_eq!(engine.model_generation, 1);
        engine
            .handle(Event::SttLoaded(
                old_stt.clone(),
                Err(anyhow::anyhow!("obsolete STT")),
            ))
            .await;
        assert!(engine.loading_stt.is_none());
        assert!(engine.failed_stt.is_none());

        let current_stt = LoadKey {
            settings: LoadSettings::Speech { use_gpu: false },
            generation: 1,
            ..old_stt.clone()
        };
        engine.loading_stt = Some(current_stt.clone());
        engine
            .handle(Event::SttLoaded(old_stt, Err(anyhow::anyhow!("late STT"))))
            .await;
        let wrong_gpu = LoadKey {
            settings: LoadSettings::Speech { use_gpu: true },
            ..current_stt.clone()
        };
        engine
            .handle(Event::SttLoaded(
                wrong_gpu,
                Err(anyhow::anyhow!("wrong GPU")),
            ))
            .await;
        assert_eq!(engine.loading_stt, Some(current_stt));

        let old_llm = llm_key(0, 999);
        engine.loading_llm = Some(old_llm.clone());
        engine
            .handle(Event::LlmLoaded(
                old_llm.clone(),
                Err(anyhow::anyhow!("obsolete cleanup")),
            ))
            .await;
        assert!(engine.loading_llm.is_none());
        assert!(engine.failed_llm.is_none());
        let current_llm = llm_key(1, 0);
        engine.loading_llm = Some(current_llm.clone());
        engine
            .handle(Event::LlmLoaded(
                old_llm,
                Err(anyhow::anyhow!("late cleanup")),
            ))
            .await;
        engine
            .handle(Event::LlmLoaded(
                llm_key(1, 999),
                Err(anyhow::anyhow!("wrong layers")),
            ))
            .await;
        assert_eq!(engine.loading_llm, Some(current_llm));
        assert!(!engine.status.lock().unwrap()["last_notice"].contains("obsolete"));
    }
}
