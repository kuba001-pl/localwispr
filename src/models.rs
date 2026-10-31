//! Model catalog and resumable, integrity-checked HuggingFace downloads.

use std::io::{IsTerminal, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::config::models_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Whisper,
    Vad,
    Llm,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub kind: ModelKind,
    pub label: &'static str,
    pub note: &'static str,
    pub file: &'static str,
    /// HuggingFace `repo/resolve/revision/path`.
    pub hf_path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

impl ModelSpec {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}", self.hf_path)
    }
    pub fn path(&self) -> PathBuf {
        models_dir().join(self.file)
    }
    pub fn is_installed(&self) -> bool {
        std::fs::metadata(self.path())
            .map(|m| m.len() == self.size)
            .unwrap_or(false)
    }
}

pub const VAD_ID: &str = "silero-v6.2";

pub const CATALOG: &[ModelSpec] = &[
    ModelSpec {
        id: "base",
        kind: ModelKind::Whisper,
        label: "Whisper base",
        note: "148 MB · fastest, weak on Polish",
        file: "ggml-base.bin",
        hf_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    ModelSpec {
        id: "small",
        kind: ModelKind::Whisper,
        label: "Whisper small",
        note: "488 MB · fast, decent multilingual",
        file: "ggml-small.bin",
        hf_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    ModelSpec {
        id: "large-v3-turbo-q5",
        kind: ModelKind::Whisper,
        label: "Whisper large-v3 turbo (Q5)",
        note: "574 MB · near-best accuracy, low VRAM",
        file: "ggml-large-v3-turbo-q5_0.bin",
        hf_path: "ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    ModelSpec {
        id: "large-v3-turbo",
        kind: ModelKind::Whisper,
        label: "Whisper large-v3 turbo",
        note: "1.6 GB · best accuracy, fastest on GPU",
        file: "ggml-large-v3-turbo.bin",
        hf_path: "ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        size: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
    ModelSpec {
        id: VAD_ID,
        kind: ModelKind::Vad,
        label: "Silero VAD v6.2",
        note: "0.9 MB · voice activity detection",
        file: "ggml-silero-v6.2.0.bin",
        hf_path: "ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin",
        size: 885_098,
        sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    },
    ModelSpec {
        id: "qwen2.5-1.5b",
        kind: ModelKind::Llm,
        label: "Qwen2.5 1.5B Instruct",
        note: "1.1 GB · fastest cleanup, weaker Polish",
        file: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        hf_path: "Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
        size: 1_117_320_736,
        sha256: "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
    },
    ModelSpec {
        id: "qwen2.5-3b",
        kind: ModelKind::Llm,
        label: "Qwen2.5 3B Instruct",
        note: "2.1 GB · recommended balance",
        file: "qwen2.5-3b-instruct-q4_k_m.gguf",
        hf_path: "Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
        size: 2_104_932_768,
        sha256: "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d",
    },
    ModelSpec {
        id: "qwen2.5-7b",
        kind: ModelKind::Llm,
        label: "Qwen2.5 7B Instruct",
        note: "4.7 GB · best quality, slower",
        file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        hf_path: "bartowski/Qwen2.5-7B-Instruct-GGUF/resolve/main/Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        size: 4_683_074_240,
        sha256: "65b8fcd92af6b4fefa935c625d1ac27ea29dcb6ee14589c55a8f115ceaaa1423",
    },
    ModelSpec {
        id: "llama-3.2-1b",
        kind: ModelKind::Llm,
        label: "Llama 3.2 1B Instruct",
        note: "0.8 GB · tiny, English-centric",
        file: "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
        hf_path: "bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf",
        size: 807_694_464,
        sha256: "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83",
    },
    ModelSpec {
        id: "llama-3.2-3b",
        kind: ModelKind::Llm,
        label: "Llama 3.2 3B Instruct",
        note: "2.0 GB · English-centric",
        file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        hf_path: "bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        size: 2_019_377_696,
        sha256: "6c1a2b41161032677be168d354123594c0e6e67d2b9227c84f296ad037c728ff",
    },
];

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn of_kind(kind: ModelKind) -> impl Iterator<Item = &'static ModelSpec> {
    CATALOG.iter().filter(move |m| m.kind == kind)
}

/// A configured model: either a catalog entry or a user-supplied file path.
#[derive(Debug, Clone)]
pub enum ModelRef {
    Catalog(&'static ModelSpec),
    Custom(PathBuf),
}

impl ModelRef {
    pub fn resolve(id_or_path: &str, kind: ModelKind) -> Result<Self> {
        if let Some(spec) = find(id_or_path) {
            if spec.kind != kind {
                bail!("model `{id_or_path}` is not a {kind:?} model");
            }
            return Ok(Self::Catalog(spec));
        }
        let p = Path::new(id_or_path);
        if p.is_absolute() {
            return Ok(Self::Custom(p.to_path_buf()));
        }
        bail!("unknown model `{id_or_path}` (use a catalog id or an absolute path)")
    }

    pub fn path(&self) -> PathBuf {
        match self {
            Self::Catalog(s) => s.path(),
            Self::Custom(p) => p.clone(),
        }
    }

    pub fn is_installed(&self) -> bool {
        match self {
            Self::Catalog(s) => s.is_installed(),
            Self::Custom(p) => p.is_file(),
        }
    }
}

/// Download progress callback: (bytes done, bytes total).
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// The lock file is deliberately persistent: unlinking it after unlock lets a
/// waiting process lock an old inode while a third process locks a new one.
struct DownloadLock(std::fs::File);

impl DownloadLock {
    async fn acquire(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening model download lock {}", path.display()))?;
        loop {
            // SAFETY: the owned file descriptor remains live for this call.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                std::io::ErrorKind::Interrupted => continue,
                _ => return Err(error).context("locking model download"),
            }
        }
    }
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        // Closing also releases flock. Explicit unlock documents the lifetime.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Downloads `spec` into the models dir. Resumes partial downloads, verifies
/// SHA-256, and only moves the file into place once it is verified.
pub async fn download(spec: &ModelSpec, progress: Progress<'_>) -> Result<PathBuf> {
    let dest = spec.path();
    tokio::fs::create_dir_all(models_dir()).await?;
    // Cover the installed check, resume, checksum, deletion, and final rename.
    // Waiting is nonblocking and cancellation drops the waiting file handle.
    let _lock = DownloadLock::acquire(&dest.with_extension("download.lock")).await?;
    if spec.is_installed() {
        return Ok(dest);
    }
    let part = dest.with_extension("part");

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("localwispr/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let mut attempts = 0;
    loop {
        attempts += 1;
        match download_attempt(&client, spec, &part, progress).await {
            Ok(()) => break,
            Err(e) if attempts < 4 => {
                tracing::warn!("download of {} interrupted ({e:#}); retrying", spec.id);
                tokio::time::sleep(Duration::from_secs(2 * attempts)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("downloading {}", spec.url())),
        }
    }

    let part_c = part.clone();
    let digest = tokio::task::spawn_blocking(move || sha256_file(&part_c)).await??;
    if digest != spec.sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        bail!(
            "checksum mismatch for {} (got {digest}); the partial file was removed",
            spec.file
        );
    }
    tokio::fs::rename(&part, &dest).await?;
    tracing::info!("installed {} → {}", spec.id, dest.display());
    Ok(dest)
}

async fn download_attempt(
    client: &reqwest::Client,
    spec: &ModelSpec,
    part: &Path,
    progress: Progress<'_>,
) -> Result<()> {
    let mut have = tokio::fs::metadata(part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if have > spec.size {
        tokio::fs::remove_file(part).await?;
        have = 0;
    }
    if have == spec.size {
        return Ok(());
    }

    let mut req = client.get(spec.url());
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await?.error_for_status()?;
    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resumed {
        have = 0;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)
        .await?;

    progress(have, spec.size);
    let mut stream = resp.bytes_stream();
    let mut last_report = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        have += chunk.len() as u64;
        if last_report.elapsed() >= Duration::from_millis(100) {
            progress(have, spec.size);
            last_report = std::time::Instant::now();
        }
    }
    file.flush().await?;
    progress(have, spec.size);
    if have != spec.size {
        bail!("connection closed at {have}/{} bytes", spec.size);
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Downloads with an indicatif progress bar when stderr is a terminal.
pub async fn download_with_terminal_progress(
    spec: &ModelSpec,
    extra: Progress<'_>,
) -> Result<PathBuf> {
    if !std::io::stderr().is_terminal() {
        tracing::info!("downloading {} ({} MB)…", spec.label, spec.size / 1_000_000);
        return download(spec, extra).await;
    }
    let bar = indicatif::ProgressBar::new(spec.size);
    bar.set_style(
        indicatif::ProgressStyle::with_template(
            "{msg:28} {bar:32.cyan/blue} {bytes:>10}/{total_bytes:10} {bytes_per_sec:>12} eta {eta}",
        )
        .unwrap()
        .progress_chars("━╸─"),
    );
    bar.set_message(spec.label);
    let cb = |done: u64, total: u64| {
        bar.set_position(done);
        extra(done, total);
    };
    let res = download(spec, &cb).await;
    match &res {
        Ok(_) => bar.finish_with_message(format!("{} ✓", spec.label)),
        Err(_) => bar.abandon_with_message(format!("{} ✗", spec.label)),
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_lock_subprocess_helper() {
        let Some(path) = std::env::var_os("LOCALWISPR_TEST_LOCK") else {
            return;
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert_eq!(
            acquired,
            std::env::var("LOCALWISPR_TEST_LOCK_EXPECT").unwrap() == "free"
        );
    }

    #[tokio::test]
    async fn download_lock_excludes_another_process_and_releases_on_drop() {
        let path =
            std::env::temp_dir().join(format!("localwispr-download-lock-{}", std::process::id()));
        let lock = DownloadLock::acquire(&path).await.unwrap();
        let child = |expected| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "models::tests::download_lock_subprocess_helper"])
                .env("LOCALWISPR_TEST_LOCK", &path)
                .env("LOCALWISPR_TEST_LOCK_EXPECT", expected)
                .output()
                .unwrap()
        };
        let output = child("busy");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(60), DownloadLock::acquire(&path))
                .await
                .is_err()
        );
        drop(lock);
        assert!(child("free").status.success());
        drop(DownloadLock::acquire(&path).await.unwrap());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn catalog_is_consistent() {
        let mut ids = std::collections::HashSet::new();
        for m in CATALOG {
            assert!(ids.insert(m.id), "duplicate id {}", m.id);
            assert_eq!(m.sha256.len(), 64);
            assert!(m.hf_path.ends_with(m.file));
        }
        assert!(find(VAD_ID).is_some());
        assert!(find(&crate::config::Transcription::default().model).is_some());
        assert!(find(&crate::config::Cleanup::default().model).is_some());
    }

    #[test]
    fn resolve_kinds() {
        assert!(ModelRef::resolve("qwen2.5-3b", ModelKind::Whisper).is_err());
        assert!(ModelRef::resolve("/x/y.bin", ModelKind::Whisper).is_ok());
        assert!(ModelRef::resolve("nope", ModelKind::Llm).is_err());
    }
}
