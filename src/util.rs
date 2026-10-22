//! Small helpers shared by the engine, the UI and the command line tools.

use std::path::Path;

/// `1.5 KiB`-style byte counts, as shown in the file browsers.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// `12.3 MiB/s`.
pub fn human_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", human_size(bytes_per_sec.max(0.0) as u64))
}

/// Seconds -> `1h 02m 03s`, used by the transfer queue.
pub fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "--:--".into();
    }
    let secs = secs.round() as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Seconds -> `150d 04h`, the shape the status bar uses.
pub fn human_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h:02}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// Epoch seconds -> `2026-10-10 13:24`.
pub fn human_time(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let ts = chrono::DateTime::from_timestamp(unix, 0);
    match ts {
        Some(ts) => ts
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => String::new(),
    }
}

/// POSIX path join that tolerates trailing slashes and `~`.
pub fn remote_join(dir: &str, name: &str) -> String {
    if dir == "/" || dir.is_empty() {
        return format!("/{name}").replace("//", "/");
    }
    let dir = dir.trim_end_matches('/');
    format!("{dir}/{name}")
}

/// Parent of a POSIX path (`/var/log` -> `/var`, `/` -> `/`).
pub fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".into(),
        Some(idx) => trimmed[..idx].to_string(),
        None => ".".into(),
    }
}

/// Last component of a POSIX path.
pub fn remote_file_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => trimmed[idx + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Normalise `a/./b/../c` (POSIX, no symlink resolution).
pub fn remote_clean(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

/// True when the character belongs to a "word" for double-click selection.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\')
}

/// Windows-safe file name (used when saving a remote file locally).
pub fn sanitize_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        out.push(match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        });
    }
    let out = out.trim_matches(|c| c == ' ' || c == '.').to_string();
    if out.is_empty() {
        "download".into()
    } else {
        out
    }
}

/// Quote one word for a POSIX shell, so a file name stays a file name.
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Unique sibling path for downloads (`file.txt` -> `file (1).txt`).
pub fn unique_local_path(path: &Path) -> std::path::PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    for i in 1..1000 {
        let candidate = parent.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// `user@host:path` split, tolerant of Windows drive letters in local paths.
pub struct RemoteSpec {
    pub user: Option<String>,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl RemoteSpec {
    /// Parse an scp/rsync style argument. Returns `None` for plain local paths.
    pub fn parse(spec: &str, default_port: u16) -> Option<RemoteSpec> {
        let (prefix, path) = spec.split_once(':')?;
        // `C:` or `\\server\share` is local, not `user@host:`.
        if prefix.len() == 1 || prefix.contains('\\') || prefix.contains('/') {
            return None;
        }
        let (user, host) = match prefix.split_once('@') {
            Some((user, host)) => (Some(user.to_string()), host.to_string()),
            None => (None, prefix.to_string()),
        };
        if host.is_empty() {
            return None;
        }
        Some(RemoteSpec {
            user,
            host,
            port: default_port,
            path: if path.is_empty() {
                ".".into()
            } else {
                path.to_string()
            },
        })
    }

    pub fn display(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}:{}", self.host, self.path),
            None => format!("{}:{}", self.host, self.path),
        }
    }
}

/// Truncate a string on a character boundary with an ellipsis.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Case-insensitive substring match used by the session search box.
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn remote_paths() {
        assert_eq!(remote_join("/var", "log"), "/var/log");
        assert_eq!(remote_join("/var/", "log"), "/var/log");
        assert_eq!(remote_join("/", "etc"), "/etc");
        assert_eq!(remote_parent("/var/log"), "/var");
        assert_eq!(remote_parent("/"), "/");
        assert_eq!(remote_file_name("/var/log/a.txt"), "a.txt");
        assert_eq!(remote_clean("/a/./b/../c"), "/a/c");
        assert_eq!(remote_clean("/../etc"), "/etc");
    }

    #[test]
    fn shell_words_are_quoted() {
        assert_eq!(shell_quote("a.txt"), "'a.txt'");
        assert_eq!(shell_quote("two words"), "'two words'");
        // An embedded quote closes, is escaped, then reopens: 'it'\''s'
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$(rm -rf /)"), r"'$(rm -rf /)'");
    }

    #[test]
    fn specs() {
        let r = RemoteSpec::parse("root@example.com:/tmp/x", 22).unwrap();
        assert_eq!(r.host, "example.com");
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.path, "/tmp/x");
        assert!(RemoteSpec::parse("C:\\Users\\me\\f.txt", 22).is_none());
        assert!(RemoteSpec::parse("plain/path.txt", 22).is_none());
    }
}
