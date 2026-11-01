//! Installation from prebuilt bundles, update checks and self-update.
//!
//! Everything comes from GitHub Releases; there is no localwispr server.
//!
//! Layout (all per user, no root):
//!
//! ```text
//! ~/.local/share/localwispr/app/
//!     versions/<bundle>/           unpacked bundles (current + previous)
//!         bin/localwispr, bin/localwispr-llm
//!         lib/                     private shared libraries (RPATH)
//!         share/localwispr/bundle.json, setup-permissions.sh
//!         share/gnome-shell/extensions/localwispr@localwispr.github.io/
//!     current -> versions/<bundle> switched atomically
//!     runtime/cuda-<major>/        CUDA libraries, only when the system lacks them
//! ~/.local/bin/localwispr -> app/current/bin/localwispr
//! ~/.local/share/gnome-shell/extensions/localwispr@… -> app/current/share/…
//! ```
//!
//! A running daemon keeps executing from its own version directory, so an
//! update never changes files under a running process; the service is
//! restarted as the final step.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::config;

pub const REPO: &str = "kuba001-pl/localwispr";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// GPU backend compiled into this binary.
pub const VARIANT: &str = if cfg!(feature = "cuda") {
    "cuda"
} else if cfg!(feature = "vulkan") {
    "vulkan"
} else {
    "cpu"
};
/// `release` for bundles built by the release workflow, `source` otherwise.
pub const CHANNEL: &str = match option_env!("LOCALWISPR_CHANNEL") {
    Some(channel) => channel,
    None => "source",
};
const EXT_UUID: &str = "localwispr@localwispr.github.io";
const UNIT: &str = "localwispr.service";
const DESKTOP_FILE: &str = "org.localwispr.Settings.desktop";
/// `variant` recorded for an install from before bundles existed.
const LEGACY: &str = "legacy";
/// Marker in a version directory: an install of it completed, so it is a
/// safe version to roll back to.
const INSTALLED: &str = ".installed";

pub static LONG_VERSION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("{VERSION} ({VARIANT}, {CHANNEL} build)"));

pub fn long_version() -> &'static str {
    LONG_VERSION.as_str()
}

/// The variant to download when updating this build. There is no CPU-only
/// release; the Vulkan bundle falls back to the CPU without a GPU.
fn release_variant() -> &'static str {
    match VARIANT {
        "cuda" => "cuda",
        _ => "vulkan",
    }
}

// ---------------------------------------------------------------- versions --

/// `v1.2.3`, `1.2.3-rc1` → (1, 2, 3). Pre-release suffixes are ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Parses `sha256sum` output: `<hex>  <name>` or `<hex> *<name>`.
fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Where release assets are downloaded from; `LOCALWISPR_RELEASE_BASE`
/// points at a mirror (used by tests).
fn release_base() -> String {
    std::env::var("LOCALWISPR_RELEASE_BASE")
        .map(|b| b.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download"))
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("localwispr/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Latest published release version (without the `v`). Uses the
/// `releases/latest` redirect rather than the rate-limited REST API.
pub async fn latest_version() -> Result<String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("localwispr/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client
        .get(format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .await
        .context("checking for updates")?;
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .unwrap_or_default();
    let tag = location
        .rsplit_once("/releases/tag/")
        .map(|(_, tag)| tag)
        .filter(|tag| parse_version(tag).is_some())
        .with_context(|| format!("no published release found (HTTP {})", resp.status()))?;
    Ok(tag.trim_start_matches('v').to_string())
}

// ------------------------------------------------------------------ layout --

#[derive(Debug, Clone)]
pub struct Layout {
    pub app: PathBuf,
    pub bin_dir: PathBuf,
    pub ext_dir: PathBuf,
    pub unit_dir: PathBuf,
    pub apps_dir: PathBuf,
}

impl Layout {
    pub fn user() -> Self {
        let data = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            app: config::data_dir().join("app"),
            bin_dir: dirs::executable_dir().unwrap_or_else(|| home.join(".local/bin")),
            ext_dir: data.join("gnome-shell/extensions").join(EXT_UUID),
            unit_dir: dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("systemd/user"),
            apps_dir: data.join("applications"),
        }
    }

    fn versions(&self) -> PathBuf {
        self.app.join("versions")
    }

    fn current(&self) -> PathBuf {
        self.app.join("current")
    }

    pub fn runtime(&self, cuda_major: &str) -> PathBuf {
        self.app.join("runtime").join(format!("cuda-{cuda_major}"))
    }

    /// Bundle name `current` points to.
    pub fn current_id(&self) -> Option<String> {
        let target = std::fs::read_link(self.current()).ok()?;
        Some(target.file_name()?.to_string_lossy().into_owned())
    }

    /// Installed bundles other than the current one, newest first.
    pub fn other_versions(&self) -> Vec<String> {
        let current = self.current_id();
        let mut v: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(self.versions())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let usable = !name.starts_with('.')
                    && Some(&name) != current.as_ref()
                    && e.path().join("bin/localwispr").is_file()
                    && e.path().join(INSTALLED).exists();
                let modified = e.metadata().and_then(|m| m.modified()).ok()?;
                usable.then_some((modified, name))
            })
            .collect();
        v.sort_by(|a, b| b.cmp(a));
        v.into_iter().map(|(_, name)| name).collect()
    }

    /// Moves (or copies) a bundle directory to `versions/<name>`.
    fn adopt(&self, bundle: &Path) -> Result<String> {
        let name = bundle
            .file_name()
            .context("bundle path has no name")?
            .to_string_lossy()
            .into_owned();
        let versions = self.versions();
        std::fs::create_dir_all(&versions)?;
        let dest = versions.join(&name);
        if same_file(bundle, &dest) {
            return Ok(name);
        }
        // Installed versions are never modified: a running daemon may be
        // executing from one. Reinstalling the same release gets a new name.
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    name.clone()
                } else {
                    format!("{name}~{n}")
                }
            })
            .find(|n| std::fs::symlink_metadata(versions.join(n)).is_err())
            .expect("unbounded");
        let dest = versions.join(&name);
        let staged = versions.join(format!(".incoming-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staged);
        if std::fs::rename(bundle, &dest).is_err() {
            // Different filesystem: copy next to the destination, then rename.
            copy_tree(bundle, &staged)
                .with_context(|| format!("copying {} to {}", bundle.display(), staged.display()))?;
            std::fs::rename(&staged, &dest)?;
        }
        Ok(name)
    }

    /// Before bundles existed, install.sh copied the binaries and the GNOME
    /// extension straight into ~/.local. Keeps them as an installed version
    /// so switching to the first bundle can be rolled back. Returns its name.
    fn preserve_legacy(&self) -> Result<Option<String>> {
        let bin = self.bin_dir.join("localwispr");
        if !std::fs::symlink_metadata(&bin).is_ok_and(|m| m.is_file()) {
            return Ok(None);
        }
        // Only keep something that is recognizably localwispr.
        let Some(version) = version_of(&bin) else {
            return Ok(None);
        };
        let name = format!("localwispr-{version}-legacy");
        let staging = self
            .app
            .join(format!(".incoming-{}", std::process::id()))
            .join(&name);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(staging.join("bin"))?;
        std::fs::create_dir_all(staging.join("share/localwispr"))?;
        // Hard links: instant, and the originals stay until replaced.
        link_or_copy(&bin, &staging.join("bin/localwispr"))?;
        let llm = self.bin_dir.join("localwispr-llm");
        if std::fs::symlink_metadata(&llm).is_ok_and(|m| m.is_file()) {
            link_or_copy(&llm, &staging.join("bin/localwispr-llm"))?;
        }
        if std::fs::symlink_metadata(&self.ext_dir).is_ok_and(|m| m.is_dir()) {
            copy_tree(
                &self.ext_dir,
                &staging.join("share/gnome-shell/extensions").join(EXT_UUID),
            )?;
        }
        std::fs::write(
            staging.join("share/localwispr/bundle.json"),
            serde_json::json!({
                "version": version,
                "variant": LEGACY,
                "channel": "source",
                "cuda_runtime": null,
            })
            .to_string(),
        )?;
        std::fs::write(staging.join(INSTALLED), "")?;
        let name = self.adopt(&staging)?;
        let _ = std::fs::remove_dir_all(&staging);
        Ok(Some(name))
    }

    /// Points `current` at `versions/<name>` atomically.
    fn switch(&self, name: &str) -> Result<()> {
        if !self.versions().join(name).join("bin/localwispr").is_file() {
            bail!("{name} is not an installed version");
        }
        replace_symlink(&Path::new("versions").join(name), &self.current())
    }

    /// Deletes versions other than `keep` that no process is running from,
    /// plus this process's and abandoned (day-old) staging directories.
    fn prune(&self, keep: &[&str]) {
        let busy = versions_in_use(&self.versions());
        let pid = std::process::id().to_string();
        let stale = |entry: &std::fs::DirEntry| {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(24 * 3600))
        };
        for entry in std::fs::read_dir(self.versions())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let remove = if name.starts_with(".incoming-") {
                name.ends_with(&format!("-{pid}")) || stale(&entry)
            } else {
                !name.starts_with('.') && !keep.contains(&name.as_str()) && !busy.contains(&name)
            };
            if remove {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        for entry in std::fs::read_dir(&self.app).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `update` stages in .incoming-<pid> and then execs `install`
            // under the same pid; install.sh removes its own.
            if name.starts_with(".incoming-")
                && (name == format!(".incoming-{pid}") || stale(&entry))
            {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Version directories that running processes (daemon, HUD, LLM worker,
/// settings) execute from.
fn versions_in_use(versions: &Path) -> std::collections::HashSet<String> {
    let versions = std::fs::canonicalize(versions).unwrap_or_else(|_| versions.to_path_buf());
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_link(e.path().join("exe")).ok())
        .filter_map(|exe| {
            let rest = exe.strip_prefix(&versions).ok()?;
            Some(
                rest.components()
                    .next()?
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect()
}

/// Version reported by `<bin> --version` if it is a localwispr binary. Gives
/// up after a few seconds.
fn version_of(bin: &Path) -> Option<String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let mut words = out.split_whitespace();
    (words.next() == Some("localwispr"))
        .then(|| words.next())
        .flatten()
        .filter(|v| parse_version(v).is_some())
        .map(str::to_owned)
}

fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to)
            .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Creates or atomically replaces `link` with a symlink to `target`.
fn replace_symlink(target: &Path, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = link.with_file_name(format!(
        ".{}.new-{}",
        link.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    // rename() cannot replace a directory (an extension installed by an
    // older install.sh); remove it first.
    if std::fs::symlink_metadata(link).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir_all(link)?;
    }
    std::fs::rename(&tmp, link).with_context(|| format!("replacing {}", link.display()))
}

/// Writes `contents` atomically; returns whether anything changed.
fn write_if_changed(path: &Path, contents: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return Ok(false);
    }
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// File names and contents of a directory (following a symlink to it).
fn snapshot(dir: &Path) -> Option<Vec<(String, Vec<u8>)>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let data = std::fs::read(e.path()).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), data))
        })
        .collect();
    files.sort();
    Some(files)
}

// -------------------------------------------------------------- bundle info --

#[derive(Debug, Deserialize)]
pub struct BundleInfo {
    pub version: String,
    pub variant: String,
    /// CUDA major version whose runtime libraries the bundle links against.
    #[serde(default)]
    pub cuda_runtime: Option<String>,
}

impl BundleInfo {
    pub fn read(bundle: &Path) -> Result<Self> {
        let path = bundle.join("share/localwispr/bundle.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("{} is not a localwispr bundle", bundle.display()))?;
        Ok(serde_json::from_str(&text)?)
    }
}

fn cuda_libs(major: &str) -> [String; 3] {
    ["libcudart", "libcublas", "libcublasLt"].map(|l| format!("{l}.so.{major}"))
}

/// Whether the dynamic linker can find `lib` without our runtime directory.
fn system_has_lib(lib: &str) -> bool {
    let from_env = std::env::var_os("LD_LIBRARY_PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(lib).exists()));
    from_env
        || ["/sbin/ldconfig", "/usr/sbin/ldconfig", "ldconfig"]
            .iter()
            .find_map(|ldconfig| Command::new(ldconfig).arg("-p").output().ok())
            .is_some_and(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|l| l.trim_start().starts_with(&format!("{lib} ")))
            })
}

// -------------------------------------------------------------- downloading --

/// Downloads `url` to `dest`, returning its SHA-256. Progress goes to stderr.
async fn fetch(client: &reqwest::Client, url: &str, dest: &Path, label: &str) -> Result<String> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match fetch_once(client, url, dest, label).await {
            Ok(digest) => return Ok(digest),
            Err(e) if attempt < 3 => {
                eprintln!("download interrupted ({e:#}); retrying");
                tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("downloading {url}")),
        }
    }
}

async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    label: &str,
) -> Result<String> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let bar = if std::io::stderr().is_terminal() {
        let bar = indicatif::ProgressBar::new(resp.content_length().unwrap_or(0));
        bar.set_style(
            indicatif::ProgressStyle::with_template(
                "{msg:24} {bar:32.cyan/blue} {bytes:>10}/{total_bytes:10} {bytes_per_sec:>12}",
            )?
            .progress_chars("━╸─"),
        );
        bar.set_message(label.to_string());
        bar
    } else {
        eprintln!(
            "downloading {label} ({} MB)…",
            resp.content_length().unwrap_or(0) / 1_000_000
        );
        indicatif::ProgressBar::hidden()
    };
    let mut file = tokio::fs::File::create(dest).await?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        bar.inc(chunk.len() as u64);
    }
    file.flush().await?;
    bar.finish();
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Downloads a release asset and checks it against `SHA256SUMS`.
async fn fetch_verified(
    client: &reqwest::Client,
    base: &str,
    sums: &HashMap<String, String>,
    asset: &str,
    dest_dir: &Path,
) -> Result<PathBuf> {
    let expected = sums
        .get(asset)
        .with_context(|| format!("the release has no {asset}"))?;
    let dest = dest_dir.join(asset);
    let digest = fetch(client, &format!("{base}{asset}"), &dest, asset).await?;
    if &digest != expected {
        let _ = std::fs::remove_file(&dest);
        bail!("checksum mismatch for {asset}");
    }
    Ok(dest)
}

fn extract(archive: &Path, into: &Path) -> Result<()> {
    std::fs::create_dir_all(into)?;
    let status = Command::new("tar")
        .arg("-xJf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .arg("--no-same-owner")
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("extracting {} failed", archive.display());
    }
    Ok(())
}

/// Makes sure the CUDA libraries a bundle needs can be found, downloading the
/// runtime archive from the same release when the system has none.
async fn ensure_cuda_runtime(
    layout: &Layout,
    major: &str,
    client: &reqwest::Client,
    base: &str,
    sums: &HashMap<String, String>,
    staging: &Path,
) -> Result<()> {
    let libs = cuda_libs(major);
    let dir = layout.runtime(major);
    if libs.iter().all(|l| dir.join(l).exists()) || libs.iter().all(|l| system_has_lib(l)) {
        return Ok(());
    }
    eprintln!("CUDA {major} libraries not found on this system; downloading them once");
    let asset = format!(
        "localwispr-cuda{major}-runtime-{}.tar.xz",
        std::env::consts::ARCH
    );
    let archive = fetch_verified(client, base, sums, &asset, staging).await?;
    let unpacked = staging.join("runtime");
    extract(&archive, &unpacked)?;
    let _ = std::fs::remove_file(&archive);
    let src = unpacked.join(format!("cuda-{major}"));
    if !libs.iter().all(|l| src.join(l).exists()) {
        bail!("{asset} is incomplete");
    }
    std::fs::create_dir_all(dir.parent().context("no parent")?)?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&src, &dir)?;
    Ok(())
}

// ------------------------------------------------------------------ update --

pub struct UpdateOptions {
    pub check: bool,
    pub yes: bool,
    pub variant: Option<String>,
    pub version: Option<String>,
    pub rollback: bool,
    pub no_service: bool,
}

/// Serialises updates and installs.
struct InstallLock(#[allow(dead_code)] std::fs::File);

impl InstallLock {
    fn acquire(layout: &Layout) -> Result<Self> {
        use std::os::fd::AsRawFd;
        // Outside app/, which uninstall deletes, so the lock file is stable.
        let path = layout.app.with_file_name(".install.lock");
        std::fs::create_dir_all(path.parent().context("no parent")?)?;
        let file = std::fs::File::create(path)?;
        // SAFETY: flock on a file descriptor we own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another localwispr install or update is running");
        }
        Ok(Self(file))
    }
}

fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

pub async fn update(opts: UpdateOptions) -> Result<()> {
    let layout = Layout::user();
    if opts.rollback {
        let previous = layout
            .other_versions()
            .into_iter()
            .next()
            .context("no previous version is installed")?;
        println!("Switching back to {previous}");
        let dir = layout.versions().join(&previous);
        if BundleInfo::read(&dir).is_ok_and(|i| i.variant == LEGACY) {
            // Builds from before bundles cannot install themselves.
            let newer = layout.current_id();
            install_bundle(
                &dir,
                InstallOptions {
                    service: !opts.no_service,
                },
            )?;
            if let Some(newer) = newer {
                println!(
                    "This build has no `localwispr update`. To switch forward again, run:\n  {} install",
                    shell_quote(&layout.versions().join(newer).join("bin/localwispr"))
                );
            }
            return Ok(());
        }
        return exec_install(&dir, opts.no_service);
    }

    let target = match &opts.version {
        Some(v) if parse_version(v).is_some() => v.trim_start_matches('v').to_string(),
        Some(v) => bail!("invalid version `{v}`"),
        None => latest_version().await?,
    };
    let variant = opts.variant.as_deref().unwrap_or(release_variant());
    if !matches!(variant, "cuda" | "vulkan") {
        bail!("unknown variant `{variant}` (cuda or vulkan)");
    }
    let newer = is_newer(&target, VERSION);
    if opts.check {
        if newer {
            println!("update available: {VERSION} → {target}");
        } else {
            println!("localwispr {} is up to date", long_version());
        }
        return Ok(());
    }
    let switching = variant != release_variant();
    if opts.version.is_none() && !newer && !switching {
        println!(
            "localwispr {} is up to date (latest release: {target})",
            long_version()
        );
        return Ok(());
    }
    // Replacing a source build, a downgrade or a reinstall is never silent.
    if !opts.yes && (CHANNEL != "release" || !newer) {
        let question = format!(
            "Replace localwispr {} with the prebuilt {target} ({variant})?",
            long_version()
        );
        if !confirm(&question) {
            bail!("not confirmed (pass --yes to skip this question)");
        }
    }

    let _lock = InstallLock::acquire(&layout)?;
    let client = client()?;
    let base = format!("{}/v{target}/", release_base());
    let sums = parse_sums(
        &client
            .get(format!("{base}SHA256SUMS"))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("release v{target} not found"))?
            .text()
            .await?,
    );
    let staging = layout.app.join(format!(".incoming-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = async {
        let asset = format!(
            "localwispr-{target}-{}-{variant}.tar.xz",
            std::env::consts::ARCH
        );
        let archive = fetch_verified(&client, &base, &sums, &asset, &staging).await?;
        extract(&archive, &staging)?;
        std::fs::remove_file(&archive)?;
        let bundle = staging.join(asset.trim_end_matches(".tar.xz"));
        let info = BundleInfo::read(&bundle)?;
        if let Some(major) = &info.cuda_runtime {
            ensure_cuda_runtime(&layout, major, &client, &base, &sums, &staging).await?;
        }
        smoke_test(&bundle, &layout, info.cuda_runtime.as_deref())?;
        anyhow::Ok(bundle)
    }
    .await;
    let bundle = match result {
        Ok(bundle) => bundle,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    drop(_lock);
    exec_install(&bundle, opts.no_service)
}

/// Runs the new binary once so a bundle with missing libraries is never
/// switched to.
fn smoke_test(bundle: &Path, layout: &Layout, cuda: Option<&str>) -> Result<()> {
    let mut cmd = Command::new(bundle.join("bin/localwispr"));
    cmd.arg("--version").stdin(Stdio::null());
    if let Some(major) = cuda {
        // The RPATH points at the runtime relative to versions/<bundle>/bin,
        // which the staged bundle is not in yet.
        let mut paths = vec![layout.runtime(major)];
        if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        cmd.env("LD_LIBRARY_PATH", std::env::join_paths(paths)?);
    }
    let out = cmd.output().context("running the new version")?;
    if !out.status.success() {
        bail!(
            "the downloaded version does not start on this system:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // The LLM worker only starts on demand; check it can load its libraries.
    let worker = bundle.join("bin/localwispr-llm");
    if !worker.is_file() {
        bail!("the downloaded version has no LLM worker");
    }
    let mut ldd = Command::new("ldd");
    ldd.arg(&worker).stdin(Stdio::null());
    if let Some(paths) = cmd.get_envs().find(|(k, _)| *k == "LD_LIBRARY_PATH") {
        ldd.env("LD_LIBRARY_PATH", paths.1.unwrap_or_default());
    }
    // Without ldd (not a glibc system) there is nothing more to check.
    if let Ok(out) = ldd.output() {
        if !out.status.success() {
            bail!(
                "cannot inspect the downloaded LLM worker: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let missing: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("not found"))
            .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
            .collect();
        if !missing.is_empty() {
            bail!(
                "the downloaded version needs libraries this system lacks: {}",
                missing.join(", ")
            );
        }
    }
    Ok(())
}

/// Replaces this process with the bundle's own `install`, so the new
/// version's install logic applies.
fn exec_install(bundle: &Path, no_service: bool) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(bundle.join("bin/localwispr"));
    cmd.arg("install");
    if no_service {
        cmd.arg("--no-service");
    }
    Err(cmd.exec()).context("starting the new version's installer")
}

/// Daemon task: when enabled in the config, checks for a new release at most
/// once a day and calls `notify` once per new version.
pub async fn check_daily(notify: impl Fn(String)) {
    const DAY: u64 = 24 * 3600;
    let state = dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/state"))
        .join(config::APP_ID)
        .join("update-check");
    // Let the daemon finish loading models and the network come up first.
    tokio::time::sleep(Duration::from_secs(120)).await;
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // "<unix time of last check> <last version announced>"
        let saved = std::fs::read_to_string(&state).unwrap_or_default();
        let mut fields = saved.split_whitespace();
        let last: u64 = fields.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let announced = fields.next().unwrap_or("").to_string();
        if config::Config::load_or_default().updates.check && now.saturating_sub(last) >= DAY {
            match latest_version().await {
                Ok(latest) => {
                    let mut record = announced.clone();
                    if is_newer(&latest, VERSION) && latest != announced {
                        notify(latest.clone());
                        desktop_notification(
                            &format!("localwispr {latest} is available"),
                            "Install it from Settings → Models → Updates, or run `localwispr update`.",
                        )
                        .await;
                        record = latest;
                    }
                    let _ = std::fs::create_dir_all(state.parent().unwrap_or(Path::new(".")));
                    let _ = std::fs::write(&state, format!("{now} {record}\n"));
                }
                Err(e) => tracing::debug!("update check failed: {e:#}"),
            }
        }
        // Wall-clock based, so suspended laptops still check daily.
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Best effort freedesktop notification.
async fn desktop_notification(summary: &str, body: &str) {
    let result = async {
        let conn = zbus::Connection::session().await?;
        let hints: HashMap<&str, zbus::Variant<'_>> = HashMap::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "localwispr",
                0u32,
                "audio-input-microphone",
                summary,
                body,
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("desktop notification failed: {e:#}");
    }
}

/// Parses `localwispr update --check` output; returns the newer version.
pub fn parse_check_output(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("update available: "))
        .and_then(|l| l.rsplit_once(" → "))
        .map(|(_, v)| v.trim().to_string())
}

/// argv that runs `localwispr <args>` in its own systemd scope when possible,
/// so restarting the service (as updates do) does not kill it with the
/// service's process group.
pub fn own_scope_argv(args: &[&str]) -> Vec<std::ffi::OsString> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "localwispr".into());
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some() || has_systemd();
    if under_systemd && which("systemd-run") {
        argv.extend(
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--",
            ]
            .map(Into::into),
        );
    }
    argv.push(exe.into());
    argv.extend(args.iter().map(Into::into));
    argv
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

// ----------------------------------------------------------------- install --

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("GNOME")))
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn has_systemd() -> bool {
    run_quiet("systemctl", &["--user", "show-environment"])
}

fn unit_file(bin: &Path) -> Result<String> {
    Ok(format!(
        "[Unit]
Description=localwispr dictation daemon
Documentation=https://github.com/{REPO}
PartOf=graphical-session.target
After=graphical-session.target pipewire.service

[Service]
Type=dbus
BusName=org.localwispr.Daemon
ExecStart={} daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
",
        systemd_quote(bin)?
    ))
}

/// Quotes the executable path for a systemd `ExecStart=` line. systemd
/// takes `$` literally there but cannot express every path.
fn systemd_quote(path: &Path) -> Result<String> {
    let path = path.to_string_lossy();
    if path.contains(['\\', '"']) || path.chars().any(char::is_control) {
        bail!("systemd cannot start a program from {path:?}; install to another directory");
    }
    Ok(format!("\"{}\"", path.replace('%', "%%")))
}

/// Quotes a path for a desktop entry `Exec=` key. Paths launchers cannot
/// quote reliably fall back to looking the program up in PATH.
fn desktop_quote(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.contains(['"', '`', '$', '\\', '%']) || path.chars().any(char::is_control) {
        return "localwispr".into();
    }
    format!("\"{path}\"")
}

/// Quotes a path for pasting into a POSIX shell.
fn shell_quote(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~+".contains(c))
    {
        path.into_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

fn desktop_entry(bin: &Path) -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=localwispr Settings
Comment=Local voice dictation
Exec={} settings
Icon=audio-input-microphone
Categories=Utility;Settings;
StartupNotify=true
",
        desktop_quote(bin)
    )
}

/// Adds the extension to GNOME's enabled list so the Shell loads it at the
/// next login (it only discovers new extensions then). `list` is the output of
/// `gsettings get org.gnome.shell enabled-extensions`.
fn with_extension_enabled(list: &str) -> Option<String> {
    let list = list.trim();
    if list.contains(&format!("'{EXT_UUID}'")) {
        return None;
    }
    let list = list.strip_prefix("@as ").unwrap_or(list).trim();
    if list == "[]" {
        Some(format!("['{EXT_UUID}']"))
    } else {
        let inner = list.strip_prefix('[')?.strip_suffix(']')?;
        Some(format!("[{inner}, '{EXT_UUID}']"))
    }
}

/// Returns true when the Shell is running the extension already.
fn enable_gnome_extension() -> bool {
    if run_quiet("gnome-extensions", &["enable", EXT_UUID]) {
        return true;
    }
    let Ok(out) = Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
    else {
        return false;
    };
    if let Some(list) = with_extension_enabled(&String::from_utf8_lossy(&out.stdout)) {
        run_quiet(
            "gsettings",
            &["set", "org.gnome.shell", "enabled-extensions", &list],
        );
    }
    false
}

pub struct InstallOptions {
    pub service: bool,
}

/// Installs the bundle this binary belongs to and makes it current.
pub fn install(opts: InstallOptions) -> Result<()> {
    let exe = std::env::current_exe()?;
    let bundle = exe
        .parent()
        .and_then(Path::parent)
        .context("cannot locate the bundle")?
        .to_path_buf();
    install_bundle(&bundle, opts)
}

fn install_bundle(bundle: &Path, opts: InstallOptions) -> Result<()> {
    let info = BundleInfo::read(bundle)?;
    let layout = Layout::user();
    let _lock = InstallLock::acquire(&layout)?;

    let old_extension = snapshot(&layout.ext_dir);
    let previous = match layout.current_id() {
        // A version whose install did not finish is no rollback target.
        Some(id) => Some(id).filter(|id| layout.versions().join(id).join(INSTALLED).exists()),
        None => layout.preserve_legacy()?,
    };
    let name = layout.adopt(bundle)?;
    layout.switch(&name)?;
    let current = layout.current();
    println!(
        "==> Installed localwispr {} ({})",
        info.version, info.variant
    );

    let bin = layout.bin_dir.join("localwispr");
    replace_symlink(&current.join("bin/localwispr"), &bin)?;
    // install.sh put the worker here before bundles existed. A daemon of that
    // version that keeps running (no service restart) looks for it here, so
    // it points at the current worker until the next install.
    let legacy_llm = layout.bin_dir.join("localwispr-llm");
    match std::fs::symlink_metadata(&legacy_llm) {
        Ok(m) if m.is_file() => replace_symlink(&current.join("bin/localwispr-llm"), &legacy_llm)?,
        Ok(_) => {
            let _ = std::fs::remove_file(&legacy_llm);
        }
        Err(_) => {}
    }
    let in_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d == layout.bin_dir));

    let new_ext = current.join("share/gnome-shell/extensions").join(EXT_UUID);
    replace_symlink(&new_ext, &layout.ext_dir)?;
    let extension_changed = old_extension != snapshot(&new_ext);

    write_if_changed(&layout.apps_dir.join(DESKTOP_FILE), &desktop_entry(&bin))?;

    // Keep one version to roll back to: the one replaced or, when the current
    // one was just repaired or never finished installing, the newest other.
    let fallback = match previous.filter(|p| *p != name) {
        Some(prev) => Some(prev),
        None => layout.other_versions().into_iter().next(),
    };
    let mut keep = vec![name.as_str()];
    keep.extend(fallback.as_deref());
    layout.prune(&keep);

    let mut notes = Vec::new();
    if !in_path {
        notes.push(format!(
            "{} is not in your PATH; add it to use the `localwispr` command",
            layout.bin_dir.display()
        ));
    }
    if is_gnome() {
        let running = enable_gnome_extension();
        if extension_changed || !running {
            notes.push(
                "Log out and back in to load the new GNOME Shell extension \
                 (GNOME cannot reload extensions in a running session)"
                    .into(),
            );
        }
    }

    // Restarting is the last step: it may end the process that started us.
    let service = opts.service && has_systemd();
    if opts.service && !service {
        notes.push(format!(
            "No systemd user session: start `{} daemon` from your compositor's autostart",
            bin.display()
        ));
    }
    if service {
        if write_if_changed(&layout.unit_dir.join(UNIT), &unit_file(&bin)?)? {
            run_quiet("systemctl", &["--user", "daemon-reload"]);
        }
        run_quiet("systemctl", &["--user", "enable", UNIT]);
        // Desktops that start graphical-session.target import these
        // themselves; others need them for the daemon to reach Wayland.
        let vars: Vec<&str> = ["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP"]
            .into_iter()
            .filter(|v| std::env::var_os(v).is_some())
            .collect();
        if !vars.is_empty() {
            let mut args = vec!["--user", "import-environment"];
            args.extend(&vars);
            run_quiet("systemctl", &args);
        }
        if !run_quiet(
            "systemctl",
            &["--user", "is-active", "--quiet", "graphical-session.target"],
        ) {
            notes.push(
                "Your session does not start graphical-session.target, so the service won't \
                 start at login. Add this to your compositor's autostart:\n    \
                 systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP && \
                 systemctl --user start localwispr"
                    .into(),
            );
        }
    }
    std::fs::write(layout.versions().join(&name).join(INSTALLED), "")?;
    for note in &notes {
        println!("  • {note}");
    }
    let _ = std::io::stdout().flush();
    drop(_lock);
    if service && !run_quiet("systemctl", &["--user", "restart", UNIT]) {
        eprintln!("!! The service failed to start; see: journalctl --user -u localwispr");
    }
    Ok(())
}

pub fn uninstall(undo_permissions: bool) -> Result<()> {
    let layout = Layout::user();
    let _lock = InstallLock::acquire(&layout)?;
    if undo_permissions {
        let script = layout
            .current()
            .join("share/localwispr/setup-permissions.sh");
        let ok = Command::new("bash")
            .arg(&script)
            .arg("--undo")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            bail!("undoing the permission changes failed; nothing was removed");
        }
    }
    if has_systemd() {
        run_quiet("systemctl", &["--user", "disable", "--now", UNIT]);
    }
    run_quiet("gnome-extensions", &["disable", EXT_UUID]);
    for path in [
        layout.unit_dir.join(UNIT),
        layout.apps_dir.join(DESKTOP_FILE),
        layout.bin_dir.join("localwispr"),
        layout.bin_dir.join("localwispr-llm"),
    ] {
        let _ = std::fs::remove_file(path);
    }
    if std::fs::symlink_metadata(&layout.ext_dir).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir_all(&layout.ext_dir)?;
    } else {
        let _ = std::fs::remove_file(&layout.ext_dir);
    }
    // The running binary lives in here; Linux keeps it mapped until exit.
    let _ = std::fs::remove_dir_all(&layout.app);
    if has_systemd() {
        run_quiet("systemctl", &["--user", "daemon-reload"]);
    }
    println!(
        "Uninstalled. Config ({}) and models ({}) were kept.",
        config::config_dir().display(),
        config::models_dir().display()
    );
    if !undo_permissions {
        println!(
            "System permission changes were kept; to remove them:\n  \
             sudo rm -f /etc/udev/rules.d/70-localwispr.rules /etc/modules-load.d/localwispr.conf\n  \
             sudo gpasswd -d $USER input   # only if nothing else needs it"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0-rc1"), Some((0, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let s = parse_sums(&format!("{a}  x.tar.xz\n{b} *y.tar.xz\nnonsense\n"));
        assert_eq!(s["x.tar.xz"], a);
        assert_eq!(s["y.tar.xz"], "b".repeat(64));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn check_output() {
        assert_eq!(
            parse_check_output("update available: 0.1.0 → 0.2.0\n").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(parse_check_output("localwispr 0.1.0 is up to date"), None);
    }

    #[test]
    fn gnome_enabled_list() {
        assert_eq!(
            with_extension_enabled("@as []").as_deref(),
            Some("['localwispr@localwispr.github.io']")
        );
        assert_eq!(
            with_extension_enabled("['a@b']\n").as_deref(),
            Some("['a@b', 'localwispr@localwispr.github.io']")
        );
        assert_eq!(
            with_extension_enabled("['localwispr@localwispr.github.io', 'x']"),
            None
        );
    }

    fn fake_bundle(root: &Path, name: &str, marker: &str) -> PathBuf {
        let b = root.join(name);
        std::fs::create_dir_all(b.join("bin")).unwrap();
        std::fs::write(b.join("bin/localwispr"), marker).unwrap();
        std::fs::write(b.join(INSTALLED), "").unwrap();
        b
    }

    fn layout(root: &Path) -> Layout {
        Layout {
            app: root.join("app"),
            bin_dir: root.join("bin"),
            ext_dir: root.join("ext"),
            unit_dir: root.join("units"),
            apps_dir: root.join("apps"),
        }
    }

    #[test]
    fn legacy_install_is_kept() {
        let tmp = std::env::temp_dir().join(format!("lw-legacy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let l = layout(&tmp);
        assert_eq!(l.preserve_legacy().unwrap(), None);
        std::fs::create_dir_all(&l.bin_dir).unwrap();
        std::fs::create_dir_all(&l.ext_dir).unwrap();
        let bin = l.bin_dir.join("localwispr");
        // Something else under that name is not ours to keep.
        std::fs::write(&bin, "#!/bin/sh\necho something 1.2.3\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert_eq!(l.preserve_legacy().unwrap(), None);
        std::fs::write(&bin, "#!/bin/sh\necho localwispr 0.0.9\n").unwrap();
        std::fs::write(l.bin_dir.join("localwispr-llm"), "old llm").unwrap();
        std::fs::write(l.ext_dir.join("extension.js"), "old ext").unwrap();
        let name = l.preserve_legacy().unwrap().unwrap();
        assert_eq!(name, "localwispr-0.0.9-legacy");
        let v = l.versions().join(&name);
        assert_eq!(
            std::fs::read_to_string(v.join("bin/localwispr-llm")).unwrap(),
            "old llm"
        );
        assert_eq!(
            std::fs::read_to_string(
                v.join("share/gnome-shell/extensions")
                    .join(EXT_UUID)
                    .join("extension.js")
            )
            .unwrap(),
            "old ext"
        );
        assert_eq!(BundleInfo::read(&v).unwrap().variant, LEGACY);
        assert_eq!(l.other_versions(), vec![name]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn exec_paths_are_quoted() {
        let odd = Path::new("/home/a b/50%$x/localwispr");
        assert_eq!(
            systemd_quote(odd).unwrap(),
            r#""/home/a b/50%%$x/localwispr""#
        );
        assert!(systemd_quote(Path::new("/a\"b/localwispr")).is_err());
        assert_eq!(
            desktop_quote(Path::new("/home/a b/lw")),
            r#""/home/a b/lw""#
        );
        assert_eq!(desktop_quote(odd), "localwispr");
        assert_eq!(
            shell_quote(Path::new("/home/x/.local/lw~2")),
            "/home/x/.local/lw~2"
        );
        assert_eq!(shell_quote(Path::new("/it's here")), r#"'/it'\''s here'"#);
    }

    #[test]
    fn running_versions_are_detected() {
        // This test binary runs from the cargo target directory.
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap();
        let busy = versions_in_use(dir.parent().unwrap());
        assert!(busy.contains(&dir.file_name().unwrap().to_string_lossy().into_owned()));
    }

    #[test]
    fn adopt_switch_prune() {
        let tmp = std::env::temp_dir().join(format!("lw-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let l = layout(&tmp);
        let incoming = tmp.join("incoming");

        let a = l.adopt(&fake_bundle(&incoming, "lw-1", "one")).unwrap();
        l.switch(&a).unwrap();
        assert_eq!(l.current_id().as_deref(), Some("lw-1"));
        assert_eq!(
            std::fs::read_to_string(l.current().join("bin/localwispr")).unwrap(),
            "one"
        );

        let b = l.adopt(&fake_bundle(&incoming, "lw-2", "two")).unwrap();
        l.switch(&b).unwrap();
        // Reinstalling an existing name never touches the installed copy.
        let b2 = l
            .adopt(&fake_bundle(&incoming, "lw-2", "two again"))
            .unwrap();
        assert_eq!(b2, "lw-2~2");
        assert_eq!(
            std::fs::read_to_string(l.current().join("bin/localwispr")).unwrap(),
            "two"
        );
        l.switch(&b2).unwrap();
        // Adopting the installed directory itself is a no-op.
        assert_eq!(l.adopt(&l.versions().join("lw-2~2")).unwrap(), "lw-2~2");

        l.adopt(&fake_bundle(&incoming, "lw-3", "three")).unwrap();
        let own = l.app.join(format!(".incoming-{}", std::process::id()));
        let other = l.app.join(".incoming-someone-else");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        l.prune(&["lw-2~2", "lw-1"]);
        assert!(!l.versions().join("lw-3").exists());
        assert!(!l.versions().join("lw-2").exists());
        assert!(!own.exists());
        // Another installer's fresh download is left alone.
        assert!(other.exists());
        assert_eq!(l.other_versions(), vec!["lw-1".to_string()]);
        assert!(l.switch("lw-3").is_err());

        // Only staging directories count as hidden leftovers.
        std::fs::create_dir_all(l.versions().join(".keep-me")).unwrap();
        l.prune(&["lw-2~2", "lw-1"]);
        assert!(l.versions().join(".keep-me").exists());

        // A directory left by the old installer is replaced by a symlink.
        std::fs::create_dir_all(l.ext_dir.join("sub")).unwrap();
        replace_symlink(&l.current(), &l.ext_dir).unwrap();
        assert!(std::fs::symlink_metadata(&l.ext_dir).unwrap().is_symlink());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
