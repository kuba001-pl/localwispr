//! localwispr — local, GPU-accelerated push-to-talk dictation for Wayland.

mod audio;
mod cleanup;
mod config;
mod daemon;
mod dbus;
mod desktop;
mod hotkey;
mod hud;
mod inject;
mod llm_proto;
mod models;
mod settings;
mod stt;
mod update;
mod wav;

use std::path::PathBuf;
use std::time::Instant;

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
            let mut llm = cleanup::LlmClient::spawn(&model.path(), cfg.cleanup.gpu_layers).await?;
            let mut c = cfg.cleanup.clone();
            c.enabled = true;
            let t = Instant::now();
            let (out, outcome) = cleanup::polish(Some(&mut llm), &text, &c).await;
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

    let mut llm = if !raw && cfg.cleanup.enabled {
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
            if live {
                replay_previews(&stt, &audio, &cfg.transcription).await?;
            }
            let t0 = Instant::now();
            let tr = stt
                .submit(audio.clone(), false, &cfg.transcription, vad.clone())
                .await??;
            let t_stt = t0.elapsed();
            let t1 = Instant::now();
            let (out, outcome) = cleanup::polish(llm.as_mut(), &tr.text, &cfg.cleanup).await;
            let t_llm = t1.elapsed();
            if i == 0 {
                println!(
                    "language: {}\nraw:      {}\nfinal:    {}",
                    tr.language, tr.text, out
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

/// Mimics the daemon while recording: a preview every `preview_interval_ms`
/// of captured audio, skipped while the previous one is still decoding.
async fn replay_previews(
    stt: &stt::SttWorker,
    audio: &[f32],
    cfg: &config::Transcription,
) -> Result<()> {
    let rate = audio::SAMPLE_RATE as usize;
    let interval = cfg.preview_interval_ms as usize * rate / 1000;
    let mut end = interval;
    while end < audio.len() {
        let start = end.saturating_sub(28 * rate);
        let t = Instant::now();
        let tr = stt
            .submit(audio[start..end].to_vec(), true, cfg, None)
            .await??;
        tracing::debug!(
            "preview {:.1}s [{}] {}",
            end as f32 / rate as f32,
            tr.language,
            tr.text
        );
        let busy = t.elapsed().as_millis() as usize * rate / 1000;
        end += interval.max(busy);
    }
    Ok(())
}
