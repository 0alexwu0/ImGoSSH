//! `igsftp` - an interactive SFTP prompt, like OpenSSH's `sftp`.
//!
//! ```text
//! sftp> ls -l
//! sftp> cd /var/log
//! sftp> get syslog ./syslog
//! sftp> put ./app.tar.gz /tmp/
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use russh_sftp::client::SftpSession;

use imgossh::cli::{self, ConnectArgs};
use imgossh::ssh::client::Connection;
use imgossh::ssh::sftp::{self, Entry, Kind};
use imgossh::ssh::transfer::{Phase, Queue};
use imgossh::util::{human_size, human_time, remote_join, RemoteSpec};

#[derive(Parser)]
#[command(
    name = "igsftp",
    version,
    about = "Interactive SFTP file transfer program"
)]
struct Args {
    /// Destination, `[user@]host[:directory]`
    destination: Option<String>,
    /// Remote port
    #[arg(short = 'P', long = "port", default_value_t = 22)]
    port: u16,
    /// Private key file
    #[arg(short = 'i')]
    identity: Option<PathBuf>,
    /// Login user
    #[arg(short = 'l')]
    user: Option<String>,
    /// Do not use ssh-agent / Pageant
    #[arg(short = 'a')]
    no_agent: bool,
    /// Ask for the password on the terminal
    #[arg(short = 'w')]
    ask_password: bool,
    /// Directory to open when the connection is up
    #[arg(long, default_value = ".")]
    start: String,
    /// Run these commands and exit
    #[arg(short = 'o', long = "command")]
    commands: Vec<String>,
    /// Read commands from a file
    #[arg(short = 'b', long = "batch")]
    batch: Option<PathBuf>,
}

/// The interactive session.
struct Repl {
    sftp: Arc<SftpSession>,
    queue: Queue,
    conn: Connection,
    cwd: String,
    local: PathBuf,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let (host, user, start) = match &args.destination {
        Some(destination) => match RemoteSpec::parse(destination, args.port) {
            Some(spec) => (
                spec.host,
                spec.user,
                if spec.path == "." {
                    args.start.clone()
                } else {
                    spec.path
                },
            ),
            None => (destination.clone(), None, args.start.clone()),
        },
        None => bail!("no destination given"),
    };

    let connect = ConnectArgs {
        user: user.or_else(|| args.user.clone()),
        port: args.port,
        identity: args.identity.clone(),
        password: args
            .ask_password
            .then(|| rpassword::prompt_password("password: ").unwrap_or_default()),
        agent: !args.no_agent,
        keepalive: 60,
        tunnels: Vec::new(),
    };
    let runtime = cli::runtime()?;
    let conn = connect.connect(&runtime, &host)?;
    let sftp = runtime.block_on(conn.sftp())?;
    let cwd = runtime
        .block_on(sftp::canonicalize(&sftp, &start))
        .unwrap_or(start.clone());

    let mut repl = Repl {
        sftp,
        queue: Queue::new(),
        conn,
        cwd,
        local: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let mut lines: Vec<String> = args.commands.clone();
    if let Some(path) = &args.batch {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        lines.extend(text.lines().map(str::to_string));
    }
    let scripted = !lines.is_empty();

    let mut index = 0usize;
    loop {
        let line = if scripted {
            match lines.get(index) {
                Some(line) => {
                    index += 1;
                    line.clone()
                }
                None => break,
            }
        } else {
            print!("sftp> ");
            let _ = std::io::stdout().flush();
            let mut buffer = String::new();
            match std::io::stdin().read_line(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => buffer,
            }
        };
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match runtime.block_on(repl.run(&line)) {
            Ok(()) => {}
            Err(err) => eprintln!("igsftp: {err:#}"),
        }
        if line == "bye" || line == "exit" || line == "quit" {
            break;
        }
    }

    runtime.block_on(repl.conn.close());
    Ok(())
}

impl Repl {
    fn absolute(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else {
            remote_join(&self.cwd, path)
        }
    }

    async fn run(&mut self, line: &str) -> Result<()> {
        let (command, rest) = match line.split_once(char::is_whitespace) {
            Some((head, tail)) => (head.to_ascii_lowercase(), tail.trim().to_string()),
            None => (line.to_ascii_lowercase(), String::new()),
        };
        let words: Vec<&str> = rest.split_whitespace().collect();

        match command.as_str() {
            "bye" | "exit" | "quit" => {}
            "help" | "?" => help(),
            "ls" | "dir" => {
                let long = command == "dir" || words.iter().any(|w| w.starts_with('-'));
                self.ls(&words, long).await?;
            }
            "cd" => self.cd(&words).await?,
            "pwd" => println!("{}", self.cwd),
            "lpwd" => println!("{}", self.local.display()),
            "lcd" => {
                let path = words
                    .first()
                    .map(PathBuf::from)
                    .unwrap_or(PathBuf::from("."));
                let path = if path.is_absolute() {
                    path
                } else {
                    self.local.join(path)
                };
                self.local = std::fs::canonicalize(&path)
                    .with_context(|| format!("cannot change to {}", path.display()))?;
            }
            "get" | "reget" => self.get(&words).await?,
            "put" | "reput" => self.put(&words).await?,
            "mkdir" => {
                for word in &words {
                    sftp::mkdir(&self.sftp, &self.absolute(word)).await?;
                }
            }
            "rmdir" => {
                for word in &words {
                    let path = self.absolute(word);
                    self.sftp
                        .remove_dir(&path)
                        .await
                        .with_context(|| format!("removing {path}"))?;
                }
            }
            "rm" | "delete" => {
                for word in &words {
                    let path = self.absolute(word);
                    let entry = self.entry_for(&path).await?;
                    sftp::remove_all(&self.sftp, &entry).await?;
                }
            }
            "rename" | "mv" => {
                if words.len() != 2 {
                    bail!("usage: rename old new");
                }
                sftp::rename(
                    &self.sftp,
                    &self.absolute(words[0]),
                    &self.absolute(words[1]),
                )
                .await?;
            }
            "stat" => {
                for word in &words {
                    let entry = self.entry_for(&self.absolute(word)).await?;
                    println!(
                        "{}  {}  {}  {}",
                        entry.permissions(),
                        human_size(entry.size),
                        human_time(entry.mtime),
                        entry.path
                    );
                }
            }
            "local" => {
                if rest.is_empty() {
                    bail!("usage: local <command>");
                }
                println!("exit code {}", run_local(&rest)?);
            }
            other => eprintln!("unknown command: {other} (try `help`)"),
        }
        Ok(())
    }

    async fn entry_for(&self, path: &str) -> Result<Entry> {
        let meta = self
            .sftp
            .symlink_metadata(path)
            .await
            .with_context(|| format!("stat {path}"))?;
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .to_string();
        let permissions = meta.permissions();
        let mut mode = 0u32;
        if permissions.owner_read {
            mode |= 0o400;
        }
        if permissions.owner_write {
            mode |= 0o200;
        }
        if permissions.owner_exec {
            mode |= 0o100;
        }
        if permissions.group_read {
            mode |= 0o040;
        }
        if permissions.group_write {
            mode |= 0o020;
        }
        if permissions.group_exec {
            mode |= 0o010;
        }
        if permissions.other_read {
            mode |= 0o004;
        }
        if permissions.other_write {
            mode |= 0o002;
        }
        if permissions.other_exec {
            mode |= 0o001;
        }
        Ok(Entry {
            name,
            path: path.to_string(),
            kind: match meta.file_type() {
                russh_sftp::protocol::FileType::Dir => Kind::Directory,
                russh_sftp::protocol::FileType::File => Kind::File,
                russh_sftp::protocol::FileType::Symlink => Kind::Symlink,
                _ => Kind::Other,
            },
            size: meta.len(),
            mtime: meta_time(meta.modified()),
            mode,
        })
    }

    async fn ls(&mut self, words: &[&str], long: bool) -> Result<()> {
        let mut path = self.cwd.clone();
        for word in words {
            if word.starts_with('-') {
                continue;
            }
            path = self.absolute(word);
        }
        let mut entries = sftp::list(&self.sftp, &path).await?;
        sftp::sort_entries(&mut entries);
        if long {
            for entry in &entries {
                println!(
                    "{}  {:>10}  {:>17}  {}{}",
                    entry.permissions(),
                    human_size(entry.size),
                    human_time(entry.mtime),
                    entry.name,
                    match entry.kind {
                        Kind::Directory => "/",
                        Kind::Symlink => "@",
                        _ => "",
                    }
                );
            }
        } else {
            for entry in &entries {
                match entry.kind {
                    Kind::Directory => println!("{}/", entry.name),
                    _ => println!("{}", entry.name),
                }
            }
        }
        Ok(())
    }

    async fn cd(&mut self, words: &[&str]) -> Result<()> {
        let target = words.first().copied().unwrap_or("/");
        let path = self.absolute(target);
        self.cwd = sftp::canonicalize(&self.sftp, &path)
            .await
            .with_context(|| format!("cannot cd to {path}"))?;
        Ok(())
    }

    async fn get(&mut self, words: &[&str]) -> Result<()> {
        if words.is_empty() {
            bail!("usage: get remote [local]");
        }
        let remote = self.absolute(words[0]);
        let local = match words.get(1) {
            Some(name) => PathBuf::from(name),
            None => self.local.clone(),
        };
        let directory = if local.is_dir() {
            local.clone()
        } else {
            local
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        std::fs::create_dir_all(&directory).ok();
        let job = self
            .queue
            .download(self.conn.clone(), remote, directory, true);
        wait_for(&self.queue, job.id).await
    }

    async fn put(&mut self, words: &[&str]) -> Result<()> {
        if words.is_empty() {
            bail!("usage: put local [remote]");
        }
        let local = PathBuf::from(words[0]);
        if !local.exists() {
            bail!("{}: No such file or directory", local.display());
        }
        let remote = words
            .get(1)
            .map(|path| self.absolute(path))
            .unwrap_or_else(|| self.cwd.clone());
        let recursive = local.is_dir();
        let job = self
            .queue
            .upload(self.conn.clone(), local, remote, recursive);
        wait_for(&self.queue, job.id).await
    }
}

async fn wait_for(queue: &Queue, id: u64) -> Result<()> {
    loop {
        let Some(job) = queue.jobs().into_iter().find(|job| job.id == id) else {
            return Ok(());
        };
        let state = job.snapshot();
        if state.phase.is_finished() {
            return match state.phase {
                Phase::Failed(err) => bail!(err),
                Phase::Cancelled => bail!("cancelled"),
                _ => Ok(()),
            };
        }
        thread::sleep(Duration::from_millis(120));
    }
}

fn meta_time(when: Result<SystemTime, std::io::Error>) -> i64 {
    when.ok()
        .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

fn run_local(command: &str) -> Result<i32> {
    #[cfg(windows)]
    let status = std::process::Command::new("cmd")
        .args(["/C", command])
        .status()
        .context("running the local command")?;
    #[cfg(not(windows))]
    let status = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        std::process::Command::new(shell)
            .arg("-c")
            .arg(command)
            .status()
            .context("running the local command")?
    };
    Ok(status.code().unwrap_or(-1))
}

fn help() {
    println!(
        "Available commands:
  ls [-l] [path]        list a remote directory
  cd <path>             change the remote directory
  pwd                   print the remote directory
  lpwd / lcd <path>     show or change the local directory
  get <remote> [local]  download a file or directory
  put <local> [remote]  upload a file or directory
  mkdir / rmdir / rm    change the remote tree
  rename <a> <b>        rename a remote entry
  stat <path>           describe one entry
  local <command>       run a command on this machine
  help                  this text
  bye                   close the connection"
    );
}
