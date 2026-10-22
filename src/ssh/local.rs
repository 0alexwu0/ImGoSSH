//! Local terminal tabs, backed by a pseudo-console (ConPTY on Windows).
//!
//! The `portable-pty` reader is blocking, so it runs on a dedicated thread
//! and pushes bytes into the same [`TermSink`] the SSH channels use. That makes
//! the terminal widget completely transport agnostic.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc;

use crate::ssh::client::{TermEvent, TermSink};

/// A running local shell.
pub struct LocalPty {
    tx: mpsc::UnboundedSender<LocalInput>,
    alive: Arc<AtomicBool>,
}

enum LocalInput {
    Bytes(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Close,
}

impl LocalPty {
    /// Spawn `shell` (or the user's default) inside a new pseudo-console.
    pub fn spawn(shell: Option<&str>, cols: u16, rows: u16, sink: TermSink) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(2),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening a pseudo-console")?;

        let (program, args) = resolve_shell(shell);
        let mut cmd = CommandBuilder::new(&program);
        for arg in &args {
            cmd.arg(arg);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("starting {program}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, mut rx) = mpsc::unbounded_channel::<LocalInput>();

        // Output: blocking reader -> sink callback.
        let alive_out = alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.emit(TermEvent::Data(buf[..n].to_vec())),
                }
            }
            alive_out.store(false, Ordering::SeqCst);
            sink.emit(TermEvent::Closed { exit: None });
        });

        // Input + resize, plus reaping the child so it does not linger.
        let alive_in = alive.clone();
        std::thread::spawn(move || {
            let mut child = child;
            let master = pair.master;
            while let Some(input) = rx.blocking_recv() {
                match input {
                    LocalInput::Bytes(bytes) => {
                        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                            break;
                        }
                    }
                    LocalInput::Resize { rows, cols } => {
                        let _ = master.resize(PtySize {
                            rows: rows.max(2),
                            cols: cols.max(2),
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    LocalInput::Close => break,
                }
            }
            alive_in.store(false, Ordering::SeqCst);
            let _ = child.kill();
        });

        Ok(Self { tx, alive })
    }

    pub fn send(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(LocalInput::Bytes(bytes));
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.tx.send(LocalInput::Resize { rows, cols });
    }

    pub fn close(&self) {
        let _ = self.tx.send(LocalInput::Close);
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

/// Pick a sensible default shell for a local terminal tab.
pub fn default_shell() -> (String, Vec<String>) {
    if let Some(pwsh) = which("pwsh.exe") {
        return (pwsh, vec!["-NoLogo".into()]);
    }
    if let Some(powershell) = which("powershell.exe") {
        return (powershell, vec!["-NoLogo".into()]);
    }
    let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    (comspec, vec![])
}

fn resolve_shell(shell: Option<&str>) -> (String, Vec<String>) {
    match shell.map(str::trim).filter(|s| !s.is_empty()) {
        Some(line) => {
            let mut parts = line.split_whitespace();
            let program = parts.next().unwrap_or("cmd.exe").to_string();
            (program, parts.map(str::to_string).collect())
        }
        None => default_shell(),
    }
}

/// Very small PATH lookup (avoids pulling in a whole `which` crate).
fn which(exe: &str) -> Option<String> {
    if PathBuf::from(exe).is_absolute() && PathBuf::from(exe).exists() {
        return Some(exe.to_string());
    }
    for dir in std::env::var("PATH").ok()?.split(';') {
        let candidate = PathBuf::from(dir).join(exe);
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}
