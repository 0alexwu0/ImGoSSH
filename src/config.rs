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
use zeroize::Zeroizing;

use crate::vault;

pub const APP_NAME: &str = "ImGoSSH";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where the source lives: the about window shows it and the Help menu opens it.
pub const REPOSITORY_URL: &str = "https://github.com/0alexwu0/ImGoSSH";
/// Who wrote it, as the about window says.
pub const APP_AUTHOR: &str = "Alex.Wu";
/// The GitHub API answer for the newest published release, which is what an
/// online update is compared against.
pub const RELEASE_API_URL: &str = "https://api.github.com/repos/0alexwu0/ImGoSSH/releases/latest";
/// Every published build, for when an update has to be run by hand.
pub const RELEASES_URL: &str = "https://github.com/0alexwu0/ImGoSSH/releases";

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
    /// Runtime-only gateway mode; never persisted as a reusable connection.
    #[serde(skip)]
    pub jumpserver: bool,
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

    /// Opt-in directory access and command execution from the connected server.
    pub workspace: WorkspaceConfig,

    /// Remote directory for SFTP / SCP sessions.
    pub remote_path: String,
    /// Local directory for SFTP / SCP sessions.
    pub local_path: String,
    pub scp_direction: ScpDirection,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub enabled: bool,
    /// Automatically create real server-side FUSE mounts for the mappings.
    pub mount: bool,
    /// Client directories exposed to the server. Older sessions use `root`.
    pub mappings: Vec<WorkspaceMapping>,
    pub root: String,
    /// Zero asks sshd to allocate a free loopback port.
    pub remote_port: u16,
    pub writable: bool,
    pub allow_exec: bool,
}

/// One client directory and its desired path on the server.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct WorkspaceMapping {
    pub local_path: String,
    pub remote_path: String,
}

impl WorkspaceConfig {
    pub fn effective_mappings(&self) -> Vec<WorkspaceMapping> {
        if !self.mappings.is_empty() {
            return self.mappings.clone();
        }
        if self.root.trim().is_empty() {
            return Vec::new();
        }
        vec![WorkspaceMapping {
            local_path: self.root.clone(),
            remote_path: "/workspace".into(),
        }]
    }
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
        // Tango, for the light ground: hues that have to stay legible on
        // white, and which the default palette below borrows only the shape of.
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
            "Classic" => Palette {
                foreground: [0xec, 0xec, 0xec],
                background: [0x1e, 0x1e, 0x1e],
                cursor: [0xec, 0xec, 0xec],
                selection: [0x2f, 0x46, 0x5c],
                ansi: [
                    [0, 0, 0],
                    [205, 0, 0],
                    [96, 255, 96],
                    [255, 255, 0],
                    [92, 92, 255],
                    [255, 0, 255],
                    [54, 255, 255],
                    [236, 236, 236],
                    [127, 127, 127],
                    [255, 0, 0],
                    [96, 255, 96],
                    [255, 255, 0],
                    [92, 92, 255],
                    [255, 0, 255],
                    [54, 255, 255],
                    [255, 255, 255],
                ],
            },
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
            // The neutral charcoal of a classic Windows console: a flat grey
            // ground and a near-white ink, which is what makes a wall of small
            // Latin text read as crisp rather than as a tint. The hues are the
            // ones a dark ground wants, with the brights held clear of the
            // normals so a bold prompt still stands out from a plain one.
            "Charcoal" => Palette {
                foreground: [0xec, 0xec, 0xec],
                background: [0x1c, 0x1c, 0x1c],
                cursor: [0xd0, 0xd0, 0xd0],
                selection: [0x2f, 0x46, 0x5c],
                ansi: [
                    [0x28, 0x2c, 0x34],
                    [0xe0, 0x6c, 0x75],
                    [0x98, 0xc3, 0x79],
                    [0xe5, 0xc0, 0x7b],
                    [0x61, 0xaf, 0xef],
                    [0xc6, 0x78, 0xdd],
                    [0x56, 0xb6, 0xc2],
                    [0xdc, 0xdf, 0xe4],
                    [0x5c, 0x63, 0x70],
                    [0xff, 0x7f, 0x89],
                    [0xa5, 0xd6, 0x8a],
                    [0xf0, 0xcf, 0x8a],
                    [0x7c, 0xbc, 0xfc],
                    [0xd7, 0x9a, 0xe8],
                    [0x4b, 0xc8, 0xdc],
                    [0xff, 0xff, 0xff],
                ],
            },
            // The default, and the one the interface is dressed with: the
            // terminal the cat is curled around, cut out of the same navy as
            // the tile it sleeps on - `(10, 19, 33)` to `(19, 36, 61)` across
            // the glass. The ink is the `SSH` of the prompt, a cool white
            // rather than the warm grey a charcoal ground wants.
            //
            // Only the colours the picture has an opinion about move: the two
            // greys that stand in for the ground, and the blue and cyan that
            // are the glow on the tile's rim and the prompt itself. Red, green,
            // yellow, magenta stay Tango, because a `ls --color` or a vim
            // scheme out there means something, and repainting it to match a
            // logo would be the icon telling the server what to say.
            _ => Palette {
                foreground: [0xd7, 0xe0, 0xe8],
                background: [0x0d, 0x15, 0x20],
                cursor: [0x25, 0xc7, 0xf6],
                selection: [0x12, 0x45, 0x6a],
                ansi: [
                    [0x1e, 0x2b, 0x3a],
                    [0xcc, 0x00, 0x00],
                    [0x26, 0xa2, 0x66],
                    [0xc4, 0xa0, 0x00],
                    [0x2e, 0x6f, 0xbe],
                    [0x75, 0x50, 0x7b],
                    [0x1f, 0xb6, 0xd8],
                    [0xd3, 0xdb, 0xe2],
                    [0x55, 0x67, 0x7c],
                    [0xef, 0x29, 0x29],
                    [0x4e, 0x9a, 0x06],
                    [0xc4, 0xa0, 0x00],
                    [0x6f, 0xa8, 0xe8],
                    [0xad, 0x7f, 0xa8],
                    [0x4a, 0xe6, 0xf7],
                    [0xf2, 0xf6, 0xf9],
                ],
            },
        }
    }

    pub fn names() -> [&'static str; 5] {
        ["ImGo Dark", "Charcoal", "Classic", "ImGo Light", "Xterm"]
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
    /// Whether the user has answered the optional session encryption prompt.
    pub session_encryption_prompted: bool,
    pub window_width: f32,
    pub window_height: f32,
    /// Ask GitHub for a newer release on startup and every few hours after.
    pub check_updates: bool,
    /// A version the user chose to stop hearing about, `0.1.4` for instance.
    pub skipped_version: String,
    /// When a release check last finished, in seconds since the epoch, so that
    /// reopening the app does not immediately ask the same question again.
    pub last_update_check: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            theme: Theme::ImGo,
            palette: "ImGo Dark".into(),
            // Use the platform's standard monospace face, with grid and CJK
            // fallbacks installed separately by the terminal renderer.
            font_family: if cfg!(target_os = "windows") {
                "Consolas"
            } else if cfg!(target_os = "macos") {
                "Menlo"
            } else {
                "DejaVu Sans Mono"
            }
            .into(),
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
            session_encryption_prompted: false,
            window_width: 1200.0,
            window_height: 760.0,
            check_updates: true,
            skipped_version: String::new(),
            last_update_check: 0,
        }
    }
}

impl Config {
    /// Read window preferences without opening or unlocking the session file.
    pub fn load() -> Self {
        read_toml(&store_dir().join("config.toml")).unwrap_or_default()
    }

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
    access: SessionsAccess,
    remembered: bool,
    pub session_error: Option<String>,
    legacy_sessions: bool,
}

enum SessionsAccess {
    Plain,
    Locked,
    Unavailable,
    Encrypted(vault::Key),
}

impl Default for Store {
    fn default() -> Self {
        Self::load()
    }
}

impl Store {
    /// Load from disk, falling back to sane defaults on any error.
    pub fn load() -> Self {
        Self::load_from(&store_dir(), true)
    }

    pub(crate) fn load_from(dir: &Path, recall: bool) -> Self {
        let config_path = dir.join("config.toml");
        let sessions_path = dir.join("sessions.toml");

        let config = read_toml::<Config>(&config_path).unwrap_or_else(|| {
            if config_path.exists() {
                log::warn!("could not parse {}, using defaults", config_path.display());
            }
            Config::default()
        });

        let mut store = Self {
            config,
            sessions: Vec::new(),
            config_path,
            sessions_path,
            access: SessionsAccess::Plain,
            remembered: false,
            session_error: None,
            legacy_sessions: false,
        };
        match std::fs::read(&store.sessions_path) {
            Ok(data) if vault::is_encrypted(&data) => {
                store.access = SessionsAccess::Locked;
                if recall {
                    if let Ok(password) = vault::recalled(&store.sessions_path) {
                        store.remembered = true;
                        let _ = store.unlock(&password);
                    }
                }
            }
            Ok(data) => match Self::parse_sessions(&data) {
                Ok(sessions) => {
                    store.sessions = sessions;
                    store.legacy_sessions = true;
                }
                Err(_) => {
                    store.access = SessionsAccess::Unavailable;
                    store.session_error =
                        Some("Cannot read sessions.toml; the file will not be overwritten".into());
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                store.access = SessionsAccess::Unavailable;
                store.session_error =
                    Some("Cannot read sessions.toml; the file will not be overwritten".into());
            }
        }
        store
    }

    pub fn save(&self) -> Result<()> {
        write_toml(&self.config_path, &self.config)?;
        match &self.access {
            // Never turn a locked, corrupt, or unreadable vault into an empty database.
            SessionsAccess::Locked | SessionsAccess::Unavailable => Ok(()),
            SessionsAccess::Plain
                if self.needs_encryption_prompt()
                    || (self.sessions.is_empty() && !self.sessions_path.exists()) =>
            {
                Ok(())
            }
            SessionsAccess::Plain => {
                atomic_write(&self.sessions_path, self.session_text()?.as_bytes())
            }
            SessionsAccess::Encrypted(key) => atomic_write(
                &self.sessions_path,
                &key.seal(self.session_text()?.as_bytes())?,
            ),
        }
    }

    fn parse_sessions(bytes: &[u8]) -> Result<Vec<Session>> {
        let text = std::str::from_utf8(bytes).context("Invalid session encoding")?;
        // TOML diagnostics may quote secrets: never return the original parse error.
        toml::from_str::<SessionsFile>(text)
            .map(|f| f.session)
            .map_err(|_| anyhow::anyhow!("Invalid session data"))
    }

    fn session_text(&self) -> Result<Zeroizing<String>> {
        Ok(Zeroizing::new(toml::to_string_pretty(&SessionsFile {
            session: self
                .sessions
                .iter()
                .filter(|s| !s.jumpserver)
                .cloned()
                .collect(),
        })?))
    }

    pub fn sessions_locked(&self) -> bool {
        matches!(self.access, SessionsAccess::Locked)
    }
    pub fn sessions_writable(&self) -> bool {
        matches!(
            self.access,
            SessionsAccess::Plain | SessionsAccess::Encrypted(_)
        )
    }
    pub fn sessions_encrypted(&self) -> bool {
        matches!(
            self.access,
            SessionsAccess::Locked | SessionsAccess::Encrypted(_)
        )
    }
    pub fn password_remembered(&self) -> bool {
        self.remembered
    }

    pub fn needs_encryption_prompt(&self) -> bool {
        matches!(self.access, SessionsAccess::Plain)
            && !self.config.session_encryption_prompted
            && (self.legacy_sessions || !self.sessions.is_empty())
    }

    pub fn decline_encryption(&mut self) -> Result<()> {
        self.config.session_encryption_prompted = true;
        self.save()
    }

    pub fn unlock(&mut self, password: &str) -> Result<()> {
        anyhow::ensure!(self.sessions_locked(), "Sessions are not locked");
        let data = std::fs::read(&self.sessions_path).context("Reading encrypted sessions")?;
        let (key, text) = vault::Key::open(&data, password)?;
        let mut saved = Self::parse_sessions(&text)?;
        // Sessions made while locked are temporary and must not become persisted on unlock.
        self.sessions.clear();
        self.sessions.append(&mut saved);
        self.access = SessionsAccess::Encrypted(key);
        Ok(())
    }

    pub fn remember_password(&mut self, password: &str) -> Result<()> {
        anyhow::ensure!(
            matches!(self.access, SessionsAccess::Encrypted(_)),
            "Unlock sessions first"
        );
        let data = std::fs::read(&self.sessions_path)?;
        vault::Key::open(&data, password)?;
        vault::remember(&self.sessions_path, password)?;
        self.remembered = true;
        Ok(())
    }

    pub fn forget_password(&mut self) -> Result<()> {
        if !self.remembered {
            return Ok(());
        }
        vault::forget(&self.sessions_path)?;
        self.remembered = false;
        Ok(())
    }

    /// Re-encrypt only after the current password has authenticated the on-disk file.
    pub fn set_encryption(&mut self, current: &str, password: Option<&str>) -> Result<()> {
        anyhow::ensure!(self.sessions_writable(), "Unlock sessions first");
        if self.sessions_encrypted() {
            vault::Key::open(&std::fs::read(&self.sessions_path)?, current)?;
        }
        let key = password.map(vault::Key::new).transpose()?;
        let text = self.session_text()?;
        let data = Zeroizing::new(match &key {
            Some(key) => key.seal(text.as_bytes())?,
            None => text.as_bytes().to_vec(),
        });
        // Remove any stale auto-unlock credential before changing the file's password.
        self.forget_password()?;
        atomic_write(&self.sessions_path, &data)?;
        self.access = key
            .map(SessionsAccess::Encrypted)
            .unwrap_or(SessionsAccess::Plain);
        self.config.session_encryption_prompted = true;
        write_toml(&self.config_path, &self.config)
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
    let text = toml::to_string_pretty(value).context("serialising settings")?;
    atomic_write(path, text.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A unique temporary file is removed on failure; encrypted writes never stage plaintext.
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod vault_tests {
    use super::*;

    #[test]
    fn temporary_gateway_credentials_cannot_be_written_to_the_session_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        seed(&mut store);
        let mut gateway = Session::new(Protocol::Ssh);
        gateway.jumpserver = true;
        gateway.password = "temporary-gateway-secret".into();
        store.sessions.push(gateway);
        store.decline_encryption().unwrap();
        let text = std::fs::read_to_string(&store.sessions_path).unwrap();
        assert!(!text.contains("temporary-gateway-secret"));
        let reopened = Store::load_from(dir.path(), false);
        assert_eq!(reopened.sessions.len(), 1);
        assert_eq!(reopened.sessions[0].name, "production-secret");
    }

    fn seed(store: &mut Store) {
        let mut session = Session::new(Protocol::Ssh);
        session.name = "production-secret".into();
        session.password = "connection-secret".into();
        store.sessions.push(session);
    }

    #[test]
    fn first_run_defers_offer_and_plaintext_until_first_session_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        assert!(!store.needs_encryption_prompt());
        store.save().unwrap();
        assert!(!store.sessions_path.exists());
        seed(&mut store);
        assert!(store.needs_encryption_prompt());
        store.save().unwrap();
        assert!(!store.sessions_path.exists());
        store.decline_encryption().unwrap();
        let reopened = Store::load_from(dir.path(), false);
        assert!(!reopened.needs_encryption_prompt());
        assert_eq!(reopened.sessions, store.sessions);
    }

    #[test]
    fn migration_locked_writes_and_password_rotation_preserve_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        seed(&mut store);
        write_toml(
            &store.sessions_path,
            &SessionsFile {
                session: store.sessions.clone(),
            },
        )
        .unwrap();
        let original = std::fs::read(&store.sessions_path).unwrap();
        let mut store = Store::load_from(dir.path(), false);
        assert!(store.needs_encryption_prompt());
        store.save().unwrap();
        assert_eq!(std::fs::read(&store.sessions_path).unwrap(), original);
        store.set_encryption("", Some("master-one")).unwrap();
        let sealed = std::fs::read(&store.sessions_path).unwrap();
        assert!(vault::is_encrypted(&sealed));
        assert!(!sealed.windows(17).any(|w| w == b"connection-secret"));
        let mut locked = Store::load_from(dir.path(), false);
        assert!(locked.sessions_locked());
        assert!(!locked.needs_encryption_prompt());
        assert!(locked.sessions.is_empty());
        seed(&mut locked);
        locked.config.font_size = 20.0;
        locked.save().unwrap();
        assert_eq!(std::fs::read(&locked.sessions_path).unwrap(), sealed);
        assert!(locked.unlock("incorrect").is_err());
        assert!(locked.sessions_locked());
        assert!(locked.set_encryption("", None).is_err());
        locked.unlock("master-one").unwrap();
        assert_eq!(locked.sessions, store.sessions);
        assert!(locked
            .set_encryption("incorrect", Some("master-two"))
            .is_err());
        assert_eq!(std::fs::read(&locked.sessions_path).unwrap(), sealed);
        locked
            .set_encryption("master-one", Some("master-two"))
            .unwrap();
        let mut reopened = Store::load_from(dir.path(), false);
        assert!(reopened.unlock("master-one").is_err());
        reopened.unlock("master-two").unwrap();
        reopened.sessions[0].host = "updated.example".into();
        reopened.save().unwrap();
        let mut reopened = Store::load_from(dir.path(), false);
        reopened.unlock("master-two").unwrap();
        assert_eq!(reopened.sessions[0].host, "updated.example");
        reopened.set_encryption("master-two", None).unwrap();
        let plain = Store::load_from(dir.path(), false);
        assert!(!plain.sessions_encrypted());
        assert!(!plain.needs_encryption_prompt());
        assert_eq!(plain.sessions, reopened.sessions);
        let config = std::fs::read_to_string(plain.config_path).unwrap();
        for secret in ["master-one", "master-two", "connection-secret"] {
            assert!(!config.contains(secret));
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn corrupt_or_unreadable_files_are_never_replaced() {
        for bytes in [
            b"password = secret\ninvalid TOML".as_slice(),
            b"ImGoSSH-SESSION-VAULT\0truncated",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sessions.toml");
            std::fs::write(&path, bytes).unwrap();
            let mut store = Store::load_from(dir.path(), false);
            seed(&mut store);
            store.save().unwrap();
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert!(!store.needs_encryption_prompt());
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sessions.toml")).unwrap();
        let mut store = Store::load_from(dir.path(), false);
        assert!(!store.sessions_writable());
        seed(&mut store);
        store.save().unwrap();
        assert!(store.sessions_path.is_dir());
    }

    #[test]
    #[ignore = "requires an unlocked native system credential store"]
    fn native_credential_auto_unlock_and_forget() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        seed(&mut store);
        store
            .set_encryption("", Some("test-vault-password"))
            .unwrap();
        // Always clean up even if an assertion panics.
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = vault::forget(&self.0);
            }
        }
        let _cleanup = Cleanup(store.sessions_path.clone());
        store.remember_password("test-vault-password").unwrap();
        let mut reopened = Store::load_from(dir.path(), true);
        assert!(!reopened.sessions_locked());
        assert!(reopened.password_remembered());
        assert_eq!(reopened.sessions, store.sessions);
        reopened
            .set_encryption("test-vault-password", Some("rotated-vault-password"))
            .unwrap();
        assert!(vault::recalled(&reopened.sessions_path).is_err());
        reopened
            .remember_password("rotated-vault-password")
            .unwrap();
        assert!(!Store::load_from(dir.path(), true).sessions_locked());
        reopened.forget_password().unwrap();
        assert!(Store::load_from(dir.path(), true).sessions_locked());
        assert!(vault::recalled(&reopened.sessions_path).is_err());
    }
}
