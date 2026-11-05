//! Text injection: put the text on the clipboard, press the paste chord,
//! then restore whatever the user had on the clipboard before.
//!
//! Two routes:
//! * **GNOME** — Mutter offers no clipboard protocol to background clients and
//!   no virtual-keyboard protocol, so the companion Shell extension does the
//!   whole sequence in-compositor (`org.localwispr.Shell1.Inject`).
//! * **Everything else** — `ext/wlr-data-control` via wl-clipboard-rs (in
//!   process, no `wl-copy` binary) plus a `/dev/uinput` virtual keyboard.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use evdev::{AttributeSet, KeyCode, uinput::VirtualDevice};
use wl_clipboard_rs::{copy, paste};

use crate::config::{Output, PasteChord};
use crate::dbus::ShellProxy;

pub const VIRTUAL_DEVICE_NAME: &str = "localwispr virtual keyboard";
const MAX_SAVED_BYTES: usize = 32 << 20;

pub fn chord_keys(chord: PasteChord) -> &'static [KeyCode] {
    match chord {
        PasteChord::ShiftInsert => &[KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_INSERT],
        PasteChord::CtrlV => &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_V],
        PasteChord::CtrlShiftV => &[
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_V,
        ],
    }
}

pub fn chord_name(chord: PasteChord) -> &'static str {
    match chord {
        PasteChord::ShiftInsert => "shift-insert",
        PasteChord::CtrlV => "ctrl-v",
        PasteChord::CtrlShiftV => "ctrl-shift-v",
    }
}

pub struct Uinput {
    dev: VirtualDevice,
}

impl Uinput {
    /// Create once at startup: the compositor needs ~100–200 ms to adopt a new
    /// input device, which must not be paid on every dictation.
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for k in [
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_INSERT,
            KeyCode::KEY_V,
        ] {
            keys.insert(k);
        }
        let dev = VirtualDevice::builder()
            .context("opening /dev/uinput (run ./setup-permissions.sh)")?
            .name(VIRTUAL_DEVICE_NAME)
            .with_keys(&keys)?
            .build()
            .context("creating virtual keyboard")?;
        Ok(Self { dev })
    }

    pub fn press_chord(&mut self, keys: &[KeyCode]) -> Result<()> {
        let ev = |k: KeyCode, v: i32| *evdev::KeyEvent::new(k, v);
        let gap = Duration::from_millis(6);
        for &k in keys {
            self.dev.emit(&[ev(k, 1)])?;
            std::thread::sleep(gap);
        }
        for &k in keys.iter().rev() {
            self.dev.emit(&[ev(k, 0)])?;
            std::thread::sleep(gap);
        }
        Ok(())
    }
}

type Saved = Vec<(String, Vec<u8>)>;
type Backup = std::result::Result<Saved, String>;

pub enum Route {
    Gnome(ShellProxy<'static>),
    GnomeBound(ShellProxy<'static>, String),
    DataControl,
}

/// A D-Bus method keeps executing remotely when its caller future is dropped.
/// Explicitly revoke this request when processing is aborted. The request ID
/// prevents a late cancellation from affecting the next utterance.
struct ShellCancellation {
    shell: ShellProxy<'static>,
    request_id: String,
    armed: bool,
}

impl Drop for ShellCancellation {
    fn drop(&mut self) {
        if self.armed {
            let shell = self.shell.clone();
            let request_id = self.request_id.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        shell.cancel_inject(&request_id),
                    )
                    .await;
                });
            }
        }
    }
}

/// The extension refused or aborted a bound insertion before pasting because
/// the focus/caret/range changed or the token expired. Nothing was inserted.
pub fn is_stale_context(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(cause.downcast_ref::<zbus::Error>(),
            Some(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.localwispr.Shell1.Error.StaleContext")
    })
}

/// An older extension that predates the called method.
pub(crate) fn unknown_method(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _)
        if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod")
        || matches!(error, zbus::Error::FDO(inner)
            if matches!(inner.as_ref(), zbus::fdo::Error::UnknownMethod(_)))
}

async fn shell_inject(shell: &ShellProxy<'static>, text: &str, cfg: &Output) -> zbus::Result<bool> {
    shell_inject_with_handoff(shell, None, text, cfg).await
}

async fn shell_inject_with_handoff(
    shell: &ShellProxy<'static>,
    token: Option<&str>,
    text: &str,
    cfg: &Output,
) -> zbus::Result<bool> {
    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
    let mut cancellation = ShellCancellation {
        shell: shell.clone(),
        request_id: NEXT_REQUEST.fetch_add(1, Ordering::Relaxed).to_string(),
        armed: true,
    };
    let result = if let Some(token) = token {
        // A bound request may fail stale/unsupported; never replay this adapted
        // text unbound. The daemon re-reads context and re-formats instead.
        shell
            .inject_bound(
                &cancellation.request_id,
                token,
                text,
                chord_name(cfg.paste_chord),
                cfg.restore_clipboard,
                cfg.restore_delay_ms,
            )
            .await
    } else {
        shell
            .inject_cancellable(
                &cancellation.request_id,
                text,
                chord_name(cfg.paste_chord),
                cfg.restore_clipboard,
                cfg.restore_delay_ms,
            )
            .await
    };
    cancellation.armed = result
        .as_ref()
        .err()
        .is_some_and(|error| !unknown_method(error));
    match result {
        Err(ref error) if token.is_none() && unknown_method(error) => {
            // Keep old installed extensions usable until the next login loads
            // the update. They cannot provide remote cancellation guarantees.
            static REPORTED: AtomicBool = AtomicBool::new(false);
            if !REPORTED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "GNOME extension lacks cancellable insertion; update it and log in again"
                );
            }
            shell
                .inject(
                    text,
                    chord_name(cfg.paste_chord),
                    cfg.restore_clipboard,
                    cfg.restore_delay_ms,
                )
                .await
        }
        result => result,
    }
}

// Cancellation and keyboard submission serialize on the keyboard mutex. The
// final flag check is the commit point; cancellation acknowledgment also waits
// for this mutex, so an already-committed chord finishes before we report idle.
async fn press_when_current<T, F>(
    keyboard: Arc<std::sync::Mutex<T>>,
    cancelled: Arc<AtomicBool>,
    paste_done: tokio::sync::oneshot::Sender<()>,
    press: F,
) -> Result<()>
where
    T: Send + 'static,
    F: FnOnce(&mut T) -> Result<()> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut keyboard = keyboard.lock().unwrap();
        anyhow::ensure!(!cancelled.load(Ordering::Acquire), "dictation cancelled");
        let result = press(&mut keyboard);
        drop(paste_done);
        result
    })
    .await?
}

async fn wait_for_keyboard<T: Send + 'static>(keyboard: Arc<std::sync::Mutex<T>>) {
    let _ = tokio::task::spawn_blocking(move || {
        drop(keyboard.lock().unwrap());
    })
    .await;
}

pub struct Injector {
    uinput: Option<Arc<std::sync::Mutex<Uinput>>>,
    clipboard: Arc<Mutex<ClipboardState>>,
}

impl Injector {
    pub fn new(want_uinput: bool) -> Self {
        let uinput = if want_uinput {
            match Uinput::new() {
                Ok(u) => Some(Arc::new(std::sync::Mutex::new(u))),
                Err(e) => {
                    tracing::warn!("{e:#}");
                    None
                }
            }
        } else {
            None
        };
        Self {
            uinput,
            clipboard: Arc::default(),
        }
    }

    pub fn has_uinput(&self) -> bool {
        self.uinput.is_some()
    }

    pub async fn wait_for_keyboard_idle(&self) {
        if let Some(keyboard) = &self.uinput {
            wait_for_keyboard(keyboard.clone()).await;
        }
    }

    pub async fn inject(
        &mut self,
        text: &str,
        route: &Route,
        cfg: &Output,
        input: (&AtomicBool, &Arc<AtomicBool>),
        warn: impl Fn(String),
    ) -> Result<()> {
        match route {
            Route::Gnome(shell) | Route::GnomeBound(shell, _) => {
                crate::hotkey::wait_for_modifiers(input.0, input.1, Duration::ZERO).await?;
                // A D-Bus error means nothing was inserted; `false` means the
                // text was pasted but the clipboard could not be preserved.
                let preserved = match route {
                    Route::GnomeBound(_, token) => {
                        shell_inject_with_handoff(shell, Some(token), text, cfg).await
                    }
                    _ => shell_inject(shell, text, cfg).await,
                }
                .context("GNOME Shell extension")?;
                if !preserved && cfg.restore_clipboard {
                    warn("the previous clipboard could not be fully restored (too large, unreadable, or changed meanwhile)".into());
                }
                Ok(())
            }
            Route::DataControl => {
                let uinput = self.uinput.clone().ok_or_else(|| {
                    anyhow!("/dev/uinput unavailable — cannot press the paste keys")
                })?;
                let state = self.clipboard.clone();
                let owned = text.to_owned();
                let restore = cfg.restore_clipboard;
                let delay = Duration::from_millis(cfg.restore_delay_ms as u64);
                let (paste_done, pasted) = tokio::sync::oneshot::channel::<()>();
                let cancelled = input.1.clone();
                let warnings = tokio::task::spawn_blocking(move || {
                    let restore_state = state.clone();
                    let mut state = state.lock().unwrap();
                    let saved = if restore {
                        SELECTIONS.map(|selection| {
                            let previous = state
                                .pending
                                .as_ref()
                                .filter(|pending| {
                                    selection == paste::ClipboardType::Regular || pending.primary
                                })
                                .map(|pending| {
                                    let index = selection_index(selection);
                                    (&pending.saved[index], &pending.owned[index])
                                });
                            backup_for_next(previous, || save_clipboard(selection))
                        })
                    } else {
                        [Ok(Vec::new()), Ok(Vec::new())]
                    };
                    // Backups can take a while; a cancelled dictation must not
                    // replace the clipboard.
                    anyhow::ensure!(!cancelled.load(Ordering::Acquire), "dictation cancelled");
                    let owned = set_clipboard(&owned)?;
                    let primary = owned[1].is_some();
                    let warnings = backup_warnings(&saved, restore, primary);
                    state.generation += 1;
                    let generation = state.generation;
                    state.pending = restore.then_some(PendingRestore {
                        generation,
                        owned,
                        saved,
                        primary,
                    });
                    // Set up restoration before returning from the blocking
                    // task, so cancelling injection cannot strand our text.
                    if restore {
                        tokio::spawn(async move {
                            let _ = pasted.await;
                            tokio::time::sleep(delay).await;
                            let _ = tokio::task::spawn_blocking(move || {
                                let mut state = restore_state.lock().unwrap();
                                let Some(pending) = state.pending.as_ref() else {
                                    return;
                                };
                                if pending.generation != generation {
                                    return;
                                }
                                let pending = state.pending.take().unwrap();
                                for ((selection, saved), owned) in SELECTIONS
                                    .into_iter()
                                    .zip(pending.saved)
                                    .zip(&pending.owned)
                                {
                                    if let (Ok(saved), Some(owned)) = (saved, owned) {
                                        restore_clipboard(selection, saved, owned);
                                    }
                                }
                            })
                            .await;
                        });
                    }
                    Ok::<_, anyhow::Error>(warnings)
                })
                .await??;
                for message in warnings {
                    warn(message);
                }

                let keys = chord_keys(cfg.paste_chord);
                async move {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    crate::hotkey::wait_for_modifiers(input.0, input.1, Duration::ZERO).await?;
                    // Restoration waits for `paste_done`, which the blocking
                    // task holds until the keys are pressed: aborting this
                    // future cannot stop a chord already being sent.
                    press_when_current(uinput, input.1.clone(), paste_done, move |keyboard| {
                        keyboard.press_chord(keys)
                    })
                    .await
                }
                .await
            }
        }
    }
}

/// Our dictation's ownership of one selection: set once another client
/// takes the selection over (the source is cancelled and stops serving).
type Owned = Arc<AtomicBool>;

/// Sets CLIPBOARD and, where supported, PRIMARY (Shift+Insert pastes PRIMARY
/// in some terminals, CLIPBOARD elsewhere).
fn set_clipboard(text: &str) -> Result<[Option<Owned>; 2]> {
    let regular = serve_selection(copy::ClipboardType::Regular, text)
        .context("setting clipboard (compositor lacks data-control?)")?;
    let primary = serve_selection(copy::ClipboardType::Primary, text)
        .inspect_err(|e| tracing::debug!("PRIMARY selection unavailable ({e}); CLIPBOARD only"))
        .ok();
    Ok([Some(regular), primary])
}

fn serve_selection(clipboard: copy::ClipboardType, text: &str) -> Result<Owned> {
    let taken = Owned::default();
    let flag = taken.clone();
    let data: Box<[u8]> = text.as_bytes().into();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    // PreparedCopy is not Send: prepare it on the thread that serves it.
    std::thread::Builder::new()
        .name("clipboard-serve".into())
        .spawn(move || {
            let mut opts = copy::Options::new();
            // prepare_copy asserts foreground mode; this thread is the server.
            opts.foreground(true);
            opts.clipboard(clipboard);
            match opts.prepare_copy(copy::Source::Bytes(data), copy::MimeType::Text) {
                Ok(prepared) => {
                    let _ = tx.send(Ok(()));
                    // Returns once another client owns the selection.
                    let _ = prepared.serve();
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
            flag.store(true, Ordering::Release);
        })?;
    rx.recv()
        .map_err(|_| anyhow!("clipboard thread exited"))??;
    Ok(taken)
}

fn backup_warnings(saved: &[Backup; 2], restore: bool, primary: bool) -> Vec<String> {
    if !restore {
        return Vec::new();
    }
    saved.iter().take(if primary { 2 } else { 1 }).enumerate()
        .filter_map(|(index, saved)| saved.as_ref().err().map(|reason| {
            let selection = if index == 0 { "clipboard" } else { "PRIMARY selection" };
            format!("Could not preserve {selection}: {reason}. Dictation will still be inserted; the previous contents cannot be restored.")
        })).collect()
}

const SELECTIONS: [paste::ClipboardType; 2] =
    [paste::ClipboardType::Regular, paste::ClipboardType::Primary];

#[derive(Default)]
struct ClipboardState {
    generation: u64,
    pending: Option<PendingRestore>,
}

struct PendingRestore {
    generation: u64,
    owned: [Option<Owned>; 2],
    saved: [Backup; 2],
    primary: bool,
}

fn still_ours(owned: &Option<Owned>) -> bool {
    owned
        .as_ref()
        .is_some_and(|taken| !taken.load(Ordering::Acquire))
}

fn selection_index(selection: paste::ClipboardType) -> usize {
    usize::from(selection == paste::ClipboardType::Primary)
}

/// While a previous dictation still owns the selection, its backup is the
/// user's real clipboard; otherwise the user copied something new.
fn backup_for_next(
    previous: Option<(&Backup, &Option<Owned>)>,
    snapshot: impl FnOnce() -> Backup,
) -> Backup {
    if let Some((saved, owned)) = previous
        && still_ours(owned)
    {
        return saved.clone();
    }
    snapshot()
}

/// Unlike a timed-out reader thread, this owns and closes the transfer FD on
/// every exit. The deadline covers the entire transfer, including partial data.
fn read_pipe_deadline(mut pipe: impl Read + AsRawFd, deadline: Instant) -> Result<Vec<u8>> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd belongs to pipe for the duration of this function.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("configuring clipboard transfer");
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("clipboard transfer exceeded its time limit (300 ms for a backup)");
        }
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(buf),
            Ok(n) => {
                if buf.len() + n > MAX_SAVED_BYTES {
                    anyhow::bail!("clipboard exceeds the 32 MiB backup limit");
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    anyhow::bail!(
                        "clipboard transfer exceeded its time limit (300 ms for a backup)"
                    );
                }
                let mut pollfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let millis = remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32;
                // SAFETY: one live pollfd is provided; poll does not retain it.
                let rc = unsafe { libc::poll(&mut pollfd, 1, millis) };
                if rc < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(std::io::Error::last_os_error())
                        .context("polling clipboard transfer");
                }
            }
            Err(e) => return Err(e).context("reading clipboard transfer"),
        }
    }
}

fn read_selection(
    selection: paste::ClipboardType,
    mime: &str,
    deadline: Instant,
) -> Result<Vec<u8>> {
    if Instant::now() >= deadline {
        anyhow::bail!("clipboard backup exceeded 300 ms");
    }
    let (pipe, _) = paste::get_contents(
        selection,
        paste::Seat::Unspecified,
        paste::MimeType::Specific(mime),
    )
    .with_context(|| format!("reading clipboard MIME {mime}"))?;
    read_pipe_deadline(pipe, deadline)
}

fn save_clipboard(selection: paste::ClipboardType) -> Backup {
    snapshot_clipboard(selection).map_err(|e| format!("{e:#}"))
}

fn snapshot_clipboard(selection: paste::ClipboardType) -> Result<Saved> {
    let deadline = Instant::now() + Duration::from_millis(300);
    let mimes = match paste::get_mime_types(selection, paste::Seat::Unspecified) {
        Ok(m) => m,
        Err(paste::Error::ClipboardEmpty) => return Ok(Vec::new()),
        Err(e) => return Err(e).context("listing clipboard MIME types"),
    };
    save_mimes(mimes.into_iter().collect(), |mime| {
        read_selection(selection, mime, deadline)
    })
}

fn save_mimes(
    mut mimes: Vec<String>,
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Saved> {
    let mut saved = Vec::new();
    let mut total = 0;
    mimes.sort();
    for mime in mimes {
        if mime.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            continue;
        }
        let data = read(&mime).with_context(|| format!("backing up MIME {mime}"))?;
        total += data.len();
        if total > MAX_SAVED_BYTES {
            anyhow::bail!("clipboard exceeds the 32 MiB backup limit");
        }
        saved.push((mime, data));
    }
    Ok(saved)
}

fn restore_clipboard(selection: paste::ClipboardType, saved: Saved, owned: &Owned) {
    // Anything copied since our paste (even the same text) must survive.
    if owned.load(Ordering::Acquire) {
        return;
    }
    let clipboard = match selection {
        paste::ClipboardType::Regular => copy::ClipboardType::Regular,
        paste::ClipboardType::Primary => copy::ClipboardType::Primary,
    };
    let res = if saved.is_empty() {
        copy::clear(clipboard, copy::Seat::All)
    } else {
        let sources = saved
            .into_iter()
            .map(|(mime, data)| copy::MimeSource {
                source: copy::Source::Bytes(data.into()),
                mime_type: copy::MimeType::Specific(mime),
            })
            .collect();
        let mut opts = copy::Options::new();
        opts.clipboard(clipboard);
        opts.copy_multi(sources)
    };
    if let Err(e) = res {
        tracing::debug!("clipboard restore failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn queued_keyboard_work_cannot_paste_after_cancellation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        rt.block_on(async {
            let keyboard = Arc::new(std::sync::Mutex::new(0usize));
            let cancelled = Arc::new(AtomicBool::new(false));
            let (release, blocked) = std::sync::mpsc::channel();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
            });
            ready.await.unwrap();
            let (paste_done, _pasted) = tokio::sync::oneshot::channel();
            let (queued, has_queued) = tokio::sync::oneshot::channel();
            let task_keyboard = keyboard.clone();
            let task_cancelled = cancelled.clone();
            let task = tokio::spawn(async move {
                queued.send(()).unwrap();
                press_when_current(task_keyboard, task_cancelled, paste_done, |presses| {
                    *presses += 1;
                    Ok(())
                })
                .await
            });
            has_queued.await.unwrap();
            cancelled.store(true, Ordering::Release);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            blocker.await.unwrap();
            wait_for_keyboard(keyboard.clone()).await;
            assert_eq!(*keyboard.lock().unwrap(), 0);
        });
    }

    #[tokio::test]
    async fn cancellation_acknowledgment_waits_for_an_already_committed_chord() {
        let keyboard = Arc::new(std::sync::Mutex::new(0usize));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (paste_done, _pasted) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(press_when_current(
            keyboard.clone(),
            cancelled.clone(),
            paste_done,
            move |presses| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                *presses += 1;
                Ok(())
            },
        ));
        ready.await.unwrap();
        cancelled.store(true, Ordering::Release);
        task.abort();
        let _ = task.await;
        let acknowledgment = wait_for_keyboard(keyboard.clone());
        tokio::pin!(acknowledgment);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut acknowledgment)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        acknowledgment.await;
        assert_eq!(*keyboard.lock().unwrap(), 1);
    }

    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[tokio::test]
    async fn aborting_shell_injection_revokes_only_its_remote_request() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        use tokio::sync::{Notify, mpsc};

        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Shell {
            started: mpsc::UnboundedSender<String>,
            cancelled: mpsc::UnboundedSender<String>,
            release: Arc<Notify>,
        }
        #[zbus::interface(name = "org.localwispr.Shell1")]
        impl Shell {
            async fn inject_cancellable(
                &self,
                request_id: String,
                _text: &str,
                _chord: &str,
                _restore: bool,
                _delay: u32,
            ) -> bool {
                self.started.send(request_id).unwrap();
                self.release.notified().await;
                true
            }
            async fn inject_bound(
                &self,
                request_id: String,
                token: &str,
                text: &str,
                chord: &str,
                restore: bool,
                delay: u32,
            ) -> zbus::fdo::Result<bool> {
                match token {
                    "unsupported" => {
                        return Err(zbus::fdo::Error::UnknownMethod("InjectBound".into()));
                    }
                    "stale" => return Err(zbus::fdo::Error::Failed("stale context".into())),
                    "handoff-token" => {}
                    _ => panic!("wrong context token"),
                }
                Ok(self
                    .inject_cancellable(request_id, text, chord, restore, delay)
                    .await)
            }
            fn cancel_inject(&self, request_id: String) {
                self.cancelled.send(request_id).unwrap();
                self.release.notify_one();
            }
        }
        let mut process = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = Bus(process);
        let (started, mut starts) = mpsc::unbounded_channel();
        let (cancelled, mut cancels) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let _server = zbus::connection::Builder::address(address.trim())
            .name("org.localwispr.Shell")
            .serve_at(
                "/org/localwispr/Shell",
                Shell {
                    started,
                    cancelled,
                    release: release.clone(),
                },
            )
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .build()
            .await
            .unwrap();
        let shell = ShellProxy::new(&client).await.unwrap();
        let task =
            tokio::spawn(
                async move { shell_inject(&shell, "cancel me", &Output::default()).await },
            );
        let id = tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let revoked = tokio::time::timeout(Duration::from_secs(2), cancels.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id, revoked);

        let shell = ShellProxy::new(&client).await.unwrap();
        let next =
            tokio::spawn(async move { shell_inject(&shell, "keep me", &Output::default()).await });
        let next_id = tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(id, next_id);
        release.notify_one();
        assert!(next.await.unwrap().unwrap());
        tokio::task::yield_now().await;
        assert!(cancels.try_recv().is_err());

        let shell = ShellProxy::new(&client).await.unwrap();
        let error =
            shell_inject_with_handoff(&shell, Some("unsupported"), "adapted", &Output::default())
                .await
                .unwrap_err();
        assert!(unknown_method(&error));
        assert!(
            starts.try_recv().is_err(),
            "bound insertion must never retry through an unbound method"
        );

        let bound = tokio::spawn(async move {
            shell_inject_with_handoff(&shell, Some("handoff-token"), "bound", &Output::default())
                .await
        });
        let bound_id = tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .unwrap()
            .unwrap();
        bound.abort();
        assert!(bound.await.unwrap_err().is_cancelled());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), cancels.recv())
                .await
                .unwrap()
                .unwrap(),
            bound_id
        );

        let shell = ShellProxy::new(&client).await.unwrap();
        let completed = tokio::spawn(async move {
            shell_inject_with_handoff(&shell, Some("handoff-token"), "bound", &Output::default())
                .await
        });
        let completed_id = tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(bound_id, completed_id);
        release.notify_one();
        assert!(completed.await.unwrap().unwrap());
        assert!(cancels.try_recv().is_err());

        let shell = ShellProxy::new(&client).await.unwrap();
        assert!(
            shell_inject_with_handoff(&shell, Some("stale"), "adapted", &Output::default())
                .await
                .is_err()
        );
        assert!(
            starts.try_recv().is_err(),
            "stale context must never retry through an unbound method"
        );
        // The best-effort revocation on an errored call is request-scoped too.
        let stale_id = tokio::time::timeout(Duration::from_secs(2), cancels.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(stale_id, completed_id);
    }

    fn owned(taken: bool) -> Option<Owned> {
        Some(Arc::new(AtomicBool::new(taken)))
    }

    #[test]
    fn ownership_follows_our_source_not_the_text() {
        assert!(still_ours(&owned(false)));
        // Taken over, even by a copy of the very same text.
        assert!(!still_ours(&owned(true)));
        // Never set (e.g. PRIMARY unsupported).
        assert!(!still_ours(&None));
    }

    #[test]
    fn overlapping_dictations_keep_original_selection_independently() {
        let regular = vec![("text/plain".into(), b"clipboard X".to_vec())];
        let primary = vec![("text/plain".into(), b"primary Y".to_vec())];
        let pending = PendingRestore {
            generation: 1,
            owned: [owned(false), owned(false)],
            saved: [Ok(regular.clone()), Ok(primary.clone())],
            primary: true,
        };
        for (index, original) in [regular, primary].into_iter().enumerate() {
            let saved =
                backup_for_next(Some((&pending.saved[index], &pending.owned[index])), || {
                    panic!("must not snapshot A as the original")
                });
            assert_eq!(saved, Ok(original));
        }
    }

    #[test]
    fn unknown_original_is_not_replaced_by_a_previous_dictation() {
        assert_eq!(
            backup_for_next(
                Some((&Err("transfer timed out".into()), &owned(false))),
                || { panic!("must not preserve previous dictation as original") }
            ),
            Err("transfer timed out".into())
        );
    }

    #[test]
    fn new_user_copy_becomes_the_next_backup() {
        let old = Ok(vec![("text/plain".into(), b"X".to_vec())]);
        let image = vec![("image/png".into(), vec![137, 80, 78, 71])];
        assert_eq!(
            backup_for_next(Some((&old, &owned(true))), || Ok(image.clone())),
            Ok(image)
        );
    }

    #[test]
    fn stalled_transfer_times_out_and_closes_reader() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"partial").unwrap();
        let start = Instant::now();
        assert!(
            read_pipe_deadline(reader, start + Duration::from_millis(30))
                .unwrap_err()
                .to_string()
                .contains("time limit")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        // Parallel tests may fork dbus-daemon while this socket exists. Its
        // pre-exec child can briefly inherit the descriptor until CLOEXEC.
        // Verify eventual peer closure without a scheduler-dependent instant.
        writer.set_nonblocking(true).unwrap();
        let closure_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match writer.write_all(b"more") {
                Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => break,
                _ => {
                    assert!(
                        Instant::now() < closure_deadline,
                        "expired reader remained open"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    fn completed_transfer_returns_all_bytes() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"hello").unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(
            read_pipe_deadline(reader, Instant::now() + Duration::from_secs(1)).unwrap(),
            b"hello".to_vec()
        );
    }

    #[test]
    fn failed_backups_are_visible_only_for_selections_we_overwrite() {
        let saved = [
            Err("MIME image/png timed out".into()),
            Err("PRIMARY unsupported".into()),
        ];
        assert!(backup_warnings(&saved, false, true).is_empty());
        let warnings = backup_warnings(&saved, true, false);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("image/png timed out"));
        assert_eq!(backup_warnings(&saved, true, true).len(), 2);
        assert!(backup_warnings(&[Ok(Vec::new()), Ok(Vec::new())], true, true).is_empty());
    }

    #[test]
    fn one_failed_mime_discards_the_entire_backup() {
        let mut reads = 0;
        let result = save_mimes(vec!["text/plain".into(), "image/png".into()], |_| {
            reads += 1;
            if reads == 2 {
                anyhow::bail!("read failed");
            }
            Ok(b"partial backup".to_vec())
        });
        assert_eq!(reads, 2);
        assert!(
            format!("{:#}", result.unwrap_err())
                .contains("backing up MIME text/plain: read failed")
        );
    }

    #[test]
    fn combined_mime_size_cannot_exceed_backup_limit() {
        let result = save_mimes(vec!["image/png".into(), "image/bmp".into()], |_| {
            Ok(vec![0; MAX_SAVED_BYTES / 2 + 1])
        });
        assert!(result.unwrap_err().to_string().contains("32 MiB"));
    }
}
