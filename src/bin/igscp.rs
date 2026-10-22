//! `igscp` - copy files to or from a remote host, like `scp`.
//!
//! ```text
//! igscp -r user@host:/var/log ./logs
//! igscp -i ~/.ssh/id_ed25519 build/app.tar.gz host:/tmp/
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::Parser;

use imgossh::cli::{self, ConnectArgs};
use imgossh::ssh::client::Connection;
use imgossh::ssh::transfer::{Phase, Queue};
use imgossh::util::{human_size, remote_file_name, RemoteSpec};

#[derive(Parser)]
#[command(
    name = "igscp",
    version,
    about = "Copy files between this machine and a remote host over SSH"
)]
struct Args {
    /// Copy directories recursively
    #[arg(short = 'r')]
    recursive: bool,
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
    /// Do not print progress
    #[arg(short = 'q')]
    quiet: bool,
    /// Keepalive interval in seconds
    #[arg(long, default_value_t = 60)]
    keepalive: u64,
    /// One or more sources followed by a destination
    #[arg(required = true)]
    specs: Vec<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    if args.specs.len() < 2 {
        bail!("usage: igscp [source...] [destination]");
    }

    let (sources, targets) = args.specs.split_at(args.specs.len() - 1);
    let target = targets[0].clone();
    let target_remote = RemoteSpec::parse(&target, args.port);
    let remote_sources: Vec<RemoteSpec> = sources
        .iter()
        .filter_map(|spec| RemoteSpec::parse(spec, args.port))
        .collect();
    let local_sources: Vec<PathBuf> = sources
        .iter()
        .filter(|spec| RemoteSpec::parse(spec, args.port).is_none())
        .map(PathBuf::from)
        .collect();

    match target_remote {
        Some(target) => {
            if !remote_sources.is_empty() {
                bail!("copying between two remote hosts is not supported");
            }
            if local_sources.is_empty() {
                bail!("no local source given");
            }
            upload(&args, &target, &local_sources)
        }
        None => {
            if remote_sources.is_empty() {
                bail!("at least one path must be on a remote host");
            }
            download(&args, &remote_sources, &PathBuf::from(&target))
        }
    }
}

fn connect(
    args: &Args,
    host: &str,
    user: Option<&str>,
) -> Result<(tokio::runtime::Runtime, Connection)> {
    let connect = ConnectArgs {
        user: user.map(str::to_string).or_else(|| args.user.clone()),
        port: args.port,
        identity: args.identity.clone(),
        password: args
            .ask_password
            .then(|| rpassword::prompt_password("password: ").unwrap_or_default()),
        agent: !args.no_agent,
        keepalive: args.keepalive,
        tunnels: Vec::new(),
    };
    let runtime = cli::runtime()?;
    let conn = connect.connect(&runtime, host)?;
    Ok((runtime, conn))
}

fn upload(args: &Args, target: &RemoteSpec, sources: &[PathBuf]) -> Result<()> {
    let (runtime, conn) = connect(args, &target.host, target.user.as_deref())?;
    let queue = Queue::new();
    for source in sources {
        if !source.exists() {
            bail!("{}: No such file or directory", source.display());
        }
        let recursive = args.recursive || source.is_dir();
        queue.upload(conn.clone(), source.clone(), target.path.clone(), recursive);
    }
    finish(args, &runtime, &conn, &queue)
}

fn download(args: &Args, sources: &[RemoteSpec], target: &Path) -> Result<()> {
    let first = sources.first().context("no source given")?;
    for source in sources {
        if source.host != first.host {
            bail!(
                "all remote sources must be on one host ({} != {})",
                source.host,
                first.host
            );
        }
    }
    let (runtime, conn) = connect(args, &first.host, first.user.as_deref())?;

    // `scp host:a.txt local.txt` renames; `scp host:a.txt dir/` keeps the name.
    let rename_to =
        if sources.len() == 1 && !target.is_dir() && !ends_with_sep(&target.to_string_lossy()) {
            Some(target.to_path_buf())
        } else {
            None
        };
    let directory = match &rename_to {
        Some(path) => path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
        None => target.to_path_buf(),
    };
    if !directory.exists() {
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }

    let queue = Queue::new();
    for source in sources {
        queue.download(
            conn.clone(),
            source.path.clone(),
            directory.clone(),
            args.recursive,
        );
    }
    let result = finish(args, &runtime, &conn, &queue);

    if let Some(destination) = rename_to {
        let wanted = directory.join(remote_file_name(&sources[0].path));
        if wanted != destination && wanted.exists() {
            std::fs::rename(&wanted, &destination)
                .with_context(|| format!("renaming to {}", destination.display()))?;
        }
    }
    result
}

fn ends_with_sep(path: &str) -> bool {
    path.ends_with('/') || path.ends_with('\\')
}

fn finish(
    args: &Args,
    runtime: &tokio::runtime::Runtime,
    conn: &Connection,
    queue: &Queue,
) -> Result<()> {
    let mut printed = 0usize;
    loop {
        let jobs = queue.jobs();
        let active = jobs.iter().any(|job| !job.is_finished());
        if !args.quiet {
            if printed > 0 {
                print!("\x1b[{}A", printed);
            }
            let mut lines = Vec::new();
            for job in &jobs {
                let state = job.snapshot();
                lines.push(format!(
                    "{} {:<34} {} {}",
                    job.direction.arrow(),
                    imgossh::util::truncate_chars(&job.source, 34),
                    cli::progress_bar(state.done, state.total, 26),
                    human_size(state.done)
                ));
            }
            for line in &lines {
                print!("\r\x1b[2K{line}");
                println!();
            }
            let _ = std::io::stdout().flush();
            printed = lines.len();
        }
        if !active {
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }

    let mut failed = 0usize;
    for job in queue.jobs() {
        match job.snapshot().phase {
            Phase::Failed(err) => {
                failed += 1;
                eprintln!("igscp: {}: {err}", job.source);
            }
            Phase::Cancelled => {
                failed += 1;
                eprintln!("igscp: {}: cancelled", job.source);
            }
            _ => {}
        }
    }
    runtime.block_on(conn.close());
    if !args.quiet {
        let _ = std::io::stdout().flush();
    }
    if failed > 0 {
        bail!("{failed} transfer(s) failed");
    }
    Ok(())
}
