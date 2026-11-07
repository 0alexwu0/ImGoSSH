//! The SSH engine: one actor task owns each connection.
//!
//! `russh` hands out a non-clonable [`client::Handle`], so the handle lives in a
//! dedicated task and everything else talks to it through [`Command`]s. That
//! keeps the UI thread free while still allowing many shell / SFTP / tunnel
//! channels to share a single TCP connection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Msg};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::MethodKind;
use russh::{Channel, ChannelMsg, Disconnect, Pty};
use russh_sftp::client::SftpSession;
use serde_json::{json, Value};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

use crate::config::{Tunnel, TunnelKind, WorkspaceConfig};
use crate::ssh::hostkeys;
use crate::ssh::workspace::{
    mount_command, start_sftp_service, unmount_command, SftpService, WorkspaceInfo,
    WorkspaceService,
};

type RemoteTargets = Arc<Mutex<HashMap<u16, (String, u16)>>>;

// ---------------------------------------------------------------------------
// Interaction with the user (passwords, passphrases, host keys)
// ---------------------------------------------------------------------------

/// Everything the UI needs to show when approving a server key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    /// `Some(...)` when the host was already known with a different key.
    pub recorded: Option<String>,
}

impl HostKeyInfo {
    pub fn is_conflict(&self) -> bool {
        self.recorded.is_some()
    }
}

/// What the engine is asking the user about.
#[derive(Debug, Clone)]
pub enum Challenge {
    HostKey(HostKeyInfo),
    Password {
        host: String,
        username: String,
        incorrect: bool,
    },
    KeyboardInteractive {
        host: String,
        username: String,
        name: String,
        instructions: String,
        prompts: Vec<SecretPrompt>,
    },
    Passphrase {
        key: PathBuf,
        incorrect: bool,
    },
}

/// One `keyboard-interactive` question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPrompt {
    pub text: String,
    pub echo: bool,
}

/// The user's answer.
#[derive(Debug, Clone)]
pub enum Reply {
    TrustKey(Trust),
    Secret(String),
    Responses(Vec<String>),
    Cancel,
}

/// How far a host key approval extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Always,
    Once,
    Never,
}

/// A pending question waiting for an answer from the UI.
pub struct Request {
    pub challenge: Challenge,
    pub reply: oneshot::Sender<Reply>,
}

/// Callback bridge: the engine pushes [`Request`]s, the UI answers them.
#[derive(Clone)]
pub struct Prompter {
    tx: mpsc::UnboundedSender<Request>,
}

impl core::fmt::Debug for Prompter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Prompter").finish_non_exhaustive()
    }
}

impl Prompter {
    /// Create a linked prompter/receiver pair.
    pub fn channel() -> (Prompter, mpsc::UnboundedReceiver<Request>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Prompter { tx }, rx)
    }

    /// Ask a question and wait for the answer.
    pub async fn ask(&self, challenge: Challenge) -> Reply {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Request {
                challenge,
                reply: tx,
            })
            .is_err()
        {
            return Reply::Cancel;
        }
        rx.await.unwrap_or(Reply::Cancel)
    }
}

// ---------------------------------------------------------------------------
// Terminal channels
// ---------------------------------------------------------------------------

/// Something that happens on an open shell channel.
#[derive(Debug, Clone)]
pub enum TermEvent {
    Data(Vec<u8>),
    Closed { exit: Option<u32> },
}

/// Callback invoked from the connection task when a channel produces output.
#[derive(Clone)]
pub struct TermSink(Arc<dyn Fn(TermEvent) + Send + Sync + 'static>);

impl TermSink {
    pub fn new(f: impl Fn(TermEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn emit(&self, event: TermEvent) {
        (self.0)(event);
    }
}

/// Input accepted by an open shell channel.
#[derive(Debug, Clone)]
pub enum TermInput {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Eof,
}

/// Handle used to drive an interactive shell / command channel.
pub struct TerminalHandle {
    tx: mpsc::UnboundedSender<TermInput>,
}

impl TerminalHandle {
    /// Write raw bytes to the remote PTY.
    pub fn send(&self, bytes: Vec<u8>) -> Result<()> {
        self.tx
            .send(TermInput::Data(bytes))
            .map_err(|_| anyhow!("channel is closed"))
    }

    /// Propagate a window resize (`SIGWINCH` on the server).
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.tx.send(TermInput::Resize { rows, cols });
    }

    /// Send end-of-file; the channel closes once the input queue drains.
    pub fn close(&self) {
        let _ = self.tx.send(TermInput::Eof);
    }
}

/// Result of a non-interactive `exec`.
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: Option<u32>,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}
// ---------------------------------------------------------------------------
// russh handler
// ---------------------------------------------------------------------------

struct Handler {
    prompter: Prompter,
    host: String,
    port: u16,
    banner: Arc<Mutex<Option<String>>>,
    remote_targets: RemoteTargets,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        let recorded = hostkeys::check(&self.host, self.port, &key);
        let previous = match &recorded {
            hostkeys::Status::Trusted => return Ok(true),
            hostkeys::Status::Changed { recorded } => Some(hostkeys::fingerprint(recorded)),
            hostkeys::Status::Unknown => None,
        };

        let info = HostKeyInfo {
            host: self.host.clone(),
            port: self.port,
            algorithm: key.algorithm().as_str().to_string(),
            fingerprint: hostkeys::fingerprint(&key),
            recorded: previous,
        };
        match self.prompter.ask(Challenge::HostKey(info)).await {
            Reply::TrustKey(Trust::Always) => {
                if let Err(err) = hostkeys::trust(&self.host, self.port, &key) {
                    log::warn!("could not update known_hosts: {err}");
                }
                Ok(true)
            }
            Reply::TrustKey(Trust::Once) => Ok(true),
            _ => Ok(false),
        }
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        *self.banner.lock().unwrap() = Some(banner.to_string());
        Ok(())
    }

    /// A remote (`-R`) tunnel: the server connected back to us, so dial the
    /// requested local endpoint and pump bytes in both directions.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = u16::try_from(connected_port)
            .ok()
            .and_then(|port| self.remote_targets.lock().unwrap().get(&port).cloned());
        log::debug!("workspace/forwarded channel port={connected_port} target={target:?}");
        let Some((host, port)) = target else {
            drop(reply);
            return Ok(());
        };
        tokio::spawn(async move {
            match TcpStream::connect((host.as_str(), port)).await {
                Ok(socket) => {
                    reply.accept().await;
                    pump_socket(channel, socket).await;
                }
                Err(err) => {
                    log::warn!("remote forward to {host}:{port} failed: {err}");
                    drop(reply);
                }
            }
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Commands understood by the actor
// ---------------------------------------------------------------------------

enum Command {
    Shell {
        cols: u16,
        rows: u16,
        term: String,
        command: Option<String>,
        sink: TermSink,
        reply: oneshot::Sender<Result<TerminalHandle>>,
    },
    Exec {
        command: String,
        reply: oneshot::Sender<Result<ExecResult>>,
    },
    Sftp {
        reply: oneshot::Sender<Result<Arc<SftpSession>>>,
    },
    StartTunnel {
        tunnel: Tunnel,
        reply: oneshot::Sender<Result<()>>,
    },
    StartWorkspace {
        config: WorkspaceConfig,
        reply: oneshot::Sender<Result<WorkspaceInfo>>,
    },
    RunWorkspace {
        command: String,
        cwd: String,
        timeout_secs: u64,
        reply: oneshot::Sender<Result<u64>>,
    },
    WorkspaceJob {
        id: u64,
        reply: oneshot::Sender<Result<Value>>,
    },
    /// A locally accepted socket that must be wired to a `direct-tcpip` channel.
    Pump {
        host: String,
        port: u16,
        socket: Box<TcpStream>,
    },
    Close,
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Parameters for a new connection.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub private_key: Option<PathBuf>,
    pub use_agent: bool,
    pub keepalive: Option<Duration>,
    pub prompter: Prompter,
}

impl ConnectOptions {
    /// Build options from a saved session.
    pub fn from_session(session: &crate::config::Session, prompter: Prompter) -> Self {
        Self {
            host: session.host.clone(),
            port: session.port,
            username: session.username.clone(),
            password: Some(session.password.clone()).filter(|p| !p.is_empty()),
            private_key: Some(session.private_key.clone())
                .filter(|p| !p.trim().is_empty())
                .map(PathBuf::from),
            use_agent: session.use_agent,
            keepalive: session.keepalive_interval(),
            prompter,
        }
    }
}

/// A live SSH connection. Cloning is cheap: every clone drives the same actor.
#[derive(Clone)]
pub struct Connection {
    host: Arc<String>,
    port: Arc<u16>,
    username: Arc<String>,
    cmds: mpsc::UnboundedSender<Command>,
    alive: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    banner: Arc<Mutex<Option<String>>>,
    sftp: Arc<AsyncMutex<Option<Arc<SftpSession>>>>,
    tunnels: Arc<Mutex<Vec<Tunnel>>>,
    workspace: Arc<Mutex<Option<WorkspaceInfo>>>,
    workspace_error: Arc<Mutex<Option<String>>>,
}

/// Alias kept for readability at call sites.
pub type SshClient = Connection;

impl Connection {
    /// Connect and authenticate.
    pub async fn connect(opts: ConnectOptions) -> Result<Self> {
        let ConnectOptions {
            host,
            port,
            username,
            keepalive,
            prompter,
            password,
            private_key,
            use_agent,
        } = opts;

        let config = Arc::new(client::Config {
            keepalive_interval: keepalive,
            keepalive_max: 3,
            ..Default::default()
        });
        let banner = Arc::new(Mutex::new(None));
        let remote_targets = Arc::new(Mutex::new(HashMap::new()));
        let handler = Handler {
            prompter: prompter.clone(),
            host: host.clone(),
            port,
            banner: banner.clone(),
            remote_targets: remote_targets.clone(),
        };

        let mut handle = client::connect(config, (host.as_str(), port), handler)
            .await
            .with_context(|| format!("connecting to {host}:{port}"))?;

        let auth = AuthContext {
            username: username.clone(),
            host: host.clone(),
            password,
            private_key,
            use_agent,
            prompter,
        };
        if let Err(err) = auth.run(&mut handle).await {
            let _ = handle
                .disconnect(Disconnect::ByApplication, "authentication failed", "en")
                .await;
            return Err(err);
        }

        let alive = Arc::new(AtomicBool::new(true));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sftp: Arc<AsyncMutex<Option<Arc<SftpSession>>>> = Arc::new(AsyncMutex::new(None));
        let tunnels: Arc<Mutex<Vec<Tunnel>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let workspace = Arc::new(Mutex::new(None));
        let workspace_error = Arc::new(Mutex::new(None));

        tokio::spawn(actor(
            handle,
            rx,
            tx.clone(),
            alive.clone(),
            error.clone(),
            sftp.clone(),
            tunnels.clone(),
            remote_targets,
            workspace.clone(),
            workspace_error.clone(),
        ));

        Ok(Self {
            host: Arc::new(host),
            port: Arc::new(port),
            username: Arc::new(username),
            cmds: tx,
            alive,
            error,
            banner,
            sftp,
            tunnels,
            workspace,
            workspace_error,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        *self.port
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// `true` while the actor is running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Last transport error, if the connection dropped.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    /// Pre-login banner sent by the server.
    pub fn banner(&self) -> Option<String> {
        self.banner.lock().unwrap().clone()
    }

    /// Open an interactive shell (or run `command` inside a PTY).
    pub async fn shell(
        &self,
        cols: u16,
        rows: u16,
        term: &str,
        command: Option<String>,
        sink: TermSink,
    ) -> Result<TerminalHandle> {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::Shell {
                cols,
                rows,
                term: term.to_string(),
                command,
                sink,
                reply,
            })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))?
    }

    /// Run a command without a PTY and capture its output.
    pub async fn exec(&self, command: &str) -> Result<ExecResult> {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::Exec {
                command: command.to_string(),
                reply,
            })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))?
    }

    /// The connection's SFTP subsystem, opened once and shared by every caller.
    pub async fn sftp(&self) -> Result<Arc<SftpSession>> {
        let cached = self.sftp.lock().await.clone();
        if let Some(existing) = cached {
            return Ok(existing);
        }
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::Sftp { reply })
            .map_err(|_| anyhow!("connection closed"))?;
        let session = rx.await.map_err(|_| anyhow!("connection closed"))??;
        *self.sftp.lock().await = Some(session.clone());
        Ok(session)
    }

    /// Start a tunnel and keep listening until the connection closes.
    pub async fn start_tunnel(&self, tunnel: &Tunnel) -> Result<()> {
        if self
            .active_tunnels()
            .iter()
            .any(|t| t.listen_port == tunnel.listen_port)
        {
            bail!("port {} is already forwarded", tunnel.listen_port);
        }
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::StartTunnel {
                tunnel: tunnel.clone(),
                reply,
            })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))??;
        self.tunnels.lock().unwrap().push(tunnel.clone());
        Ok(())
    }

    /// Tunnels currently listening.
    pub fn active_tunnels(&self) -> Vec<Tunnel> {
        self.tunnels.lock().unwrap().clone()
    }

    /// Share one selected local directory for the lifetime of this connection.
    pub async fn start_workspace(&self, config: &WorkspaceConfig) -> Result<WorkspaceInfo> {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::StartWorkspace {
                config: config.clone(),
                reply,
            })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))?
    }

    pub fn workspace_info(&self) -> Option<WorkspaceInfo> {
        self.workspace.lock().unwrap().clone()
    }

    pub fn workspace_error(&self) -> Option<String> {
        self.workspace_error.lock().unwrap().clone()
    }

    /// Start a command through the local workspace service. This keeps the
    /// command runner inside the client UI; no helper script is required on
    /// the SSH server.
    pub async fn run_workspace(&self, command: &str, cwd: &str, timeout_secs: u64) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::RunWorkspace {
                command: command.to_string(),
                cwd: cwd.to_string(),
                timeout_secs,
                reply,
            })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))?
    }

    pub async fn workspace_job(&self, id: u64) -> Result<Value> {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Command::WorkspaceJob { id, reply })
            .map_err(|_| anyhow!("connection closed"))?;
        rx.await.map_err(|_| anyhow!("connection closed"))?
    }

    /// Ask the actor to hang up.
    pub async fn close(&self) {
        let _ = self.cmds.send(Command::Close);
    }
}
// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

async fn actor(
    mut handle: client::Handle<Handler>,
    mut rx: mpsc::UnboundedReceiver<Command>,
    self_tx: mpsc::UnboundedSender<Command>,
    alive: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    sftp: Arc<AsyncMutex<Option<Arc<SftpSession>>>>,
    tunnels: Arc<Mutex<Vec<Tunnel>>>,
    remote_targets: RemoteTargets,
    workspace: Arc<Mutex<Option<WorkspaceInfo>>>,
    workspace_error: Arc<Mutex<Option<String>>>,
) {
    let mut workspace_service = None;
    let mut sftp_services: Vec<SftpService> = Vec::new();
    let mut mounted_paths: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    None => break,
                    Some(Command::Close) => {
                        for path in mounted_paths.iter().rev() {
                            let _ = run_exec(&mut handle, &unmount_command(path)).await;
                        }
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                        break;
                    }
                    Some(Command::Shell { cols, rows, term, command, sink, reply }) => {
                        let result = open_shell(&mut handle, cols, rows, &term, command, sink).await;
                        let _ = reply.send(result);
                    }
                    Some(Command::Exec { command, reply }) => {
                        let result = run_exec(&mut handle, &command).await;
                        let _ = reply.send(result);
                    }
                    Some(Command::Sftp { reply }) => {
                        let result = open_sftp(&mut handle).await;
                        match result {
                            Ok(session) => {
                                *sftp.lock().await = Some(session.clone());
                                let _ = reply.send(Ok(session));
                            }
                            Err(err) => {
                                let _ = reply.send(Err(err));
                            }
                        }
                    }
                    Some(Command::StartTunnel { tunnel, reply }) => {
                        let result = match tunnel.kind {
                            TunnelKind::Remote => {
                                if tunnel.listen_port == 0 {
                                    Err(anyhow!("a remote tunnel requires a nonzero listen port"))
                                } else if remote_targets.lock().unwrap().contains_key(&tunnel.listen_port) {
                                    Err(anyhow!("remote port is already forwarded"))
                                } else {
                                    remote_targets.lock().unwrap().insert(tunnel.listen_port,
                                        (tunnel.remote_host.clone(), tunnel.remote_port));
                                    let result = handle.tcpip_forward("127.0.0.1", u32::from(tunnel.listen_port)).await;
                                    if result.is_err() { remote_targets.lock().unwrap().remove(&tunnel.listen_port); }
                                    result.map(|_| ()).map_err(anyhow::Error::from)
                                }
                            },
                            _ => start_local_listener(&self_tx, &tunnel).await,
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::StartWorkspace { config, reply }) => {
                        *workspace_error.lock().unwrap() = None;
                        let result = async {
                            if workspace_service.is_some() { bail!("this connection already has a shared workspace") }
                            if !config.enabled { bail!("workspace sharing is disabled") }
                            if config.remote_port != 0 && remote_targets.lock().unwrap().contains_key(&config.remote_port) {
                                bail!("workspace remote port is already forwarded");
                            }
                            let mut service = WorkspaceService::start(&config).await?;
                            // Keep a control endpoint for commands typed in the
                            // server shell, including when real sshfs mounts are
                            // enabled.
                            {
                                let port = handle.tcpip_forward("127.0.0.1", u32::from(config.remote_port))
                                    .await.context("sshd must allow remote TCP forwarding for workspace sharing")?;
                                let port = if config.remote_port == 0 { u16::try_from(port)? } else { config.remote_port };
                                if port == 0 { bail!("sshd did not allocate a workspace port") }
                                remote_targets.lock().unwrap().insert(port, ("127.0.0.1".into(), service.port));
                                service.info.url = format!("http://127.0.0.1:{port}");
                            }
                            if config.mount {
                            for (index, mapping) in config.effective_mappings().into_iter().enumerate() {
                                let sftp = start_sftp_service(
                                    tokio::fs::canonicalize(&mapping.local_path).await
                                        .with_context(|| format!("opening local mount {}", mapping.local_path))?,
                                    config.writable,
                                ).await?;
                                let mount_port = handle.tcpip_forward("127.0.0.1", 0).await
                                    .context("sshd must allow remote forwarding for the SFTP mount")?;
                                remote_targets.lock().unwrap().insert(
                                    u16::try_from(mount_port)?,
                                    ("127.0.0.1".into(), sftp.port),
                                );
                                let result = run_exec(&mut handle, &mount_command(mount_port, &mapping.remote_path)).await?;
                                if !result.ok() {
                                    // A missing sshfs (or another per-directory mount
                                    // error) must not turn a usable SSH connection into
                                    // a failed connection. Remove the forward and retain
                                    // the diagnostic in the workspace status instead.
                                    let mount_port = u16::try_from(mount_port)?;
                                    remote_targets.lock().unwrap().remove(&mount_port);
                                    let _ = handle
                                        .cancel_tcpip_forward("127.0.0.1", u32::from(mount_port))
                                        .await;
                                    let detail = result.stderr_text().trim().to_string();
                                    let detail = if detail.is_empty() {
                                        format!("sshfs mount exited with status {:?}", result.exit)
                                    } else {
                                        detail
                                    };
                                    if let Some(mount) = service.info.mounts.get_mut(index) {
                                        mount.mounted = false;
                                        mount.error = Some(detail);
                                    }
                                    continue;
                                }
                                if let Some(mount) = service.info.mounts.get_mut(index) {
                                    mount.mounted = true;
                                }
                                mounted_paths.push(mapping.remote_path);
                                sftp_services.push(sftp);
                            }
                            }
                            let info = service.info.clone();
                            *workspace.lock().unwrap() = Some(info.clone());
                            workspace_service = Some(service);
                            Ok(info)
                        }.await;
                        if result.is_err() {
                            *workspace_error.lock().unwrap() = result.as_ref().err().map(|err| format!("{err:#}"));
                            for path in mounted_paths.drain(..).rev() {
                                let _ = run_exec(&mut handle, &unmount_command(&path)).await;
                            }
                            sftp_services.clear();
                        }
                        let _ = reply.send(result);
                    }
                    Some(Command::RunWorkspace { command, cwd, timeout_secs, reply }) => {
                        let result = match workspace_service.as_ref() {
                            Some(service) => workspace_request(
                                service.port,
                                &service.info.token,
                                "POST",
                                "/run",
                                match serde_json::to_vec(&json!({
                                    "command": command,
                                    "cwd": cwd,
                                    "timeout_secs": timeout_secs,
                                })) {
                                    Ok(body) => body,
                                    Err(err) => {
                                        let _ = reply.send(Err(err.into()));
                                        continue;
                                    }
                                },
                            ).await.and_then(|value| {
                                value.get("id").and_then(Value::as_u64)
                                    .context("workspace service returned no job id")
                            }),
                            None => Err(anyhow!("reverse workspace is not connected")),
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::WorkspaceJob { id, reply }) => {
                        let result = match workspace_service.as_ref() {
                            Some(service) => workspace_request(
                                service.port,
                                &service.info.token,
                                "GET",
                                &format!("/jobs/{id}"),
                                Vec::new(),
                            ).await,
                            None => Err(anyhow!("reverse workspace is not connected")),
                        };
                        let _ = reply.send(result);
                    }
                    Some(Command::Pump { host, port, socket }) => {
                        match handle
                            .channel_open_direct_tcpip(&host, u32::from(port), "127.0.0.1", 0)
                            .await
                        {
                            Ok(channel) => { tokio::spawn(pump_socket(channel, *socket)); }
                            Err(err) => log::warn!("cannot forward to {host}:{port}: {err}"),
                        }
                    }
                }
            }
            result = &mut handle => {
                alive.store(false, Ordering::SeqCst);
                match result {
                    Ok(()) => log::info!("ssh connection closed"),
                    Err(err) => {
                        log::warn!("ssh connection error: {err:?}");
                        *error.lock().unwrap() = Some(format!("{err:?}"));
                    }
                }
                break;
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    for path in mounted_paths.iter().rev() {
        let _ = run_exec(&mut handle, &unmount_command(path)).await;
    }
    tunnels.lock().unwrap().clear();
    remote_targets.lock().unwrap().clear();
    *workspace.lock().unwrap() = None;
    *workspace_error.lock().unwrap() = None;
    drop(workspace_service);
    drop(sftp_services);
}

/// Terminal modes advertised with the PTY request.
fn terminal_modes() -> Vec<(Pty, u32)> {
    vec![
        (Pty::VERASE, 0x7f),
        (Pty::VINTR, 0x03),
        (Pty::VQUIT, 0x1c),
        (Pty::VEOF, 0x04),
        (Pty::VKILL, 0x15),
        (Pty::VSUSP, 0x1a),
        (Pty::VSTART, 0x11),
        (Pty::VSTOP, 0x13),
        (Pty::ECHO, 1),
        (Pty::ICRNL, 1),
        (Pty::ONLCR, 1),
    ]
}

async fn open_shell(
    handle: &mut client::Handle<Handler>,
    cols: u16,
    rows: u16,
    term: &str,
    command: Option<String>,
    sink: TermSink,
) -> Result<TerminalHandle> {
    let channel = handle.channel_open_session().await?;
    channel
        .request_pty(
            false,
            term,
            cols.max(2) as u32,
            rows.max(2) as u32,
            0,
            0,
            &terminal_modes(),
        )
        .await?;
    // Servers only hand out 24-bit colour once they know we can take it; a
    // server that refuses the env request simply keeps sending what it did.
    let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
    match command {
        Some(command) if !command.trim().is_empty() => channel.exec(true, command).await?,
        _ => channel.request_shell(true).await?,
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut channel = channel;
        let mut exit = None;
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) => sink.emit(TermEvent::Data(data.to_vec())),
                        Some(ChannelMsg::ExtendedData { data, .. }) => sink.emit(TermEvent::Data(data.to_vec())),
                        Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
                input = rx.recv() => {
                    match input {
                        Some(TermInput::Data(bytes)) => {
                            if channel.data_bytes(bytes).await.is_err() { break }
                        }
                        Some(TermInput::Resize { rows, cols }) => {
                            let _ = channel
                                .window_change(cols.max(2) as u32, rows.max(2) as u32, 0, 0)
                                .await;
                        }
                        Some(TermInput::Eof) => { let _ = channel.eof().await; }
                        None => break,
                    }
                }
            }
        }
        sink.emit(TermEvent::Closed { exit });
    });

    Ok(TerminalHandle { tx })
}

async fn run_exec(handle: &mut client::Handle<Handler>, command: &str) -> Result<ExecResult> {
    let channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    let mut channel = channel;
    let mut out = ExecResult::default();
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => out.stdout.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => out.stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => out.exit = Some(exit_status),
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    Ok(out)
}

async fn workspace_request(
    port: u16,
    token: &str,
    method: &str,
    route: &str,
    body: Vec<u8>,
) -> Result<Value> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await?;
    let request = format!(
        "{method} {route} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(request.as_bytes()).await?;
    socket.write_all(&body).await?;
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("invalid workspace response")?;
    let header = String::from_utf8_lossy(&response[..split]);
    let status = header
        .split_whitespace()
        .nth(1)
        .context("workspace response has no status")?;
    let body = &response[split + 4..];
    if status != "200" {
        let error = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
        bail!("workspace request failed ({status}): {error}");
    }
    Ok(serde_json::from_slice(body)?)
}

async fn open_sftp(handle: &mut client::Handle<Handler>) -> Result<Arc<SftpSession>> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    SftpSession::new(channel.into_stream())
        .await
        .map(Arc::new)
        .context("opening the SFTP subsystem")
}

/// Copy bytes between an SSH channel and a local TCP socket until either side ends.
async fn pump_socket(channel: Channel<Msg>, mut socket: TcpStream) {
    let mut stream = channel.into_stream();
    if let Err(err) = tokio::io::copy_bidirectional(&mut socket, &mut stream).await {
        log::debug!("tunnel pump ended: {err}");
    }
}

// ---------------------------------------------------------------------------
// Local / dynamic tunnels
// ---------------------------------------------------------------------------

async fn start_local_listener(
    self_tx: &mpsc::UnboundedSender<Command>,
    tunnel: &Tunnel,
) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", tunnel.listen_port))
        .await
        .with_context(|| format!("cannot listen on 127.0.0.1:{}", tunnel.listen_port))?;
    let tx = self_tx.clone();
    let dynamic = tunnel.kind == TunnelKind::Dynamic;
    let (host, port) = (tunnel.remote_host.clone(), tunnel.remote_port);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            if dynamic {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut socket = socket;
                    if let Ok((host, port)) = socks5_handshake(&mut socket).await {
                        let _ = tx.send(Command::Pump {
                            host,
                            port,
                            socket: Box::new(socket),
                        });
                    }
                });
            } else {
                let _ = tx.send(Command::Pump {
                    host: host.clone(),
                    port,
                    socket: Box::new(socket),
                });
            }
        }
    });
    Ok(())
}

/// Minimal SOCKS5 handshake (`CONNECT`, no authentication).
async fn socks5_handshake(socket: &mut TcpStream) -> Result<(String, u16)> {
    let mut len = [0u8; 1];
    socket.read_exact(&mut len).await?;
    let mut methods = vec![0u8; len[0] as usize];
    socket.read_exact(&mut methods).await?;
    socket.write_all(&[0x05, 0x00]).await?;

    let mut head = [0u8; 4];
    socket.read_exact(&mut head).await?;
    if head[0] != 0x05 || head[1] != 0x01 {
        let _ = socket
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        bail!("only SOCKS5 CONNECT is supported");
    }
    let host = match head[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            socket.read_exact(&mut addr).await?;
            std::net::Ipv4Addr::from(addr).to_string()
        }
        0x03 => {
            let mut l = [0u8; 1];
            socket.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            socket.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        0x04 => {
            let mut addr = [0u8; 16];
            socket.read_exact(&mut addr).await?;
            std::net::Ipv6Addr::from(addr).to_string()
        }
        other => bail!("unsupported SOCKS address type {other}"),
    };
    let mut port_bytes = [0u8; 2];
    socket.read_exact(&mut port_bytes).await?;
    let port = u16::from_be_bytes(port_bytes);
    socket
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .await?;
    Ok((host, port))
}
// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

struct AuthContext {
    username: String,
    host: String,
    password: Option<String>,
    private_key: Option<PathBuf>,
    use_agent: bool,
    prompter: Prompter,
}

impl AuthContext {
    async fn run(&self, handle: &mut client::Handle<Handler>) -> Result<()> {
        // `none` is the standard way to learn which methods the server offers.
        let mut remaining = match handle.authenticate_none(&self.username).await? {
            client::AuthResult::Success => return Ok(()),
            client::AuthResult::Failure {
                remaining_methods, ..
            } => remaining_methods,
        };
        log::debug!(
            "{} offers: {}",
            self.host,
            remaining
                .iter()
                .map(<&str>::from)
                .collect::<Vec<_>>()
                .join(", ")
        );

        if remaining.contains(&MethodKind::PublicKey) {
            match &self.private_key {
                Some(path) => {
                    if self.with_key(handle, path).await? {
                        return Ok(());
                    }
                }
                None => {
                    if self.use_agent && self.with_agent(handle).await {
                        return Ok(());
                    }
                }
            }
        }

        if remaining.contains(&MethodKind::Password) {
            let mut supplied = self.password.clone();
            let mut incorrect = false;
            loop {
                let password = match supplied.take() {
                    Some(password) => password,
                    None => match self
                        .prompter
                        .ask(Challenge::Password {
                            host: self.host.clone(),
                            username: self.username.clone(),
                            incorrect,
                        })
                        .await
                    {
                        Reply::Secret(password) => password,
                        _ => break,
                    },
                };
                match handle
                    .authenticate_password(&self.username, password)
                    .await?
                {
                    client::AuthResult::Success => return Ok(()),
                    client::AuthResult::Failure {
                        remaining_methods, ..
                    } => remaining = remaining_methods,
                }
                incorrect = true;
                if !remaining.contains(&MethodKind::Password) {
                    break;
                }
            }
        }

        if remaining.contains(&MethodKind::KeyboardInteractive) {
            return self.keyboard_interactive(handle).await;
        }

        bail!("authentication failed: no accepted method")
    }

    async fn with_key(&self, handle: &mut client::Handle<Handler>, path: &Path) -> Result<bool> {
        if !path.exists() {
            bail!("private key {} does not exist", path.display());
        }
        let mut prompted = false;
        let key = loop {
            let passphrase = if looks_unencrypted(path) && !prompted {
                None
            } else {
                match self
                    .prompter
                    .ask(Challenge::Passphrase {
                        key: path.to_path_buf(),
                        incorrect: prompted,
                    })
                    .await
                {
                    Reply::Secret(pass) => Some(pass),
                    _ => return Ok(false),
                }
            };
            match load_secret_key(path, passphrase.as_deref()) {
                Ok(key) => break key,
                Err(err) if !prompted => {
                    prompted = true;
                    log::debug!("loading {}: {err}", path.display());
                }
                Err(err) => bail!("cannot load {}: {err}", path.display()),
            }
        };

        let hash = handle.best_supported_rsa_hash().await?.flatten();
        let result = handle
            .authenticate_publickey(
                self.username.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await?;
        Ok(result.success())
    }

    async fn with_agent(&self, handle: &mut client::Handle<Handler>) -> bool {
        let Some(mut agent) = connect_agent().await else {
            log::debug!("no ssh agent available");
            return false;
        };
        let Ok(identities) = agent.request_identities().await else {
            return false;
        };
        let hash = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        for identity in identities {
            let public = identity.public_key().into_owned();
            match handle
                .authenticate_publickey_with(self.username.clone(), public, hash, &mut agent)
                .await
            {
                Ok(result) if result.success() => return true,
                Ok(_) => {}
                Err(err) => log::debug!("agent signature rejected: {err:?}"),
            }
        }
        false
    }

    async fn keyboard_interactive(&self, handle: &mut client::Handle<Handler>) -> Result<()> {
        let mut response = handle
            .authenticate_keyboard_interactive_start(self.username.clone(), None::<String>)
            .await?;
        // A stored password answers a plain password challenge silently. Servers
        // that only offer keyboard-interactive (Ubuntu/PAM does this by default)
        // would otherwise pop the dialog on every connect even when the password
        // is already in the session.
        let mut auto = self.password.clone().filter(|p| !p.is_empty());
        loop {
            response = match response {
                client::KeyboardInteractiveAuthResponse::Success => return Ok(()),
                client::KeyboardInteractiveAuthResponse::Failure { .. } => {
                    bail!("keyboard-interactive authentication failed")
                }
                client::KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    let prompts: Vec<SecretPrompt> = prompts
                        .into_iter()
                        .map(|p| SecretPrompt {
                            text: p.prompt,
                            echo: p.echo,
                        })
                        .collect();
                    let plain = is_plain_password_prompt(&prompts);
                    let answers = match (plain, auto.take()) {
                        (true, Some(password)) => vec![password; prompts.len()],
                        _ => match self
                            .prompter
                            .ask(Challenge::KeyboardInteractive {
                                host: self.host.clone(),
                                username: self.username.clone(),
                                name,
                                instructions,
                                prompts,
                            })
                            .await
                        {
                            Reply::Responses(answers) => answers,
                            _ => bail!("authentication cancelled"),
                        },
                    };
                    handle
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?
                }
            };
        }
    }
}

/// True when the server is only asking for the login password (no 2FA).
fn is_plain_password_prompt(prompts: &[SecretPrompt]) -> bool {
    !prompts.is_empty() && prompts.iter().all(|prompt| !prompt.echo)
}

/// OpenSSH marks encrypted keys with a header, so plain keys never prompt.
fn looks_unencrypted(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| !text.contains("ENCRYPTED"))
        .unwrap_or(false)
}

/// Try Pageant first on Windows, then the `SSH_AUTH_SOCK` agent.
async fn connect_agent() -> Option<AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>> {
    #[cfg(windows)]
    {
        if let Ok(client) = AgentClient::connect_pageant().await {
            return Some(client.dynamic());
        }
        if let Ok(pipe) = std::env::var("SSH_AUTH_SOCK") {
            if let Ok(client) = AgentClient::connect_named_pipe(pipe.as_str()).await {
                return Some(client.dynamic());
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        AgentClient::connect_env()
            .await
            .ok()
            .map(|client| client.dynamic())
    }
}
