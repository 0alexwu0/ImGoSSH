//! Persistent configuration and session storage.
//!
//! Layout (under the OS config directory, e.g. `%APPDATA%\ImGoSSH`):
//!
//! * `config.toml`   - application settings (theme, fonts, defaults, window state)
//! * `sessions.toml` - the saved session tree shown in the left sidebar
//! * `known_hosts`   - host key database (OpenSSH format, shared semantics)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "ImGoSSH";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where the source lives: the about window shows it and the Help menu opens it.
pub const REPOSITORY_URL: &str = "https://github.com/0alexwu0/ImGoSSH";
/// Who wrote it, as the about window says.
pub const APP_AUTHOR: &str = "Alex.Wu";

/// Where settings live. `IMGOSSH_HOME` overrides it (handy for portable use).
pub fn store_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("IMGOSSH_HOME") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

pub fn known_hosts_path() -> PathBuf {
    store_dir().join("known_hosts")
}

/// Make sure the storage directory exists.
pub fn ensure_store_dir() -> Result<PathBuf> {
    let dir = store_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Session model
// ---------------------------------------------------------------------------

/// Protocol of a saved session.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Sftp,
    Scp,
    Local,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Sftp => "SFTP",
            Protocol::Scp => "SCP",
            Protocol::Local => "LOCAL",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Ssh => "\u{1f5a5}",
            Protocol::Sftp => "\u{1f4c1}",
            Protocol::Scp => "\u{1f4e4}",
            Protocol::Local => "\u{25b6}",
        }
    }

    /// Protocols that need an SSH connection.
    pub fn is_remote(self) -> bool {
        !matches!(self, Protocol::Local)
    }
}

/// Direction of an SCP session: fetch a remote file or send a local one.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScpDirection {
    #[default]
    Download,
    Upload,
}

/// SSH tunnel flavour: local, remote, or dynamic (SOCKS5).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

impl TunnelKind {
    pub fn label(self) -> &'static str {
        match self {
            TunnelKind::Local => "Local (L)",
            TunnelKind::Remote => "Remote (R)",
            TunnelKind::Dynamic => "Dynamic (D)",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Tunnel {
    pub kind: TunnelKind,
    pub listen_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl Default for Tunnel {
    fn default() -> Self {
        Self {
            kind: TunnelKind::Local,
            listen_port: 8080,
            remote_host: "localhost".into(),
            remote_port: 80,
        }
    }
}

impl Tunnel {
    /// `ssh -L`-style description used in the tunnel list.
    pub fn describe(&self) -> String {
        match self.kind {
            TunnelKind::Local => format!(
                "-L {}:{}:{}",
                self.listen_port, self.remote_host, self.remote_port
            ),
            TunnelKind::Remote => format!(
                "-R {}:{}:{}",
                self.listen_port, self.remote_host, self.remote_port
            ),
            TunnelKind::Dynamic => format!("-D {}", self.listen_port),
        }
    }
}

/// A saved session. All fields have defaults so older files keep loading.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Session {
    pub name: String,
    /// Slash separated tree path, e.g. `Clients/Production`.
    pub folder: String,
    pub protocol: Protocol,

    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Path to an OpenSSH private key.
    pub private_key: String,
    /// Ask the running ssh-agent / Pageant instead of loading a key file.
    pub use_agent: bool,

    /// Command to run instead of a shell (`ssh host -- command`).
    pub startup_command: String,
    /// Script typed into the terminal right after the shell starts.
    pub startup_script: String,
    /// `TERM` value advertised to the server.
    pub term: String,

    pub cols: u16,
    pub rows: u16,
    /// Open the SFTP browser next to a newly started terminal.
    pub sftp_panel: bool,
    /// Seconds between SSH keepalive packets; 0 disables them.
    pub keepalive: u64,
    /// Automatically start the configured tunnels on connect.
    pub auto_tunnels: bool,
    pub tunnels: Vec<Tunnel>,

    /// Remote directory for SFTP / SCP sessions.
    pub remote_path: String,
    /// Local directory for SFTP / SCP sessions.
    pub local_path: String,
    pub scp_direction: ScpDirection,
}

impl Session {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            name: format!("New {} session", protocol.label()),
            protocol,
            port: 22,
            term: "xterm-256color".into(),
            cols: 100,
            rows: 30,
            sftp_panel: true,
            keepalive: 60,
            username: std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    /// `user@host:port`, used in tabs and dialogs.
    pub fn target(&self) -> String {
        if !self.protocol.is_remote() {
            return "Local terminal".into();
        }
        let host = if self.host.is_empty() {
            "unknown"
        } else {
            &self.host
        };
        if self.port == 22 {
            format!("{}@{}", self.username, host)
        } else {
            format!("{}@{}:{}", self.username, host, self.port)
        }
    }

    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            self.target()
        } else {
            self.name.clone()
        }
    }

    /// Folder components, outermost first.
    pub fn folder_parts(&self) -> Vec<&str> {
        self.folder
            .split('/')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect()
    }

    pub fn set_folder_parts(&mut self, parts: &[String]) {
        self.folder = parts.join("/");
    }

    /// Effective keepalive interval.
    pub fn keepalive_interval(&self) -> Option<std::time::Duration> {
        if self.keepalive == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(self.keepalive))
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal appearance
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
    /// The default dark look.
    #[default]
    ImGo,
}

/// A 16 colour ANSI palette plus the terminal defaults.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Palette {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub selection: [u8; 3],
    pub ansi: [[u8; 3]; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Self::preset("ImGo Dark")
    }
}

impl Palette {
    /// The built-in palettes. An unknown name falls back to [`Palette::default`].
    pub fn preset(name: &str) -> Palette {
        let base: [[u8; 3]; 16] = [
            [0x2e, 0x34, 0x36],
            [0xcc, 0x00, 0x00],
            [0x26, 0xa2, 0x66],
            [0xc4, 0xa0, 0x00],
            [0x34, 0x65, 0xa4],
            [0x75, 0x50, 0x7b],
            [0x06, 0x98, 0x9a],
            [0xd3, 0xd7, 0xcf],
            [0x55, 0x57, 0x53],
            [0xef, 0x29, 0x29],
            [0x4e, 0x9a, 0x06],
            [0xc4, 0xa0, 0x00],
            [0x72, 0x9f, 0xcf],
            [0xad, 0x7f, 0xa8],
            [0x34, 0xe2, 0xe2],
            [0xee, 0xee, 0xec],
        ];
        match name {
            "Xterm" => Palette {
                foreground: [0xc0, 0xc0, 0xc0],
                background: [0x00, 0x00, 0x00],
                cursor: [0xc0, 0xc0, 0xc0],
                selection: [0x34, 0x65, 0xa4],
                ansi: [
                    [0, 0, 0],
                    [205, 0, 0],
                    [0, 205, 0],
                    [205, 205, 0],
                    [0, 0, 238],
                    [205, 0, 205],
                    [0, 205, 205],
                    [229, 229, 229],
                    [127, 127, 127],
                    [255, 0, 0],
                    [0, 255, 0],
                    [255, 255, 0],
                    [92, 92, 255],
                    [255, 0, 255],
                    [0, 255, 255],
                    [255, 255, 255],
                ],
            },
            "ImGo Light" => Palette {
                foreground: [0x00, 0x00, 0x00],
                background: [0xff, 0xff, 0xff],
                cursor: [0x00, 0x00, 0x00],
                selection: [0xb3, 0xd7, 0xf7],
                ansi: base,
            },
            _ => Palette {
                foreground: [0xd5, 0xd8, 0xdc],
                background: [0x14, 0x17, 0x1a],
                cursor: [0xae, 0xaf, 0xb0],
                selection: [0x2d, 0x4f, 0x78],
                ansi: base,
            },
        }
    }

    pub fn names() -> [&'static str; 3] {
        ["ImGo Dark", "ImGo Light", "Xterm"]
    }
}

// ---------------------------------------------------------------------------
// Application settings
// ---------------------------------------------------------------------------

fn default_sftp_width() -> f32 {
    380.0
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// `"auto"`, `"en"` or `"zh"`.
    pub language: String,
    pub theme: Theme,
    pub palette: String,
    pub font_family: String,
    pub font_size: f32,
    /// Row height as a multiple of the text box. Ideographs fill that box from
    /// edge to edge, so the extra air is what keeps a page of Chinese from
    /// reading as one solid band.
    pub line_spacing: f32,
    pub scrollback: usize,
    pub cursor_blink: bool,
    pub blink_ms: u64,
    pub audible_bell: bool,
    pub copy_on_select: bool,
    pub alt_gr_click_menu: bool,
    pub default_protocol: Protocol,
    pub default_port: u16,
    pub default_username: String,
    pub default_cols: u16,
    pub default_rows: u16,
    pub default_term: String,
    pub default_keepalive: u64,
    pub open_sftp_panel: bool,
    pub show_sidebar: bool,
    pub show_toolbar: bool,
    pub show_statusbar: bool,
    /// Live server statistics in the status bar.
    pub show_server_monitor: bool,
    pub sidebar_width: f32,
    /// Width of the file browser docked to the right of a terminal.
    /// `sftp_height` is the old name from when the panel sat at the bottom.
    #[serde(alias = "sftp_height", default = "default_sftp_width")]
    pub sftp_width: f32,
    pub confirm_close: bool,
    pub store_passwords: bool,
    pub window_width: f32,
    pub window_height: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            theme: Theme::ImGo,
            palette: "ImGo Dark".into(),
            // Whatever the platform, this is the family whose face covers the
            // whole terminal grid; a machine without it is detected at startup
            // and the next complete face takes over.
            font_family: "Cascadia Mono".into(),
            font_size: 14.0,
            line_spacing: 1.15,
            scrollback: 5000,
            cursor_blink: true,
            blink_ms: 500,
            audible_bell: false,
            copy_on_select: true,
            alt_gr_click_menu: true,
            default_protocol: Protocol::Ssh,
            default_port: 22,
            default_username: std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_default(),
            default_cols: 100,
            default_rows: 30,
            default_term: "xterm-256color".into(),
            default_keepalive: 60,
            open_sftp_panel: true,
            show_sidebar: true,
            show_toolbar: true,
            show_statusbar: true,
            show_server_monitor: true,
            sidebar_width: 250.0,
            sftp_width: default_sftp_width(),
            confirm_close: true,
            store_passwords: true,
            window_width: 1200.0,
            window_height: 760.0,
        }
    }
}

impl Config {
    pub fn palette(&self) -> Palette {
        Palette::preset(&self.palette)
    }

    /// Prefill a brand new session from the defaults.
    pub fn new_session(&self, protocol: Protocol) -> Session {
        let mut s = Session::new(protocol);
        s.port = self.default_port;
        s.username = self.default_username.clone();
        s.cols = self.default_cols;
        s.rows = self.default_rows;
        s.term = self.default_term.clone();
        s.keepalive = self.default_keepalive;
        s.sftp_panel = self.open_sftp_panel;
        s
    }
}

// ---------------------------------------------------------------------------
// On-disk store
// ---------------------------------------------------------------------------

/// Everything we persist, with load/save helpers.
pub struct Store {
    pub config: Config,
    pub sessions: Vec<Session>,
    config_path: PathBuf,
    sessions_path: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::load()
    }
}

impl Store {
    /// Load from disk, falling back to sane defaults on any error.
    pub fn load() -> Self {
        let dir = store_dir();
        let config_path = dir.join("config.toml");
        let sessions_path = dir.join("sessions.toml");

        let config = read_toml::<Config>(&config_path).unwrap_or_else(|| {
            if config_path.exists() {
                log::warn!("could not parse {}, using defaults", config_path.display());
            }
            Config::default()
        });

        let sessions = read_toml::<SessionsFile>(&sessions_path)
            .map(|f| f.session)
            .unwrap_or_default();

        Self {
            config,
            sessions,
            config_path,
            sessions_path,
        }
    }

    pub fn save(&self) -> Result<()> {
        ensure_store_dir()?;
        write_toml(&self.config_path, &self.config)?;
        write_toml(
            &self.sessions_path,
            &SessionsFile {
                session: self.sessions.clone(),
            },
        )
    }

    /// Index of a session by its position in [`Store::sessions`].
    pub fn find(&self, name: &str, folder: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|s| s.name == name && s.folder == folder)
    }

    pub fn remove(&mut self, index: usize) -> Option<Session> {
        if index < self.sessions.len() {
            Some(self.sessions.remove(index))
        } else {
            None
        }
    }

    /// Rename a folder (and every session below it).
    pub fn rename_folder(&mut self, old: &str, new: &str) {
        let old = old.trim_matches('/');
        let new = new.trim_matches('/');
        for s in &mut self.sessions {
            let f = s.folder.trim_matches('/');
            if f == old {
                s.folder = new.to_string();
            } else if let Some(rest) = f.strip_prefix(&format!("{old}/")) {
                s.folder = if new.is_empty() {
                    rest.to_string()
                } else {
                    format!("{new}/{rest}")
                };
            }
        }
    }

    pub fn delete_folder(&mut self, folder: &str) {
        let folder = folder.trim_matches('/');
        self.sessions.retain(|s| {
            let f = s.folder.trim_matches('/');
            !(f == folder || f.starts_with(&format!("{folder}/")))
        });
    }

    pub fn folders(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &self.sessions {
            let mut path = String::new();
            for part in s.folder_parts() {
                path = if path.is_empty() {
                    part.to_string()
                } else {
                    format!("{path}/{part}")
                };
                if !out.contains(&path) {
                    out.push(path.clone());
                }
            }
        }
        out.sort();
        out
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SessionsFile {
    #[serde(default)]
    session: Vec<Session>,
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<T>(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            log::warn!("parsing {}: {err}", path.display());
            None
        }
    }
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = toml::to_string_pretty(value).context("serialising settings")?;
    // Write atomically so a crash cannot truncate the session database.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
