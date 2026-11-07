//! Opt-in reverse workspace API, reachable through an authenticated SSH forward.
//! File operations are rooted; executed programs run with the client's OS rights.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File as LocalFile, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

use crate::config::WorkspaceConfig;

use russh::server::{self, ChannelOpenHandle, Msg as ServerMsg};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Data, File as SftpFile, FileAttributes, Handle, Name, Status, StatusCode,
};

const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_LOG: usize = 1024 * 1024;

/// Runtime credentials. Intentionally not Debug/Serialize or persisted.
#[derive(Clone)]
pub struct WorkspaceInfo {
    pub url: String,
    pub token: String,
    pub root: String,
    pub writable: bool,
    pub allow_exec: bool,
    pub mounts: Vec<WorkspaceMount>,
}

#[derive(Clone)]
pub struct WorkspaceMount {
    pub id: String,
    pub local_path: String,
    pub remote_path: String,
    /// Whether the client directory is currently mounted on the server.
    /// HTTP workspaces (when `mount` is disabled) are considered mounted.
    pub mounted: bool,
    /// A per-directory setup error.  A failed sshfs mount does not tear down
    /// the SSH connection; callers can surface this to the user instead.
    pub error: Option<String>,
}

/// A temporary SSH/SFTP endpoint bound to loopback on the client. The server
/// reaches it through one SSH remote-forward channel per configured mapping.
pub(crate) struct SftpService {
    pub port: u16,
    stop: watch::Sender<bool>,
}

impl Drop for SftpService {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

pub(crate) async fn start_sftp_service(root: PathBuf, writable: bool) -> Result<SftpService> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .map_err(|err| anyhow!("generating temporary SFTP host key: {err}"))?;
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let (stop, mut stopped) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break };
                    let config = config.clone();
                    let root = root.clone();
                    tokio::spawn(async move {
                        if let Ok(session) = server::run_stream(config, socket, SftpSession::new(root, writable)).await {
                            let _ = session.await;
                        }
                    });
                }
            }
        }
    });
    Ok(SftpService { port, stop })
}

/// Start one sshfs mount on the Linux server. The SSH connection on the
/// server goes to a loopback port that is forwarded back to this client.
pub(crate) fn mount_command(remote_port: u32, remote_path: &str) -> String {
    let path = crate::util::shell_quote(remote_path);
    let log = format!("/tmp/imgossh-sshfs-{remote_port}.log");
    let log = crate::util::shell_quote(&log);
    format!(
        "command -v sshfs >/dev/null 2>&1 || {{ echo 'ImGoSSH: sshfs is not installed on the server' >&2; exit 127; }}; command -v mountpoint >/dev/null 2>&1 || {{ echo 'ImGoSSH: mountpoint is not installed on the server' >&2; exit 127; }}; mkdir -p {path} || exit 1; nohup sshfs -f -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -p {remote_port} workspace-mount@127.0.0.1:/ {path} >{log} 2>&1 & pid=$!; for i in $(seq 1 100); do mountpoint -q {path} && {{ echo IMGOSSH_MOUNT_OK; exit 0; }}; kill -0 $pid 2>/dev/null || {{ cat {log}; exit 1; }}; sleep 0.1; done; cat {log}; kill $pid 2>/dev/null; exit 1",
        path = path,
        log = log,
        remote_port = remote_port,
    )
}

pub(crate) fn unmount_command(remote_path: &str) -> String {
    let path = crate::util::shell_quote(remote_path);
    format!(
        "if command -v fusermount3 >/dev/null 2>&1; then fusermount3 -u {path}; elif command -v fusermount >/dev/null 2>&1; then fusermount -u {path}; else umount {path}; fi",
        path = path
    )
}

struct SftpSession {
    root: PathBuf,
    writable: bool,
    channels: Arc<tokio::sync::Mutex<HashMap<ChannelId, Channel<ServerMsg>>>>,
}

impl SftpSession {
    fn new(root: PathBuf, writable: bool) -> Self {
        Self {
            root,
            writable,
            channels: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

impl server::Handler for SftpSession {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<ServerMsg>,
        reply: ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let Some(channel) = self.channels.lock().await.remove(&channel) else {
            return Ok(());
        };
        session.channel_success(channel.id())?;
        russh_sftp::server::run(
            channel.into_stream(),
            LocalSftp::new(self.root.clone(), self.writable),
        )
        .await;
        Ok(())
    }
}

struct LocalSftp {
    root: PathBuf,
    writable: bool,
    next: u64,
    files: HashMap<String, LocalFile>,
    dirs: HashMap<String, VecDeque<SftpFile>>,
}

impl LocalSftp {
    fn new(root: PathBuf, writable: bool) -> Self {
        Self {
            root,
            writable,
            next: 1,
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }
    fn handle(&mut self) -> String {
        let value = format!("h{}", self.next);
        self.next += 1;
        value
    }
    fn path(&self, value: &str) -> Result<PathBuf, StatusCode> {
        if value.contains(['\\', ':', '\0']) {
            return Err(StatusCode::NoSuchFile);
        }
        let mut out = self.root.clone();
        for part in std::path::Path::new(value).components() {
            match part {
                std::path::Component::Normal(name) => out.push(name),
                std::path::Component::RootDir | std::path::Component::CurDir => {}
                _ => return Err(StatusCode::PermissionDenied),
            }
        }
        Ok(out)
    }
    fn attrs(path: &Path) -> Result<FileAttributes, StatusCode> {
        fs::metadata(path)
            .map(|meta| FileAttributes::from(&meta))
            .map_err(|_| StatusCode::NoSuchFile)
    }
    fn writable(&self) -> Result<(), StatusCode> {
        if self.writable {
            Ok(())
        } else {
            Err(StatusCode::PermissionDenied)
        }
    }
}

fn apply_attrs(path: &Path, attrs: &FileAttributes) -> Result<(), StatusCode> {
    if let Some(size) = attrs.size {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|_| StatusCode::Failure)?;
        file.set_len(size).map_err(|_| StatusCode::Failure)?;
    }
    #[cfg(unix)]
    if let Some(mode) = attrs.permissions {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|_| StatusCode::Failure)?;
    }
    Ok(())
}

impl russh_sftp::server::Handler for LocalSftp {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        if flags.contains(russh_sftp::protocol::OpenFlags::WRITE)
            || flags.contains(russh_sftp::protocol::OpenFlags::CREATE)
        {
            self.writable()?;
        }
        let path = self.path(&filename)?;
        let file: LocalFile = OpenOptions::from(flags)
            .open(path)
            .map_err(|_| StatusCode::NoSuchFile)?;
        let handle = self.handle();
        self.files.insert(handle.clone(), file);
        Ok(Handle { id, handle })
    }
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        self.files.remove(&handle);
        self.dirs.remove(&handle);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let file = self.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::Failure)?;
        let mut data = vec![0; len as usize];
        let n = file.read(&mut data).map_err(|_| StatusCode::Failure)?;
        data.truncate(n);
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        Ok(Data { id, data })
    }
    async fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> Result<russh_sftp::protocol::Attrs, StatusCode> {
        let file = self.files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let meta = file.metadata().map_err(|_| StatusCode::Failure)?;
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        self.writable()?;
        let file = self.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::Failure)?;
        file.write_all(&data).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.writable()?;
        apply_attrs(&self.path(&path)?, &attrs)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.writable()?;
        let file = self.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        if let Some(size) = attrs.size {
            file.set_len(size).map_err(|_| StatusCode::Failure)?;
        }
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let path = self.path(&path)?;
        let mut entries = VecDeque::new();
        for entry in fs::read_dir(path).map_err(|_| StatusCode::NoSuchFile)? {
            let entry = entry.map_err(|_| StatusCode::Failure)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push_back(SftpFile::new(name, Self::attrs(&entry.path())?));
        }
        let handle = self.handle();
        self.dirs.insert(handle.clone(), entries);
        Ok(Handle { id, handle })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        let entries = self.dirs.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        if entries.is_empty() {
            return Err(StatusCode::Eof);
        }
        let mut files = Vec::new();
        for _ in 0..64 {
            if let Some(file) = entries.pop_front() {
                files.push(file);
            } else {
                break;
            }
        }
        Ok(Name { id, files })
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let _ = self.path(&path)?;
        Ok(Name {
            id,
            files: vec![SftpFile::dummy("/")],
        })
    }
    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, StatusCode> {
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: Self::attrs(&self.path(&path)?)?,
        })
    }
    async fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, StatusCode> {
        self.stat(id, path).await
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.writable()?;
        fs::create_dir(self.path(&path)?).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.writable()?;
        fs::remove_file(self.path(&path)?).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.writable()?;
        fs::remove_dir(self.path(&path)?).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, StatusCode> {
        self.writable()?;
        fs::rename(self.path(&old)?, self.path(&new)?).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
}

impl WorkspaceInfo {
    /// Paste into a POSIX shell on the server to define the shell helper.
    pub fn setup_command(&self) -> String {
        format!(
            "export IMGOSSH_WORKSPACE_URL='{}' IMGOSSH_WORKSPACE_TOKEN='{}'; export IMGOSSH_WORKSPACE_HELPER; IMGOSSH_WORKSPACE_HELPER=$(mktemp \"${{TMPDIR:-/tmp}}/imgossh-workspace.XXXXXXXX\") && curl --noproxy '*' --connect-timeout 10 --max-time 35 --fail --silent --show-error -H \"Authorization: Bearer $IMGOSSH_WORKSPACE_TOKEN\" \"$IMGOSSH_WORKSPACE_URL/client.sh\" -o \"$IMGOSSH_WORKSPACE_HELPER\" && igworkspace() {{ sh \"$IMGOSSH_WORKSPACE_HELPER\" \"$@\"; }}",
            self.url, self.token
        )
    }
}

pub(crate) struct WorkspaceService {
    pub port: u16,
    pub info: WorkspaceInfo,
    stop: watch::Sender<bool>,
}

impl Drop for WorkspaceService {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

struct State {
    mounts: Vec<MountState>,
    token: String,
    writable: bool,
    allow_exec: bool,
    jobs: Mutex<BTreeMap<u64, Job>>,
    stop: watch::Receiver<bool>,
}

struct MountState {
    id: String,
    root: PathBuf,
    remote_path: String,
}

struct Job {
    snapshot: Arc<Mutex<JobSnapshot>>,
    cancel: watch::Sender<bool>,
}

#[derive(Default, Serialize)]
struct JobSnapshot {
    id: u64,
    status: String,
    exit_code: Option<i32>,
    #[serde(skip)]
    stdout: VecDeque<u8>,
    #[serde(skip)]
    stderr: VecDeque<u8>,
    #[serde(skip)]
    stdout_bytes: u64,
    #[serde(skip)]
    stderr_bytes: u64,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl WorkspaceService {
    pub async fn start(config: &WorkspaceConfig) -> Result<Self> {
        let mappings = config.effective_mappings();
        if mappings.is_empty() {
            bail!("choose at least one local workspace directory");
        }
        let mut mounts = Vec::with_capacity(mappings.len());
        let mut info_mounts = Vec::with_capacity(mappings.len());
        let mut remote_paths = std::collections::HashSet::new();
        for (index, mapping) in mappings.iter().enumerate() {
            let root = tokio::fs::canonicalize(&mapping.local_path)
                .await
                .with_context(|| {
                    format!("opening local workspace directory {}", mapping.local_path)
                })?;
            if !root.is_dir() {
                bail!("workspace path must be a directory: {}", mapping.local_path);
            }
            let remote_path = validate_remote_path(&mapping.remote_path)?;
            if !remote_paths.insert(remote_path.clone()) {
                bail!("duplicate workspace mount path: {remote_path}");
            }
            let id = format!("m{}", index + 1);
            mounts.push(MountState {
                id: id.clone(),
                root: root.clone(),
                remote_path: remote_path.clone(),
            });
            info_mounts.push(WorkspaceMount {
                id,
                local_path: root.to_string_lossy().into_owned(),
                remote_path,
                mounted: !config.mount,
                error: None,
            });
        }
        let mut random = [0u8; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow!("generating workspace credentials"))?;
        let token = URL_SAFE_NO_PAD.encode(random);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (stop, stopped) = watch::channel(false);
        let state = Arc::new(State {
            mounts,
            token: token.clone(),
            writable: config.writable,
            allow_exec: config.allow_exec,
            jobs: Mutex::new(BTreeMap::new()),
            stop: stopped.clone(),
        });
        tokio::spawn(async move {
            let mut stopped = stopped;
            let permits = Arc::new(Semaphore::new(16));
            let mut requests = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.changed() => break,
                    Some(_) = requests.join_next(), if !requests.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                        let state = state.clone();
                        requests.spawn(async move {
                            let _permit = permit;
                            let _ = tokio::time::timeout(Duration::from_secs(30), serve(socket, state)).await;
                        });
                    }
                }
            }
            // Drop closes all requests; every job also observes `stopped`.
        });
        Ok(Self {
            port,
            info: WorkspaceInfo {
                url: String::new(), // filled after sshd allocates its listener
                token,
                root: info_mounts[0].local_path.clone(),
                writable: config.writable,
                allow_exec: config.allow_exec,
                mounts: info_mounts,
            },
            stop,
        })
    }
}

struct Request {
    method: String,
    route: String,
    path: String,
    body: Vec<u8>,
}

// Deliberately one request per connection: bounded HTTP/1.1 without chunked bodies.
async fn request(socket: &mut TcpStream, token: &str) -> Result<Request> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            if index > 8192 {
                bail!("headers too large")
            }
            break index + 4;
        }
        if bytes.len() > 8192 {
            bail!("headers too large")
        }
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await?;
        if count == 0 {
            bail!("incomplete request")
        }
        bytes.extend_from_slice(&chunk[..count]);
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if first.len() != 3 || !matches!(first[2], "HTTP/1.1" | "HTTP/1.0") {
        bail!("invalid HTTP request");
    }
    let (method, target) = (first[0].to_string(), first[1].to_string());
    let mut authorized = false;
    let mut length = None;
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':').context("invalid header")?;
        let value = value.trim();
        if key.eq_ignore_ascii_case("authorization") {
            authorized = value == format!("Bearer {token}");
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            bail!("chunked requests are unsupported")
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                bail!("duplicate content length")
            }
            length = Some(value.parse::<usize>()?);
        }
    }
    if !authorized {
        bail!("unauthorized")
    }
    let length = length.unwrap_or(0);
    if length > MAX_BODY {
        bail!("request exceeds 16 MiB")
    }
    let mut body = bytes[header_end..].to_vec();
    body.truncate(length);
    let received = body.len();
    body.resize(length, 0);
    socket.read_exact(&mut body[received..]).await?;
    let (route, query) = target.split_once('?').unwrap_or((&target, ""));
    let path = query
        .split('&')
        .find_map(|p| p.strip_prefix("path="))
        .unwrap_or("");
    let path = path.replace('+', " ");
    let path = percent_encoding::percent_decode_str(&path)
        .decode_utf8()?
        .into_owned();
    Ok(Request {
        method,
        route: route.into(),
        path,
        body,
    })
}

async fn serve(mut socket: TcpStream, state: Arc<State>) -> Result<()> {
    let response = match request(&mut socket, &state.token).await {
        Ok(request) => dispatch(request, &state).await,
        Err(err) => Err(err),
    };
    let (status, content_type, body) = match response {
        Ok(body) => ("200 OK", "application/octet-stream", body),
        Err(err) => {
            let status = if err.to_string() == "unauthorized" {
                "401 Unauthorized"
            } else {
                "400 Bad Request"
            };
            (
                status,
                "application/json",
                serde_json::to_vec(&json!({"error": err.to_string()}))?,
            )
        }
    };
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", body.len()).as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.shutdown().await?;
    Ok(())
}

fn json_bytes(value: Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&value)?)
}

async fn dispatch(request: Request, state: &Arc<State>) -> Result<Vec<u8>> {
    let Request {
        method,
        route,
        path,
        body,
    } = request;
    match (method.as_str(), route.as_str()) {
        ("GET", "/client.py") => {
            Ok(include_bytes!("../../integrations/workspace/igworkspace.py").to_vec())
        }
        ("GET", "/client.sh") => {
            // Git may check the file out with CRLF on Windows. POSIX shells
            // must receive LF regardless of the client's platform.
            Ok(include_str!("../../integrations/workspace/igworkspace.sh")
                .replace("\r\n", "\n")
                .into_bytes())
        }
        ("GET", "/info") => json_bytes(json!({
            "root": state.mounts.first().map(|mount| &mount.root), "os": std::env::consts::OS,
            "mounts": state.mounts.iter().map(|mount| json!({"id": mount.id, "path": mount.remote_path, "local": mount.root})).collect::<Vec<_>>(),
            "writable": state.writable, "allow_exec": state.allow_exec,
        })),
        ("GET", "/list") => {
            let path = resolve_request_path(state, &path, false).await?;
            let mut dir = tokio::fs::read_dir(path).await?;
            let mut entries = Vec::new();
            while let Some(entry) = dir.next_entry().await? {
                if entries.len() >= 10000 {
                    bail!("directory exceeds 10000 entries")
                }
                let meta = entry.metadata().await?;
                entries.push(json!({"name": entry.file_name().to_string_lossy(),
                    "directory": meta.is_dir(), "symlink": meta.is_symlink(), "size": meta.len()}));
            }
            json_bytes(json!({"entries": entries}))
        }
        ("GET", "/file") => {
            let path = resolve_request_path(state, &path, false).await?;
            let file = tokio::fs::File::open(path).await?;
            let mut bytes = Vec::new();
            file.take(MAX_BODY as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > MAX_BODY {
                bail!("file exceeds 16 MiB")
            }
            Ok(bytes)
        }
        ("PUT", "/file") => {
            if !state.writable {
                bail!("workspace file writes are disabled")
            }
            let path = resolve_request_path(state, &path, true).await?;
            if path == state.mounts[0].root {
                bail!("cannot replace the workspace root")
            }
            // Replace atomically, so an interrupted upload cannot leave half a
            // source file, or overwrite another path through an existing hard link.
            tokio::task::spawn_blocking(move || -> Result<()> {
                use std::io::Write;
                let mut temp =
                    tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
                temp.write_all(&body)?;
                if let Ok(meta) = std::fs::metadata(&path) {
                    temp.as_file().set_permissions(meta.permissions())?;
                }
                temp.persist(&path)?;
                Ok(())
            })
            .await??;
            json_bytes(json!({"ok": true}))
        }
        ("POST", "/mkdir") => {
            if !state.writable {
                bail!("workspace file writes are disabled")
            }
            let path = resolve_request_path(state, &path, true).await?;
            tokio::fs::create_dir(path).await?;
            json_bytes(json!({"ok": true}))
        }
        ("POST", "/run") => {
            let spec: RunSpec = serde_json::from_slice(&body)?;
            let id = submit_job(state, spec).await?;
            json_bytes(json!({"id": id}))
        }
        ("POST", "/shell/run") => {
            let mut spec = shell_run_spec(&body)?;
            if spec.cwd.is_empty() {
                spec.cwd = state.mounts[0].remote_path.clone();
            }
            let id = submit_job(state, spec).await?;
            Ok(format!("{id}\n").into_bytes())
        }
        ("GET", route) if route.starts_with("/shell/jobs/") => {
            let (id, field) = route[12..].split_once('/').context("missing job field")?;
            let id: u64 = id.parse()?;
            let jobs = state.jobs.lock().unwrap();
            let job = jobs.get(&id).context("job not found")?;
            let snapshot = job.snapshot.lock().unwrap();
            match field {
                "status" => Ok(format!(
                    "{}\n{}\n",
                    snapshot.status,
                    snapshot
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_default()
                )
                .into_bytes()),
                "stdout" | "stderr" => {
                    let offset = if path.is_empty() {
                        0
                    } else {
                        path.parse::<u64>()?
                    };
                    let (log, total) = if field == "stdout" {
                        (&snapshot.stdout, snapshot.stdout_bytes)
                    } else {
                        (&snapshot.stderr, snapshot.stderr_bytes)
                    };
                    if offset > total {
                        bail!("log offset exceeds output length")
                    }
                    let first = total - log.len() as u64;
                    let skipped = offset < first;
                    // Two metadata lines precede the raw bytes; the shell
                    // helper reads these without parsing JSON or losing final
                    // newlines in command substitution.
                    let mut response = format!("{total}\n{}\n", u8::from(skipped)).into_bytes();
                    response.extend(log.iter().skip(offset.saturating_sub(first) as usize));
                    Ok(response)
                }
                _ => bail!("unknown job field"),
            }
        }
        ("GET" | "DELETE", route) if route.starts_with("/jobs/") => {
            let id: u64 = route[6..].parse()?;
            let jobs = state.jobs.lock().unwrap();
            let job = jobs.get(&id).context("job not found")?;
            if method == "DELETE" {
                let _ = job.cancel.send(true);
            }
            let snapshot = job.snapshot.lock().unwrap();
            let mut value = serde_json::to_value(&*snapshot)?;
            value["stdout"] =
                String::from_utf8_lossy(&snapshot.stdout.iter().copied().collect::<Vec<_>>())
                    .into_owned()
                    .into();
            value["stderr"] =
                String::from_utf8_lossy(&snapshot.stderr.iter().copied().collect::<Vec<_>>())
                    .into_owned()
                    .into();
            json_bytes(value)
        }
        _ => bail!("unknown workspace endpoint"),
    }
}

fn shell_run_spec(body: &[u8]) -> Result<RunSpec> {
    let mut fields = HashMap::new();
    for pair in std::str::from_utf8(body)?
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').context("invalid form field")?;
        if !matches!(key, "command" | "cwd" | "timeout_secs") {
            bail!("unknown run field")
        }
        let value = value.replace('+', " ");
        let value = percent_encoding::percent_decode_str(&value)
            .decode_utf8()?
            .into_owned();
        if fields.insert(key, value).is_some() {
            bail!("duplicate run field")
        }
    }
    Ok(RunSpec {
        command: fields.remove("command").context("missing command")?,
        cwd: fields.remove("cwd").unwrap_or_default(),
        timeout_secs: fields
            .remove("timeout_secs")
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or_else(default_timeout),
    })
}

async fn submit_job(state: &Arc<State>, spec: RunSpec) -> Result<u64> {
    if !state.allow_exec {
        bail!("workspace command execution is disabled")
    }
    let cwd = resolve_request_path(state, &spec.cwd, false).await?;
    if !cwd.is_dir() {
        bail!("working directory must be a directory")
    }
    start_job(state.clone(), spec, cwd)
}

/// Canonicalize every existing component, including links/junctions, before use.
/// A selected workspace must not be concurrently modified by untrusted local users.
async fn resolve(root: &Path, path: &str, create: bool) -> Result<PathBuf> {
    // Forbid Windows drive prefixes, ADS and alternate separators on every OS.
    if path.contains(['\\', ':', '\0']) {
        bail!("invalid workspace path")
    }
    let mut candidate = root.to_path_buf();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.ends_with(['.', ' ']) {
                    bail!("invalid workspace path component")
                }
                let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                if matches!(
                    base.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) || (base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && base.as_bytes()[3].is_ascii_digit())
                {
                    bail!("reserved workspace path component");
                }
                candidate.push(part);
            }
            Component::CurDir => {}
            _ => bail!("workspace paths must be relative and cannot contain '..'"),
        }
    }
    let resolved = match tokio::fs::canonicalize(&candidate).await {
        Ok(path) => path,
        Err(err) if create && err.kind() == std::io::ErrorKind::NotFound => {
            // Reject dangling links rather than following them when creating a file.
            if tokio::fs::symlink_metadata(&candidate).await.is_ok() {
                bail!("dangling links are unsupported")
            }
            let parent = candidate.parent().context("missing parent")?;
            tokio::fs::canonicalize(parent)
                .await?
                .join(candidate.file_name().context("missing filename")?)
        }
        Err(err) => return Err(err.into()),
    };
    if !resolved.starts_with(root) {
        bail!("path is outside the shared workspace")
    }
    Ok(resolved)
}

async fn resolve_request_path(state: &State, path: &str, create: bool) -> Result<PathBuf> {
    let mut normalized = path.replace('\\', "/");
    if normalized.is_empty() {
        normalized = "/".into();
    }
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    let mount = state
        .mounts
        .iter()
        .filter(|mount| {
            let prefix = mount.remote_path.trim_end_matches('/');
            prefix.is_empty()
                || prefix == "/"
                || normalized == prefix
                || normalized.starts_with(&format!("{prefix}/"))
        })
        .max_by_key(|mount| mount.remote_path.len())
        .context("path is not under any configured workspace mount")?;
    let prefix = mount.remote_path.trim_end_matches('/');
    let relative = if prefix.is_empty() || prefix == "/" {
        normalized.trim_start_matches('/')
    } else {
        normalized
            .strip_prefix(prefix)
            .unwrap_or("")
            .trim_start_matches('/')
    };
    resolve(&mount.root, relative, create).await
}

fn validate_remote_path(path: &str) -> Result<String> {
    let value = path.trim().replace('\\', "/");
    if value.is_empty() || !value.starts_with('/') || value.contains("//") {
        bail!("remote mount path must be an absolute normalized path")
    }
    for component in value.split('/').filter(|part| !part.is_empty()) {
        if component == "." || component == ".." || component.contains('\0') {
            bail!("remote mount path cannot contain . or ..")
        }
    }
    Ok(if value.len() > 1 {
        value.trim_end_matches('/').into()
    } else {
        value
    })
}

#[derive(Deserialize)]
struct RunSpec {
    command: String,
    #[serde(default)]
    cwd: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    300
}

fn start_job(state: Arc<State>, spec: RunSpec, cwd: PathBuf) -> Result<u64> {
    if spec.command.trim().is_empty() {
        bail!("command cannot be empty")
    }
    if !(1..=3600).contains(&spec.timeout_secs) {
        bail!("timeout_secs must be between 1 and 3600")
    }
    let mut jobs = state.jobs.lock().unwrap();
    if jobs
        .values()
        .filter(|job| job.snapshot.lock().unwrap().status == "running")
        .count()
        >= 4
    {
        bail!("four workspace jobs are already running");
    }
    let id = jobs.keys().next_back().copied().unwrap_or(0) + 1;
    if jobs.len() >= 64 {
        if let Some(old) = jobs
            .iter()
            .find(|(_, job)| job.snapshot.lock().unwrap().status != "running")
            .map(|(id, _)| *id)
        {
            jobs.remove(&old);
        }
    }
    let snapshot = Arc::new(Mutex::new(JobSnapshot {
        id,
        status: "running".into(),
        ..Default::default()
    }));
    let (cancel, cancelled) = watch::channel(false);
    jobs.insert(
        id,
        Job {
            snapshot: snapshot.clone(),
            cancel,
        },
    );
    drop(jobs);
    tokio::spawn(async move {
        let result = run_process(&spec, cwd, snapshot.clone(), cancelled, state.stop.clone()).await;
        let mut snapshot = snapshot.lock().unwrap();
        match result {
            Ok((status, code)) => {
                snapshot.status = status.into();
                snapshot.exit_code = code;
            }
            Err(err) => {
                append_log(&mut snapshot, true, format!("{err:#}\n").as_bytes());
                snapshot.status = "failed".into();
            }
        }
    });
    Ok(id)
}

async fn run_process(
    spec: &RunSpec,
    cwd: PathBuf,
    snapshot: Arc<Mutex<JobSnapshot>>,
    mut cancel: watch::Receiver<bool>,
    mut stop: watch::Receiver<bool>,
) -> Result<(&'static str, Option<i32>)> {
    #[cfg(windows)]
    let mut command = {
        let mut command = tokio::process::Command::new("powershell.exe");
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding;\n{}\n$imgosshCommandSucceeded = $?; if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}; if (-not $imgosshCommandSucceeded) {{ exit 1 }}",
            spec.command
        );
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ]);
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", &spec.command]);
        command.process_group(0);
        command
    };
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut readers = JoinSet::new();
    for (reader, stderr) in [
        (
            Box::new(child.stdout.take().unwrap()) as Box<dyn AsyncRead + Send + Unpin>,
            false,
        ),
        (
            Box::new(child.stderr.take().unwrap()) as Box<dyn AsyncRead + Send + Unpin>,
            true,
        ),
    ] {
        let snapshot = snapshot.clone();
        readers.spawn(read_log(reader, snapshot, stderr));
    }
    let outcome = if *cancel.borrow() || *stop.borrow() {
        ("cancelled", None)
    } else {
        tokio::select! {
            result = child.wait() => ("finished", result?.code()),
            _ = cancel.changed() => ("cancelled", None),
            _ = stop.changed() => ("cancelled", None),
            _ = tokio::time::sleep(Duration::from_secs(spec.timeout_secs)) => ("timed_out", None),
        }
    };
    if outcome.0 != "finished" {
        // Kill the process tree on Windows; on Unix kill its dedicated group.
        if let Some(pid) = child.id() {
            #[cfg(windows)]
            {
                let mut kill = tokio::process::Command::new("taskkill.exe");
                kill.creation_flags(0x08000000);
                let _ = kill
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
            #[cfg(unix)]
            {
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    // A detached descendant may retain the pipes. Never wait indefinitely for it.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while readers.join_next().await.is_some() {}
    })
    .await;
    Ok(outcome)
}

async fn read_log(
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    snapshot: Arc<Mutex<JobSnapshot>>,
    stderr: bool,
) {
    let mut bytes = [0; 8192];
    while let Ok(count) = reader.read(&mut bytes).await {
        if count == 0 {
            break;
        }
        append_log(&mut snapshot.lock().unwrap(), stderr, &bytes[..count]);
    }
}

fn append_log(snapshot: &mut JobSnapshot, stderr: bool, bytes: &[u8]) {
    if stderr {
        snapshot.stderr_bytes += bytes.len() as u64;
    } else {
        snapshot.stdout_bytes += bytes.len() as u64;
    }
    let (log, truncated) = if stderr {
        (&mut snapshot.stderr, &mut snapshot.stderr_truncated)
    } else {
        (&mut snapshot.stdout, &mut snapshot.stdout_truncated)
    };
    log.extend(bytes);
    if log.len() > MAX_LOG {
        log.drain(..log.len() - MAX_LOG);
        *truncated = true;
    }
}
