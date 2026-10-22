//! `igssh` - an OpenSSH-style client built on the ImGoSSH engine.
//!
//! ```text
//! igssh -l root -p 2222 -i ~/.ssh/id_ed25519 -L 8080:localhost:80 host
//! igssh host -e "uptime"
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode as CKey, KeyEvent, KeyEventKind,
    KeyModifiers as M, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use vt100::MouseProtocolMode;

use imgossh::cli::{self, ConnectArgs};
use imgossh::config::TunnelKind;
use imgossh::ssh::client::{Connection, TermEvent, TermSink, TerminalHandle};
use imgossh::terminal::{
    encode_mouse, Emulator, InputEncoder, KeyCode, Mods, MouseReport, TermState, MOUSE_LEFT,
    MOUSE_MIDDLE, MOUSE_RIGHT, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};

#[derive(Parser)]
#[command(
    name = "igssh",
    version,
    about = "Open an SSH shell, or run one command on a remote host"
)]
struct Args {
    /// Destination, `[user@]host`
    destination: String,
    /// Command to run instead of an interactive shell
    #[arg(short = 'e')]
    command: Option<String>,
    /// Login user (defaults to $USER)
    #[arg(short = 'l')]
    user: Option<String>,
    /// Remote port
    #[arg(short = 'p', default_value_t = 22)]
    port: u16,
    /// Private key file
    #[arg(short = 'i')]
    identity: Option<PathBuf>,
    /// Always ask for the password on the terminal
    #[arg(short = 'w')]
    ask_password: bool,
    /// Do not use ssh-agent / Pageant
    #[arg(short = 'a')]
    no_agent: bool,
    /// Local forward, e.g. `-L 8080:localhost:80`
    #[arg(short = 'L')]
    local: Vec<String>,
    /// Remote forward, e.g. `-R 9000:localhost:22`
    #[arg(short = 'R')]
    remote: Vec<String>,
    /// Dynamic (SOCKS5) forward, e.g. `-D 1080`
    #[arg(short = 'D')]
    dynamic: Vec<String>,
    /// Run without a PTY and capture the output
    #[arg(short = 'x', long)]
    exec: bool,
    /// Keepalive interval in seconds
    #[arg(long, default_value_t = 60)]
    keepalive: u64,
    /// TERM value sent to the server
    #[arg(long, default_value = "xterm-256color")]
    term: String,
}

/// Restores the console when the shell exits (including on panic).
struct RawMode;

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let mut tunnels = Vec::new();
    for spec in &args.local {
        tunnels.push(cli::parse_tunnel(TunnelKind::Local, spec)?);
    }
    for spec in &args.remote {
        tunnels.push(cli::parse_tunnel(TunnelKind::Remote, spec)?);
    }
    for spec in &args.dynamic {
        tunnels.push(cli::parse_tunnel(TunnelKind::Dynamic, spec)?);
    }

    let (user, host) = split_destination(&args.destination, args.user.clone());
    let connect = ConnectArgs {
        user,
        port: args.port,
        identity: args.identity.clone(),
        password: args
            .ask_password
            .then(|| rpassword::prompt_password("password: ").unwrap_or_default()),
        agent: !args.no_agent,
        keepalive: args.keepalive,
        tunnels,
    };

    let runtime = cli::runtime()?;
    let conn = connect.connect(&runtime, &host)?;

    if args.exec {
        let command = args.command.clone().unwrap_or_default();
        let result = runtime.block_on(conn.exec(&command))?;
        std::io::stdout().write_all(&result.stdout)?;
        std::io::stderr().write_all(&result.stderr)?;
        return Ok(());
    }

    let (cols, rows) = size().unwrap_or((80, 24));
    enable_raw_mode()?;
    let _raw = RawMode;
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let code = runtime.block_on(run_shell(conn, args.term, args.command, cols, rows))?;
    drop(_raw);
    std::io::stdout().flush()?;
    std::process::exit(code);
}

fn split_destination(destination: &str, user: Option<String>) -> (Option<String>, String) {
    match destination.split_once('@') {
        Some((name, host)) => (Some(name.to_string()), host.to_string()),
        None => (Some(user.unwrap_or_default()), destination.to_string()),
    }
}

async fn run_shell(
    conn: Connection,
    term: String,
    command: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<i32> {
    let emu = Arc::new(Mutex::new(Emulator::new(rows, cols, 0)));
    let done = Arc::new(AtomicBool::new(false));
    let exit = Arc::new(AtomicI32::new(0));
    let shared: Arc<Mutex<Option<Arc<TerminalHandle>>>> = Arc::new(Mutex::new(None));

    let sink = {
        let emu = emu.clone();
        let done = done.clone();
        let exit = exit.clone();
        let shared = shared.clone();
        TermSink::new(move |event| match event {
            TermEvent::Data(bytes) => {
                let responses = emu.lock().unwrap().feed(&bytes);
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&bytes);
                if !responses.is_empty() {
                    let _ = out.write_all(&responses);
                }
                let _ = out.flush();
            }
            TermEvent::Closed { exit: code } => {
                exit.store(code.unwrap_or(0) as i32, Ordering::SeqCst);
                if let Some(handle) = shared.lock().unwrap().as_ref() {
                    handle.close();
                }
                done.store(true, Ordering::SeqCst);
            }
        })
    };

    let handle = Arc::new(conn.shell(cols, rows, &term, command, sink).await?);
    *shared.lock().unwrap() = Some(handle.clone());

    let reader = {
        let emu = emu.clone();
        let done = done.clone();
        thread::spawn(move || drive_input(emu, handle, done))
    };

    while !done.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(40));
    }
    let _ = reader.join();
    let code = exit.load(Ordering::SeqCst);
    conn.close().await;
    Ok(code)
}

/// Read the local console and forward it to the remote PTY.
fn drive_input(emu: Arc<Mutex<Emulator>>, handle: Arc<TerminalHandle>, done: Arc<AtomicBool>) {
    let mut mouse = false;
    while !done.load(Ordering::SeqCst) {
        let state = emu.lock().unwrap().state();
        let want_mouse = state.mouse_mode != MouseProtocolMode::None;
        if want_mouse != mouse {
            mouse = want_mouse;
            let mut out = std::io::stdout().lock();
            if want_mouse {
                let _ = execute!(out, EnableMouseCapture);
            } else {
                let _ = execute!(out, DisableMouseCapture);
            }
            let _ = out.flush();
        }

        match event::poll(Duration::from_millis(60)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => break,
        }
        let Ok(event) = event::read() else { break };
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if let Some(bytes) = encode_key(state, key) {
                    let _ = handle.send(bytes);
                }
            }
            Event::Resize(cols, rows) => {
                handle.resize(rows, cols);
                emu.lock().unwrap().resize(rows, cols);
            }
            Event::Mouse(mouse_event) => {
                if let Some(bytes) = encode_mouse_event(state, mouse_event) {
                    let _ = handle.send(bytes);
                }
            }
            _ => {}
        }
    }
}

fn encode_key(state: TermState, key: KeyEvent) -> Option<Vec<u8>> {
    let mods = Mods {
        ctrl: key.modifiers.contains(M::CONTROL),
        alt: key.modifiers.contains(M::ALT),
        shift: key.modifiers.contains(M::SHIFT),
    };
    let code = match key.code {
        CKey::Backspace => KeyCode::Backspace,
        CKey::Enter => KeyCode::Enter,
        CKey::Left => KeyCode::Left,
        CKey::Right => KeyCode::Right,
        CKey::Up => KeyCode::Up,
        CKey::Down => KeyCode::Down,
        CKey::Home => KeyCode::Home,
        CKey::End => KeyCode::End,
        CKey::PageUp => KeyCode::PageUp,
        CKey::PageDown => KeyCode::PageDown,
        CKey::Tab => KeyCode::Tab,
        CKey::BackTab => KeyCode::BackTab,
        CKey::Delete => KeyCode::Delete,
        CKey::Insert => KeyCode::Insert,
        CKey::Esc => KeyCode::Escape,
        CKey::F(number) => KeyCode::F(number),
        CKey::Char(character) => KeyCode::Char(character),
        _ => return None,
    };
    InputEncoder(state).encode(code, mods)
}

fn button_of(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => MOUSE_LEFT,
        MouseButton::Middle => MOUSE_MIDDLE,
        MouseButton::Right => MOUSE_RIGHT,
    }
}

fn encode_mouse_event(state: TermState, event: crossterm::event::MouseEvent) -> Option<Vec<u8>> {
    if state.mouse_mode == MouseProtocolMode::None {
        return None;
    }
    let report = match event.kind {
        MouseEventKind::Down(button) => MouseReport::Press {
            button: button_of(button),
        },
        MouseEventKind::Up(_) => MouseReport::Release,
        MouseEventKind::Drag(button) => MouseReport::Drag {
            button: button_of(button),
        },
        MouseEventKind::ScrollUp => MouseReport::Press {
            button: MOUSE_WHEEL_UP,
        },
        MouseEventKind::ScrollDown => MouseReport::Press {
            button: MOUSE_WHEEL_DOWN,
        },
        _ => return None,
    };
    encode_mouse(state, report, event.row, event.column, Mods::default())
}
