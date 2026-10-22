//! Shared plumbing for the command line tools (`igssh`, `igscp`, `igsftp`).
//!
//! The engine asks questions through a [`Prompter`]; on a TTY we answer them
//! from a helper thread that owns stdin while the handshake is in flight.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use tokio::sync::{mpsc, mpsc::error::TryRecvError};

use crate::config::{Tunnel, TunnelKind};
use crate::ssh::client::{Challenge, ConnectOptions, Connection, Prompter, Reply, Request, Trust};

/// Build the multi-threaded runtime the tools share.
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .context("starting the async runtime")
}

fn read_line(prompt: &str) -> String {
    let mut out = io::stdout();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim_end_matches(['\r', '\n']).to_string()
}

fn read_secret(prompt: &str) -> String {
    rpassword::prompt_password(prompt).unwrap_or_default()
}

/// Answer one engine question on the controlling terminal.
pub fn answer(challenge: &Challenge) -> Reply {
    match challenge {
        Challenge::HostKey(info) => {
            eprintln!(
                "The authenticity of host '{}:{}' cannot be established.",
                info.host, info.port
            );
            eprintln!(
                "{} key fingerprint is {}.",
                info.algorithm, info.fingerprint
            );
            if let Some(recorded) = &info.recorded {
                eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
                eprintln!("WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!");
                eprintln!("     The stored key for this host is: {recorded}");
                eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
            }
            loop {
                let reply = read_line("Continue connecting (yes/no)? ");
                match reply.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => return Reply::TrustKey(Trust::Always),
                    "n" | "no" => return Reply::TrustKey(Trust::Never),
                    "" => return Reply::TrustKey(Trust::Never),
                    _ => continue,
                }
            }
        }
        Challenge::Password {
            host,
            username,
            incorrect,
        } => {
            if *incorrect {
                eprintln!("Permission denied, please try again.");
            }
            let secret = read_secret(&format!("{username}@{host}'s password: "));
            if secret.is_empty() {
                Reply::Cancel
            } else {
                Reply::Secret(secret)
            }
        }
        Challenge::Passphrase { key, incorrect } => {
            if *incorrect {
                eprintln!("That passphrase was not accepted.");
            }
            let secret = read_secret(&format!("Enter passphrase for key '{}': ", key.display()));
            if secret.is_empty() {
                Reply::Cancel
            } else {
                Reply::Secret(secret)
            }
        }
        Challenge::KeyboardInteractive {
            name,
            instructions,
            prompts,
            ..
        } => {
            if !name.is_empty() {
                eprintln!("{name}");
            }
            if !instructions.is_empty() {
                eprintln!("{instructions}");
            }
            let mut answers = Vec::with_capacity(prompts.len());
            for prompt in prompts {
                answers.push(if prompt.echo {
                    read_line(&prompt.text)
                } else {
                    read_secret(&prompt.text)
                });
            }
            Reply::Responses(answers)
        }
    }
}

/// Answer prompts on a background thread until `stop` is set.
pub fn spawn_prompt_thread(
    mut rx: mpsc::UnboundedReceiver<Request>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        match rx.try_recv() {
            Ok(request) => {
                let reply = answer(&request.challenge);
                if request.reply.send(reply).is_err() {
                    break;
                }
            }
            Err(TryRecvError::Empty) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryRecvError::Disconnected) => break,
        }
    })
}

/// Everything the tools accept on the command line for one connection.
#[derive(Debug, Clone, Default)]
pub struct ConnectArgs {
    pub user: Option<String>,
    pub port: u16,
    pub identity: Option<PathBuf>,
    pub password: Option<String>,
    pub agent: bool,
    pub keepalive: u64,
    pub tunnels: Vec<Tunnel>,
}

impl ConnectArgs {
    /// Connect, answering host-key and credential prompts on this TTY.
    pub fn connect(&self, rt: &tokio::runtime::Runtime, host: &str) -> Result<Connection> {
        let username = self.user.clone().unwrap_or_else(default_user);
        let (prompter, rx) = Prompter::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_prompt_thread(rx, stop.clone());

        let opts = ConnectOptions {
            host: host.to_string(),
            port: if self.port == 0 { 22 } else { self.port },
            username,
            password: self.password.clone(),
            private_key: self.identity.clone(),
            use_agent: self.agent,
            keepalive: (self.keepalive > 0).then(|| Duration::from_secs(self.keepalive)),
            prompter: prompter.clone(),
        };
        let conn = match rt.block_on(Connection::connect(opts)) {
            Ok(conn) => conn,
            Err(err) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                return Err(err);
            }
        };
        for tunnel in &self.tunnels {
            if let Err(err) = rt.block_on(conn.start_tunnel(tunnel)) {
                bail!("tunnel {}: {err:#}", tunnel.describe());
            }
        }
        stop.store(true, Ordering::SeqCst);
        let _ = thread.join();
        drop(prompter);
        Ok(conn)
    }
}

fn default_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

/// Parse an OpenSSH-style `-L`/`-R`/`-D` argument.
pub fn parse_tunnel(kind: TunnelKind, spec: &str) -> Result<Tunnel> {
    if kind == TunnelKind::Dynamic {
        let port: u16 = spec
            .rsplit(':')
            .next()
            .unwrap_or(spec)
            .parse()
            .with_context(|| format!("invalid dynamic port {spec}"))?;
        return Ok(Tunnel {
            kind,
            listen_port: port,
            remote_host: String::new(),
            remote_port: 0,
        });
    }
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 {
        bail!("expected [bind_address:]port:host:hostport, got {spec}");
    }
    let listen_port: u16 = parts[parts.len() - 3]
        .parse()
        .with_context(|| format!("invalid listen port in {spec}"))?;
    let remote_port: u16 = parts[2]
        .parse()
        .with_context(|| format!("invalid remote port in {spec}"))?;
    Ok(Tunnel {
        kind,
        listen_port,
        remote_host: parts[1].to_string(),
        remote_port,
    })
}

/// A one-line progress bar for stdout.
pub fn progress_bar(done: u64, total: u64, width: usize) -> String {
    let ratio = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    };
    let filled = (ratio * width as f64).round() as usize;
    let bar = "#".repeat(filled) + &"-".repeat(width.saturating_sub(filled));
    format!("[{bar}] {:>3.0}%", ratio * 100.0)
}
