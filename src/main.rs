//! localwispr — local, GPU-accelerated push-to-talk dictation for Wayland.

mod audio;
mod cleanup;
mod config;
mod cursor_context;
mod daemon;
mod dbus;
mod desktop;
mod fence;
mod hotkey;
mod hud;
mod inject;
mod insertion;
mod llm_proto;
mod models;
mod settings;
mod stt;
mod update;
mod wav;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "localwispr", version = update::long_version(), about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the dictation daemon (default).
    Daemon,
    /// Open the settings window.
    Settings,
    /// Layer-shell HUD (started automatically by the daemon).
    Hud,
    /// Download the models required by the current config.
    Download {
        /// Download a specific catalog model instead.
        #[arg(long)]
        model: Option<String>,
    },
    /// List models and whether they are installed.
    Models,
    /// List microphones.
    Devices,
    /// Transcribe (and optionally polish) a WAV file — useful for benchmarking.
    Transcribe {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Skip the LLM cleanup stage.
        #[arg(long)]
        raw: bool,
        /// Repeat N times and report timings.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Replay the live previews the daemon would run while recording
        /// before the final pass (they share the language decision).
        #[arg(long)]
        live: bool,
    },
    /// Run the cleanup LLM on a piece of text.
    Cleanup {
        text: String,
    },
    /// Control a running daemon.
    Toggle,
    Start,
    Stop,
    Cancel,
    /// Print the running daemon's status.
    Status,
    /// Update to the latest release from GitHub (or check for one).
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
        /// Don't ask for confirmation.
        #[arg(long, short)]
        yes: bool,
        /// Switch GPU backend: `cuda` (NVIDIA) or `vulkan` (any GPU, CPU fallback).
        #[arg(long)]
        variant: Option<String>,
        /// Install this release instead of the latest (e.g. `0.2.0`).
        #[arg(long, conflicts_with = "rollback")]
        version: Option<String>,
        /// Switch back to the previously installed version.
        #[arg(long, conflicts_with_all = ["check", "variant"])]
        rollback: bool,
        /// Don't install or restart the systemd user service.
        #[arg(long)]
        no_service: bool,
    },
    /// Install the bundle this binary belongs to (used by install.sh and `update`).
    #[command(hide = true)]
    Install {
        #[arg(long)]
        no_service: bool,
    },
    /// Remove localwispr (keeps config and models).
    Uninstall {
        /// Also undo setup-permissions.sh (asks for sudo).
        #[arg(long)]
        undo_permissions: bool,
    },
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("LOCALWISPR_LOG").unwrap_or_else(|_| {
        EnvFilter::new("info,whisper_rs=warn,whisper_rs::whisper_logging_hook=off,whisper_rs::ggml_logging_hook=off,zbus=warn")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging();
    whisper_rs::install_logging_hooks();

    match cli.command.unwrap_or(Command::Daemon) {
        // GTK frontends run their own main loop (no tokio on the main thread).
        Command::Settings => settings::run(),
        Command::Hud => hud::run(),
        Command::Install { no_service } => update::install(update::InstallOptions {
            service: !no_service,
        }),
        Command::Uninstall { undo_permissions } => update::uninstall(undo_permissions),
        other => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()?
            .block_on(async_main(other)),
    }
}

async fn async_main(cmd: Command) -> Result<()> {
    match cmd {
        Command::Daemon => daemon::run().await,
        Command::Download { model } => {
            if let Some(id) = model {
                let spec = models::find(&id).with_context(|| format!("unknown model `{id}`"))?;
                models::download_with_terminal_progress(spec, &|_, _| {}).await?;
                Ok(())
            } else {
                daemon::provision(&config::Config::load()?).await
            }
        }
        Command::Models => {
            let cfg = config::Config::load_or_default();
            for m in models::CATALOG {
                let active = m.id == cfg.transcription.model || m.id == cfg.cleanup.model;
                println!(
                    "{} {:<20} {:<30} {}{}",
                    if m.is_installed() { "✓" } else { " " },
                    m.id,
                    m.label,
                    m.note,
                    if active { "  ← selected" } else { "" }
                );
            }
            println!("\nmodels dir: {}", config::models_dir().display());
            Ok(())
        }
        Command::Devices => {
            for d in audio::list_input_devices() {
                println!(
                    "{} {}\n    id: {}",
                    if d.is_default { "*" } else { " " },
                    d.name,
                    d.id
                );
            }
            Ok(())
        }
        Command::Transcribe {
            files,
            raw,
            repeat,
            live,
        } => transcribe_files(&files, raw, repeat, live).await,
        Command::Cleanup { text } => {
            let cfg = config::Config::load_or_default();
            let model = models::ModelRef::resolve(&cfg.cleanup.model, models::ModelKind::Llm)?;
            let llm = cleanup::LlmClient::spawn(&model.path(), cfg.cleanup.gpu_layers).await?;
            let mut c = cfg.cleanup.clone();
            c.enabled = true;
            let t = Instant::now();
            let (out, outcome) = cleanup::polish(Some(&llm), &text, &c, cleanup::Pass::Final).await;
            println!("{out}\n  [{outcome:?}, {} ms]", t.elapsed().as_millis());
            Ok(())
        }
        Command::Toggle | Command::Start | Command::Stop | Command::Cancel | Command::Status => {
            let conn = zbus::Connection::session().await?;
            let proxy = dbus::DaemonProxy::new(&conn).await?;
            let res = match cmd {
                Command::Toggle => proxy.toggle().await,
                Command::Start => proxy.start().await,
                Command::Stop => proxy.stop().await,
                Command::Cancel => proxy.cancel().await,
                _ => {
                    let mut s: Vec<_> = proxy.get_status().await?.into_iter().collect();
                    s.sort();
                    for (k, v) in s {
                        println!("{k:>16}: {v}");
                    }
                    Ok(())
                }
            };
            res.context("is the localwispr daemon running?")
        }
        Command::Update {
            check,
            yes,
            variant,
            version,
            rollback,
            no_service,
        } => {
            update::update(update::UpdateOptions {
                check,
                yes,
                variant,
                version,
                rollback,
                no_service,
            })
            .await
        }
        Command::Settings | Command::Hud | Command::Install { .. } | Command::Uninstall { .. } => {
            unreachable!()
        }
    }
}

async fn transcribe_files(files: &[PathBuf], raw: bool, repeat: u32, live: bool) -> Result<()> {
    let cfg = config::Config::load_or_default();
    let model = models::ModelRef::resolve(&cfg.transcription.model, models::ModelKind::Whisper)?;
    if !model.is_installed() {
        bail!("model not installed — run `localwispr download`");
    }
    let t = Instant::now();
    let stt = tokio::task::spawn_blocking({
        let p = model.path();
        let gpu = cfg.transcription.use_gpu;
        move || stt::SttWorker::spawn(p, gpu)
    })
    .await??;
    eprintln!("whisper load+warmup: {} ms", t.elapsed().as_millis());

    let llm = if !raw && cfg.cleanup.enabled {
        let m = models::ModelRef::resolve(&cfg.cleanup.model, models::ModelKind::Llm)?;
        Some(cleanup::LlmClient::spawn(&m.path(), cfg.cleanup.gpu_layers).await?)
    } else {
        None
    };
    let vad = cfg
        .transcription
        .vad
        .then(|| {
            models::find(models::VAD_ID)
                .filter(|m| m.is_installed())
                .map(|m| m.path())
        })
        .flatten();

    for file in files {
        let audio = wav::read_mono_16k(file)?;
        if audio.is_empty() {
            bail!("{}: empty audio", file.display());
        }
        if files.len() > 1 {
            println!("== {}", file.display());
        }
        for i in 0..repeat {
            stt.new_utterance();
            let mut shown = None;
            let mut preview_polish = None;
            if live {
                let polish_llm = llm.as_ref().filter(|_| cfg.cleanup.live_preview);
                (shown, preview_polish) = replay_previews(&stt, polish_llm, &audio, &cfg).await?;
                // Released: like the daemon, stop a preview polish in flight.
                if let Some(l) = polish_llm {
                    l.cancel_pending().await;
                }
            }
            let t0 = Instant::now();
            let tr = stt
                .submit(audio.clone(), false, &cfg.transcription, vad.clone())
                .await??;
            let t_stt = t0.elapsed();
            let t1 = Instant::now();
            let reused = cleanup::reuse_preview_polish(preview_polish, &tr.text, &cfg.cleanup)
                .map(|polished| (polished, cleanup::Outcome::Polished));
            let was_reused = reused.is_some();
            let (out, outcome) = match reused {
                Some(r) => r,
                None => {
                    cleanup::polish(llm.as_ref(), &tr.text, &cfg.cleanup, cleanup::Pass::Final)
                        .await
                }
            };
            let t_llm = t1.elapsed();
            if i == 0 {
                println!(
                    "language: {}\nraw:      {}\nfinal:    {}",
                    tr.language, tr.text, out
                );
            }
            if live {
                println!(
                    "preview:  {} (last shown before release{})",
                    shown.as_deref().unwrap_or(""),
                    match (shown.as_deref() == Some(out.as_str()), was_reused) {
                        (true, true) => "; identical; polish reused",
                        (true, false) => "; identical",
                        (false, true) => "; polish reused",
                        (false, false) => "",
                    }
                );
            }
            println!(
                "run {}: {:.1}s audio · stt {} ms · cleanup {} ms ({outcome:?}) · total {} ms",
                i + 1,
                tr.audio_secs,
                t_stt.as_millis(),
                t_llm.as_millis(),
                t0.elapsed().as_millis()
            );
        }
    }
    Ok(())
}

/// Mimics the daemon while recording, in real time: a preview every
/// `preview_interval_ms` of captured audio (skipped while one is decoding)
/// and, with `llm`, one preview polish at a time. Returns the last text the
/// overlay would show and the newest successful polish (raw, polished).
async fn replay_previews(
    stt: &stt::SttWorker,
    llm: Option<&cleanup::LlmClient>,
    audio: &[f32],
    cfg: &config::Config,
) -> Result<(Option<String>, Option<(String, String)>)> {
    type Polished = (String, String, cleanup::Outcome, Instant);
    let rate = audio::SAMPLE_RATE as usize;
    let tcfg = &cfg.transcription;
    let interval = Duration::from_millis(tcfg.preview_interval_ms as u64);
    let start = Instant::now();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Polished>();
    let mut shown: Option<String> = None;
    let mut in_flight = false;
    let mut queued: Option<String> = None;
    let mut last_raw: Option<String> = None;
    let mut polished: Option<(String, String)> = None;
    let spawn_polish = |raw: String| {
        let llm = llm.map(cleanup::LlmClient::shared);
        let tx = tx.clone();
        let c = cfg.cleanup.clone();
        tokio::spawn(async move {
            let t = Instant::now();
            let (text, outcome) =
                cleanup::polish(llm.as_ref(), &raw, &c, cleanup::Pass::Preview).await;
            tracing::debug!("  polish {} ms ({outcome:?})", t.elapsed().as_millis());
            let _ = tx.send((raw, text, outcome, Instant::now()));
        });
    };
    let mut next = start + interval;
    loop {
        // Handle finished polishes while waiting for the next tick.
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(next.into()) => break,
                Some((raw, text, outcome, _)) = rx.recv() => {
                    in_flight = false;
                    if outcome == cleanup::Outcome::Polished {
                        polished = Some((raw, text.clone()));
                    }
                    tracing::debug!("  shown {:.1}s: {text}", start.elapsed().as_secs_f32());
                    shown = Some(text);
                    if let Some(q) = queued.take() && last_raw.as_ref() != Some(&q) {
                        in_flight = true;
                        last_raw = Some(q.clone());
                        spawn_polish(q);
                    }
                }
            }
        }
        let end = (start.elapsed().as_secs_f64() * rate as f64) as usize;
        if end >= audio.len() {
            break;
        }
        let from = end.saturating_sub(28 * rate);
        let t = Instant::now();
        let tr = stt
            .submit(audio[from..end].to_vec(), true, tcfg, None)
            .await??;
        tracing::debug!(
            "preview {:.1}s {} ms [{}] {}",
            end as f32 / rate as f32,
            t.elapsed().as_millis(),
            tr.language,
            tr.text
        );
        if !tr.text.is_empty() {
            if llm.is_some() {
                if last_raw.as_deref() != Some(tr.text.as_str()) {
                    if in_flight {
                        queued = Some(tr.text);
                    } else {
                        in_flight = true;
                        last_raw = Some(tr.text.clone());
                        spawn_polish(tr.text);
                    }
                }
            } else {
                shown = Some(cleanup::fallback(&tr.text, &cfg.cleanup));
            }
        }
        // Like a tokio interval with MissedTickBehavior::Skip.
        next += interval;
        while next <= Instant::now() {
            next += interval;
        }
    }
    // Audio ends here (key released); sleep until the clip's real duration.
    let release = start + Duration::from_secs_f64(audio.len() as f64 / rate as f64);
    tokio::time::sleep_until(release.into()).await;
    while let Ok((raw, text, outcome, at)) = rx.try_recv() {
        if at <= release {
            if outcome == cleanup::Outcome::Polished {
                polished = Some((raw, text.clone()));
            }
            shown = Some(text);
        }
    }
    Ok((shown, polished))
}
