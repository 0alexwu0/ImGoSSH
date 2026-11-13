//! Windows launch handoff. Credentials travel only over a local named pipe.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::runtime::Runtime;
use zeroize::Zeroizing;

use crate::startup::Args;

const MAX_REQUEST: usize = 64 * 1024;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

pub enum Launch {
    Forwarded,
    Window(Option<Instance>),
}

/// Owned by the first window until it closes, including before its UI is ready.
pub struct Instance {
    // Dropping the runtime cancels the listener and releases the pipe name.
    _runtime: Runtime,
    requests: mpsc::Receiver<Args>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
}

impl Instance {
    pub fn start(args: &Args) -> io::Result<Launch> {
        // Keep this stable even when the first launch has not created its
        // settings directory yet (canonicalize would add a Windows \\?\ prefix).
        let store = std::path::absolute(crate::config::store_dir())?;
        let home = dirs::home_dir().unwrap_or_default();
        Self::start_at(&pipe_name(&home, &store), args)
    }

    pub(crate) fn start_at(name: &str, args: &Args) -> io::Result<Launch> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let _guard = runtime.enter();
        let deadline = std::time::Instant::now() + HANDOFF_TIMEOUT;
        loop {
            match ServerOptions::new().first_pipe_instance(true).create(name) {
                Ok(server) => {
                    drop(_guard);
                    return Ok(Launch::Window(Some(Self::receive(
                        runtime,
                        name,
                        Some(server),
                    ))));
                }
                // FILE_FLAG_FIRST_PIPE_INSTANCE reports ACCESS_DENIED when a
                // window already owns this name. Ordinary launches may still
                // open another window; only JumpServer launches are forwarded.
                Err(err) if err.raw_os_error() == Some(5) => {
                    if !args.is_jumpserver() {
                        drop(_guard);
                        return Ok(Launch::Window(Some(Self::receive(runtime, name, None))));
                    }
                    match ClientOptions::new().open(name) {
                        Ok(client) => {
                            allow_foreground(&client);
                            runtime.block_on(async {
                                tokio::time::timeout(HANDOFF_TIMEOUT, forward(client, args)).await?
                            })?;
                            return Ok(Launch::Forwarded);
                        }
                        Err(err) if matches!(err.raw_os_error(), Some(2 | 231)) => {
                            // The owner may be shutting down or accepting another
                            // launch. Retry election as well as connection.
                            if std::time::Instant::now() >= deadline {
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "the existing ImGoSSH window did not accept the connection",
                                ));
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn receive(runtime: Runtime, name: &str, server: Option<NamedPipeServer>) -> Self {
        let (tx, requests) = mpsc::sync_channel(64);
        let repaint = Arc::new(Mutex::new(None));
        runtime.spawn(receive(name.to_owned(), server, tx, repaint.clone()));
        Self {
            _runtime: runtime,
            requests,
            repaint,
        }
    }

    pub fn attach(&self, ctx: &egui::Context) {
        *self.repaint.lock().unwrap() = Some(ctx.clone());
        // Requests can arrive while fonts and the window are being created.
        ctx.request_repaint();
    }

    pub fn try_recv(&self) -> Option<Args> {
        self.requests.try_recv().ok()
    }
}

async fn receive(
    name: String,
    server: Option<NamedPipeServer>,
    tx: mpsc::SyncSender<Args>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
) {
    let server = match server {
        Some(server) => server,
        None => loop {
            // Other manually opened windows take over if the owner closes.
            match ServerOptions::new().first_pipe_instance(true).create(&name) {
                Ok(server) => break server,
                Err(err) if err.raw_os_error() == Some(5) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(_) => return,
            }
        },
    };
    listen(name, server, tx, repaint).await;
}

fn pipe_name(home: &Path, store: &Path) -> String {
    let identity = format!("{}\0{}", home.display(), store.display()).to_lowercase();
    let digest = ring::digest::digest(&ring::digest::SHA256, identity.as_bytes());
    let suffix: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\ImGoSSH-launch-v1-{suffix}")
}

fn allow_foreground(client: &NamedPipeClient) {
    // The launcher can grant its foreground rights to the existing GUI.
    unsafe {
        let mut pid = 0;
        if windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
            client.as_raw_handle(),
            &mut pid,
        ) != 0
        {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
        }
    }
}

async fn forward(mut client: NamedPipeClient, args: &Args) -> io::Result<()> {
    let payload = Zeroizing::new(serde_json::to_vec(args).map_err(|_| invalid_request())?);
    if payload.len() > MAX_REQUEST {
        return Err(invalid_request());
    }
    client.write_u32(payload.len() as u32).await?;
    client.write_all(&payload).await?;
    if client.read_u8().await? != 1 {
        return Err(invalid_request());
    }
    Ok(())
}

async fn listen(
    name: String,
    mut server: NamedPipeServer,
    tx: mpsc::SyncSender<Args>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
) {
    loop {
        if server.connect().await.is_err() {
            return;
        }
        // Always keep one pipe handle alive so concurrent launches cannot
        // accidentally elect a second owner between requests.
        let next = match ServerOptions::new().create(&name) {
            Ok(next) => next,
            Err(_) => return,
        };
        let mut client = std::mem::replace(&mut server, next);
        let tx = tx.clone();
        let repaint = repaint.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(HANDOFF_TIMEOUT, async {
                let len = client.read_u32().await? as usize;
                if len == 0 || len > MAX_REQUEST {
                    return Err(invalid_request());
                }
                let mut payload = Zeroizing::new(vec![0; len]);
                client.read_exact(&mut payload).await?;
                // Do not include serde errors: they can contain credentials.
                let args: Args = serde_json::from_slice(&payload).map_err(|_| invalid_request())?;
                if !args.is_jumpserver() {
                    return Err(invalid_request());
                }
                tx.try_send(args).map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "the launch queue is unavailable")
                })?;
                if let Some(ctx) = repaint.lock().unwrap().as_ref() {
                    ctx.request_repaint();
                }
                // Acknowledge only once the request is queued in the owner.
                client.write_u8(1).await
            })
            .await;
        });
    }
}

fn invalid_request() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid ImGoSSH launch request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Protocol};
    use clap::Parser;

    fn name(dir: &tempfile::TempDir) -> String {
        pipe_name(dir.path(), dir.path())
    }

    fn window(name: &str, args: &Args) -> Instance {
        match Instance::start_at(name, args).unwrap() {
            Launch::Window(Some(instance)) => instance,
            _ => panic!("expected a receiving window"),
        }
    }

    #[test]
    fn child_launch() {
        let Ok(name) = std::env::var("IMGOSSH_TEST_LAUNCH_PIPE") else {
            return;
        };
        let args = Args::try_parse_from([
            "imgossh",
            "--jumpserver",
            "--sftp",
            "--host=127.0.0.1",
            "--port=2222",
            "--username=JMS-token@domain",
            "--password=-token+/=\"&",
        ])
        .unwrap();
        assert!(matches!(
            Instance::start_at(&name, &args).unwrap(),
            Launch::Forwarded
        ));
    }

    #[test]
    fn forwards_from_another_process_before_ui_is_ready_and_releases_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let name = name(&dir);
        let ordinary = Args::try_parse_from(["imgossh"]).unwrap();
        let owner = window(&name, &ordinary);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instance::tests::child_launch"])
            .env("IMGOSSH_TEST_LAUNCH_PIPE", &name)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        let session = owner
            .requests
            .recv_timeout(HANDOFF_TIMEOUT)
            .unwrap()
            .into_startup(&Config::default())
            .connection
            .unwrap();
        assert_eq!(session.protocol, Protocol::Sftp);
        assert_eq!(session.port, 2222);
        assert_eq!(session.username, "JMS-token@domain");
        assert_eq!(session.password, "-token+/=\"&");
        assert!(session.jumpserver);
        assert!(!session.use_agent);
        assert!(!session.auto_tunnels);
        assert!(owner.try_recv().is_none());
        drop(owner);
        // The OS releases ownership; there is no stale lock/endpoint file.
        drop(window(&name, &ordinary));
    }

    #[test]
    fn concurrent_launches_queue_once_each_and_manual_windows_can_take_over() {
        let dir = tempfile::tempdir().unwrap();
        let name = name(&dir);
        let ordinary = Args::try_parse_from(["imgossh"]).unwrap();
        let owner = window(&name, &ordinary);
        let standby = window(&name, &ordinary);
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let name = name.clone();
                std::thread::spawn(move || {
                    let args = Args::try_parse_from([
                        "imgossh",
                        "--jumpserver",
                        "--host=gateway",
                        &format!("--name=Asset {i}"),
                    ])
                    .unwrap();
                    assert!(matches!(
                        Instance::start_at(&name, &args).unwrap(),
                        Launch::Forwarded
                    ));
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let mut titles: Vec<_> = (0..8)
            .map(|_| {
                owner
                    .requests
                    .recv_timeout(HANDOFF_TIMEOUT)
                    .unwrap()
                    .into_startup(&Config::default())
                    .connection
                    .unwrap()
                    .name
            })
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            (0..8).map(|i| format!("Asset {i}")).collect::<Vec<_>>()
        );
        assert!(owner.try_recv().is_none());
        assert!(standby.try_recv().is_none());
        drop(owner);
        // Wait for the already-open manual window to acquire ownership.
        let deadline = std::time::Instant::now() + HANDOFF_TIMEOUT;
        loop {
            let _guard = standby._runtime.enter();
            if ClientOptions::new().open(&name).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        let args = Args::try_parse_from(["imgossh", "--jumpserver", "--host=gateway"]).unwrap();
        assert!(matches!(
            Instance::start_at(&name, &args).unwrap(),
            Launch::Forwarded
        ));
        assert!(standby
            .requests
            .recv_timeout(HANDOFF_TIMEOUT)
            .unwrap()
            .is_jumpserver());
    }

    #[test]
    fn different_users_and_config_directories_have_distinct_endpoints() {
        assert_ne!(
            pipe_name(Path::new("alice"), Path::new("config")),
            pipe_name(Path::new("bob"), Path::new("config"))
        );
        assert_ne!(
            pipe_name(Path::new("alice"), Path::new("config")),
            pipe_name(Path::new("alice"), Path::new("portable"))
        );
    }
}
