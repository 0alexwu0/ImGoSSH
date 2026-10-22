//! The `known_hosts` database.
//!
//! Entries are read from both `%APPDATA%\ImGoSSH\known_hosts` and the
//! user's `~/.ssh/known_hosts`, so keys accepted here are understood by the
//! OpenSSH tools (and vice versa). New approvals are written to ours.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use russh::keys::{parse_public_key_base64, PublicKey};

use crate::config::known_hosts_path;

/// Result of comparing a presented key against the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The host is not in the database yet.
    Unknown,
    /// A key of the same algorithm matches.
    Trusted,
    /// The host is known but the key changed (possible man-in-the-middle).
    Changed { recorded: PublicKey },
}

/// Every file we consult, most trusted first.
pub fn files() -> Vec<PathBuf> {
    let mut out = vec![known_hosts_path()];
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".ssh").join("known_hosts"));
    }
    out
}

/// The host pattern OpenSSH would use for this endpoint.
pub fn pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Keys recorded for `host:port` (any algorithm).
pub fn lookup(host: &str, port: u16) -> Vec<PublicKey> {
    let target = pattern(host, port);
    let mut keys = Vec::new();
    for path in files() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(hosts), Some(algo), Some(blob)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if hosts.starts_with('|') {
                // Hashed entry: we cannot match it without the HMAC key.
                continue;
            }
            if !matches_pattern(hosts, &target) {
                continue;
            }
            if let Ok(key) = parse_public_key_base64(blob) {
                if key.algorithm().as_str() == algo {
                    keys.push(key);
                }
            }
        }
    }
    keys
}

/// OpenSSH `known_hosts` matching: a comma separated list where an entry with
/// a leading `!` excludes the host, and any other match includes it.
fn matches_pattern(patterns: &str, target: &str) -> bool {
    let mut matched = false;
    for entry in patterns.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some(negative) = entry.strip_prefix('!') {
            if pattern_hit(negative, target) {
                return false;
            }
        } else if pattern_hit(entry, target) {
            matched = true;
        }
    }
    matched
}

fn pattern_hit(pattern: &str, target: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        glob_match(pattern, target)
    } else {
        pattern.eq_ignore_ascii_case(target)
    }
}

/// Small `*`/`?` matcher for `known_hosts` wildcards.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi].to_ascii_lowercase() == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Compare a presented key with the database.
pub fn check(host: &str, port: u16, key: &PublicKey) -> Status {
    let recorded = lookup(host, port);
    let same_algo = recorded
        .iter()
        .filter(|k| k.algorithm() == key.algorithm())
        .collect::<Vec<_>>();
    if same_algo.is_empty() {
        // A different algorithm may still be known; treat the new one as unknown.
        return Status::Unknown;
    }
    if same_algo.iter().any(|k| *k == key) {
        Status::Trusted
    } else {
        Status::Changed {
            recorded: (*same_algo[0]).clone(),
        }
    }
}

/// Append an approval to our own database.
pub fn trust(host: &str, port: u16, key: &PublicKey) -> Result<()> {
    let path = known_hosts_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let openssh = key.to_openssh().context("encoding public key")?;
    let line = format!("{} {}\n", pattern(host, port), openssh);

    // Replace a conflicting entry rather than leaving two keys for one host.
    if check(host, port, key) != Status::Trusted {
        replace_host_entry(host, port, &line)?;
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Drop every line for `host:port` in our own file and append `line`.
fn replace_host_entry(host: &str, port: u16, line: &str) -> Result<()> {
    let path = known_hosts_path();
    let target = pattern(host, port);
    let mut kept = String::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for existing in text.lines() {
            let first = existing.split_whitespace().next().unwrap_or("");
            if first.starts_with('|') || !matches_pattern(first, &target) {
                kept.push_str(existing);
                kept.push('\n');
            }
        }
    }
    kept.push_str(line);
    std::fs::write(&path, kept).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Human readable `SHA256:...` fingerprint.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert_eq!(pattern("example.com", 22), "example.com");
        assert_eq!(pattern("example.com", 2222), "[example.com]:2222");
        assert!(matches_pattern("example.com", "example.com"));
        assert!(matches_pattern("*.example.com", "host.example.com"));
        assert!(!matches_pattern("*.example.com", "example.com"));
        assert!(matches_pattern(
            "!bad.example.com,*.example.com",
            "ok.example.com"
        ));
        assert!(!matches_pattern(
            "!bad.example.com,*.example.com",
            "bad.example.com"
        ));
        assert!(matches_pattern("[example.com]:2222", "[example.com]:2222"));
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }
}
