//! SSH engine.
//!
//! * [`client`] - connection actor: authentication, shell/exec channels, tunnels.
//! * [`hostkeys`] - the `known_hosts` database.
//! * [`sftp`] - remote directory listing helpers.
//! * [`transfer`] - SCP-style upload/download queue with progress.
//! * [`local`] - local terminal tabs backed by a pseudo-console.

pub mod client;
pub mod hostkeys;
pub mod local;
pub mod monitor;
pub mod sftp;
pub mod transfer;

use client::TerminalHandle;
use local::LocalPty;

/// What a terminal tab is attached to.
pub enum Backend {
    /// A remote shell over an SSH channel.
    Ssh(TerminalHandle),
    /// A local shell inside a pseudo-console.
    Local(LocalPty),
}

impl Backend {
    /// Write raw bytes to the shell.
    pub fn send(&self, bytes: Vec<u8>) {
        match self {
            Backend::Ssh(handle) => {
                if let Err(err) = handle.send(bytes) {
                    log::debug!("terminal input dropped: {err}");
                }
            }
            Backend::Local(pty) => pty.send(bytes),
        }
    }

    /// Tell the shell that the window changed size.
    pub fn resize(&self, rows: u16, cols: u16) {
        match self {
            Backend::Ssh(handle) => handle.resize(rows, cols),
            Backend::Local(pty) => pty.resize(rows, cols),
        }
    }

    /// Hang up the channel / kill the child process.
    pub fn close(&self) {
        match self {
            Backend::Ssh(handle) => handle.close(),
            Backend::Local(pty) => pty.close(),
        }
    }
}
