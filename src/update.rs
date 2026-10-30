//! Online updates: the newest release on GitHub, downloaded and installed.
//!
//! Every published build is one archive per platform and GitHub records a
//! `sha256:` digest for each asset, so an update needs no installer and no
//! service: ask the release API for the newest tag, compare it with the version
//! compiled into this binary, fetch the archive built for this platform, check
//! its digest, then move the new files over the ones beside the running
//! executable. That last step is the delicate one - a running executable cannot
//! be written to, so every new file is copied in under `.new` first and the
//! swap itself is a rename, which Windows allows even for the program asking.
//!
//! None of it runs on the UI thread. The worker writes into one [`Mutex`] and
//! asks egui for the frame that shows the change, the same way the transfer
//! queue reports its progress.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use egui::Context;
use serde::Deserialize;

use crate::config::Config;
use crate::config::{APP_NAME, APP_VERSION, RELEASE_API_URL};
use crate::t;

/// How long an answer is worth keeping, so reopening the app every few minutes
/// does not spend the anonymous GitHub request allowance on the same answer.
pub const RECHECK_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// What a file is called while the one it replaces is moved out of the way.
const BACKUP_SUFFIX: &str = ".old";
/// What a new file is called until every binary has been copied in.
const WAITING_SUFFIX: &str = ".new";
/// How often a download reports its progress, and the window its speed is
/// measured over.
const TICK: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// A release version: three numbers and an optional `-rc1` style tail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// Empty means a published release, which outranks any of its candidates.
    pre: String,
}

impl Version {
    /// Read `v0.1.3`, `0.1` or `1.2.0-rc2`, ignoring a leading `v` and any
    /// build metadata.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        // Build metadata is written after a `+`, and does not order a release.
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = text.split_once('-').unwrap_or((text, ""));
        let mut numbers = core.split('.');
        let major = numbers.next()?.parse().ok()?;
        let minor = numbers.next().unwrap_or("0").parse().ok()?;
        let patch = numbers.next().unwrap_or("0").parse().ok()?;
        let pre = pre.trim().to_ascii_lowercase().to_string();
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// The version as it is written on a release page.
    #[must_use]
    pub fn text(&self) -> String {
        if self.pre.is_empty() {
            format!("{}.{}.{}", self.major, self.minor, self.patch)
        } else {
            format!("{}.{}.{}-{}", self.major, self.minor, self.patch, self.pre)
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) | (false, false) => self.pre.cmp(&other.pre),
                // A release is newer than its own candidates.
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The version compiled into this binary.
#[must_use]
pub fn current_version() -> Version {
    Version::parse(APP_VERSION).unwrap_or_default()
}
// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

/// One file attached to a release.
#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The lower-case hex SHA-256 GitHub recorded, when it recorded one.
    pub sha256: Option<String>,
}

/// A published release.
#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub title: String,
    /// The release notes with the markdown taken off, ready to read in a window.
    pub notes: String,
    pub url: String,
    /// The day it was published, `2026-10-28`, which is all a user needs.
    pub published: String,
    pub assets: Vec<Asset>,
}

impl Release {
    #[must_use]
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag)
    }

    /// The archive built for the platform this process runs on, if the release
    /// has one.
    #[must_use]
    pub fn asset(&self) -> Option<&Asset> {
        pick_asset(&self.assets)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApiRelease {
    tag_name: String,
    name: String,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    assets: Vec<ApiAsset>,
}

impl From<ApiRelease> for Release {
    fn from(api: ApiRelease) -> Self {
        let title = if api.name.trim().is_empty() {
            api.tag_name.clone()
        } else {
            api.name.clone()
        };
        Self {
            tag: api.tag_name,
            title,
            notes: release_notes(api.body.as_deref().unwrap_or("")),
            url: api.html_url,
            published: api
                .published_at
                .unwrap_or_default()
                .split('T')
                .next()
                .unwrap_or("")
                .to_string(),
            assets: api
                .assets
                .into_iter()
                .map(|asset| Asset {
                    name: asset.name,
                    url: asset.browser_download_url,
                    size: asset.size,
                    sha256: asset.digest.as_deref().and_then(parse_digest),
                })
                .collect(),
        }
    }
}

/// The hex out of a `sha256:<hex>` digest. Anything that is not sixty-four hex
/// digits is not a SHA-256, and pretending otherwise would verify nothing.
#[must_use]
fn parse_digest(text: &str) -> Option<String> {
    let hex = text.rsplit(':').next()?.trim().to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Take the markdown off a release body.
///
/// A changelog is prose, and the markup that decorates it on GitHub - headings,
/// bold, code ticks - only gets in the way of reading it in a dialog. The fenced
/// blocks and the download table go with it: one is build instructions, and the
/// other lists the checksums this module has already checked for itself.
#[must_use]
pub fn release_notes(body: &str) -> String {
    let mut out = String::new();
    let mut pending_blank = false;
    let mut fenced = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || line.starts_with('|') || line.starts_with("![") || line.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        let text = clean_markdown(line);
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if pending_blank {
                out.push('\n');
            }
        }
        out.push_str(&text);
        pending_blank = false;
    }
    out
}

fn clean_markdown(line: &str) -> String {
    let mut text = line;
    while let Some(rest) = text.strip_prefix('#') {
        text = rest;
    }
    text = text.trim_start_matches('>');
    let bullet = if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
        text = rest;
        true
    } else {
        false
    };
    let text: String = text.trim().chars().filter(|c| *c != '`').collect();
    let text = text.replace("**", "");
    if text.is_empty() {
        String::new()
    } else if bullet {
        format!("\u{2022} {text}")
    } else {
        text
    }
}

/// The word that marks an archive as built for this operating system.
#[must_use]
pub fn platform_token() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn names_platform(name: &str) -> bool {
    let aliases: &[&str] = if cfg!(windows) {
        &["windows", "win64", "mingw"]
    } else if cfg!(target_os = "macos") {
        &["macos", "darwin", "osx"]
    } else {
        &["linux"]
    };
    aliases.iter().any(|token| name.contains(token))
}

/// How well an archive name suits this CPU: `0` rules it out, `2` names it.
fn arch_score(name: &str) -> u8 {
    let (ours, theirs): (&[&str], &[&str]) = if cfg!(target_arch = "aarch64") {
        (&["aarch64", "arm64"], &["x86_64", "amd64", "x64"])
    } else if cfg!(target_arch = "x86_64") {
        (&["x86_64", "amd64", "x64"], &["aarch64", "arm64"])
    } else {
        return 1;
    };
    if theirs.iter().any(|token| name.contains(token)) {
        0
    } else if ours.iter().any(|token| name.contains(token)) {
        2
    } else {
        1
    }
}

/// The one archive a user of this build should download.
#[must_use]
pub fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let mut best: Option<(u8, &Asset)> = None;
    for asset in assets {
        let name = asset.name.to_ascii_lowercase();
        if !(name.ends_with(".zip") || name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
            continue;
        }
        if !names_platform(&name) {
            continue;
        }
        let score = arch_score(&name);
        if score == 0 {
            continue;
        }
        if best.is_none_or(|(previous, _)| score > previous) {
            best = Some((score, asset));
        }
    }
    best.map(|(_, asset)| asset)
}

/// Where the newest release is asked for. Debug builds honour
/// `IMGOSSH_UPDATE_FEED`, so the whole flow can be walked against a test
/// repository without waiting for a real release.
#[must_use]
pub fn feed_url() -> String {
    if cfg!(debug_assertions) {
        if let Ok(url) = std::env::var("IMGOSSH_UPDATE_FEED") {
            if !url.trim().is_empty() {
                return url.trim().to_string();
            }
        }
    }
    RELEASE_API_URL.to_string()
}

fn user_agent() -> String {
    format!("{APP_NAME}/{APP_VERSION}")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build()
}

/// Why a job stopped: the user changed their mind, or something went wrong.
enum Stopped {
    Cancelled,
    Failed(String),
}

impl Stopped {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

impl From<std::io::Error> for Stopped {
    fn from(err: std::io::Error) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<zip::result::ZipError> for Stopped {
    fn from(err: zip::result::ZipError) -> Self {
        Self::failed(format!(
            "{}: {err}",
            t!("The package could not be unpacked")
        ))
    }
}

type Job<T> = Result<T, Stopped>;

/// Ask GitHub for the newest published release.
fn fetch_release(url: &str) -> Job<Release> {
    let response = agent()
        .get(url)
        .set("User-Agent", &user_agent())
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(403 | 429, _) => {
                Stopped::failed(t!("GitHub is answering too many requests right now").to_string())
            }
            ureq::Error::Status(404, _) => {
                Stopped::failed(t!("No published release was found").to_string())
            }
            ureq::Error::Status(code, _) => {
                Stopped::failed(format!("{} {code}", t!("GitHub answered")))
            }
            transport => Stopped::failed(format!("{}: {transport}", t!("Could not reach GitHub"))),
        })?;
    let body = response
        .into_string()
        .map_err(|err| Stopped::failed(err.to_string()))?;
    let api: ApiRelease = serde_json::from_str(&body).map_err(|err| {
        Stopped::failed(format!(
            "{}: {err}",
            t!("The release list could not be read")
        ))
    })?;
    Ok(Release::from(api))
}
// ---------------------------------------------------------------------------
// What the interface is told
// ---------------------------------------------------------------------------

/// How far the update has got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stage {
    /// Nothing asked yet.
    #[default]
    Idle,
    /// Waiting for the release API.
    Checking,
    /// This build is the newest one.
    Latest,
    /// There is something newer to download.
    Available,
    Downloading,
    Verifying,
    Installing,
    /// The new files are in place and only a restart is missing.
    Ready,
    Failed,
}

/// Everything the update window shows.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub stage: Stage,
    /// Whatever the API last answered, newer or not, so the window can say
    /// which version this build was compared with.
    pub release: Option<Release>,
    pub downloaded: u64,
    pub total: u64,
    /// Bytes per second over the last tick.
    pub rate: f64,
    pub error: Option<String>,
    /// `None` means the published archive carries no checksum to compare with.
    pub verified: Option<bool>,
    pub checked_at: Option<SystemTime>,
    /// The files the install step replaced.
    pub replaced: Vec<String>,
    /// The files it left alone because another program was using them.
    pub skipped: Vec<String>,
}

impl Status {
    #[must_use]
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.total as f64) as f32
        }
    }

    /// The version an update would put on this machine.
    #[must_use]
    pub fn available(&self) -> Option<Version> {
        self.release.as_ref().and_then(Release::version)
    }

    #[must_use]
    pub fn busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Checking | Stage::Downloading | Stage::Verifying | Stage::Installing
        )
    }

    /// Roughly how long the rest of a download will take, in seconds.
    #[must_use]
    pub fn eta(&self) -> Option<f64> {
        if self.rate <= 1.0 || self.total < self.downloaded {
            return None;
        }
        Some((self.total - self.downloaded) as f64 / self.rate)
    }
}

/// One job at a time, off the UI thread, with its progress on show.
pub struct Updater {
    status: Arc<Mutex<Status>>,
    busy: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    /// Set once the new binaries are on disk and only a relaunch is missing.
    restart: Arc<AtomicBool>,
    feed: String,
}

/// The shared state, taken even after a worker panicked: an update that died
/// must not be able to freeze the window.
fn guarded(slot: &Mutex<Status>) -> std::sync::MutexGuard<'_, Status> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

impl Updater {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::default())),
            busy: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            restart: Arc::new(AtomicBool::new(false)),
            feed: feed_url(),
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        guarded(&self.status).clone()
    }

    #[must_use]
    pub fn stage(&self) -> Stage {
        guarded(&self.status).stage
    }

    /// True while a worker thread is out fetching something.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn ready_to_restart(&self) -> bool {
        self.restart.load(Ordering::Relaxed)
    }

    /// Stop a download: what came in is thrown away and nothing is installed.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Ask GitHub for the newest release. Does nothing while a job is running.
    pub fn check(&self, ctx: &Context) {
        if self.busy.swap(true, Ordering::Relaxed) {
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        {
            let mut status = guarded(&self.status);
            status.stage = Stage::Checking;
            status.error = None;
        }
        let status = Arc::clone(&self.status);
        let feed = self.feed.clone();
        self.spawn("update-check", ctx.clone(), move || {
            let outcome = fetch_release(&feed);
            let mut status = guarded(&status);
            status.checked_at = Some(SystemTime::now());
            status.error = None;
            match outcome {
                Ok(release) => {
                    let newer = release
                        .version()
                        .is_some_and(|version| version > current_version());
                    status.stage = if newer {
                        Stage::Available
                    } else {
                        Stage::Latest
                    };
                    status.release = Some(release);
                }
                Err(Stopped::Failed(message)) => {
                    status.stage = Stage::Failed;
                    status.error = Some(message);
                }
                Err(Stopped::Cancelled) => status.stage = Stage::Idle,
            }
        });
    }

    /// Download the newest archive, check it, and move it into place.
    pub fn install(&self, ctx: &Context) {
        if self.busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let asset = guarded(&self.status)
            .release
            .as_ref()
            .and_then(Release::asset)
            .cloned();
        let Some(asset) = asset else {
            self.busy.store(false, Ordering::Relaxed);
            return;
        };
        self.cancel.store(false, Ordering::Relaxed);
        {
            let mut status = guarded(&self.status);
            status.stage = Stage::Downloading;
            status.error = None;
            status.verified = None;
            status.downloaded = 0;
            status.total = asset.size;
            status.rate = 0.0;
            status.replaced.clear();
            status.skipped.clear();
        }
        let status = Arc::clone(&self.status);
        let cancel = Arc::clone(&self.cancel);
        let restart = Arc::clone(&self.restart);
        self.spawn("update-install", ctx.clone(), move || {
            let outcome = run_job(&status, &cancel, &asset);
            let mut status = guarded(&status);
            match outcome {
                Ok((replaced, skipped)) => {
                    status.stage = Stage::Ready;
                    status.replaced = replaced;
                    status.skipped = skipped;
                    status.error = None;
                    status.rate = 0.0;
                    restart.store(true, Ordering::Relaxed);
                }
                Err(Stopped::Cancelled) => {
                    status.stage = Stage::Available;
                    status.downloaded = 0;
                    status.rate = 0.0;
                }
                Err(Stopped::Failed(message)) => {
                    status.stage = Stage::Failed;
                    status.error = Some(message);
                }
            }
        });
    }

    /// Run `work` on its own thread, then ask for the frame that shows what it
    /// decided. A thread that cannot start is reported rather than left waiting
    /// for an answer that will never come.
    fn spawn(&self, name: &str, ctx: Context, work: impl FnOnce() + Send + 'static) {
        let busy = Arc::clone(&self.busy);
        let status = Arc::clone(&self.status);
        let painted = ctx.clone();
        let started = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                work();
                busy.store(false, Ordering::Relaxed);
                painted.request_repaint();
            });
        if let Err(err) = started {
            log::warn!("{name}: {err}");
            {
                let mut status = guarded(&status);
                status.stage = Stage::Failed;
                status.error = Some(format!("{}: {err}", t!("The update could not start")));
            }
            self.busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        }
    }
}

// ---------------------------------------------------------------------------
// The job: download, check, unpack, install
// ---------------------------------------------------------------------------

fn stage(slot: &Arc<Mutex<Status>>, value: Stage) {
    guarded(slot).stage = value;
}

/// Download, check, unpack and install, so that every way the job can end has
/// the same tidy-up behind it.
fn run_job(
    status: &Arc<Mutex<Status>>,
    cancel: &AtomicBool,
    asset: &Asset,
) -> Job<(Vec<String>, Vec<String>)> {
    let dir = job_dir();
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let outcome = job_inside(&dir, status, cancel, asset);
    // The archive and what was unpacked from it have served their purpose once
    // the files are in place - or not - either way they are thirty-odd
    // megabytes of somebody else's temporary folder.
    let _ = fs::remove_dir_all(&dir);
    outcome
}

fn job_inside(
    dir: &Path,
    status: &Arc<Mutex<Status>>,
    cancel: &AtomicBool,
    asset: &Asset,
) -> Job<(Vec<String>, Vec<String>)> {
    let archive = download(asset, dir, status, cancel)?;

    stage(status, Stage::Verifying);
    let digest = sha256_file(&archive)?;
    match &asset.sha256 {
        Some(expected) => {
            let same = digest.eq_ignore_ascii_case(expected);
            guarded(status).verified = Some(same);
            if !same {
                return Err(Stopped::failed(
                    t!("The download did not match the published checksum").to_string(),
                ));
            }
        }
        // Nothing published to compare with: the window says so out loud.
        None => guarded(status).verified = None,
    }

    let unpacked = dir.join("unpacked");
    fs::create_dir_all(&unpacked)?;
    extract(&archive, &unpacked)?;

    stage(status, Stage::Installing);
    install_files(&unpacked)
}

/// Where the archive and its contents wait: the temporary folder, out of reach
/// of the installation directory.
fn job_dir() -> PathBuf {
    std::env::temp_dir().join(format!("{}-update-{}", APP_NAME, std::process::id()))
}

fn download(
    asset: &Asset,
    dir: &Path,
    status: &Arc<Mutex<Status>>,
    cancel: &AtomicBool,
) -> Job<PathBuf> {
    let response = agent()
        .get(&asset.url)
        .set("User-Agent", &user_agent())
        .call()
        .map_err(|err| Stopped::failed(format!("{}: {err}", t!("The download failed"))))?;
    let total = response
        .header("content-length")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or((asset.size > 0).then_some(asset.size));
    {
        let mut status = guarded(status);
        status.total = total.unwrap_or(0);
        status.downloaded = 0;
    }

    let path = dir.join(&asset.name);
    let mut file = fs::File::create(&path)?;
    let mut body = response.into_reader();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut window = Instant::now();
    let mut since = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Stopped::Cancelled);
        }
        let read = body
            .read(&mut buffer)
            .map_err(|err| Stopped::failed(format!("{}: {err}", t!("The download stopped"))))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        since += read as u64;
        if window.elapsed() >= TICK {
            let seconds = window.elapsed().as_secs_f64().max(0.001);
            let mut status = guarded(status);
            status.downloaded = done;
            status.rate = since as f64 / seconds;
            window = Instant::now();
            since = 0;
        }
    }
    file.flush()?;
    if total.is_some_and(|total| done != total) {
        return Err(Stopped::failed(
            t!("The download stopped halfway through").to_string(),
        ));
    }
    guarded(status).downloaded = done;
    Ok(path)
}

fn sha256_file(path: &Path) -> Job<String> {
    use ring::digest::{Context as Digest, SHA256};

    let mut file = fs::File::open(path)?;
    let mut digest = Digest::new(&SHA256);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Unpack an archive into a folder of its own.
fn extract(archive: &Path, into: &Path) -> Job<()> {
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        unpack_tar(archive, into)
    } else if name.ends_with(".zip") {
        unpack_zip(archive, into)
    } else {
        Err(Stopped::failed(
            t!("This package cannot be unpacked").to_string(),
        ))
    }
}

fn unpack_zip(archive: &Path, into: &Path) -> Job<()> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|err| {
        Stopped::failed(format!("{}: {err}", t!("This package cannot be unpacked")))
    })?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        // `enclosed_name` refuses a member that would climb out of the folder.
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let target = into.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// The Linux package is a `.tar.gz`; the other platforms ship plain zips.
#[cfg(target_os = "linux")]
fn unpack_tar(archive: &Path, into: &Path) -> Job<()> {
    let file = fs::File::open(archive)?;
    let mut packed = tar::Archive::new(flate2::read::GzDecoder::new(file));
    packed.unpack(into)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn unpack_tar(_archive: &Path, _into: &Path) -> Job<()> {
    Err(Stopped::failed(
        t!("This package cannot be unpacked").to_string(),
    ))
}

/// The names a release archive is built from.
fn is_release_file(name: &str) -> bool {
    matches!(
        name,
        "imgossh"
            | "imgossh.exe"
            | "igssh"
            | "igssh.exe"
            | "igscp"
            | "igscp.exe"
            | "igsftp"
            | "igsftp.exe"
            | "license"
            | "readme.md"
    )
}

/// Every installable file in an unpacked archive, by lower-case name.
///
/// The Windows zip puts its binaries at the root while the Linux and macOS
/// packages wrap them in a named folder, so the tree is searched rather than
/// assumed and the shallowest copy of a name wins.
fn collect(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut found: BTreeMap<String, (usize, PathBuf)> = BTreeMap::new();
    let mut stack = vec![(0usize, root.to_path_buf())];
    while let Some((depth, dir)) = stack.pop() {
        if depth > 3 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push((depth + 1, path));
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !is_release_file(&name) {
                continue;
            }
            match found.get(&name) {
                Some((shallowest, _)) if *shallowest <= depth => {}
                _ => {
                    found.insert(name, (depth, path));
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(name, (_, path))| (name, path))
        .collect()
}

/// The folder the running executable lives in, which is what gets replaced.
#[must_use]
pub fn install_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    exe.as_ref()
        .and_then(|path| path.parent())
        .map_or_else(|| PathBuf::from("."), |dir| dir.to_path_buf())
}

fn own_file_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| format!("imgossh{}", std::env::consts::EXE_SUFFIX))
}

fn holds(names: &[String], name: &str) -> bool {
    names
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(name))
}

/// Move the archive's files over the installed ones.
///
/// Every new file is copied in beside its target under `.new` before anything
/// moves, so a full disk or a half-written copy leaves the installation exactly
/// as it was. A file that will not move aside - a command line tool running in
/// another window, say - is left alone and reported rather than failing the
/// whole update.
fn install_files(unpacked: &Path) -> Job<(Vec<String>, Vec<String>)> {
    let dir = install_dir();
    let own = own_file_name();
    let installed: Vec<String> = fs::read_dir(&dir)?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    // Only what is already installed is replaced; the archive may hold files
    // this folder has never seen.
    let wanted: Vec<(String, PathBuf)> = collect(unpacked)
        .into_iter()
        .filter_map(|(name, source)| {
            let existing = installed
                .iter()
                .find(|held| held.eq_ignore_ascii_case(&name))?;
            Some((existing.clone(), source))
        })
        .collect();
    if !holds(
        &wanted
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        &own,
    ) {
        return Err(Stopped::failed(
            t!("The package does not hold a build of this program").to_string(),
        ));
    }

    let mut waiting = Vec::with_capacity(wanted.len());
    for (name, source) in &wanted {
        let aside = dir.join(format!("{name}{WAITING_SUFFIX}"));
        let _ = fs::remove_file(&aside);
        fs::copy(source, &aside)?;
        make_executable(&aside);
        waiting.push((name.clone(), aside));
    }

    let mut replaced = Vec::new();
    let mut skipped = Vec::new();
    for (name, aside) in &waiting {
        let target = dir.join(name);
        let backup = dir.join(format!("{name}{BACKUP_SUFFIX}"));
        let _ = fs::remove_file(&backup);
        if target.exists() && fs::rename(&target, &backup).is_err() {
            log::warn!("{name} is in use and stays as it is");
            skipped.push(name.clone());
            let _ = fs::remove_file(aside);
            continue;
        }
        if let Err(err) = fs::rename(aside, &target) {
            // Put back whatever was moved out of the way.
            let _ = fs::rename(&backup, &target);
            let _ = fs::remove_file(aside);
            return Err(Stopped::failed(format!(
                "{} {name}: {err}",
                t!("Could not install")
            )));
        }
        replaced.push(name.clone());
    }

    if !holds(&replaced, &own) {
        return Err(Stopped::failed(
            t!("The running program could not be replaced").to_string(),
        ));
    }
    Ok((replaced, skipped))
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// Delete the `.old` copies a previous update left beside the binaries.
pub fn cleanup_backups() {
    let Ok(entries) = fs::read_dir(install_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(BACKUP_SUFFIX) {
            // The program that installed the update may well still be running,
            // in which case this simply fails and tries again next start.
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Start the newly installed program, so closing this one does not look like an
/// update that stopped halfway.
pub fn relaunch() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match std::process::Command::new(&exe).spawn() {
        Ok(_) => log::info!("relaunched after the update"),
        Err(err) => log::warn!("could not start {exe:?}: {err}"),
    }
}

// ---------------------------------------------------------------------------
// The questions the shell asks between frames
// ---------------------------------------------------------------------------

/// Whole seconds since the epoch, which is how the settings file remembers a
/// check without needing a date type of its own.
#[must_use]
pub fn now_seconds() -> i64 {
    epoch_seconds(SystemTime::now())
}

/// The same, for a moment the updater already recorded.
#[must_use]
pub fn epoch_seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// Whether it is worth asking GitHub again: never asked, or asked long enough
/// ago that the answer may have changed.
#[must_use]
pub fn is_due(config: &Config) -> bool {
    if !config.check_updates {
        return false;
    }
    if config.last_update_check <= 0 {
        return true;
    }
    let Some(then) =
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(config.last_update_check as u64))
    else {
        return true;
    };
    SystemTime::now()
        .duration_since(then)
        .map_or(true, |gone| gone >= RECHECK_AFTER)
}

/// A version the user asked not to be told about again.
#[must_use]
pub fn is_skipped(config: &Config, version: &Version) -> bool {
    Version::parse(&config.skipped_version).is_some_and(|skipped| skipped == *version)
}

/// When a check last finished, for the settings window to say so.
#[must_use]
pub fn last_check(config: &Config) -> Option<SystemTime> {
    if config.last_update_check <= 0 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(config.last_update_check as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    fn sample(name: &str) -> Asset {
        Asset {
            name: name.into(),
            url: format!("https://example.invalid/{name}"),
            size: 1024,
            sha256: None,
        }
    }

    #[test]
    fn versions_order_the_way_a_release_page_does() {
        assert!(version("v0.2.0") > version("0.1.9"));
        assert!(version("0.1.10") > version("0.1.9"));
        assert_eq!(version("v0.1.3"), version("0.1.3"));
        assert!(version("1.0.0") > version("1.0.0-rc1"));
        assert!(version("1.0.0-rc2") > version("1.0.0-rc1"));
        assert_eq!(version("1.2").text(), "1.2.0");
        assert_eq!(version("2.0.0+build7").text(), "2.0.0");
        assert!(Version::parse("no release yet").is_none());
    }

    #[test]
    fn the_package_for_this_machine_is_the_one_picked() {
        let assets = [
            sample("ImGoSSH-0.2.0-linux-x86_64.tar.gz"),
            sample("ImGoSSH-0.2.0-macos-aarch64.zip"),
            sample("ImGoSSH-0.2.0-macos-x86_64.zip"),
            sample("ImGoSSH-0.2.0-windows-x64.zip"),
            sample("ImGoSSH-0.2.0-sources.zip"),
        ];
        let picked = pick_asset(&assets).expect("this platform is published");
        let name = picked.name.to_ascii_lowercase();
        assert!(name.contains(platform_token()), "picked {name}");
        assert!(!name.contains("sources"), "a source bundle is no package");
        if cfg!(target_arch = "aarch64") {
            assert!(
                name.contains("aarch64") || name.contains("arm64"),
                "picked {name}"
            );
        }
        if cfg!(target_arch = "x86_64") {
            assert!(
                !name.contains("aarch64") && !name.contains("arm64"),
                "picked {name}"
            );
        }
    }

    #[test]
    fn a_package_for_another_platform_is_not_picked() {
        assert!(pick_asset(&[sample("ImGoSSH-0.2.0-someother-os.zip")]).is_none());
        assert!(pick_asset(&[sample("ImGoSSH-0.2.0-windows-x64.txt")]).is_none());
    }

    #[test]
    fn only_a_real_sha256_is_trusted() {
        let hex = "a".repeat(64);
        assert_eq!(
            parse_digest(&format!("sha256:{hex}")).as_deref(),
            Some(hex.as_str())
        );
        assert_eq!(parse_digest(&hex).as_deref(), Some(hex.as_str()));
        assert!(parse_digest("md5:d41d8cd98f00b204e9800998ecf8427e").is_none());
        assert!(parse_digest("").is_none());
    }

    #[test]
    fn notes_keep_the_changelog_and_drop_the_markup() {
        let body = "## Changelog\n\n- **Fixed**: a closed link said nothing.\n\n\
                    ```\ncargo build --release\n```\n\n\
                    | Package | SHA-256 |\n| --- | --- |\n| Windows | `abc` |\n";
        let notes = release_notes(body);
        assert!(notes.contains("Changelog"));
        assert!(notes.contains("\u{2022} Fixed: a closed link said nothing."));
        assert!(!notes.contains("cargo build"));
        assert!(!notes.contains("SHA-256"));
        assert!(!notes.contains('`'));
        assert!(!notes.contains("**"));
    }

    #[test]
    fn a_release_is_read_whether_or_not_it_fills_every_field() {
        let api: ApiRelease = serde_json::from_str(r#"{"tag_name":"v9.9.9"}"#).expect("json");
        let release = Release::from(api);
        assert_eq!(release.tag, "v9.9.9");
        // A release with no name is called by its tag.
        assert_eq!(release.title, "v9.9.9");
        assert!(release.notes.is_empty());
        assert!(release.asset().is_none());
    }

    #[test]
    fn progress_reads_as_a_bar_and_an_estimate() {
        let status = Status {
            stage: Stage::Downloading,
            downloaded: 25,
            total: 100,
            rate: 50.0,
            ..Status::default()
        };
        assert!((status.percent() - 0.25).abs() < 0.001);
        assert_eq!(status.eta(), Some(1.5));
        assert!(status.busy());
        // An answer with nothing known about its size never invents a percentage.
        assert_eq!(Status::default().percent(), 0.0);
        assert_eq!(Status::default().eta(), None);
    }
}
