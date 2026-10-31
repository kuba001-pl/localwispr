//! Layer-shell HUD (KDE Plasma, Hyprland, Sway, COSMIC, …).
//!
//! A pure view: it subscribes to the daemon's D-Bus signals and draws the
//! capsule. Click-through, never takes focus, unmapped while idle.
//! Namespace `localwispr` — e.g. Hyprland: `layerrule = blur, localwispr`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Result, bail};
use futures_util::{FutureExt, StreamExt};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::{Config, FrontendChoice, Hud as HudCfg};
use crate::dbus::DaemonProxy;

const BAR_W: f64 = 4.0;
const BAR_GAP: f64 = 4.0;
const BARS_H: f64 = 34.0;
const BAR_MIN: f64 = 4.0;
const CAPSULE_W: i32 = 420;
const CAPSULE_PAD_X: i32 = 24;
const TEXT_LINES: i32 = 2;

fn capsule_width(bands: usize) -> i32 {
    CAPSULE_W.max((bands * 2) as i32 * (BAR_W + BAR_GAP) as i32 + 2 * CAPSULE_PAD_X)
}

/// The newest words that fit in TEXT_LINES lines of `width` px, cut at a word
/// boundary ("… ask what you can do for your country.") so the preview never
/// shows half a word.
fn text_tail(label: &gtk4::Label, text: &str, width: i32) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let layout = label.create_pango_layout(None);
    layout.set_wrap(gtk4::pango::WrapMode::WordChar);
    layout.set_width(width * gtk4::pango::SCALE);
    let fits = |t: &str| {
        layout.set_text(t);
        layout.line_count() <= TEXT_LINES
    };
    let words: Vec<&str> = text.split(' ').collect();
    if words.len() < 2 || fits(&text) {
        return text; // a single overlong word wraps by character instead
    }
    let tail = |i: usize| format!("… {}", words[i..].join(" "));
    // Smallest start index whose tail fits.
    let (mut lo, mut hi) = (1, words.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(&tail(mid)) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    tail(lo)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Hidden,
    Recording,
    Processing,
    Notice,
    FadingOut,
}

struct State {
    cfg: HudCfg,
    mode: Mode,
    target: Vec<f64>,
    current: Vec<f64>,
    opacity: f64,
    time: f64,
    last_frame: Option<i64>,
    notice_gen: u64,
    ticking: bool,
    pending_notice: Option<String>,
    daemon_state: String,
}

impl State {
    /// False means the frame callback can be removed until the next show.
    fn advance(&mut self, dt: f64) -> bool {
        self.time += dt;
        let k = 1.0 - (-dt * 20.0).exp();
        for i in 0..self.current.len() {
            let target = self.target[i];
            self.current[i] += (target - self.current[i]) * k;
        }
        let target_op = if matches!(self.mode, Mode::Hidden | Mode::FadingOut) {
            0.0
        } else {
            1.0
        };
        let rate = if target_op > self.opacity { 14.0 } else { 22.0 };
        self.opacity += (target_op - self.opacity) * (1.0 - (-dt * rate).exp());
        if self.mode == Mode::Hidden || (self.mode == Mode::FadingOut && self.opacity < 0.02) {
            self.mode = Mode::Hidden;
            self.opacity = 0.0;
            self.ticking = false;
            self.last_frame = None;
            return false;
        }
        true
    }

    fn configure(&mut self, cfg: HudCfg) -> bool {
        if self.cfg == cfg {
            return false;
        }
        let bands = cfg.bands as usize;
        if self.current.len() != bands {
            self.current = vec![0.0; bands];
            self.target = vec![0.0; bands];
        }
        self.cfg = cfg;
        true
    }
}

/// Layer-shell is only meaningful (and only safely queryable) on Wayland.
pub fn layer_shell_available() -> bool {
    gdk::Display::default().is_some_and(|d| d.type_().name() == "GdkWaylandDisplay")
        && gtk4_layer_shell::is_supported()
}

/// Set by the daemon, which keeps the HUD's stdin open for its lifetime.
pub const PARENT_PIPE_ENV: &str = "LOCALWISPR_HUD_PARENT_PIPE";

pub fn run() -> Result<()> {
    if std::env::var_os(PARENT_PIPE_ENV).is_some() {
        std::thread::spawn(|| {
            let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
            std::process::exit(0);
        });
    }
    gtk4::init()?;
    if !layer_shell_available() {
        bail!("this compositor does not support wlr-layer-shell");
    }
    let app = gtk4::Application::builder()
        .application_id("org.localwispr.Hud")
        .build();
    app.connect_activate(build);
    let code = app.run_with_args::<&str>(&[]);
    if code != glib::ExitCode::SUCCESS {
        bail!("HUD exited with {code:?}");
    }
    Ok(())
}

fn css(cfg: &HudCfg) -> String {
    format!(
        "window.lw-hud {{ background: transparent; }}
         .lw-capsule {{
            background-color: rgba(0, 0, 0, {op});
            border-radius: 28px;
            box-shadow: 0 8px 24px rgba(0, 0, 0, 0.32);
            padding: 10px {pad}px 12px {pad}px;
            margin: 18px;
         }}
         .lw-live {{ color: rgba(255, 255, 255, 0.62); font-style: italic; font-size: 13px; }}
         .lw-notice {{ color: rgba(255, 196, 180, 0.95); font-size: 13px; }}",
        op = cfg.opacity,
        pad = CAPSULE_PAD_X,
    )
}

fn build(app: &gtk4::Application) {
    let cfg = Config::load_or_default().hud;
    let bands = cfg.bands as usize;

    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&css(&cfg));
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let win = gtk4::Window::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .build();
    win.add_css_class("lw-hud");
    win.init_layer_shell();
    win.set_namespace(Some("localwispr"));
    win.set_layer(Layer::Overlay);
    win.set_anchor(Edge::Bottom, true);
    win.set_margin(Edge::Bottom, cfg.bottom_margin.min(i32::MAX as u32) as i32);
    win.set_keyboard_mode(KeyboardMode::None);

    let capsule = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    capsule.add_css_class("lw-capsule");
    capsule.set_size_request(capsule_width(bands), -1);
    let label = gtk4::Label::new(None);
    label.add_css_class("lw-live");
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label.set_justify(gtk4::Justification::Center);
    label.set_visible(false);
    let area = gtk4::DrawingArea::new();
    area.set_content_height(BARS_H as i32);
    area.set_content_width(((bands * 2) as f64 * (BAR_W + BAR_GAP)) as i32);
    area.set_halign(gtk4::Align::Center);
    capsule.append(&label);
    capsule.append(&area);
    win.set_child(Some(&capsule));

    let state = Rc::new(RefCell::new(State {
        cfg,
        mode: Mode::Hidden,
        target: vec![0.0; bands],
        current: vec![0.0; bands],
        opacity: 0.0,
        time: 0.0,
        last_frame: None,
        notice_gen: 0,
        ticking: false,
        pending_notice: None,
        daemon_state: "idle".into(),
    }));

    // Click-through: an empty input region on every map.
    win.connect_map(|w| {
        if let Some(surface) = w.surface() {
            surface.set_input_region(Some(&gtk4::cairo::Region::create()));
        }
    });

    {
        let state = state.clone();
        area.set_draw_func(move |_, cr, w, h| draw(&state.borrow(), cr, w as f64, h as f64));
    }

    let ui = Ui {
        win: win.clone(),
        capsule,
        label,
        area,
        provider,
        state,
    };
    glib::spawn_future_local(async move {
        loop {
            if let Err(e) = listen(&ui).await {
                tracing::warn!("HUD lost the daemon ({e}); retrying");
            }
            ui.reset();
            glib::timeout_future(Duration::from_secs(2)).await;
        }
    });

    // No visible window while idle: keep the application alive explicitly.
    std::mem::forget(app.hold());
}

#[derive(Clone)]
struct Ui {
    win: gtk4::Window,
    capsule: gtk4::Box,
    label: gtk4::Label,
    area: gtk4::DrawingArea,
    provider: gtk4::CssProvider,
    state: Rc<RefCell<State>>,
}

impl Ui {
    fn show(&self, mode: Mode) {
        let mut s = self.state.borrow_mut();
        if !frontend_enabled(s.cfg.frontend) {
            return;
        }
        if matches!(s.mode, Mode::Hidden | Mode::FadingOut) {
            s.current.iter_mut().for_each(|v| *v = 0.0);
            s.target.iter_mut().for_each(|v| *v = 0.0);
            s.last_frame = None;
        }
        s.mode = mode;
        drop(s);
        self.win.set_visible(true);
        self.start_animation();
    }

    fn start_animation(&self) {
        if self.state.borrow().ticking {
            return;
        }
        self.state.borrow_mut().ticking = true;
        let ui = self.clone();
        self.win.add_tick_callback(move |widget, clock| {
            let mut s = ui.state.borrow_mut();
            let now = clock.frame_time();
            let dt = s
                .last_frame
                .map(|t| (now - t) as f64 / 1e6)
                .unwrap_or(1.0 / 60.0)
                .min(0.1);
            s.last_frame = Some(now);
            if !s.advance(dt) {
                drop(s);
                widget.set_visible(false);
                return glib::ControlFlow::Break;
            }
            widget.set_opacity(s.opacity);
            ui.area.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    fn reset(&self) {
        let mut s = self.state.borrow_mut();
        s.daemon_state = "idle".into();
        s.pending_notice = None;
        s.notice_gen += 1;
        s.target.fill(0.0);
        drop(s);
        self.hide();
    }

    fn apply_config(&self, cfg: HudCfg) {
        let mut s = self.state.borrow_mut();
        if !s.configure(cfg.clone()) {
            return;
        }
        let bands = cfg.bands as usize;
        let daemon_state = s.daemon_state.clone();
        drop(s);
        self.area
            .set_content_width(((bands * 2) as f64 * (BAR_W + BAR_GAP)) as i32);
        self.capsule.set_size_request(capsule_width(bands), -1);
        self.provider.load_from_string(&css(&cfg));
        self.win
            .set_margin(Edge::Bottom, cfg.bottom_margin.min(i32::MAX as u32) as i32);
        if self.state.borrow().mode != Mode::Notice {
            self.label
                .set_visible(cfg.show_live_text && !self.label.text().is_empty());
        }
        self.set_daemon_state(&daemon_state);
    }

    fn set_daemon_state(&self, state: &str) {
        self.state.borrow_mut().daemon_state = state.into();
        if !frontend_enabled(self.state.borrow().cfg.frontend) {
            self.hide();
            return;
        }
        match state {
            "recording" => {
                if self.state.borrow().mode != Mode::Recording {
                    self.set_text("", false);
                }
                self.show(Mode::Recording);
            }
            "processing" => self.show(Mode::Processing),
            _ => {
                let pending = self.state.borrow_mut().pending_notice.take();
                if let Some(message) = pending {
                    self.hide();
                    self.notice(&message);
                } else if self.state.borrow().mode != Mode::Notice {
                    self.hide();
                }
            }
        }
    }

    fn notice(&self, message: &str) {
        if notice_should_wait(self.state.borrow().mode) {
            self.state.borrow_mut().pending_notice = Some(message.into());
            return;
        }
        self.set_text(message, true);
        self.show(Mode::Notice);
        let generation = {
            let mut s = self.state.borrow_mut();
            s.notice_gen += 1;
            s.notice_gen
        };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            glib::timeout_future(Duration::from_millis(2800)).await;
            let still = {
                let s = ui.state.borrow();
                s.mode == Mode::Notice && s.notice_gen == generation
            };
            if still {
                ui.hide();
            }
        });
    }

    fn hide(&self) {
        let mut s = self.state.borrow_mut();
        if s.mode != Mode::Hidden {
            s.mode = Mode::FadingOut;
        }
    }

    fn set_text(&self, text: &str, notice: bool) {
        let (show_text, bands) = {
            let s = self.state.borrow();
            (s.cfg.show_live_text || notice, s.cfg.bands as usize)
        };
        if notice {
            self.label.set_text(text);
        } else {
            let width = capsule_width(bands) - 2 * CAPSULE_PAD_X;
            self.label.set_text(&text_tail(&self.label, text, width));
        }
        self.label.set_visible(show_text && !text.is_empty());
        if notice {
            self.label.remove_css_class("lw-live");
            self.label.add_css_class("lw-notice");
        } else {
            self.label.remove_css_class("lw-notice");
            self.label.add_css_class("lw-live");
        }
    }
}

fn frontend_enabled(frontend: FrontendChoice) -> bool {
    matches!(frontend, FrontendChoice::Auto | FrontendChoice::LayerShell)
}

fn notice_should_wait(mode: Mode) -> bool {
    matches!(mode, Mode::Recording | Mode::Processing)
}

async fn reload_appearance(ui: &Ui, proxy: &DaemonProxy<'_>) -> zbus::Result<()> {
    let (frontend, bands, bottom_margin, show_live_text, opacity) = proxy.get_hud_config().await?;
    let frontend = match frontend.as_str() {
        "auto" => FrontendChoice::Auto,
        "layer-shell" => FrontendChoice::LayerShell,
        "gnome" => FrontendChoice::Gnome,
        _ => FrontendChoice::None,
    };
    ui.apply_config(HudCfg {
        frontend,
        bands: bands.clamp(4, 48),
        bottom_margin,
        show_live_text,
        opacity: opacity.clamp(0.0, 1.0),
    });
    Ok(())
}

async fn listen(ui: &Ui) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = DaemonProxy::new(&conn).await?;
    let mut owners = proxy.inner().receive_owner_changed().await?;
    let mut states = proxy.receive_state_changed().await?;
    let mut levels = proxy.receive_levels().await?;
    let mut texts = proxy.receive_partial_text().await?;
    let mut notices = proxy.receive_notice().await?;
    // Subscribe before the snapshot: queued transitions are applied afterward.
    reload_appearance(ui, &proxy).await?;
    let status = proxy.get_status().await?;
    ui.set_daemon_state(status.get("state").map(String::as_str).unwrap_or("idle"));
    let refresh = glib::timeout_future(Duration::from_secs(1)).fuse();
    futures_util::pin_mut!(refresh);
    loop {
        futures_util::select! {
            owner = owners.next().fuse() => {
                if !matches!(owner, Some(Some(_))) { ui.reset(); return Ok(()); }
            }
            _ = refresh => {
                reload_appearance(ui, &proxy).await?;
                refresh.set(glib::timeout_future(Duration::from_secs(1)).fuse());
            }
            sig = states.next() => {
                let Some(sig) = sig else { return Ok(()) };
                ui.set_daemon_state(sig.args()?.state());
            }
            sig = levels.next() => {
                let Some(sig) = sig else { return Ok(()) };
                let args = sig.args()?;
                let mut s = ui.state.borrow_mut();
                s.target.fill(0.0);
                let n = s.target.len().min(args.bands().len());
                s.target[..n].copy_from_slice(&args.bands()[..n]);
            }
            sig = texts.next() => {
                let Some(sig) = sig else { return Ok(()) };
                let mode = ui.state.borrow().mode;
                if matches!(mode, Mode::Recording | Mode::Processing) {
                    ui.set_text(sig.args()?.text(), false);
                }
            }
            sig = notices.next() => {
                let Some(sig) = sig else { return Ok(()) };
                let args = sig.args()?;
                if *args.level() != "info" { ui.notice(args.message()); }
            }
        }
    }
}

fn draw(s: &State, cr: &gtk4::cairo::Context, w: f64, h: f64) {
    let n = s.current.len();
    if n == 0 {
        return;
    }
    let total = (n * 2) as f64 * (BAR_W + BAR_GAP) - BAR_GAP;
    let x0 = (w - total) / 2.0;
    let mid = h / 2.0;
    for slot in 0..n * 2 {
        // Mirror: lowest band in the centre, highest at both ends.
        let band = if slot < n { n - 1 - slot } else { slot - n };
        let (level, alpha) = match s.mode {
            Mode::Processing => {
                // Travelling shimmer while the transcript is being polished.
                let phase = s.time * 7.0 - slot as f64 * 0.45;
                let wave = 0.5 + 0.5 * phase.sin();
                (0.08 + 0.18 * wave, 0.35 + 0.55 * wave)
            }
            Mode::Notice => (0.0, 0.35),
            _ => (s.current[band].clamp(0.0, 1.0), 0.92),
        };
        let bh = BAR_MIN + level * (h - BAR_MIN);
        let x = x0 + slot as f64 * (BAR_W + BAR_GAP);
        let y = mid - bh / 2.0;
        let r = BAR_W / 2.0;
        cr.new_sub_path();
        cr.arc(x + r, y + r, r, std::f64::consts::PI, 0.0);
        cr.arc(x + r, y + bh - r, r, 0.0, std::f64::consts::PI);
        cr.close_path();
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        let _ = cr.fill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            cfg: HudCfg::default(),
            mode: Mode::Recording,
            target: vec![0.0; 16],
            current: vec![0.0; 16],
            opacity: 1.0,
            time: 0.0,
            last_frame: None,
            notice_gen: 0,
            ticking: true,
            pending_notice: None,
            daemon_state: "recording".into(),
        }
    }

    #[test]
    fn fading_stops_frames_and_resets_timing() {
        let mut s = state();
        s.last_frame = Some(100);
        s.mode = Mode::FadingOut;
        let mut frames = 0;
        while s.advance(1.0 / 60.0) {
            frames += 1;
            assert!(
                frames < 30,
                "fade must finish within the daemon dissolve bound"
            );
        }
        assert_eq!(s.mode, Mode::Hidden);
        assert_eq!(s.opacity, 0.0);
        assert!(!s.ticking);
        assert_eq!(s.last_frame, None);
        assert!(!s.advance(1.0 / 60.0));
    }

    #[test]
    fn appearance_resize_discards_stale_bands_in_both_directions() {
        let mut s = state();
        s.current.fill(0.7);
        s.target.fill(0.9);
        let mut cfg = s.cfg.clone();
        cfg.bands = 4;
        cfg.opacity = 0.25;
        cfg.bottom_margin = 100;
        assert!(s.configure(cfg.clone()));
        assert_eq!(s.current, vec![0.0; 4]);
        assert_eq!(s.target, vec![0.0; 4]);
        assert_eq!(s.cfg, cfg);
        assert!(!s.configure(cfg.clone()));
        cfg.bands = 48;
        assert!(s.configure(cfg));
        assert_eq!(s.current, vec![0.0; 48]);
        assert_eq!(s.target, vec![0.0; 48]);
    }

    #[test]
    fn errors_replace_fading_capsules_but_wait_for_dictation() {
        assert!(notice_should_wait(Mode::Recording));
        assert!(notice_should_wait(Mode::Processing));
        assert!(!notice_should_wait(Mode::FadingOut));
        assert!(!notice_should_wait(Mode::Hidden));
        assert!(!notice_should_wait(Mode::Notice));
    }

    #[test]
    fn explicit_frontend_selection_gates_layer_shell() {
        assert!(frontend_enabled(FrontendChoice::Auto));
        assert!(frontend_enabled(FrontendChoice::LayerShell));
        assert!(!frontend_enabled(FrontendChoice::Gnome));
        assert!(!frontend_enabled(FrontendChoice::None));
    }
}
