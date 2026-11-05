//! Settings window (GTK4 + libadwaita).
//!
//! Every change is written to config.toml (debounced) and the running daemon
//! is asked to reload, so settings apply live.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use anyhow::Result;
use futures_util::{FutureExt, StreamExt};
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;

use crate::config::{Accuracy, Config, FrontendChoice, PasteChord, PttMode};
use crate::dbus::DaemonProxy;
use crate::hotkey::pretty_chord;
use crate::models::{self, ModelKind};

const LANGUAGES: &[(&str, &str)] = &[
    ("pl", "Polish"),
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("uk", "Ukrainian"),
    ("ru", "Russian"),
    ("cs", "Czech"),
    ("sk", "Slovak"),
    ("sv", "Swedish"),
    ("no", "Norwegian"),
    ("da", "Danish"),
    ("fi", "Finnish"),
    ("hu", "Hungarian"),
    ("ro", "Romanian"),
    ("el", "Greek"),
    ("tr", "Turkish"),
    ("he", "Hebrew"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
];

struct Ctx {
    cfg: RefCell<Config>,
    writable: bool,
    app: adw::Application,
    capturing: Cell<bool>,
    capture_buttons: RefCell<Vec<gtk::Button>>,
    save_timer: Cell<Option<glib::SourceId>>,
    daemon: RefCell<Option<DaemonProxy<'static>>>,
    toasts: adw::ToastOverlay,
    /// Download progress bars keyed by model id.
    progress: RefCell<HashMap<String, gtk::ProgressBar>>,
    downloads: RefCell<HashSet<String>>,
    model_rows: RefCell<Vec<(String, adw::ActionRow, gtk::Button)>>,
}

type Shared = Rc<Ctx>;

impl Ctx {
    fn update(self: &Rc<Self>, f: impl FnOnce(&mut Config)) {
        if !self.writable {
            self.toast("Fix config.toml and reopen Settings before editing");
            return;
        }
        f(&mut self.cfg.borrow_mut());
        if let Some(id) = self.save_timer.take() {
            id.remove();
        }
        let me = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(350), move || {
            me.save_timer.set(None);
            me.save_now();
        });
        self.save_timer.set(Some(id));
    }

    fn save_now(self: &Rc<Self>) {
        if !self.writable {
            return;
        }
        if let Err(e) = self.cfg.borrow().save() {
            self.toast(&format!("Could not save settings: {e:#}"));
            return;
        }
        let me = self.clone();
        let hold = self.app.hold();
        glib::spawn_future_local(async move {
            let _hold = hold;
            let daemon = me.daemon.borrow().clone();
            // A close can happen before connect_daemon has finished subscribing.
            let result = async {
                if let Some(d) = daemon {
                    return d.reload_config().await;
                }
                let conn = zbus::Connection::session().await?;
                DaemonProxy::new(&conn).await?.reload_config().await
            }
            .await;
            if result.is_err() {
                me.toast("Saved — the daemon is not running, changes apply on next start");
            }
        });
    }

    fn toast(&self, msg: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(msg).timeout(4).build());
    }
}

pub fn run() -> Result<()> {
    let app = adw::Application::builder()
        .application_id("org.localwispr.Settings")
        .build();
    app.connect_activate(|app| {
        // Single instance: a second launch just raises the window.
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        build(app);
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn build(app: &adw::Application) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);
    let toasts = adw::ToastOverlay::new();
    let (cfg, load_error) = match Config::load() {
        Ok(cfg) => (cfg, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    let ctx: Shared = Rc::new(Ctx {
        cfg: RefCell::new(cfg),
        writable: load_error.is_none(),
        app: app.clone(),
        capturing: Cell::new(false),
        capture_buttons: RefCell::default(),
        save_timer: Cell::new(None),
        daemon: RefCell::new(None),
        toasts: toasts.clone(),
        progress: RefCell::default(),
        downloads: RefCell::default(),
        model_rows: RefCell::default(),
    });

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(
        &dictation_page(&ctx),
        Some("dictation"),
        "Dictation",
        "audio-input-microphone-symbolic",
    );
    stack.add_titled_with_icon(
        &transcription_page(&ctx),
        Some("transcription"),
        "Speech",
        "accessories-dictionary-symbolic",
    );
    stack.add_titled_with_icon(
        &cleanup_page(&ctx),
        Some("cleanup"),
        "Cleanup",
        "edit-clear-all-symbolic",
    );
    stack.add_titled_with_icon(
        &output_page(&ctx),
        Some("output"),
        "Output",
        "preferences-desktop-display-symbolic",
    );
    stack.add_titled_with_icon(
        &models_page(&ctx),
        Some("models"),
        "Models",
        "folder-download-symbolic",
    );

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    header.add_css_class("flat");
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    toasts.set_child(Some(&stack));
    view.set_content(Some(&toasts));

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("localwispr")
        .default_width(680)
        .default_height(720)
        .content(&view)
        .build();

    let esc = gtk::EventControllerKey::new();
    {
        let win = win.clone();
        let ctx = ctx.clone();
        esc.connect_key_pressed(move |_, key, _, _| {
            // While capturing a hotkey, Escape may be the key being assigned.
            if key == gtk::gdk::Key::Escape && !ctx.capturing.get() {
                win.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    win.add_controller(esc);
    {
        let ctx = ctx.clone();
        win.connect_close_request(move |_| {
            // Flush a pending debounced save.
            if let Some(id) = ctx.save_timer.take() {
                id.remove();
                ctx.save_now();
            }
            glib::Propagation::Proceed
        });
    }
    if let Some(error) = load_error {
        stack.set_sensitive(false);
        let dialog = adw::AlertDialog::builder()
            .heading("Could not load settings")
            .body(format!(
                "{error}\n\nSettings are read-only. Fix config.toml and reopen this window."
            ))
            .build();
        dialog.add_response("close", "Close");
        dialog.present(Some(&win));
    }
    win.present();

    connect_daemon(ctx);
}

fn connect_daemon(ctx: Shared) {
    glib::spawn_future_local(async move {
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let Ok(proxy) = DaemonProxy::new(&conn).await else {
            return;
        };
        let Ok(mut owners) = proxy.inner().receive_owner_changed().await else {
            return;
        };
        let Ok(mut progress) = proxy.receive_download_progress().await else {
            return;
        };
        let Ok(mut finished) = proxy.receive_download_finished().await else {
            return;
        };
        *ctx.daemon.borrow_mut() = Some(proxy.clone());
        if let Ok(status) = proxy.get_status().await {
            for id in status
                .get("downloading")
                .into_iter()
                .flat_map(|s| s.split(','))
            {
                if !id.is_empty() {
                    ctx.downloads.borrow_mut().insert(id.to_owned());
                }
            }
            refresh_model_rows(&ctx);
        }
        loop {
            futures_util::select! {
                owner = owners.next().fuse() => {
                    match owner {
                        Some(Some(_)) => { *ctx.daemon.borrow_mut() = Some(proxy.clone()); }
                        _ => {
                            ctx.downloads.borrow_mut().clear();
                            *ctx.daemon.borrow_mut() = None;
                            for bar in ctx.progress.borrow().values() { bar.set_visible(false); }
                            refresh_model_rows(&ctx);
                            if owner.is_none() { break; }
                        }
                    }
                }
                sig = progress.next() => {
                    let Some(sig) = sig else { break };
                    let Ok(a) = sig.args() else { continue };
                    ctx.downloads.borrow_mut().insert(a.model().to_string());
                    refresh_model_rows(&ctx);
                    if let Some(bar) = ctx.progress.borrow().get(*a.model()) {
                        bar.set_visible(true);
                        bar.set_fraction(*a.done() as f64 / (*a.total()).max(1) as f64);
                        bar.set_text(Some(if a.done() >= a.total() { "Verifying…" } else { "Downloading…" }));
                        bar.set_show_text(true);
                    }
                }
                sig = finished.next() => {
                    let Some(sig) = sig else { break };
                    let Ok(a) = sig.args() else { continue };
                    ctx.downloads.borrow_mut().remove(*a.model());
                    if let Some(bar) = ctx.progress.borrow().get(*a.model()) { bar.set_visible(false); }
                    refresh_model_rows(&ctx);
                    if !a.ok() { ctx.toast(&format!("Download failed: {}", a.error())); }
                }
            }
        }
        ctx.downloads.borrow_mut().clear();
        *ctx.daemon.borrow_mut() = None;
        for bar in ctx.progress.borrow().values() {
            bar.set_visible(false);
        }
        refresh_model_rows(&ctx);
    });
}

// ----------------------------------------------------------------- widgets --

fn group(title: &str, desc: Option<&str>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    if let Some(d) = desc {
        g.set_description(Some(d));
    }
    g
}

fn switch_row(
    ctx: &Shared,
    title: &str,
    subtitle: &str,
    get: impl Fn(&Config) -> bool,
    set: impl Fn(&mut Config, bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    row.set_active(get(&ctx.cfg.borrow()));
    let ctx = ctx.clone();
    row.connect_active_notify(move |r| {
        let v = r.is_active();
        ctx.update(|c| set(c, v));
    });
    row
}

#[allow(clippy::too_many_arguments)]
fn spin_row(
    ctx: &Shared,
    title: &str,
    subtitle: &str,
    (min, max, step): (f64, f64, f64),
    digits: u32,
    get: impl Fn(&Config) -> f64,
    set: impl Fn(&mut Config, f64) + 'static,
) -> adw::SpinRow {
    let adj = gtk::Adjustment::new(get(&ctx.cfg.borrow()), min, max, step, step * 10.0, 0.0);
    let row = adw::SpinRow::builder()
        .title(title)
        .subtitle(subtitle)
        .adjustment(&adj)
        .digits(digits)
        .build();
    let ctx = ctx.clone();
    row.connect_value_notify(move |r| {
        let v = r.value();
        ctx.update(|c| set(c, v));
    });
    row
}

fn combo_row(
    ctx: &Shared,
    title: &str,
    subtitle: &str,
    items: &[&str],
    selected: usize,
    set: impl Fn(&mut Config, usize) + 'static,
) -> adw::ComboRow {
    let model = gtk::StringList::new(items);
    let row = adw::ComboRow::builder()
        .title(title)
        .subtitle(subtitle)
        .model(&model)
        .build();
    row.set_selected(selected as u32);
    let ctx = ctx.clone();
    row.connect_selected_notify(move |r| {
        let i = r.selected() as usize;
        ctx.update(|c| set(c, i));
    });
    row
}

fn entry_row(
    ctx: &Shared,
    title: &str,
    get: impl Fn(&Config) -> String,
    set: impl Fn(&mut Config, String) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    row.set_text(&get(&ctx.cfg.borrow()));
    // Every edit goes to the shared config; its debounced save (flushed on
    // window close) writes it, so closing without pressing Enter loses nothing.
    let ctx = ctx.clone();
    row.connect_changed(move |r| {
        let v = r.text().to_string();
        ctx.update(|c| set(c, v));
    });
    row
}

fn hotkey_row(
    ctx: &Shared,
    title: &str,
    subtitle: &str,
    get: impl Fn(&Config) -> Vec<String> + 'static,
    set: impl Fn(&mut Config, Vec<String>) + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    let button = gtk::Button::with_label(&pretty_chord(&get(&ctx.cfg.borrow())));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("pill");
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    ctx.capture_buttons.borrow_mut().push(button.clone());
    let ctx = ctx.clone();
    let set = Rc::new(set);
    let get = Rc::new(get);
    button.connect_clicked(move |b| {
        let Some(daemon) = ctx.daemon.borrow().clone() else {
            ctx.toast("Start the localwispr daemon to record hotkeys");
            return;
        };
        if ctx.capturing.replace(true) {
            return;
        }
        for button in ctx.capture_buttons.borrow().iter() {
            button.set_sensitive(false);
        }
        b.set_label("Press keys…");
        b.add_css_class("suggested-action");
        let (b, ctx, set, get) = (b.clone(), ctx.clone(), set.clone(), get.clone());
        glib::spawn_future_local(async move {
            let keys = daemon.capture_hotkey(8000).await.unwrap_or_default();
            ctx.capturing.set(false);
            for button in ctx.capture_buttons.borrow().iter() {
                button.set_sensitive(true);
            }
            b.remove_css_class("suggested-action");
            if keys.is_empty() {
                b.set_label(&pretty_chord(&get(&ctx.cfg.borrow())));
                ctx.toast("No key pressed");
                return;
            }
            b.set_label(&pretty_chord(&keys));
            ctx.update(|c| set(c, keys));
        });
    });
    row
}

fn page(groups: &[adw::PreferencesGroup]) -> adw::PreferencesPage {
    let p = adw::PreferencesPage::new();
    for g in groups {
        p.add(g);
    }
    p
}

// ------------------------------------------------------------------- pages --

fn dictation_page(ctx: &Shared) -> adw::PreferencesPage {
    let keys = group(
        "Hotkeys",
        Some(
            "Read directly from your keyboard, so they work in every app. With Caps Lock, run setup-permissions.sh to make the key inert.",
        ),
    );
    keys.add(&hotkey_row(
        ctx,
        "Push to talk",
        "Hold to dictate",
        |c| c.hotkeys.push_to_talk.clone(),
        |c, k| c.hotkeys.push_to_talk = k,
    ));
    let mode = match ctx.cfg.borrow().hotkeys.mode {
        PttMode::Hold => 0,
        PttMode::Toggle => 1,
    };
    keys.add(&combo_row(
        ctx,
        "Mode",
        "Hold the key, or press once to start and again to stop",
        &["Hold", "Toggle"],
        mode,
        |c, i| {
            c.hotkeys.mode = if i == 1 {
                PttMode::Toggle
            } else {
                PttMode::Hold
            }
        },
    ));
    keys.add(&hotkey_row(
        ctx,
        "Open settings",
        "",
        |c| c.hotkeys.open_settings.clone(),
        |c, k| c.hotkeys.open_settings = k,
    ));
    keys.add(&hotkey_row(
        ctx,
        "Cancel dictation",
        "Discard the current recording",
        |c| c.hotkeys.cancel.clone(),
        |c, k| c.hotkeys.cancel = k,
    ));
    keys.add(&spin_row(
        ctx,
        "Ignore taps shorter than",
        "Milliseconds",
        (0.0, 2000.0, 50.0),
        0,
        |c| c.hotkeys.min_hold_ms as f64,
        |c, v| c.hotkeys.min_hold_ms = v as u32,
    ));

    let mic = group("Microphone", None);
    let mic_row = adw::ComboRow::builder().title("Input device").build();
    mic.add(&mic_row);
    populate_microphones(ctx, &mic_row);
    mic.add(&switch_row(
        ctx,
        "Keep microphone open",
        "Off: the mic is only opened while you hold the key. On: pre-roll and no clipped first word, but the mic is always live",
        |c| c.audio.keep_warm,
        |c, v| c.audio.keep_warm = v,
    ));
    mic.add(&spin_row(
        ctx,
        "Pre-roll",
        "Audio kept from just before the key press, in ms (only with Keep microphone open)",
        (0.0, 1500.0, 50.0),
        0,
        |c| c.audio.preroll_ms as f64,
        |c, v| c.audio.preroll_ms = v as u32,
    ));

    page(&[keys, mic])
}

fn populate_microphones(ctx: &Shared, row: &adw::ComboRow) {
    let ctx = ctx.clone();
    let row = row.clone();
    glib::spawn_future_local(async move {
        // Give connect_daemon a moment; fall back to local enumeration.
        glib::timeout_future(Duration::from_millis(150)).await;
        let daemon = ctx.daemon.borrow().clone();
        let mut devices: Vec<(String, String)> = match daemon {
            Some(d) => d
                .list_microphones()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, name, _)| (id, name))
                .collect(),
            None => Vec::new(),
        };
        if devices.is_empty() {
            devices = gtk::gio::spawn_blocking(|| {
                crate::audio::list_input_devices()
                    .into_iter()
                    .map(|d| (d.id, d.name))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
        }
        devices.insert(0, (String::new(), "System default".into()));
        let current = ctx.cfg.borrow().audio.device.clone();
        if !current.is_empty() && !devices.iter().any(|(id, _)| *id == current) {
            devices.push((current.clone(), format!("{current} (not connected)")));
        }
        let names: Vec<&str> = devices.iter().map(|(_, n)| n.as_str()).collect();
        row.set_model(Some(&gtk::StringList::new(&names)));
        let sel = devices
            .iter()
            .position(|(id, _)| *id == current)
            .unwrap_or(0);
        row.set_selected(sel as u32);
        let ids: Vec<String> = devices.into_iter().map(|(id, _)| id).collect();
        row.connect_selected_notify(move |r| {
            if let Some(id) = ids.get(r.selected() as usize).cloned() {
                ctx.update(|c| c.audio.device = id);
            }
        });
    });
}

fn model_combo(
    ctx: &Shared,
    kind: ModelKind,
    title: &str,
    current: &str,
    set: impl Fn(&mut Config, String) + 'static,
) -> adw::ComboRow {
    let specs: Vec<&models::ModelSpec> = models::of_kind(kind).collect();
    let mut labels: Vec<String> = specs
        .iter()
        .map(|m| {
            format!(
                "{}{}",
                m.label,
                if m.is_installed() { "" } else { "  (download)" }
            )
        })
        .collect();
    let mut ids: Vec<String> = specs.iter().map(|m| m.id.to_owned()).collect();
    if !ids.iter().any(|i| i == current) {
        labels.push(format!("Custom: {current}"));
        ids.push(current.to_owned());
    }
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = ids.iter().position(|i| i == current).unwrap_or(0);
    let row = adw::ComboRow::builder()
        .title(title)
        .subtitle(
            specs
                .get(sel)
                .map(|m| m.note)
                .unwrap_or("custom model file"),
        )
        .model(&gtk::StringList::new(&refs))
        .build();
    row.set_selected(sel as u32);
    let ctx = ctx.clone();
    row.connect_selected_notify(move |r| {
        let i = r.selected() as usize;
        if let Some(spec) = specs.get(i) {
            r.set_subtitle(spec.note);
            if !spec.is_installed() {
                ctx.toast(&format!(
                    "Downloading {} — it activates automatically when ready",
                    spec.label
                ));
            }
        }
        if let Some(id) = ids.get(i).cloned() {
            ctx.update(|c| set(c, id));
        }
    });
    row
}

fn transcription_page(ctx: &Shared) -> adw::PreferencesPage {
    let cfg = ctx.cfg.borrow().clone();
    let model = group("Speech model", None);
    model.add(&model_combo(
        ctx,
        ModelKind::Whisper,
        "Whisper model",
        &cfg.transcription.model,
        |c, id| c.transcription.model = id,
    ));
    model.add(&switch_row(
        ctx,
        "Use GPU",
        "CUDA acceleration",
        |c| c.transcription.use_gpu,
        |c, v| c.transcription.use_gpu = v,
    ));
    let acc = match cfg.transcription.accuracy {
        Accuracy::Fast => 0,
        Accuracy::Balanced => 1,
        Accuracy::Best => 2,
    };
    model.add(&combo_row(
        ctx,
        "Decoding",
        "Fast: greedy · Balanced: retries unclear audio · Best: beam search",
        &["Fast", "Balanced", "Best"],
        acc,
        |c, i| {
            c.transcription.accuracy =
                [Accuracy::Fast, Accuracy::Balanced, Accuracy::Best][i.min(2)]
        },
    ));

    let langs = group(
        "Languages",
        Some("Select one language to force it, several to auto-detect among them only."),
    );
    let any = adw::SwitchRow::builder()
        .title("Detect any language")
        .subtitle("All ~100 Whisper languages")
        .build();
    any.set_active(cfg.transcription.languages.is_empty());
    let expander = adw::ExpanderRow::builder()
        .title("Allowed languages")
        .expanded(!cfg.transcription.languages.is_empty())
        .build();
    expander.set_sensitive(!cfg.transcription.languages.is_empty());
    let checks: Rc<RefCell<Vec<(String, gtk::CheckButton)>>> = Rc::default();
    for (code, name) in LANGUAGES {
        let row = adw::ActionRow::builder()
            .title(*name)
            .subtitle(*code)
            .build();
        let check = gtk::CheckButton::new();
        check.set_active(cfg.transcription.languages.iter().any(|l| l == code));
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        expander.add_row(&row);
        checks.borrow_mut().push((code.to_string(), check));
    }
    // Languages set in config.toml that have no checkbox are kept.
    let extra_langs: Vec<String> = cfg
        .transcription
        .languages
        .iter()
        .filter(|l| !checks.borrow().iter().any(|(code, _)| code == *l))
        .cloned()
        .collect();
    let sync_langs = {
        let ctx = ctx.clone();
        let checks = checks.clone();
        let any = any.clone();
        Rc::new(move || {
            let selected: Vec<String> = if any.is_active() {
                Vec::new()
            } else {
                checks
                    .borrow()
                    .iter()
                    .filter(|(_, c)| c.is_active())
                    .map(|(code, _)| code.clone())
                    .chain(extra_langs.iter().cloned())
                    .collect()
            };
            if selected.is_empty() && !any.is_active() {
                any.set_active(true);
            }
            ctx.update(|c| c.transcription.languages = selected);
        })
    };
    for (_, check) in checks.borrow().iter() {
        let sync = sync_langs.clone();
        check.connect_toggled(move |_| sync());
    }
    {
        let expander = expander.clone();
        let checks = checks.clone();
        let sync = sync_langs.clone();
        any.connect_active_notify(move |a| {
            expander.set_sensitive(!a.is_active());
            expander.set_expanded(!a.is_active());
            if !a.is_active() && !checks.borrow().iter().any(|(_, c)| c.is_active()) {
                // Sensible starting point for a bilingual setup.
                for (code, c) in checks.borrow().iter() {
                    if code == "pl" || code == "en" {
                        c.set_active(true);
                    }
                }
            }
            sync();
        });
    }
    langs.add(&any);
    langs.add(&expander);

    let quality = group("Recognition", None);
    quality.add(&switch_row(
        ctx,
        "Voice activity detection",
        "Skips silence and noise — prevents phantom text",
        |c| c.transcription.vad,
        |c, v| c.transcription.vad = v,
    ));
    quality.add(&switch_row(
        ctx,
        "Live preview",
        "Show the transcript while you speak",
        |c| c.transcription.live_preview,
        |c, v| c.transcription.live_preview = v,
    ));
    quality.add(&spin_row(
        ctx,
        "Preview interval",
        "Milliseconds between live updates",
        (150.0, 3000.0, 50.0),
        0,
        |c| c.transcription.preview_interval_ms as f64,
        |c, v| c.transcription.preview_interval_ms = v as u32,
    ));
    quality.add(&entry_row(
        ctx,
        "Vocabulary hint (names, jargon)",
        |c| c.transcription.initial_prompt.clone(),
        |c, v| c.transcription.initial_prompt = v,
    ));
    quality.add(&spin_row(
        ctx,
        "CPU threads",
        "",
        (1.0, 32.0, 1.0),
        0,
        |c| c.transcription.threads as f64,
        |c, v| c.transcription.threads = v as u32,
    ));

    page(&[model, langs, quality])
}

fn cleanup_page(ctx: &Shared) -> adw::PreferencesPage {
    let cfg = ctx.cfg.borrow().clone();
    let main = group(
        "Polishing",
        Some(
            "A local LLM removes fillers and fixes punctuation. It never translates, and output that drifts from what you said is discarded.",
        ),
    );
    main.add(&switch_row(
        ctx,
        "Polish with LLM",
        "Off: insert raw Whisper text",
        |c| c.cleanup.enabled,
        |c, v| c.cleanup.enabled = v,
    ));
    main.add(&model_combo(
        ctx,
        ModelKind::Llm,
        "Cleanup model",
        &cfg.cleanup.model,
        |c, id| c.cleanup.model = id,
    ));
    main.add(&switch_row(
        ctx,
        "Polish the live preview",
        "The overlay shows the text that will be inserted (more GPU while dictating)",
        |c| c.cleanup.live_preview,
        |c, v| c.cleanup.live_preview = v,
    ));
    main.add(&switch_row(
        ctx,
        "Remove obvious fillers without LLM",
        "um, uh, yyy, eee… (also used as fallback)",
        |c| c.cleanup.remove_fillers,
        |c, v| c.cleanup.remove_fillers = v,
    ));
    main.add(&entry_row(
        ctx,
        "Extra instructions",
        |c| c.cleanup.custom_instructions.clone(),
        |c, v| c.cleanup.custom_instructions = v,
    ));

    let guard = group("Safety check", None);
    guard.add(&spin_row(
        ctx,
        "Max length change",
        "Fraction of words the LLM may add/remove",
        (0.05, 2.0, 0.05),
        2,
        |c| c.cleanup.max_length_drift as f64,
        |c, v| c.cleanup.max_length_drift = v as f32,
    ));
    guard.add(&spin_row(
        ctx,
        "Skip LLM below",
        "Words — short phrases are inserted as-is",
        (0.0, 50.0, 1.0),
        0,
        |c| c.cleanup.skip_below_words as f64,
        |c, v| c.cleanup.skip_below_words = v as u32,
    ));
    guard.add(&spin_row(
        ctx,
        "GPU layers",
        "999 = whole model on GPU, 0 = CPU only",
        (0.0, 999.0, 1.0),
        0,
        |c| c.cleanup.gpu_layers as f64,
        |c, v| c.cleanup.gpu_layers = v as u32,
    ));
    page(&[main, guard])
}

fn output_page(ctx: &Shared) -> adw::PreferencesPage {
    let cfg = ctx.cfg.borrow().clone();
    let out = group("Text insertion", None);
    out.add(&switch_row(
        ctx,
        "Context-aware recognition",
        "Use nearby text locally to help recognize names and technical terms; requires surrounding-text or accessibility support",
        |c| c.transcription.context_aware,
        |c, v| c.transcription.context_aware = v,
    ));
    out.add(&switch_row(
        ctx,
        "Smarter text insertion",
        "Match nearby capitalization, spacing and punctuation when the app exposes cursor context",
        |c| c.output.cursor_aware,
        |c, v| c.output.cursor_aware = v,
    ));
    let chord = match cfg.output.paste_chord {
        PasteChord::ShiftInsert => 0,
        PasteChord::CtrlV => 1,
        PasteChord::CtrlShiftV => 2,
    };
    out.add(&combo_row(
        ctx,
        "Paste shortcut",
        "Shift+Insert works in terminals and most apps",
        &["Shift + Insert", "Ctrl + V", "Ctrl + Shift + V"],
        chord,
        |c, i| {
            c.output.paste_chord = [
                PasteChord::ShiftInsert,
                PasteChord::CtrlV,
                PasteChord::CtrlShiftV,
            ][i.min(2)]
        },
    ));
    out.add(&switch_row(
        ctx,
        "Restore clipboard",
        "Put back what you had copied",
        |c| c.output.restore_clipboard,
        |c, v| c.output.restore_clipboard = v,
    ));
    out.add(&spin_row(
        ctx,
        "Restore delay",
        "Milliseconds after pasting",
        (100.0, 3000.0, 50.0),
        0,
        |c| c.output.restore_delay_ms as f64,
        |c, v| c.output.restore_delay_ms = v as u32,
    ));
    out.add(&switch_row(
        ctx,
        "Trailing space",
        "At the end of a field, or when cursor context is unavailable",
        |c| c.output.append_space,
        |c, v| c.output.append_space = v,
    ));

    let hud = group("Overlay", None);
    let fe = match cfg.hud.frontend {
        FrontendChoice::Auto => 0,
        FrontendChoice::LayerShell => 1,
        FrontendChoice::Gnome => 2,
        FrontendChoice::None => 3,
    };
    hud.add(&combo_row(
        ctx,
        "Overlay backend",
        "Auto picks the GNOME extension or layer-shell",
        &["Auto", "Layer shell", "GNOME Shell extension", "Hidden"],
        fe,
        |c, i| {
            c.hud.frontend = [
                FrontendChoice::Auto,
                FrontendChoice::LayerShell,
                FrontendChoice::Gnome,
                FrontendChoice::None,
            ][i.min(3)]
        },
    ));
    hud.add(&switch_row(
        ctx,
        "Live text",
        "Show the transcript above the waveform",
        |c| c.hud.show_live_text,
        |c, v| c.hud.show_live_text = v,
    ));
    hud.add(&spin_row(
        ctx,
        "Frequency bands",
        "Bars are mirrored, so twice as many are drawn",
        (4.0, 48.0, 1.0),
        0,
        |c| c.hud.bands as f64,
        |c, v| c.hud.bands = v as u32,
    ));
    hud.add(&spin_row(
        ctx,
        "Distance from bottom",
        "Pixels",
        (0.0, 600.0, 4.0),
        0,
        |c| c.hud.bottom_margin as f64,
        |c, v| c.hud.bottom_margin = v as u32,
    ));
    hud.add(&spin_row(
        ctx,
        "Background opacity",
        "",
        (0.0, 1.0, 0.05),
        2,
        |c| c.hud.opacity,
        |c, v| c.hud.opacity = v,
    ));
    page(&[out, hud])
}

fn models_page(ctx: &Shared) -> adw::PreferencesPage {
    let mut groups = Vec::new();
    for (kind, title) in [
        (ModelKind::Whisper, "Speech models"),
        (ModelKind::Llm, "Cleanup models"),
        (ModelKind::Vad, "Voice detection"),
    ] {
        let g = group(title, None);
        for spec in models::of_kind(kind) {
            let row = adw::ActionRow::builder()
                .title(spec.label)
                .subtitle(spec.note)
                .build();
            let bar = gtk::ProgressBar::builder()
                .valign(gtk::Align::Center)
                .width_request(120)
                .visible(false)
                .build();
            let button = gtk::Button::builder().valign(gtk::Align::Center).build();
            row.add_suffix(&bar);
            row.add_suffix(&button);
            let id = spec.id.to_owned();
            {
                let ctx = ctx.clone();
                let id = id.clone();
                let bar = bar.clone();
                button.connect_clicked(move |b| {
                    let Some(spec) = models::find(&id) else { return };
                    if spec.is_installed() {
                        let cfg = ctx.cfg.borrow();
                        if cfg.transcription.model == spec.id || cfg.cleanup.model == spec.id {
                            ctx.toast("This model is selected — pick another one before deleting it");
                            return;
                        }
                        drop(cfg);
                        let _ = std::fs::remove_file(spec.path());
                        refresh_model_rows(&ctx);
                    } else {
                        let Some(d) = ctx.daemon.borrow().clone() else {
                            ctx.toast("Start the daemon to download models (or run `localwispr download --model …`)");
                            return;
                        };
                        if !ctx.downloads.borrow_mut().insert(id.clone()) { return; }
                        b.set_sensitive(false);
                        bar.set_visible(true);
                        bar.set_fraction(0.0);
                        let id = id.clone();
                        let ctx = ctx.clone();
                        glib::spawn_future_local(async move {
                            if let Err(e) = d.download_model(&id).await {
                                ctx.downloads.borrow_mut().remove(&id);
                                if let Some(bar) = ctx.progress.borrow().get(&id) { bar.set_visible(false); }
                                refresh_model_rows(&ctx);
                                ctx.toast(&format!("Could not start download: {e}"));
                            }
                        });
                    }
                });
            }
            ctx.progress.borrow_mut().insert(id.clone(), bar);
            ctx.model_rows.borrow_mut().push((id, row.clone(), button));
            g.add(&row);
        }
        groups.push(g);
    }
    let dir = group("Storage", None);
    let row = adw::ActionRow::builder()
        .title("Models folder")
        .subtitle(crate::config::models_dir().display().to_string())
        .subtitle_selectable(true)
        .build();
    dir.add(&row);

    let status = group("Daemon", None);
    let status_rows: Vec<(&str, &str, adw::ActionRow)> = [
        ("state", "State"),
        ("stt_model", "Speech model"),
        ("llm_model", "Cleanup model"),
        ("frontend", "Overlay"),
        ("injection", "Text insertion"),
        ("last_notice", "Last message"),
    ]
    .into_iter()
    .map(|(k, t)| {
        let r = adw::ActionRow::builder()
            .title(t)
            .subtitle("—")
            .subtitle_selectable(true)
            .build();
        status.add(&r);
        (k, t, r)
    })
    .collect();
    {
        let weak = Rc::downgrade(ctx);
        glib::timeout_add_local(Duration::from_millis(1500), move || {
            let Some(ctx) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ctx.app.windows().is_empty() {
                return glib::ControlFlow::Break;
            }
            let ctx2 = ctx.clone();
            let rows = status_rows
                .iter()
                .map(|(k, _, r)| (k.to_string(), r.clone()))
                .collect::<Vec<_>>();
            glib::spawn_future_local(async move {
                let daemon = ctx2.daemon.borrow().clone();
                let st = match daemon {
                    Some(d) => d.get_status().await.ok(),
                    None => None,
                };
                for (k, r) in rows {
                    let v = st.as_ref().map(|s| s.get(&k).cloned().unwrap_or_default());
                    r.set_subtitle(&match v {
                        Some(v) if !v.is_empty() => v,
                        Some(_) => "—".into(),
                        None if k == "state" => "not running".into(),
                        None => "—".into(),
                    });
                }
            });
            glib::ControlFlow::Continue
        });
    }
    groups.push(status);
    groups.push(dir);
    groups.push(updates_group(ctx));
    refresh_model_rows(ctx);
    page(&groups)
}

/// Runs `localwispr <args>` and returns (success, stdout, stderr).
async fn run_self(argv: Vec<std::ffi::OsString>) -> Result<(bool, String, String), glib::Error> {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    let proc = gtk::gio::Subprocess::newv(
        &args,
        gtk::gio::SubprocessFlags::STDOUT_PIPE | gtk::gio::SubprocessFlags::STDERR_PIPE,
    )?;
    let (out, err) = proc.communicate_utf8_future(None).await?;
    let text = |s: Option<glib::GString>| s.map(|s| s.to_string()).unwrap_or_default();
    Ok((proc.is_successful(), text(out), text(err)))
}

fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
}

fn updates_group(ctx: &Shared) -> adw::PreferencesGroup {
    use crate::update;
    let g = group(
        "Updates",
        Some("Releases come from GitHub. Nothing is installed unless you click Install."),
    );
    let row = adw::ActionRow::builder()
        .title("localwispr")
        .subtitle(update::long_version())
        .subtitle_selectable(true)
        .build();
    let spinner = adw::Spinner::builder().visible(false).build();
    let button = gtk::Button::builder()
        .label("Check now")
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&spinner);
    row.add_suffix(&button);
    g.add(&row);
    g.add(&switch_row(
        ctx,
        "Check for updates automatically",
        "Once a day, ask GitHub for the latest version; you get a notice when one is out",
        |c| c.updates.check,
        |c, v| c.updates.check = v,
    ));

    let available: Rc<RefCell<Option<String>>> = Rc::default();
    let check = {
        let (row, spinner, button, available) = (
            row.clone(),
            spinner.clone(),
            button.clone(),
            available.clone(),
        );
        let ctx = ctx.clone();
        move |quiet: bool| {
            let (row, spinner, button, available, ctx) = (
                row.clone(),
                spinner.clone(),
                button.clone(),
                available.clone(),
                ctx.clone(),
            );
            spinner.set_visible(true);
            button.set_sensitive(false);
            glib::spawn_future_local(async move {
                let result = run_self(update::own_scope_argv(&["update", "--check"])).await;
                spinner.set_visible(false);
                button.set_sensitive(true);
                match result {
                    Ok((true, out, _)) => match update::parse_check_output(&out) {
                        Some(v) => {
                            row.set_subtitle(&format!(
                                "{} · version {v} is available",
                                update::long_version()
                            ));
                            button.set_label(&format!("Install {v}"));
                            button.add_css_class("suggested-action");
                            *available.borrow_mut() = Some(v);
                        }
                        None => {
                            row.set_subtitle(&format!("{} · up to date", update::long_version()));
                        }
                    },
                    Ok((false, _, err)) if !quiet => {
                        ctx.toast(&format!("Update check failed: {}", last_line(&err)))
                    }
                    Err(e) if !quiet => ctx.toast(&format!("Update check failed: {e}")),
                    _ => {}
                }
            });
        }
    };
    let install = {
        let (row, spinner, button) = (row.clone(), spinner.clone(), button.clone());
        let ctx = ctx.clone();
        move |version: String| {
            let (row, spinner, button, ctx) =
                (row.clone(), spinner.clone(), button.clone(), ctx.clone());
            spinner.set_visible(true);
            button.set_sensitive(false);
            button.set_label("Installing…");
            // The app stays alive while the window is closed mid-update.
            let hold = ctx.app.hold();
            glib::spawn_future_local(async move {
                let _hold = hold;
                let argv = update::own_scope_argv(&["update", "--yes", "--version", &version]);
                let result = run_self(argv).await;
                spinner.set_visible(false);
                match result {
                    Ok((true, out, _)) => {
                        button.set_label("Installed");
                        button.remove_css_class("suggested-action");
                        row.set_subtitle(&format!(
                            "Updated to {version}. Reopen Settings to use the new version."
                        ));
                        let notes: Vec<&str> = out
                            .lines()
                            .filter_map(|l| l.trim_start().strip_prefix("• "))
                            .collect();
                        if notes.is_empty() {
                            ctx.toast(&format!("Updated to localwispr {version}"));
                        } else {
                            let dialog = adw::AlertDialog::builder()
                                .heading(format!("Updated to localwispr {version}"))
                                .body(notes.join("\n\n"))
                                .build();
                            dialog.add_response("close", "OK");
                            dialog.present(ctx.app.active_window().as_ref());
                        }
                    }
                    Ok((false, _, err)) => {
                        button.set_sensitive(true);
                        button.set_label(&format!("Install {version}"));
                        ctx.toast(&format!("Update failed: {}", last_line(&err)));
                    }
                    Err(e) => {
                        button.set_sensitive(true);
                        button.set_label(&format!("Install {version}"));
                        ctx.toast(&format!("Update failed: {e}"));
                    }
                }
            });
        }
    };
    {
        let check = check.clone();
        let available = available.clone();
        button.connect_clicked(move |_| match available.borrow().clone() {
            Some(v) => install(v),
            None => check(false),
        });
    }
    if ctx.cfg.borrow().updates.check {
        check(true);
    }
    g
}

fn refresh_model_rows(ctx: &Shared) {
    for (id, row, button) in ctx.model_rows.borrow().iter() {
        let Some(spec) = models::find(id) else {
            continue;
        };
        let installed = spec.is_installed();
        button.set_sensitive(!ctx.downloads.borrow().contains(id));
        if installed {
            button.set_icon_name("user-trash-symbolic");
            button.set_tooltip_text(Some("Delete"));
            row.set_subtitle(&format!("{} · installed", spec.note));
        } else {
            button.set_icon_name("folder-download-symbolic");
            button.set_tooltip_text(Some("Download"));
            row.set_subtitle(spec.note);
        }
    }
}
