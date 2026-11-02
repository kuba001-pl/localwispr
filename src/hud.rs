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
const BARS_H: f64 = 26.0;
const BAR_MIN: f64 = 4.0;
const CAPSULE_W: i32 = 220;
const CAPSULE_MAX_W: i32 = 420;
const CAPSULE_PAD_X: i32 = 20;
const WAVEFORM_W: i32 = 180;
const SHADOW_MARGIN: i32 = 18;
const TEXT_LINES: i32 = 2;

/// Exact critically damped motion preserves velocity across new destinations.
/// It starts gently and behaves consistently at different frame rates.
fn spring(
    value: f64,
    velocity: f64,
    target: f64,
    dt: f64,
    frequency: f64,
    precision: f64,
) -> (f64, f64) {
    let offset = value - target;
    let step = (velocity + frequency * offset) * dt;
    let decay = (-frequency * dt).exp();
    let next = target + (offset + step) * decay;
    let speed = (velocity - frequency * step) * decay;
    if (next - target).abs() < precision && speed.abs() < precision * 12.0 {
        (target, 0.0)
    } else {
        (next, speed)
    }
}

fn capsule_width(text_width: i32, limit: i32) -> i32 {
    let limit = limit.clamp(2 * CAPSULE_PAD_X + 1, CAPSULE_MAX_W);
    text_width
        .saturating_add(2 * CAPSULE_PAD_X)
        .clamp(CAPSULE_W.min(limit), limit)
}

fn animations_enabled() -> bool {
    gtk4::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

/// The newest words that fit in TEXT_LINES lines of `width` px, cut at a word
/// boundary ("… ask what you can do for your country.") so the preview never
/// shows half a word.
fn text_tail(label: &gtk4::Label, text: &str, width: i32) -> String {
    let layout = label.create_pango_layout(None);
    layout.set_wrap(gtk4::pango::WrapMode::WordChar);
    layout.set_width(width * gtk4::pango::SCALE);
    fitting_text_tail(text, |t| {
        layout.set_text(t);
        layout.line_count() <= TEXT_LINES
    })
}

fn fitting_text_tail(text: &str, fits: impl Fn(&str) -> bool) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let words: Vec<&str> = text.split(' ').collect();
    if fits(&text) {
        return text;
    }
    let last = words.last().copied().unwrap_or_default();
    if words.len() < 2 || !fits(&format!("… {last}")) {
        // Preserve the newest characters of an oversized URL or unbroken
        // transcript, with every slice on a valid Unicode boundary.
        let chars: Vec<char> = last.chars().collect();
        let tail = |i: usize| format!("… {}", chars[i..].iter().collect::<String>());
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if fits(&tail(mid)) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        return tail(lo);
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
    opacity_velocity: f64,
    time: f64,
    last_frame: Option<i64>,
    notice_gen: u64,
    ticking: bool,
    pending_notice: Option<String>,
    daemon_state: String,
    raw_text: String,
    text_is_notice: bool,
    width: f64,
    target_width: f64,
    width_velocity: f64,
}

impl State {
    /// False means the frame callback can be removed until the next show.
    fn advance(&mut self, dt: f64, animate: bool) -> bool {
        if animate {
            self.time += dt;
        }
        for i in 0..self.current.len() {
            let target = self.target[i];
            let rate = if target > self.current[i] { 20.0 } else { 12.0 };
            let k = 1.0 - (-dt * rate).exp();
            self.current[i] += (target - self.current[i]) * k;
        }
        let target_op = if matches!(self.mode, Mode::Hidden | Mode::FadingOut) {
            0.0
        } else {
            1.0
        };
        if animate {
            if target_op > 0.0 {
                (self.opacity, self.opacity_velocity) = spring(
                    self.opacity,
                    self.opacity_velocity,
                    target_op,
                    dt,
                    20.0,
                    0.002,
                );
            } else {
                // Finish before the daemon inserts text, preserving its latency.
                self.opacity += (target_op - self.opacity) * (1.0 - (-dt * 65.0).exp());
                self.opacity_velocity = 0.0;
            }
            (self.width, self.width_velocity) = spring(
                self.width,
                self.width_velocity,
                self.target_width,
                dt,
                12.0,
                0.08,
            );
        } else {
            self.opacity = target_op;
            self.opacity_velocity = 0.0;
            self.width = self.target_width;
            self.width_velocity = 0.0;
        }
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
            border-radius: 24px;
            box-shadow: 0 6px 20px rgba(0, 0, 0, 0.28);
            padding: 10px {pad}px;
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

    let capsule = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    capsule.add_css_class("lw-capsule");
    // GTK size requests include the CSS margins reserved for the shadow.
    capsule.set_size_request(CAPSULE_W + 2 * SHADOW_MARGIN, -1);
    let label = gtk4::Label::new(None);
    label.add_css_class("lw-live");
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label.set_justify(gtk4::Justification::Center);
    // Keep the label's natural width from overriding the animated capsule.
    label.set_max_width_chars(1);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.set_lines(TEXT_LINES);
    label.set_margin_bottom(6);
    let text_layout = label.create_pango_layout(Some("Ag\nAg"));
    label.set_height_request(text_layout.pixel_size().1);
    let text_revealer = gtk4::Revealer::new();
    text_revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    text_revealer.set_transition_duration(280);
    text_revealer.set_child(Some(&label));
    let area = gtk4::DrawingArea::new();
    area.set_content_height(BARS_H as i32);
    area.set_content_width(WAVEFORM_W);
    area.set_halign(gtk4::Align::Center);
    capsule.append(&text_revealer);
    capsule.append(&area);
    win.set_child(Some(&capsule));

    let state = Rc::new(RefCell::new(State {
        cfg,
        mode: Mode::Hidden,
        target: vec![0.0; bands],
        current: vec![0.0; bands],
        opacity: 0.0,
        opacity_velocity: 0.0,
        time: 0.0,
        last_frame: None,
        notice_gen: 0,
        ticking: false,
        pending_notice: None,
        daemon_state: "idle".into(),
        raw_text: String::new(),
        text_is_notice: false,
        width: CAPSULE_W as f64,
        target_width: CAPSULE_W as f64,
        width_velocity: 0.0,
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
        text_revealer,
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
    text_revealer: gtk4::Revealer,
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
        if !animations_enabled() {
            s.opacity = 1.0;
        }
        drop(s);
        self.win.set_opacity(self.state.borrow().opacity);
        self.apply_geometry();
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
            if !s.advance(dt, animations_enabled()) {
                drop(s);
                widget.set_visible(false);
                return glib::ControlFlow::Break;
            }
            let opacity = s.opacity;
            drop(s);
            widget.set_opacity(opacity);
            ui.apply_geometry();
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
        let daemon_state = s.daemon_state.clone();
        drop(s);
        self.provider.load_from_string(&css(&cfg));
        self.win
            .set_margin(Edge::Bottom, cfg.bottom_margin.min(i32::MAX as u32) as i32);
        let (text, notice) = {
            let s = self.state.borrow();
            (s.raw_text.clone(), s.text_is_notice)
        };
        self.set_text(&text, notice);
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
                    let compact = capsule_width(0, self.width_limit()) as f64;
                    {
                        let mut s = self.state.borrow_mut();
                        s.width = compact;
                        s.target_width = compact;
                        s.width_velocity = 0.0;
                    }
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

    fn width_limit(&self) -> i32 {
        self.win
            .surface()
            .and_then(|surface| WidgetExt::display(&self.win).monitor_at_surface(&surface))
            .map(|monitor| monitor.geometry().width() - 2 * SHADOW_MARGIN)
            .unwrap_or(CAPSULE_MAX_W)
            .clamp(2 * CAPSULE_PAD_X + 1, CAPSULE_MAX_W)
    }

    fn apply_geometry(&self) {
        let limit = self.width_limit() as f64;
        let (width, opacity, margin) = {
            let mut s = self.state.borrow_mut();
            if s.width > limit {
                s.width = limit;
                s.width_velocity = 0.0;
            }
            s.target_width = s.target_width.min(limit);
            (s.width.round() as i32, s.opacity, s.cfg.bottom_margin)
        };
        if self.capsule.width_request() != width + 2 * SHADOW_MARGIN {
            self.capsule.set_size_request(width + 2 * SHADOW_MARGIN, -1);
            self.area
                .set_content_width(WAVEFORM_W.min(width - 2 * CAPSULE_PAD_X));
            self.render_text(width);
        }
        let shift = if animations_enabled() {
            ((1.0 - opacity) * 6.0).round() as u32
        } else {
            0
        };
        self.win.set_margin(
            Edge::Bottom,
            margin.saturating_sub(shift).min(i32::MAX as u32) as i32,
        );
    }

    fn render_text(&self, width: i32) {
        let tail = text_tail(
            &self.label,
            &self.state.borrow().raw_text,
            (width - 2 * CAPSULE_PAD_X).max(1),
        );
        if self.label.text().as_str() != tail {
            self.label.set_text(&tail);
        }
    }

    fn set_text(&self, text: &str, notice: bool) {
        if notice {
            self.label.remove_css_class("lw-live");
            self.label.add_css_class("lw-notice");
        } else {
            self.label.remove_css_class("lw-notice");
            self.label.add_css_class("lw-live");
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let show_text = (self.state.borrow().cfg.show_live_text || notice) && !text.is_empty();
        let layout = self.label.create_pango_layout(Some(&text));
        layout.set_width(-1);
        let wanted = capsule_width(
            if show_text { layout.pixel_size().0 } else { 0 },
            self.width_limit(),
        ) as f64;
        {
            let mut s = self.state.borrow_mut();
            s.raw_text = text;
            s.text_is_notice = notice;
            s.target_width =
                if show_text && !notice && matches!(s.mode, Mode::Recording | Mode::Processing) {
                    s.target_width.max(wanted).min(self.width_limit() as f64)
                } else {
                    wanted
                };
            if s.mode == Mode::Hidden || !animations_enabled() {
                s.width = s.target_width;
                s.width_velocity = 0.0;
            }
        }
        self.text_revealer.set_reveal_child(show_text);
        self.apply_geometry();
        self.render_text(self.state.borrow().width.round() as i32);
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
    let natural = (n * 2) as f64 * (BAR_W + BAR_GAP) - BAR_GAP;
    let scale = (w / natural).min(1.0);
    let bar_w = BAR_W * scale;
    let step = (BAR_W + BAR_GAP) * scale;
    let total = natural * scale;
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
        let x = x0 + slot as f64 * step;
        let y = mid - bh / 2.0;
        let r = bar_w / 2.0;
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
            opacity_velocity: 0.0,
            time: 0.0,
            last_frame: None,
            notice_gen: 0,
            ticking: true,
            pending_notice: None,
            daemon_state: "recording".into(),
            raw_text: String::new(),
            text_is_notice: false,
            width: CAPSULE_W as f64,
            target_width: CAPSULE_W as f64,
            width_velocity: 0.0,
        }
    }

    #[test]
    fn fading_stops_frames_and_resets_timing() {
        let mut s = state();
        s.last_frame = Some(100);
        s.mode = Mode::FadingOut;
        let mut frames = 0;
        while s.advance(1.0 / 60.0, true) {
            frames += 1;
            assert!(
                frames <= 4,
                "fade must finish within the daemon dissolve bound"
            );
        }
        assert_eq!(s.mode, Mode::Hidden);
        assert_eq!(s.opacity, 0.0);
        assert!(!s.ticking);
        assert_eq!(s.last_frame, None);
        assert!(!s.advance(1.0 / 60.0, true));
    }

    #[test]
    fn compact_width_grows_with_text_and_fits_narrow_monitors() {
        assert_eq!(capsule_width(0, CAPSULE_MAX_W), 220);
        assert_eq!(capsule_width(190, CAPSULE_MAX_W), 230);
        assert_eq!(capsule_width(2000, CAPSULE_MAX_W), 420);
        assert_eq!(capsule_width(2000, 180), 180);
        assert_eq!(capsule_width(i32::MAX, CAPSULE_MAX_W), 420);
    }

    #[test]
    fn transcript_tail_preserves_newest_words_and_unicode_characters() {
        let fits = |text: &str| text.chars().count() <= 15;
        assert_eq!(fitting_text_tail("  a   few words  ", fits), "a few words");
        assert_eq!(
            fitting_text_tail("one two three four five six", fits),
            "… four five six"
        );
        let token = format!("{}終わり😀", "長".repeat(80));
        for text in [token.clone(), format!("earlier words {token}")] {
            let tail = fitting_text_tail(&text, fits);
            assert!(fits(&tail));
            assert!(tail.starts_with("… "));
            assert!(tail.ends_with("終わり😀"));
        }
    }

    #[test]
    fn width_animation_converges_without_overshoot_at_different_frame_rates() {
        for hz in [30, 60, 144] {
            let mut s = state();
            s.target_width = CAPSULE_MAX_W as f64;
            for _ in 0..hz {
                let before = s.width;
                assert!(s.advance(1.0 / hz as f64, true));
                assert!(s.width >= before && s.width <= s.target_width);
            }
            assert_eq!(s.width, s.target_width);
        }
    }

    #[test]
    fn width_starts_gently_and_retargeting_preserves_momentum() {
        let mut s = state();
        s.target_width = 320.0;
        s.advance(1.0 / 60.0, true);
        assert!(s.width > 220.0 && s.width < 224.0);
        for _ in 0..10 {
            s.advance(1.0 / 60.0, true);
        }
        let velocity = s.width_velocity;
        assert!(velocity > 0.0);
        s.target_width = 420.0;
        assert_eq!(s.width_velocity, velocity);
        let width = s.width;
        s.advance(1.0 / 60.0, true);
        assert!(s.width - width > 1.0);
        assert!(s.width_velocity > 0.0);
    }

    #[test]
    fn spring_positions_match_across_frame_rates_before_settling() {
        let mut positions = Vec::new();
        for hz in [30, 60, 144] {
            let (mut width, mut velocity) = (220.0, 0.0);
            for _ in 0..hz {
                (width, velocity) = spring(width, velocity, 420.0, 0.3 / hz as f64, 12.0, 0.08);
            }
            positions.push(width);
        }
        for width in positions.iter().skip(1) {
            assert!((width - positions[0]).abs() < 1e-8);
        }
    }

    #[test]
    fn reduced_motion_snaps_geometry_and_finishes_exit_immediately() {
        let mut s = state();
        s.opacity = 0.0;
        s.target_width = 340.0;
        assert!(s.advance(1.0 / 60.0, false));
        assert_eq!(s.width, 340.0);
        assert_eq!(s.opacity, 1.0);
        s.mode = Mode::FadingOut;
        assert!(!s.advance(1.0 / 60.0, false));
        assert_eq!(s.mode, Mode::Hidden);
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
