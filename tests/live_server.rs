//! Opt-in checks against a real SSH server. Everything here is #[ignore]d so
//! the normal suite stays offline and fast.
//!
//!     $env:IMGOSSH_TEST_HOST = "192.0.2.10"
//!     $env:IMGOSSH_TEST_USER = "root"
//!     $env:IMGOSSH_TEST_PASS = "..."
//!     cargo test --test live_server -- --ignored --nocapture --test-threads=1

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use imgossh::terminal::Emulator;
use std::time::Duration;

use imgossh::ssh::client::{Challenge, ConnectOptions, Connection, Prompter, Reply, Trust};
use imgossh::ssh::sftp::{self, Kind};
use imgossh::ssh::transfer::{Phase, Queue};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name} to run the live tests"))
}

fn optional(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// Connect, answering the host-key question with "always" and nothing else.
///
/// A saved password must be used silently: if the engine has to ask, the test
/// fails rather than hanging on a prompt nobody can see.
async fn connect() -> Connection {
    let password = required("IMGOSSH_TEST_PASS");
    let (prompter, mut rx) = Prompter::channel();
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let reply = match &request.challenge {
                Challenge::HostKey(info) => {
                    println!(
                        "host key {} {} {}",
                        info.algorithm,
                        info.fingerprint,
                        if info.is_conflict() {
                            "CONFLICT"
                        } else {
                            "new"
                        }
                    );
                    Reply::TrustKey(Trust::Always)
                }
                Challenge::Password { .. } | Challenge::KeyboardInteractive { .. } => {
                    panic!("the engine asked for a password it already had")
                }
                _ => Reply::Cancel,
            };
            let _ = request.reply.send(reply);
        }
    });

    let opts = ConnectOptions {
        host: required("IMGOSSH_TEST_HOST"),
        port: optional("IMGOSSH_TEST_PORT", "22")
            .parse()
            .expect("IMGOSSH_TEST_PORT"),
        username: optional("IMGOSSH_TEST_USER", "root"),
        password: Some(password),
        private_key: None,
        use_agent: false,
        keepalive: None,
        prompter,
    };
    Connection::connect(opts).await.expect("connect")
}

#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_shell_runs_a_command() {
    let conn = connect().await;
    let result = conn.exec("printf '%s' imgossh-ok").await.expect("exec");
    assert!(
        result.ok(),
        "exit {:?}: {}",
        result.exit,
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"imgossh-ok");
    conn.close().await;
}

#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_sftp_lists_reads_and_writes() {
    let conn = connect().await;
    let sftp_session = conn.sftp().await.expect("sftp");
    let home = sftp::canonicalize(&sftp_session, ".")
        .await
        .expect("canonicalize");
    println!("remote home: {home}");

    let dir = format!("{home}/imgossh-live");
    let _ = sftp::remove_all(&sftp_session, &dir_entry(&dir)).await;
    sftp::mkdir(&sftp_session, &dir).await.expect("mkdir");

    let payload = b"ImGoSSH live payload".to_vec();
    let remote = format!("{dir}/payload.txt");
    sftp::write_all(&sftp_session, &remote, &payload)
        .await
        .expect("write");
    let back = sftp::read_to_end(&sftp_session, &remote, 1 << 20)
        .await
        .expect("read");
    assert_eq!(back, payload, "the file came back different");

    let entries = sftp::list(&sftp_session, &dir).await.expect("list");
    assert!(
        entries.iter().any(|e| e.name == "payload.txt"),
        "listing missed the file: {:?}",
        entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    sftp::remove_all(&sftp_session, &dir_entry(&dir))
        .await
        .expect("cleanup");
    conn.close().await;
}

#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_sftp_stats_and_chmods_a_file() {
    let conn = connect().await;
    let sftp_session = conn.sftp().await.expect("sftp");
    let home = sftp::canonicalize(&sftp_session, ".")
        .await
        .expect("canonicalize");

    let dir = format!("{home}/imgossh-props");
    let _ = sftp::remove_all(&sftp_session, &dir_entry(&dir)).await;
    sftp::mkdir(&sftp_session, &dir).await.expect("mkdir");
    let remote = format!("{dir}/note.txt");
    sftp::write_all(&sftp_session, &remote, b"properties")
        .await
        .expect("write");

    let before = sftp::stat(&sftp_session, &remote).await.expect("stat");
    assert_eq!(before.kind, Kind::File, "a plain file is not one");
    assert_eq!(before.size, b"properties".len() as u64, "wrong size");
    assert!(before.modified > 0, "no modification time: {before:?}");

    sftp::set_mode(&sftp_session, &remote, 0o600)
        .await
        .expect("chmod");
    let after = sftp::stat(&sftp_session, &remote).await.expect("re-stat");
    assert_eq!(after.mode, 0o600, "the mode did not stick");

    // A folder keeps its own bits, without the file-type bits leaking in.
    sftp::set_mode(&sftp_session, &dir, 0o750)
        .await
        .expect("chmod dir");
    let folder = sftp::stat(&sftp_session, &dir).await.expect("stat dir");
    assert_eq!(folder.kind, Kind::Directory);
    assert_eq!(folder.mode, 0o750, "the folder mode kept its type bits");

    // A birth time is a bonus: zero when the server will not say.
    let born = sftp::created(&conn, &remote).await;
    println!("birth time: {born}");
    assert!(born >= 0);

    sftp::remove_all(&sftp_session, &dir_entry(&dir))
        .await
        .expect("cleanup");
    conn.close().await;
}
#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_transfer_queue_round_trips_a_file() {
    let conn = connect().await;
    let sftp_session = conn.sftp().await.expect("sftp");
    let home = sftp::canonicalize(&sftp_session, ".")
        .await
        .expect("canonicalize");

    let payload = vec![7u8; 256 * 1024];
    let mut local = std::env::temp_dir();
    local.push(format!("imgossh-live-{}.bin", std::process::id()));
    std::fs::write(&local, &payload).expect("write local");

    let remote_dir = format!("{home}/imgossh-live-xfer");
    let queue = Queue::new();
    queue.upload(conn.clone(), local.clone(), remote_dir.clone(), false);
    wait(&queue).await;

    let mut down = std::env::temp_dir();
    down.push(format!("imgossh-live-dl-{}", std::process::id()));
    std::fs::create_dir_all(&down).expect("create download dir");
    queue.download(
        conn.clone(),
        format!("{remote_dir}/{}", file_name(&local)),
        down.clone(),
        false,
    );
    wait(&queue).await;

    let got = std::fs::read(down.join(file_name(&local))).expect("read download");
    assert_eq!(got.len(), payload.len(), "downloaded a different size");

    let _ = sftp::remove_all(&sftp_session, &dir_entry(&remote_dir)).await;
    let _ = std::fs::remove_file(&local);
    let _ = std::fs::remove_dir_all(&down);
    conn.close().await;
}

/// A bare directory handle, enough for the recursive remove helpers.
fn dir_entry(path: &str) -> sftp::Entry {
    sftp::Entry {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        kind: Kind::Directory,
        size: 0,
        mtime: 0,
        mode: 0,
    }
}

fn file_name(path: &PathBuf) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string()
}

async fn wait(queue: &Queue) {
    for _ in 0..600 {
        if queue.active() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for job in queue.jobs() {
        match job.snapshot().phase {
            Phase::Failed(err) => panic!("transfer {}: {err}", job.source),
            Phase::Cancelled => panic!("transfer {} cancelled", job.source),
            _ => {}
        }
    }
}

#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_monitor_collects_server_stats() {
    use imgossh::ssh::monitor::Monitor;

    let conn = connect().await;
    let monitor = Monitor::new();
    monitor.attach(conn.clone(), Duration::from_millis(400), || {});
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if monitor.snapshot().is_some() {
            break;
        }
    }
    // One more interval so the counters have a previous sample to differ from.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let info = monitor.snapshot().expect("no stats came back");
    println!(
        "host={:?} os={:?} kernel={:?} cpu={:.1}% load={:?} mem={}/{} swap={}/{} rx={:.0} tx={:.0} up={}s users={:?}",
        info.hostname,
        info.os,
        info.kernel,
        info.cpu,
        info.load,
        info.mem_used,
        info.mem_total,
        info.swap_used,
        info.swap_total,
        info.rx,
        info.tx,
        info.uptime,
        info.users
    );
    for disk in &info.disks {
        println!(
            "  disk {} {:.0}% ({} total, {} free)",
            disk.mount, disk.percent, disk.total, disk.free
        );
    }
    assert!(!info.hostname.is_empty(), "hostname missing");
    assert!(!info.kernel.is_empty(), "kernel missing");
    assert!(info.mem_total > 0, "memory total missing");
    assert!(info.uptime > 0, "uptime missing");
    assert!(!info.disks.is_empty(), "no filesystems");
    assert!(
        info.disks.iter().any(|disk| disk.mount == "/"),
        "root fs missing"
    );
    monitor.stop();
    conn.close().await;
}

/// A terminal has to get two things right for a shell to feel native:
/// characters land exactly once, and `Tab` is handed to the remote
/// completion engine instead of moving focus around inside the app.
#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_shell_tab_completes_a_command() {
    use imgossh::ssh::client::{TermEvent, TermSink};
    use imgossh::terminal::Emulator;

    let conn = connect().await;
    let emu = Arc::new(Mutex::new(Emulator::new(24, 80, 500)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let sink = {
        let emu = emu.clone();
        TermSink::new(move |event| match event {
            TermEvent::Data(bytes) => {
                let answers = emu.lock().unwrap().feed(&bytes);
                if !answers.is_empty() {
                    let _ = tx.send(answers);
                }
            }
            TermEvent::Closed { .. } => {}
        })
    };
    let handle = conn
        .shell(80, 24, "xterm-256color", None, sink)
        .await
        .expect("shell");
    // Shells ask "where is the cursor?" before they print a prompt; a real tab
    // forwards the answers, and the harness has to drain them too.
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            let _ = bytes;
        }
    });

    let screen = || emu.lock().unwrap().visible_text();
    let tag = format!("imgoshtab{}", std::process::id());
    let path = format!("/tmp/{tag}");
    let marker = format!("IMGOSHPAYLOAD{}", std::process::id());

    // Remove names left behind by an interrupted run: bash only completes the
    // common prefix, so stale siblings would stall the Tab below.
    handle
        .send(
            format!("rm -f /tmp/imgoshtab*; printf {marker} > {path}; echo IMGOSHREADY\r")
                .into_bytes(),
        )
        .expect("warm up");
    assert!(
        wait_for(&emu, "IMGOSHREADY").await,
        "the shell never answered"
    );
    // Clear the deck so the assertions below only look at what we type.
    handle.send(b"clear\r".to_vec()).expect("clear");
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Type everything but the trailing digits of the unique file name.
    let typed = "cat /tmp/imgoshtab";
    handle.send(typed.as_bytes().to_vec()).expect("type");
    assert!(
        wait_for(&emu, typed).await,
        "typed text never echoed: {:?}",
        screen()
    );
    let line = screen();
    assert!(
        !line.contains("catt") && !line.contains("aact") && !line.contains("/tmp//tmp"),
        "characters were echoed twice: {line:?}"
    );

    handle.send(b"\t".to_vec()).expect("tab");
    assert!(
        wait_for(&emu, &path).await,
        "Tab did not complete {typed:?} to {path:?}: {:?}",
        screen()
    );

    handle.send(b"\r".to_vec()).expect("enter");
    // The payload only ever reaches the screen if Enter really ran the
    // completed command, because it was never typed.
    assert!(
        wait_for(&emu, &marker).await,
        "Enter did not run the completed command: {:?}",
        screen()
    );

    let _ = handle.send(format!("rm -f /tmp/imgoshtab*\r").into_bytes());
    handle.close();
    conn.close().await;
}
/// Boxes have to arrive as boxes. A remote without a UTF-8 locale draws them
/// with the DEC special graphics set - `lqqqk` for a top edge - which the
/// terminal has to turn into line drawing characters.
#[tokio::test]
#[ignore = "needs a real SSH server"]
async fn live_shell_draws_line_drawing() {
    use imgossh::ssh::client::{TermEvent, TermSink};

    let conn = connect().await;
    let emu = Arc::new(Mutex::new(Emulator::new(30, 100, 500)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let sink = {
        let emu = emu.clone();
        TermSink::new(move |event| match event {
            TermEvent::Data(bytes) => {
                let answers = emu.lock().unwrap().feed(&bytes);
                if !answers.is_empty() {
                    let _ = tx.send(answers);
                }
            }
            TermEvent::Closed { .. } => {}
        })
    };
    let handle = conn
        .shell(100, 30, "xterm-256color", None, sink)
        .await
        .expect("shell");
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let screen = || emu.lock().unwrap().visible_text();
    // A corner of the set, spelled the way ncurses spells it: six of the
    // horizontal line letter between the two corners.
    let want = format!("┌{}┐", "─".repeat(6));

    handle.send(b"clear\r".to_vec()).expect("clear");
    tokio::time::sleep(Duration::from_millis(400)).await;

    // The deterministic half: any POSIX shell can print the set by hand, so
    // this says whether the translation works on this server at all.
    let printf = format!("printf '\\033(0l{}k\\033(B\\n'\r", "q".repeat(6));
    handle.send(printf.into_bytes()).expect("printf");
    assert!(
        wait_for(&emu, &want).await,
        "the special graphics set never became a box: {want:?} missing from {:?}",
        screen()
    );

    // The real half: nvtop, with the locale forced away from UTF-8 so it has
    // to fall back onto the same set the GUI has to render.
    handle
        .send(b"command -v nvtop >/dev/null || echo IMGOSHNO\"NVTOP\"\r".to_vec())
        .expect("probe");
    tokio::time::sleep(Duration::from_millis(700)).await;
    if screen().contains("IMGOSHNONVTOP") {
        println!("nvtop is not installed here; the byte level check is the verdict");
        handle.close();
        conn.close().await;
        return;
    }

    handle
        .send(b"LC_ALL=C LANG=C nvtop\r".to_vec())
        .expect("nvtop");
    let boxed = || {
        screen()
            .chars()
            .any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
    };
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if boxed() {
            break;
        }
    }

    let view = screen();
    let dump = std::env::temp_dir().join("imgossh-nvtop-screen.txt");
    std::fs::write(&dump, &view).expect("dump the screen");
    let edges = view
        .lines()
        .filter(|line| line.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)))
        .take(8)
        .collect::<Vec<_>>();
    for line in &edges {
        println!("{line}");
    }
    println!("full screen written to {}", dump.display());
    assert!(
        !edges.is_empty(),
        "nvtop drew no line drawing characters at all"
    );

    handle.send(b"q".to_vec()).expect("quit nvtop");
    tokio::time::sleep(Duration::from_millis(400)).await;
    handle.close();
    conn.close().await;
}
/// Poll the emulator until `needle` shows up on screen.
async fn wait_for(emu: &Arc<Mutex<Emulator>>, needle: &str) -> bool {
    for _ in 0..80 {
        if emu.lock().unwrap().visible_text().contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}
