//! SCP-style upload / download queue with live progress.
//!
//! The job list is driven by the SFTP subsystem rather than the scp protocol,
//! which gives exact byte counts (and therefore real progress bars and transfer
//! rates) for free.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Result};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::t;

use crate::ssh::client::Connection;
use crate::ssh::sftp;
use crate::util::{remote_join, sanitize_file_name};

/// Which way the bytes flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Upload => "Upload",
            Direction::Download => "Download",
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Upload => "\u{2191}",
            Direction::Download => "\u{2193}",
        }
    }
}

/// Where a job is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Queued
    }
}

impl Phase {
    pub fn is_finished(&self) -> bool {
        matches!(self, Phase::Done | Phase::Failed(_) | Phase::Cancelled)
    }

    pub fn label(&self) -> String {
        match self {
            Phase::Queued => t!("queued").into(),
            Phase::Running => t!("transferring").into(),
            Phase::Done => t!("done").into(),
            Phase::Failed(err) => t!("failed: {0}", err),
            Phase::Cancelled => t!("cancelled").into(),
        }
    }
}

/// Live state of a transfer.
#[derive(Debug, Clone)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub current: String,
    pub phase: Phase,
    pub started: Instant,
    pub finished: Option<Instant>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            done: 0,
            total: 0,
            current: String::new(),
            phase: Phase::Queued,
            started: Instant::now(),
            finished: None,
        }
    }
}

impl Progress {
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            return match self.phase {
                Phase::Done => 100.0,
                _ => 0.0,
            };
        }
        (self.done as f32 / self.total as f32 * 100.0).min(100.0)
    }

    pub fn rate(&self) -> f64 {
        let end = self.finished.unwrap_or_else(Instant::now);
        let secs = end.duration_since(self.started).as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.done as f64 / secs
        }
    }

    pub fn eta(&self) -> f64 {
        let rate = self.rate();
        if rate <= 0.0 || self.done >= self.total {
            return 0.0;
        }
        (self.total - self.done) as f64 / rate
    }
}

/// A single queued transfer.
#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub direction: Direction,
    pub source: String,
    pub target: String,
    state: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn snapshot(&self) -> Progress {
        self.state.lock().unwrap().clone()
    }

    pub fn percent(&self) -> f32 {
        self.state.lock().unwrap().percent()
    }

    pub fn is_finished(&self) -> bool {
        self.state.lock().unwrap().phase.is_finished()
    }

    /// Request cancellation; the worker notices between chunks.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The list of transfers shown in the "Transfers" dialog.
#[derive(Clone, Default)]
pub struct Queue {
    next: Arc<AtomicU64>,
    jobs: Arc<Mutex<Vec<Job>>>,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.jobs.lock().unwrap().clone()
    }

    pub fn active(&self) -> usize {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| !j.is_finished())
            .count()
    }

    pub fn clear_finished(&self) {
        self.jobs.lock().unwrap().retain(|j| !j.is_finished());
    }

    fn push(&self, direction: Direction, source: String, target: String) -> Job {
        let job = Job {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            direction,
            source,
            target,
            state: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.jobs.lock().unwrap().push(job.clone());
        job
    }

    /// Queue a remote path (file or directory) for download into `local_dir`.
    pub fn download(
        &self,
        conn: Connection,
        remote: String,
        local_dir: PathBuf,
        recursive: bool,
    ) -> Job {
        let job = self.push(
            Direction::Download,
            remote.clone(),
            local_dir.display().to_string(),
        );
        let state = job.state.clone();
        let cancel = job.cancel.clone();
        tokio::spawn(async move {
            let (worker_state, worker_cancel) = (state.clone(), cancel.clone());
            run(state, cancel, Direction::Download, async move {
                let sftp = conn.sftp().await?;
                let local_dir = local_dir;
                if recursive {
                    let meta = sftp.symlink_metadata(&remote).await.ok();
                    let is_dir = meta.map(|m| m.is_dir()).unwrap_or(false);
                    if is_dir {
                        let name = crate::util::remote_file_name(&remote);
                        let target = local_dir.join(sanitize_file_name(&name));
                        std::fs::create_dir_all(&target).ok();
                        pull_tree(&sftp, &remote, &target, &worker_state, &worker_cancel).await
                    } else {
                        let name = crate::util::remote_file_name(&remote);
                        let target = local_dir.join(sanitize_file_name(&name));
                        download_file(&sftp, &remote, &target, &worker_state, &worker_cancel).await
                    }
                } else {
                    let name = crate::util::remote_file_name(&remote);
                    let target = local_dir.join(sanitize_file_name(&name));
                    download_file(&sftp, &remote, &target, &worker_state, &worker_cancel).await
                }
            })
            .await;
        });
        job
    }

    /// Download one remote file to an exact local path the user chose.
    ///
    /// [`Queue::download`] names the copy after the remote file; this keeps the
    /// name picked in the save dialog instead.
    pub fn download_to_file(&self, conn: Connection, remote: String, local: PathBuf) -> Job {
        let job = self.push(
            Direction::Download,
            remote.clone(),
            local.display().to_string(),
        );
        let state = job.state.clone();
        let cancel = job.cancel.clone();
        tokio::spawn(async move {
            let (worker_state, worker_cancel) = (state.clone(), cancel.clone());
            run(state, cancel, Direction::Download, async move {
                let sftp = conn.sftp().await?;
                download_file(&sftp, &remote, &local, &worker_state, &worker_cancel).await
            })
            .await;
        });
        job
    }
    /// Queue a local path (file or directory) for upload into `remote_dir`.
    pub fn upload(
        &self,
        conn: Connection,
        local: PathBuf,
        remote_dir: String,
        recursive: bool,
    ) -> Job {
        let job = self.push(
            Direction::Upload,
            local.display().to_string(),
            remote_dir.clone(),
        );
        let state = job.state.clone();
        let cancel = job.cancel.clone();
        tokio::spawn(async move {
            let (worker_state, worker_cancel) = (state.clone(), cancel.clone());
            run(state, cancel, Direction::Upload, async move {
                let sftp = conn.sftp().await?;
                mkdir_all(&sftp, &remote_dir).await?;
                if recursive && local.is_dir() {
                    let target = remote_join(
                        &remote_dir,
                        &local.file_name().unwrap_or_default().to_string_lossy(),
                    );
                    sftp::mkdir(&sftp, &target).await.ok();
                    push_tree(&sftp, &local, &target, &worker_state, &worker_cancel).await
                } else if local.is_dir() {
                    push_tree(&sftp, &local, &remote_dir, &worker_state, &worker_cancel).await
                } else {
                    let target = remote_join(&remote_dir, &file_name(&local));
                    upload_file(&sftp, &local, &target, &worker_state, &worker_cancel).await
                }
            })
            .await;
        });
        job
    }
}

/// SFTP has no `mkdir -p`, so walk down from the first existing ancestor.
async fn mkdir_all(sftp: &SftpSession, dir: &str) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();
    let mut current = crate::util::remote_clean(dir);
    loop {
        if current == "/" || current == "." || current.is_empty() {
            break;
        }
        if sftp.metadata(&current).await.is_ok() {
            break;
        }
        missing.push(current.clone());
        let parent = crate::util::remote_parent(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    for path in missing.iter().rev() {
        match sftp::mkdir(sftp, path).await {
            Ok(()) => {}
            // A parallel job may have created it in the meantime.
            Err(err) if sftp.metadata(path).await.is_err() => return Err(err),
            Err(_) => {}
        }
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

async fn run<F>(state: Arc<Mutex<Progress>>, cancel: Arc<AtomicBool>, direction: Direction, fut: F)
where
    F: std::future::Future<Output = Result<()>>,
{
    {
        let mut p = state.lock().unwrap();
        p.phase = Phase::Running;
        p.started = Instant::now();
    }
    if cancel.load(Ordering::Relaxed) {
        state.lock().unwrap().phase = Phase::Cancelled;
        return;
    }
    log::debug!("{direction:?} starting");
    match fut.await {
        Ok(()) => {
            let mut p = state.lock().unwrap();
            if cancel.load(Ordering::Relaxed) {
                p.phase = Phase::Cancelled;
            } else {
                p.phase = Phase::Done;
                p.finished = Some(Instant::now());
            }
        }
        Err(err) => {
            let mut p = state.lock().unwrap();
            p.phase = if cancel.load(Ordering::Relaxed) {
                Phase::Cancelled
            } else {
                Phase::Failed(format!("{err:#}"))
            };
            p.finished = Some(Instant::now());
            log::warn!("transfer failed: {err:#}");
        }
    }
}

async fn download_file(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    state: &Arc<Mutex<Progress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let mut src = sftp::open_read(sftp, remote).await?;
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let total = src.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut file = tokio::fs::File::create(local).await?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        {
            let mut p = state.lock().unwrap();
            p.total = total;
            p.current = remote.to_string();
        }
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        state.lock().unwrap().done += n as u64;
    }
    file.flush().await?;
    Ok(())
}

async fn upload_file(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    state: &Arc<Mutex<Progress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let meta = tokio::fs::metadata(local).await?;
    let mut file = tokio::fs::File::open(local).await?;
    let mut dst = sftp::open_write(sftp, remote).await?;
    let mut buf = vec![0u8; 256 * 1024];
    {
        let mut p = state.lock().unwrap();
        p.total = meta.len();
        p.current = remote.to_string();
    }
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        state.lock().unwrap().done += n as u64;
    }
    dst.flush().await?;
    dst.shutdown().await.ok();
    Ok(())
}

async fn pull_tree(
    sftp: &SftpSession,
    remote: &str,
    local_dir: &Path,
    state: &Arc<Mutex<Progress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let entries = sftp::list(sftp, remote).await?;
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let target = local_dir.join(sanitize_file_name(&entry.name));
        if entry.is_dir() {
            std::fs::create_dir_all(&target).ok();
            Box::pin(pull_tree(sftp, &entry.path, &target, state, cancel)).await?;
        } else {
            download_file(sftp, &entry.path, &target, state, cancel).await?;
        }
    }
    Ok(())
}

async fn push_tree(
    sftp: &SftpSession,
    local_dir: &Path,
    remote_dir: &str,
    state: &Arc<Mutex<Progress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(local_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = remote_join(remote_dir, &name);
        if path.is_dir() {
            sftp::mkdir(sftp, &target).await.ok();
            Box::pin(push_tree(sftp, &path, &target, state, cancel)).await?;
        } else {
            upload_file(sftp, &path, &target, state, cancel).await?;
        }
    }
    Ok(())
}
