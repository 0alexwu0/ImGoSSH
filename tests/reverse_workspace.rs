//! Real SSH forwarding + workspace HTTP + local process integration, no sshd needed.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use imgossh::config::{Session, Tunnel, TunnelKind, WorkspaceConfig, WorkspaceMapping};
use imgossh::ssh::client::{Challenge, ConnectOptions, Connection, Prompter, Reply, Trust};
use imgossh::ssh::workspace::WorkspaceInfo;
use russh::{keys::PrivateKey, server, Channel, ChannelId};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct Gateway {
    listeners: Vec<tokio::task::JoinHandle<()>>,
    deny: bool,
    mount_succeeds: bool,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

impl server::Handler for Gateway {
    type Error = russh::Error;
    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        // Simulate the remote mount result without requiring Linux/FUSE.
        let command = String::from_utf8_lossy(command);
        assert!(command.contains("sshfs") || command.contains("fusermount"));
        session.channel_success(channel)?;
        if self.mount_succeeds || command.contains("fusermount") {
            session.data(channel, b"IMGOSSH_MOUNT_OK\n".to_vec())?;
            session.exit_status_request(channel, 0)?;
        } else {
            session.extended_data(
                channel,
                1,
                b"ImGoSSH: sshfs is not installed on the server\n".to_vec(),
            )?;
            session.exit_status_request(channel, 127)?;
        }
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        assert_eq!(
            address, "127.0.0.1",
            "reverse listeners must request loopback"
        );
        if self.deny {
            return Ok(false);
        }
        let listener = TcpListener::bind((address, *port as u16)).await.unwrap();
        *port = listener.local_addr().unwrap().port().into();
        let port = *port;
        let handle = session.handle();
        self.listeners.push(tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let handle = handle.clone();
                tokio::spawn(async move {
                    if let Ok(channel) = handle
                        .channel_open_forwarded_tcpip(
                            "127.0.0.1",
                            port,
                            "127.0.0.1",
                            peer.port().into(),
                        )
                        .await
                    {
                        let _ =
                            tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream())
                                .await;
                    }
                });
            }
        }));
        Ok(true)
    }
}

async fn connection(deny: bool) -> Connection {
    connection_with_mount(deny, false).await
}

async fn connection_with_mount(deny: bool, mount_succeeds: bool) -> Connection {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap()],
        auth_rejection_time: Duration::ZERO,
        ..Default::default()
    });
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let session = server::run_stream(
            config,
            socket,
            Gateway {
                listeners: vec![],
                deny,
                mount_succeeds,
                channels: HashMap::new(),
            },
        )
        .await
        .unwrap();
        let _ = session.await;
    });
    let (prompter, mut requests) = Prompter::channel();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let reply = match request.challenge {
                Challenge::HostKey(_) => Reply::TrustKey(Trust::Once),
                _ => Reply::Cancel,
            };
            let _ = request.reply.send(reply);
        }
    });
    let session = Session {
        host: "127.0.0.1".into(),
        username: "workspace-test".into(),
        port,
        ..Default::default()
    };
    Connection::connect(ConnectOptions::from_session(&session, prompter))
        .await
        .unwrap()
}

fn config(root: &std::path::Path, writable: bool, allow_exec: bool) -> WorkspaceConfig {
    WorkspaceConfig {
        enabled: true,
        mount: false,
        mappings: vec![WorkspaceMapping {
            local_path: root.to_string_lossy().into_owned(),
            remote_path: "/".into(),
        }],
        root: root.to_string_lossy().into_owned(),
        writable,
        allow_exec,
        remote_port: 0,
    }
}

async fn http(info: &WorkspaceInfo, method: &str, route: &str, body: &[u8]) -> (u16, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut socket = TcpStream::connect(info.url.trim_start_matches("http://")).await.unwrap();
        socket.write_all(format!("{method} {route} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n", info.token, body.len()).as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        let end = response.windows(4).position(|v| v == b"\r\n\r\n").unwrap() + 4;
        let code = std::str::from_utf8(&response[..end]).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        (code, response[end..].to_vec())
    }).await.expect("workspace HTTP request stalled")
}

async fn api(info: &WorkspaceInfo, method: &str, route: &str, body: Value) -> Value {
    let (status, bytes) = http(info, method, route, &serde_json::to_vec(&body).unwrap()).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

async fn done(info: &WorkspaceInfo, id: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let job = api(info, "GET", &format!("/jobs/{id}"), Value::Null).await;
            if job["status"] != "running" {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn server_edits_client_code_runs_it_and_reads_logs() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(dir.path(), true, true))
        .await
        .unwrap();
    assert!(!info.token.is_empty());
    assert!(conn.workspace_info().is_some());
    assert!(conn
        .start_workspace(&config(dir.path(), true, true))
        .await
        .is_err());
    let mut bad_token = info.clone();
    bad_token.token = "wrong".into();
    assert_eq!(http(&bad_token, "GET", "/info", b"").await.0, 401);
    let meta = api(&info, "GET", "/info", Value::Null).await;
    assert_eq!(meta["writable"], true);
    assert_eq!(meta["allow_exec"], true);
    api(&info, "POST", "/mkdir?path=project", Value::Null).await;
    #[cfg(windows)]
    let (name, code, run) = (
        "check.ps1",
        "Write-Output 'edited-client-code'; [Console]::Error.WriteLine('error-log'); exit 7",
        "& ./check.ps1",
    );
    #[cfg(not(windows))]
    let (name, code, run) = (
        "check.sh",
        "printf 'edited-client-code\\n'; printf 'error-log\\n' >&2; exit 7",
        "sh ./check.sh",
    );
    let route = format!("/file?path=project/{name}");
    assert_eq!(http(&info, "PUT", &route, code.as_bytes()).await.0, 200);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("project").join(name)).unwrap(),
        code
    );
    assert_eq!(http(&info, "GET", &route, b"").await.1, code.as_bytes());
    let spaced = "/file?path=project/file+%E4%B8%AD%E6%96%87%2B%25.txt";
    assert_eq!(http(&info, "PUT", spaced, b"encoded-name").await.0, 200);
    assert_eq!(http(&info, "GET", spaced, b"").await.1, b"encoded-name");
    assert!(dir.path().join("project/file 中文+%.txt").exists());
    let entries = api(&info, "GET", "/list?path=project", Value::Null).await;
    assert!(entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == name));
    let id = api(
        &info,
        "POST",
        "/run",
        json!({"command": run, "cwd": "project"}),
    )
    .await["id"]
        .as_u64()
        .unwrap();
    let job = done(&info, id).await;
    assert_eq!(job["status"], "finished", "{job}");
    assert_eq!(job["exit_code"], 7, "{job}");
    assert!(
        job["stdout"]
            .as_str()
            .unwrap()
            .contains("edited-client-code"),
        "{job}"
    );
    assert!(
        job["stderr"].as_str().unwrap().contains("error-log"),
        "{job}"
    );
    assert!(
        String::from_utf8(http(&info, "GET", "/client.py", b"").await.1)
            .unwrap()
            .contains("def main():")
    );
    conn.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while conn.is_alive() || conn.workspace_info().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn client_runner_starts_and_reads_a_local_job() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    conn.start_workspace(&config(dir.path(), false, true))
        .await
        .unwrap();
    #[cfg(windows)]
    let command = "Write-Output runner-ok";
    #[cfg(not(windows))]
    let command = "printf runner-ok";
    let id = conn.run_workspace(command, "", 30).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = conn.workspace_job(id).await.unwrap();
            if job["status"] != "running" {
                assert_eq!(job["status"], "finished");
                assert!(job["stdout"].as_str().unwrap().contains("runner-ok"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    conn.close().await;
}

#[tokio::test]
async fn server_can_run_client_program_with_mounting_enabled() {
    for mount_succeeds in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let conn = connection_with_mount(false, mount_succeeds).await;
        let mut settings = config(dir.path(), true, true);
        settings.mount = true;
        settings.mappings[0].remote_path = "/data/sss".into();
        let info = tokio::time::timeout(Duration::from_secs(10), conn.start_workspace(&settings))
            .await
            .expect("workspace setup stalled")
            .unwrap();
        assert!(
            !info.url.is_empty(),
            "mount mode must expose the control endpoint"
        );
        assert_eq!(info.mounts[0].mounted, mount_succeeds);
        if !mount_succeeds {
            assert!(info.mounts[0]
                .error
                .as_ref()
                .unwrap()
                .contains("sshfs is not installed"));
        }
        // These requests go through the SSH server's reverse listener, just
        // like igworkspace.py run/logs invoked in the server terminal.
        #[cfg(windows)]
        let command = "Set-Content -Path ran-on-client.txt -Value client-run; Write-Output server-triggered; exit 7";
        #[cfg(not(windows))]
        let command = "echo client-run > ran-on-client.txt; echo server-triggered; exit 7";
        let id = api(
            &info,
            "POST",
            "/run",
            json!({
                "command": command, "cwd": "/data/sss", "timeout_secs": 30,
            }),
        )
        .await["id"]
            .as_u64()
            .unwrap();
        let job = done(&info, id).await;
        assert_eq!(job["status"], "finished");
        assert_eq!(job["exit_code"], 7);
        assert!(job["stdout"].as_str().unwrap().contains("server-triggered"));
        assert!(
            std::fs::read_to_string(dir.path().join("ran-on-client.txt"))
                .unwrap()
                .contains("client-run")
        );
        assert!(conn.is_alive());
        conn.close().await;
    }
}

#[tokio::test]
async fn multiple_mounts_are_selected_by_server_path() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("one.txt"), b"one").unwrap();
    std::fs::write(second.path().join("two.txt"), b"two").unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&WorkspaceConfig {
            enabled: true,
            mount: false,
            mappings: vec![
                WorkspaceMapping {
                    local_path: first.path().to_string_lossy().into_owned(),
                    remote_path: "/project".into(),
                },
                WorkspaceMapping {
                    local_path: second.path().to_string_lossy().into_owned(),
                    remote_path: "/data".into(),
                },
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(info.mounts.len(), 2);
    let project = api(&info, "GET", "/list?path=/project", Value::Null).await;
    let data = api(&info, "GET", "/list?path=/data", Value::Null).await;
    assert_eq!(project["entries"][0]["name"], "one.txt");
    assert_eq!(data["entries"][0]["name"], "two.txt");
    assert_eq!(
        http(&info, "GET", "/file?path=/project/one.txt", b"")
            .await
            .1,
        b"one"
    );
    assert_eq!(
        http(&info, "GET", "/file?path=/data/two.txt", b"").await.1,
        b"two"
    );
    conn.close().await;
}

#[tokio::test]
async fn permissions_path_boundaries_and_forwarding_denial() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside").unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(dir.path(), false, false))
        .await
        .unwrap();
    assert_eq!(http(&info, "PUT", "/file?path=no.txt", b"no").await.0, 400);
    assert_eq!(http(&info, "POST", "/mkdir?path=no", b"").await.0, 400);
    assert_eq!(
        http(&info, "POST", "/run", br#"{"command":"echo no"}"#)
            .await
            .0,
        400
    );
    for path in [
        "../secret.txt",
        "%2e%2e/secret.txt",
        "/secret.txt",
        "C:%5csecret.txt",
        "..%5csecret.txt",
        "NUL",
        "..%20/secret.txt",
    ] {
        assert_eq!(
            http(&info, "GET", &format!("/file?path={path}"), b"")
                .await
                .0,
            400,
            "{path}"
        );
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert_eq!(
            http(&info, "GET", "/file?path=escape/secret.txt", b"")
                .await
                .0,
            400
        );
    }
    #[cfg(windows)]
    {
        // Junctions do not require Windows Developer Mode / symlink privileges.
        let result = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(dir.path().join("escape"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            http(&info, "GET", "/file?path=escape/secret.txt", b"")
                .await
                .0,
            400
        );
    }
    conn.close().await;
    let denied = connection(true).await;
    assert!(denied
        .start_workspace(&config(dir.path(), true, true))
        .await
        .is_err());
    assert!(
        denied.is_alive(),
        "workspace setup failure must not disconnect SSH"
    );
    assert!(denied.workspace_info().is_none());
    denied.close().await;
}

#[tokio::test]
async fn jobs_stream_timeout_cancel_and_stop_on_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(dir.path(), true, true))
        .await
        .unwrap();
    #[cfg(windows)]
    let slow = "Write-Output 'started'; Start-Sleep -Seconds 30; Set-Content -Path unexpected.txt -Value 'late'";
    #[cfg(not(windows))]
    let slow = "printf 'started\\n'; sleep 30; echo late > unexpected.txt";
    let id = api(
        &info,
        "POST",
        "/run",
        json!({"command": slow, "timeout_secs": 1}),
    )
    .await["id"]
        .as_u64()
        .unwrap();
    let job = done(&info, id).await;
    assert_eq!(job["status"], "timed_out", "{job}");
    let id = api(&info, "POST", "/run", json!({"command": slow})).await["id"]
        .as_u64()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = api(&info, "GET", &format!("/jobs/{id}"), Value::Null).await;
            if job["stdout"].as_str().unwrap().contains("started") {
                assert_eq!(job["status"], "running");
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    api(&info, "DELETE", &format!("/jobs/{id}"), Value::Null).await;
    assert_eq!(done(&info, id).await["status"], "cancelled");
    #[cfg(windows)]
    let disconnect_probe = "Set-Content -Path ready.txt -Value 'ready'; Start-Sleep -Seconds 2; Set-Content -Path unexpected.txt -Value 'late'";
    #[cfg(not(windows))]
    let disconnect_probe = "echo ready > ready.txt; sleep 2; echo late > unexpected.txt";
    api(&info, "POST", "/run", json!({"command": disconnect_probe})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dir.path().join("ready.txt").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    conn.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while conn.workspace_info().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Wait past the command's delayed write to verify actual process cleanup.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!dir.path().join("unexpected.txt").exists());
}

#[tokio::test]
async fn remote_tunnel_uses_configured_client_target() {
    let local = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let local_port = local.local_addr().unwrap().port();
    let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let remote_port = probe.local_addr().unwrap().port();
    drop(probe);
    let conn = connection(false).await;
    conn.start_tunnel(&Tunnel {
        kind: TunnelKind::Remote,
        listen_port: remote_port,
        remote_host: "127.0.0.1".into(),
        remote_port: local_port,
    })
    .await
    .unwrap();
    let echo = tokio::spawn(async move {
        let (mut socket, _) = local.accept().await.unwrap();
        socket.write_all(b"actual-client-target").await.unwrap();
    });
    let mut socket = TcpStream::connect(("127.0.0.1", remote_port))
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, b"actual-client-target");
    echo.await.unwrap();
    conn.close().await;
}

#[tokio::test]
#[ignore = "requires Python 3; set IMGOSSH_TEST_PYTHON to its executable"]
async fn downloaded_python_helper_reads_writes_and_runs() {
    let client_dir = tempfile::tempdir().unwrap();
    let server_dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(client_dir.path(), true, true))
        .await
        .unwrap();
    let script = server_dir.path().join("igworkspace.py");
    std::fs::write(&script, http(&info, "GET", "/client.py", b"").await.1).unwrap();
    async fn helper(
        info: &WorkspaceInfo,
        script: &std::path::Path,
        args: &[&str],
    ) -> std::process::Output {
        tokio::process::Command::new(
            std::env::var("IMGOSSH_TEST_PYTHON").unwrap_or_else(|_| "python3".into()),
        )
        .arg(script)
        .args(args)
        .env("IMGOSSH_WORKSPACE_URL", &info.url)
        .env("IMGOSSH_WORKSPACE_TOKEN", &info.token)
        .env("PYTHONIOENCODING", "utf-8")
        .output()
        .await
        .unwrap()
    }
    let output = helper(&info, &script, &["info"]).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let local = server_dir.path().join("edited.txt");
    std::fs::write(&local, "updated-through-helper").unwrap();
    let output = helper(
        &info,
        &script,
        &["put", local.to_str().unwrap(), "file 中文+%.txt"],
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = helper(&info, &script, &["cat", "file 中文+%.txt"]).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"updated-through-helper");
    #[cfg(windows)]
    let command = "Write-Output 'helper-run'; exit 5";
    #[cfg(not(windows))]
    let command = "echo helper-run; exit 5";
    let output = helper(&info, &script, &["run", command]).await;
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("helper-run"));
    conn.close().await;
}

// Restrict the simulated server to a shell, curl and core utilities. The
// client process retains its normal PATH so it can run PowerShell on Windows.
struct ShellServer {
    _tools: tempfile::TempDir,
    shell: std::path::PathBuf,
    path: std::ffi::OsString,
}

impl ShellServer {
    fn new() -> Self {
        let tools = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let (shell, path) = {
            let base = std::env::var_os("IMGOSSH_TEST_GIT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "C:/Program Files/Git".into());
            let bin = base.join("usr/bin");
            let shell = bin.join("sh.exe");
            assert!(
                shell.is_file(),
                "set IMGOSSH_TEST_GIT to Git for Windows for the shell integration test"
            );
            (
                shell,
                std::env::join_paths([bin, base.join("mingw64/bin")]).unwrap(),
            )
        };
        #[cfg(unix)]
        let (shell, path) = {
            let search = std::env::var_os("PATH").unwrap_or_default();
            for name in [
                "sh", "curl", "cat", "mktemp", "rm", "rmdir", "sed", "tail", "sleep",
            ] {
                let source = std::env::split_paths(&search)
                    .map(|dir| dir.join(name))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| panic!("{name} is required for shell tests"));
                std::os::unix::fs::symlink(source, tools.path().join(name)).unwrap();
            }
            (tools.path().join("sh"), tools.path().as_os_str().to_owned())
        };
        Self {
            _tools: tools,
            shell,
            path,
        }
    }

    async fn script(
        &self,
        info: &WorkspaceInfo,
        cwd: &std::path::Path,
        script: &str,
        args: &[&str],
    ) -> std::process::Output {
        // Pass opaque arguments via the environment: MSYS shells parse native
        // Windows command lines differently from Rust's Windows argv quoting.
        let mut source = String::from("if command -v python3 >/dev/null 2>&1 || command -v python >/dev/null 2>&1 || command -v jq >/dev/null 2>&1; then exit 99; fi\nexport TMPDIR=.\nset --\n");
        let mut command = tokio::process::Command::new(&self.shell);
        for (index, arg) in args.iter().enumerate() {
            let name = format!("IMGOSSH_TEST_ARG_{index}");
            command.env(&name, arg);
            source.push_str(&format!("set -- \"$@\" \"${name}\"\n"));
        }
        source.push_str(script);
        std::fs::write(cwd.join("server-test.sh"), source).unwrap();
        command
            .arg("./server-test.sh")
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("IMGOSSH_WORKSPACE_URL", &info.url)
            .env("IMGOSSH_WORKSPACE_TOKEN", &info.token)
            // Git's native curl needs POSIX output paths converted to Windows
            // paths, but the client's command and mapping are opaque data.
            .env_remove("MSYS_NO_PATHCONV")
            .env("MSYS2_ARG_CONV_EXCL", "command@;cwd@;timeout_secs=")
            .kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(20), command.output())
            .await
            .expect("shell helper stalled")
            .unwrap()
    }
}

#[tokio::test]
async fn shell_helper_runs_and_cancels_without_python_or_jq() {
    let server = ShellServer::new();
    let client_dir = tempfile::tempdir().unwrap();
    let server_dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    let mut settings = config(client_dir.path(), false, true);
    settings.mappings[0].remote_path = "/data/sss".into();
    let info = conn.start_workspace(&settings).await.unwrap();
    let shell_bytes = http(&info, "GET", "/client.sh", b"").await.1;
    assert!(
        !shell_bytes.contains(&b'\r'),
        "the downloaded shell helper must use LF"
    );
    std::fs::write(server_dir.path().join("helper.sh"), shell_bytes).unwrap();
    #[cfg(windows)]
    let command = r#"[Console]::Out.Write('quote " + % & 中文 $(touch server-marker)'); [Console]::Error.Write('error-tail'); exit 7"#;
    #[cfg(not(windows))]
    let command =
        r#"printf '%s' 'quote " + % & 中文 $(touch server-marker)'; printf error-tail >&2; exit 7"#;
    let output = server
        .script(
            &info,
            server_dir.path(),
            "sh ./helper.sh run \"$1\" --cwd /data/sss --timeout 30",
            &[command],
        )
        .await;
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "quote \" + % & 中文 $(touch server-marker)"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("error-tail"));
    assert!(!server_dir.path().join("server-marker").exists());

    // Initialization defines a function in the SSH shell and works without
    // Python as well. The default cwd selects the first configured mapping.
    #[cfg(windows)]
    let command = "[Console]::Write('setup-ok')";
    #[cfg(not(windows))]
    let command = "printf setup-ok";
    let output = server
        .script(
            &info,
            server_dir.path(),
            &format!("{}\nigworkspace run \"$1\"", info.setup_command()),
            &[command],
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"setup-ok");

    #[cfg(windows)]
    let command = "Write-Output waiting; Start-Sleep -Seconds 60";
    #[cfg(not(windows))]
    let command = "echo waiting; sleep 60";
    let output = server
        .script(
            &info,
            server_dir.path(),
            "sh ./helper.sh run \"$1\" --detach",
            &[command],
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id: u64 = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let output = server
        .script(
            &info,
            server_dir.path(),
            "sh ./helper.sh cancel \"$1\"",
            &[&id.to_string()],
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(done(&info, id).await["status"], "cancelled");
    let output = server
        .script(
            &info,
            server_dir.path(),
            "sh ./helper.sh logs \"$1\" --follow",
            &[&id.to_string()],
        )
        .await;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled"));

    let mut bad_token = info.clone();
    bad_token.token = "invalid".into();
    let output = server
        .script(&bad_token, server_dir.path(), "sh ./helper.sh info", &[])
        .await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unauthorized"));
    conn.close().await;
}

#[tokio::test]
async fn shell_api_keeps_log_cursors_after_truncation_and_checks_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(dir.path(), false, true))
        .await
        .unwrap();
    #[cfg(windows)]
    let command = "[Console]::Write(('x' * (1024 * 1024 + 100))); [Console]::Error.Write('tail')";
    #[cfg(not(windows))]
    let command = "head -c 1048676 /dev/zero | tr '\\000' x; printf tail >&2";
    let id = api(&info, "POST", "/run", json!({"command": command})).await["id"]
        .as_u64()
        .unwrap();
    let job = done(&info, id).await;
    assert_eq!(job["status"], "finished");
    let (status, bytes) = http(
        &info,
        "GET",
        &format!("/shell/jobs/{id}/stdout?path=0"),
        b"",
    )
    .await;
    assert_eq!(status, 200);
    assert!(bytes.starts_with(b"1048676\n1\n"));
    assert_eq!(bytes.len(), b"1048676\n1\n".len() + 1024 * 1024);
    assert_eq!(
        http(
            &info,
            "GET",
            &format!("/shell/jobs/{id}/stdout?path=1048676"),
            b""
        )
        .await
        .1,
        b"1048676\n0\n"
    );
    assert_eq!(
        http(
            &info,
            "GET",
            &format!("/shell/jobs/{id}/stderr?path=2"),
            b""
        )
        .await
        .1,
        b"4\n0\nil"
    );
    assert_eq!(
        http(
            &info,
            "GET",
            &format!("/shell/jobs/{id}/stdout?path=99999999"),
            b""
        )
        .await
        .0,
        400
    );
    conn.close().await;

    let conn = connection(false).await;
    let info = conn
        .start_workspace(&config(dir.path(), false, false))
        .await
        .unwrap();
    let (status, bytes) = http(&info, "POST", "/shell/run", b"command=echo+blocked").await;
    assert_eq!(status, 400);
    assert!(String::from_utf8_lossy(&bytes).contains("execution is disabled"));
    conn.close().await;
}
