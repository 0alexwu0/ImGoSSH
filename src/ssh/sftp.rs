//! Remote directory helpers built on the SFTP subsystem.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileAttributes;
use russh_sftp::protocol::FileType;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::ssh::client::Connection;
use crate::util::{remote_clean, remote_join, shell_quote};

/// What kind of object an entry refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    /// Nothing was said about it.
    #[default]
    Other,
}

/// One row of the remote file browser.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Directory
    }

    /// `drwxr-xr-x`, the way `ls -l` renders it.
    pub fn permissions(&self) -> String {
        let mut out = String::with_capacity(10);
        out.push(match self.kind {
            Kind::Directory => 'd',
            Kind::Symlink => 'l',
            Kind::File => '-',
            Kind::Other => '?',
        });
        let bits = [
            (0o400, 'r'),
            (0o200, 'w'),
            (0o100, 'x'),
            (0o040, 'r'),
            (0o020, 'w'),
            (0o010, 'x'),
            (0o004, 'r'),
            (0o002, 'w'),
            (0o001, 'x'),
        ];
        for (mask, ch) in bits {
            out.push(if self.mode & mask != 0 { ch } else { '-' });
        }
        out
    }
}

/// Sort directories first, then case-insensitively by name.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// List a remote directory.
pub async fn list(sftp: &SftpSession, path: &str) -> Result<Vec<Entry>> {
    let read = sftp
        .read_dir(path)
        .await
        .with_context(|| format!("listing {path}"))?;
    let mut out = Vec::new();
    for entry in read {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let meta = entry.metadata();
        let kind = match entry.file_type() {
            FileType::Dir => Kind::Directory,
            FileType::Symlink => Kind::Symlink,
            FileType::File => Kind::File,
            FileType::Other => Kind::Other,
        };
        let path = if entry.path().is_empty() {
            remote_join(path, &name)
        } else {
            entry.path()
        };
        out.push(Entry {
            mtime: to_unix(meta.modified()),
            mode: mode_bits(&meta),
            size: meta.len(),
            name,
            path,
            kind,
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

fn to_unix(time: std::io::Result<SystemTime>) -> i64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Permission bits only: the mode without the file-type flags in front of it.
fn permission_bits(permissions: Option<u32>) -> u32 {
    permissions.unwrap_or(0) & 0o7777
}

fn mode_bits(meta: &russh_sftp::client::fs::Metadata) -> u32 {
    permission_bits(meta.permissions)
}

/// What the properties window wants to know about one remote object.
///
/// SFTP reports a modification time and an access time. A birth time is not
/// part of the protocol, so [`created`] asks the server for it separately.
#[derive(Debug, Clone, Default)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub mode: u32,
    pub modified: i64,
    pub accessed: i64,
    pub owner: String,
    pub group: String,
}

/// Ask the server about one path. A symlink is reported as the link itself.
pub async fn stat(sftp: &SftpSession, path: &str) -> Result<Stat> {
    let meta = match sftp.symlink_metadata(path).await {
        Ok(meta) => meta,
        // A dangling link is the usual reason for that to fail.
        Err(_) => sftp.metadata(path).await.context(format!("stat {path}"))?,
    };
    Ok(Stat {
        kind: match meta.file_type() {
            FileType::Dir => Kind::Directory,
            FileType::Symlink => Kind::Symlink,
            FileType::File => Kind::File,
            FileType::Other => Kind::Other,
        },
        size: meta.len(),
        mode: permission_bits(meta.permissions),
        modified: to_unix(meta.modified()),
        accessed: to_unix(meta.accessed()),
        owner: named(meta.user, meta.uid),
        group: named(meta.group, meta.gid),
    })
}

/// `root (0)`, or whichever half the server was willing to name.
fn named(name: Option<String>, id: Option<u32>) -> String {
    match (name, id) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (Some(name), None) => name,
        (None, Some(id)) => id.to_string(),
        (None, None) => String::new(),
    }
}

/// chmod a remote path. Only the permission bits are sent.
pub async fn set_mode(sftp: &SftpSession, path: &str, mode: u32) -> Result<()> {
    let attrs = FileAttributes {
        permissions: Some(mode & 0o7777),
        ..Default::default()
    };
    sftp.set_metadata(path, attrs)
        .await
        .with_context(|| format!("setting permissions on {path}"))
}

/// When a file was born, in epoch seconds, or `0` when the server will not
/// say. SFTP has no birth time, so this asks `stat(1)` over an exec channel.
pub async fn created(conn: &Connection, path: &str) -> i64 {
    let command = format!("stat -c %W -- {}", shell_quote(path));
    let Ok(result) = conn.exec(&command).await else {
        return 0;
    };
    if !result.ok() {
        return 0;
    }
    result
        .stdout_text()
        .trim()
        .parse::<i64>()
        .unwrap_or(0)
        .max(0)
}
/// Resolve `.`/`..`/`~` against the server's view of the filesystem.
pub async fn canonicalize(sftp: &SftpSession, path: &str) -> Result<String> {
    if path.is_empty() || path == "." {
        return Ok(sftp.canonicalize(".").await?);
    }
    let cleaned = remote_clean(path);
    match sftp.canonicalize(&cleaned).await {
        Ok(resolved) => Ok(resolved),
        // Some servers refuse to resolve a path that does not exist yet.
        Err(_) => Ok(cleaned),
    }
}

/// Create a directory.
pub async fn mkdir(sftp: &SftpSession, path: &str) -> Result<()> {
    sftp.create_dir(path)
        .await
        .with_context(|| format!("creating {path}"))
}

/// Delete a file or (empty) directory.
pub async fn remove(sftp: &SftpSession, entry: &Entry) -> Result<()> {
    match entry.kind {
        Kind::Directory => sftp.remove_dir(&entry.path).await,
        _ => sftp.remove_file(&entry.path).await,
    }
    .with_context(|| format!("deleting {}", entry.path))
}

/// Delete recursively, depth first.
pub async fn remove_all(sftp: &SftpSession, entry: &Entry) -> Result<()> {
    if entry.kind != Kind::Directory {
        return remove(sftp, entry).await;
    }
    for child in list(sftp, &entry.path).await? {
        Box::pin(remove_all(sftp, &child)).await?;
    }
    sftp.remove_dir(&entry.path)
        .await
        .with_context(|| format!("deleting {}", entry.path))
}

/// Rename inside the same directory tree.
pub async fn rename(sftp: &SftpSession, from: &str, to: &str) -> Result<()> {
    sftp.rename(from, to)
        .await
        .with_context(|| format!("renaming {from}"))
}

/// Flags used when a remote file is opened for reading.
pub const READ_FLAGS: OpenFlags = OpenFlags::READ;

/// Flags used when uploading (truncate-or-create).
pub const WRITE_FLAGS: OpenFlags = OpenFlags::WRITE
    .union(OpenFlags::CREATE)
    .union(OpenFlags::TRUNCATE);

/// Open a remote file for reading.
pub async fn open_read(sftp: &SftpSession, path: &str) -> Result<russh_sftp::client::fs::File> {
    sftp.open_with_flags(path, READ_FLAGS)
        .await
        .with_context(|| format!("opening {path}"))
}

/// Open (or create) a remote file for writing.
pub async fn open_write(sftp: &SftpSession, path: &str) -> Result<russh_sftp::client::fs::File> {
    sftp.open_with_flags(path, WRITE_FLAGS)
        .await
        .with_context(|| format!("creating {path}"))
}

/// Read a whole remote file, e.g. to preview it.
pub async fn read_to_end(sftp: &SftpSession, path: &str, limit: u64) -> Result<Vec<u8>> {
    let mut file = open_read(sftp, path).await?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        read += n as u64;
        if read >= limit {
            break;
        }
    }
    Ok(buf)
}

/// Write a local buffer to a remote file.
pub async fn write_all(sftp: &SftpSession, path: &str, data: &[u8]) -> Result<()> {
    let mut file = open_write(sftp, path).await?;
    file.write_all(data).await?;
    file.flush().await?;
    file.shutdown().await.ok();
    Ok(())
}
