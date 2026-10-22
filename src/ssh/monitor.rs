//! Live server statistics for the status bar, gathered over the SSH connection.
//!
//! The strip at the bottom of a session shows the far side's hostname, CPU,
//! memory, throughput, uptime, logged-in users and disk usage. Those numbers
//! come from one short, POSIX-shell compatible probe run every couple of
//! seconds, whose sections are parsed back out of its output.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ssh::client::Connection;

/// How long to wait between two probes.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);

/// Filesystems that never hold user data.
const PSEUDO_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "iso9660",
    "mqueue",
    "nsfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "rpc_pipefs",
    "securityfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// One mounted filesystem.
#[derive(Debug, Clone)]
pub struct Disk {
    /// Where it is mounted, as shown in the bar.
    pub mount: String,
    /// Used space, 0..=100.
    pub percent: f64,
    /// Total size in bytes.
    pub total: u64,
    /// Free space in bytes.
    pub free: u64,
}

/// A snapshot of the remote host.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    /// Name the server calls itself.
    pub hostname: String,
    /// `PRETTY_NAME` from `/etc/os-release`, when there is one.
    pub os: String,
    /// `uname -srm`.
    pub kernel: String,
    /// CPU load, 0..=100.
    pub cpu: f64,
    /// 1 / 5 / 15 minute load averages.
    pub load: [f32; 3],
    /// RAM in use, in bytes.
    pub mem_used: u64,
    /// RAM installed, in bytes.
    pub mem_total: u64,
    /// Swap in use, in bytes.
    pub swap_used: u64,
    /// Swap configured, in bytes.
    pub swap_total: u64,
    /// Received bytes per second.
    pub rx: f64,
    /// Sent bytes per second.
    pub tx: f64,
    /// Seconds since the machine booted.
    pub uptime: u64,
    /// Accounts with an open session.
    pub users: Vec<String>,
    /// Real mounted filesystems.
    pub disks: Vec<Disk>,
}

/// The probe. Plain commands and `@section` markers only, so it survives
/// `sh`, `dash`, `bash`, `zsh` and even `csh` on the far side.
const PROBE: &str = concat!(
    "echo @host; hostname 2>/dev/null || uname -n 2>/dev/null; ",
    "echo @os; grep '^PRETTY_NAME=' /etc/os-release 2>/dev/null; ",
    "echo @uname; uname -srm 2>/dev/null; ",
    "echo @load; cat /proc/loadavg 2>/dev/null; ",
    "echo @uptime; cat /proc/uptime 2>/dev/null; ",
    "echo @cpu; head -n 1 /proc/stat 2>/dev/null; ",
    "echo @mem; grep -E '^(MemTotal|MemFree|MemAvailable|Buffers|Cached|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null; ",
    "echo @net; cat /proc/net/dev 2>/dev/null; ",
    "echo @df; df -Pk 2>/dev/null; ",
    "echo @users; who 2>/dev/null; ",
    "echo @end",
);

/// Counters from the previous probe, needed to turn them into rates.
#[derive(Default)]
struct Delta {
    cpu: Option<(u64, u64)>,
    net: Option<(u64, u64)>,
}

impl Delta {
    /// Build one snapshot from the probe output.
    fn apply(&mut self, text: &str, elapsed: f64) -> ServerInfo {
        let blocks = sections(text);
        let mut info = ServerInfo::default();

        info.hostname = first_value(blocks.get("host").map_or(&[][..], |v| v));
        info.os = blocks
            .get("os")
            .map_or(&[][..], |v| v)
            .iter()
            .find_map(|line| line.trim().strip_prefix("PRETTY_NAME="))
            .map(|value| value.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        info.kernel = first_value(blocks.get("uname").map_or(&[][..], |v| v));

        if let Some(line) = blocks.get("load").map_or(&[][..], |v| v).first() {
            let mut parts = line.split_whitespace().flat_map(str::parse::<f32>);
            info.load = [
                parts.next().unwrap_or(0.0),
                parts.next().unwrap_or(0.0),
                parts.next().unwrap_or(0.0),
            ];
        }

        if let Some(line) = blocks.get("uptime").map_or(&[][..], |v| v).first() {
            info.uptime = line
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|v| v.max(0.0) as u64)
                .unwrap_or(0);
        }

        if let Some(line) = blocks
            .get("cpu")
            .map_or(&[][..], |v| v)
            .iter()
            .find(|line| line.starts_with("cpu"))
        {
            let fields: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .map(|v| v.parse::<u64>().unwrap_or(0))
                .collect();
            if fields.len() >= 5 {
                // user nice system idle iowait irq softirq steal; the guest
                // counters are already folded into user, so stop at eight.
                let total: u64 = fields.iter().take(8).sum();
                let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
                if let Some((before, idle_before)) = self.cpu {
                    if total > before {
                        let busy =
                            (total - before).saturating_sub(idle.saturating_sub(idle_before));
                        info.cpu =
                            (busy as f64 / (total - before) as f64 * 100.0).clamp(0.0, 100.0);
                    }
                }
                self.cpu = Some((total, idle));
            }
        }

        let mem = meminfo(blocks.get("mem").map_or(&[][..], |v| v));
        info.mem_total = mem.get("MemTotal").copied().unwrap_or(0);
        info.mem_used = match mem.get("MemAvailable").copied() {
            Some(available) => info.mem_total.saturating_sub(available),
            None => {
                let free = mem.get("MemFree").copied().unwrap_or(0);
                let buffers = mem.get("Buffers").copied().unwrap_or(0);
                let cached = mem.get("Cached").copied().unwrap_or(0);
                info.mem_total.saturating_sub(free + buffers + cached)
            }
        };
        info.swap_total = mem.get("SwapTotal").copied().unwrap_or(0);
        info.swap_used = info
            .swap_total
            .saturating_sub(mem.get("SwapFree").copied().unwrap_or(0));

        if let Some(now) = parse_net(blocks.get("net").map_or(&[][..], |v| v)) {
            if let Some(before) = self.net {
                if now.0 >= before.0 && now.1 >= before.1 && elapsed > 0.0 {
                    info.rx = (now.0 - before.0) as f64 / elapsed;
                    info.tx = (now.1 - before.1) as f64 / elapsed;
                }
            }
            self.net = Some(now);
        }

        info.disks = parse_df(blocks.get("df").map_or(&[][..], |v| v));
        info.users = parse_users(blocks.get("users").map_or(&[][..], |v| v));
        info
    }
}

/// A running host monitor. Cloning is cheap: every clone drives the same task.
#[derive(Clone)]
pub struct Monitor {
    info: Arc<Mutex<Option<ServerInfo>>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    /// Create the shared slots before there is a connection to poll.
    pub fn new() -> Self {
        Self {
            info: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start polling. Call from inside the tokio runtime, once per connection.
    pub fn attach<F: Fn() + Send + Sync + 'static>(
        &self,
        conn: Connection,
        interval: Duration,
        repaint: F,
    ) {
        let info = self.info.clone();
        let stop = self.stop.clone();
        let paused = self.paused.clone();
        tokio::spawn(async move {
            let mut delta = Delta::default();
            let mut at = Instant::now();
            let mut misses = 0u32;
            loop {
                if stop.load(Ordering::Relaxed) || !conn.is_alive() {
                    break;
                }
                if paused.load(Ordering::Relaxed) {
                    tokio::time::sleep(interval).await;
                    delta = Delta::default();
                    at = Instant::now();
                    continue;
                }
                match conn.exec(PROBE).await {
                    Ok(result) => {
                        let now = Instant::now();
                        let elapsed = now.duration_since(at).as_secs_f64().max(0.25);
                        at = now;
                        let snapshot = delta.apply(&result.stdout_text(), elapsed);
                        *info.lock().unwrap() = Some(snapshot);
                        misses = 0;
                    }
                    Err(err) => {
                        log::debug!("host monitor probe failed: {err}");
                        misses += 1;
                        if misses >= 3 {
                            break;
                        }
                    }
                }
                repaint();
                tokio::time::sleep(interval).await;
            }
            *info.lock().unwrap() = None;
        });
    }

    /// The newest snapshot, or `None` while nothing has come back yet.
    pub fn snapshot(&self) -> Option<ServerInfo> {
        self.info.lock().unwrap().clone()
    }

    /// Stop polling.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Skip probes while the numbers are not on screen.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Split the probe output into `@section` blocks.
fn sections(text: &str) -> BTreeMap<&str, Vec<&str>> {
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(name) = line.strip_prefix('@') {
            if name.is_empty() {
                continue;
            }
            current = Some(name);
            out.entry(name).or_default();
            continue;
        }
        if let Some(name) = current {
            out.entry(name).or_default().push(line);
        }
    }
    out
}

/// The first non-empty line of a block.
fn first_value(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('@'))
        .unwrap_or("")
        .to_string()
}

/// `/proc/meminfo` -> name to bytes.
fn meminfo<'a>(lines: &[&'a str]) -> BTreeMap<&'a str, u64> {
    let mut out = BTreeMap::new();
    for line in lines {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let unit = rest.split_whitespace().nth(1).unwrap_or("kB");
        let scale = match unit {
            "kB" => 1024,
            "MB" => 1024 * 1024,
            _ => 1,
        };
        out.insert(key.trim(), value * scale);
    }
    out
}

/// `/proc/net/dev` -> total received and sent bytes, loopback excluded.
fn parse_net(lines: &[&str]) -> Option<(u64, u64)> {
    let (mut rx, mut tx, mut seen) = (0u64, 0u64, false);
    for line in lines {
        let Some((face, rest)) = line.split_once(':') else {
            continue;
        };
        let face = face.trim();
        if face.is_empty() || face == "lo" {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        rx += fields[0].parse::<u64>().unwrap_or(0);
        tx += fields[8].parse::<u64>().unwrap_or(0);
        seen = true;
    }
    seen.then_some((rx, tx))
}

/// `df -Pk` -> the filesystems worth showing.
fn parse_df(lines: &[&str]) -> Vec<Disk> {
    let mut out: Vec<Disk> = Vec::new();
    let mut wrapped = false;
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        if wrapped {
            if let Some(last) = out.last_mut() {
                last.mount = fields[0].to_string();
            }
            wrapped = false;
            continue;
        }
        if fields[0] == "Filesystem" {
            continue;
        }
        let Some(position) = fields.iter().rposition(|field| field.ends_with('%')) else {
            continue;
        };
        let percent: f64 = fields[position]
            .trim_end_matches('%')
            .parse()
            .unwrap_or(0.0);
        let blocks = |index: usize| {
            fields
                .get(index)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024
        };
        let mount = fields[position + 1..].join(" ");
        wrapped = mount.is_empty();
        if is_pseudo(fields[0], &mount) {
            wrapped = false;
            continue;
        }
        out.push(Disk {
            mount,
            percent,
            total: blocks(1),
            free: blocks(3),
        });
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

/// True for filesystems the status bar should not spend room on.
fn is_pseudo(file_system: &str, mount: &str) -> bool {
    let name = file_system
        .rsplit('/')
        .next()
        .unwrap_or(file_system)
        .to_lowercase();
    if PSEUDO_FS.iter().any(|pseudo| name.starts_with(pseudo)) {
        return true;
    }
    mount == "/snap"
        || mount.starts_with("/snap/")
        || mount.starts_with("/proc")
        || mount.starts_with("/sys")
        || mount.starts_with("/dev")
        || mount.starts_with("/run")
}

/// The accounts behind `who`, de-duplicated.
fn parse_users(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        if let Some(user) = line.split_whitespace().next() {
            if !user.is_empty() && !out.iter().any(|known| known == user) {
                out.push(user.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
@host
demo-server
@os
PRETTY_NAME=\"Ubuntu 22.04.5 LTS\"
@uname
Linux 5.15.0-136-generic x86_64
@load
0.08 0.12 0.10 1/163 21187
@uptime
12960000.51 25920000.02
@cpu
cpu  1234 56 789 456789 12 0 34 0 0 0
@mem
MemTotal:        7256324 kB
MemFree:         1234560 kB
MemAvailable:    3145728 kB
Buffers:          234560 kB
Cached:          1572864 kB
SwapTotal:             0 kB
SwapFree:              0 kB
@net
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1111111    9999    0    0    0     0          0         0  1111111    9999    0    0    0     0       0          0
  eth0: 4725891   12345    0    0    0     0          0         0  3812345    9876    0    0    0     0       0          0
@df
Filesystem     1024-blocks      Used Available Capacity Mounted on
/dev/vda1        41152744  11048704  28206420  29% /
devtmpfs           362816         0    362816   0% /dev
tmpfs              362816      1024    361792   1% /dev/shm
/dev/vdb1        20511692    301234  19162340   2% /data
/dev/vda2          487652     20480    467172   5% /boot/efi
overlay         41152744  11048704  28206420  29% /var/lib/docker/overlay2/x/merged
@users
root     pts/0        2026-10-11 10:51 (198.51.100.7)
root     pts/1        2026-10-11 10:52 (198.51.100.7)
deploy   pts/2        2026-10-11 09:10 (192.0.2.5)
@end
";

    #[test]
    fn probe_sections_are_parsed() {
        let mut delta = Delta::default();
        let info = delta.apply(SAMPLE, 2.0);
        assert_eq!(info.hostname, "demo-server");
        assert_eq!(info.os, "Ubuntu 22.04.5 LTS");
        assert_eq!(info.kernel, "Linux 5.15.0-136-generic x86_64");
        assert_eq!(info.uptime, 12960000);
        assert_eq!(info.load[0], 0.08);
        assert_eq!(info.mem_total, 7256324 * 1024);
        assert_eq!(info.mem_used, (7256324 - 3145728) * 1024);
        assert_eq!(info.users, vec!["root".to_string(), "deploy".to_string()]);
        // Loopback is ignored, and the first sample has no rate yet.
        assert_eq!(info.rx, 0.0);
    }

    #[test]
    fn counters_become_rates_on_the_second_sample() {
        let mut delta = Delta::default();
        delta.apply(SAMPLE, 2.0);
        let second = SAMPLE.replace("  eth0: 4725891", "  eth0: 4735891");
        let info = delta.apply(&second, 2.0);
        assert!((info.rx - 5000.0).abs() < 1.0);
    }

    #[test]
    fn cpu_percent_comes_from_two_stat_samples() {
        let mut delta = Delta::default();
        delta.apply("@cpu\ncpu  100 0 100 800 0 0 0 0\n@end", 1.0);
        let info = delta.apply("@cpu\ncpu  150 0 150 900 0 0 0 0\n@end", 1.0);
        assert!((info.cpu - 50.0).abs() < 0.01, "got {}", info.cpu);
    }

    #[test]
    fn only_real_filesystems_are_kept() {
        let mut delta = Delta::default();
        let info = delta.apply(SAMPLE, 2.0);
        let mounts: Vec<&str> = info.disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/boot/efi", "/data"]);
        assert!((info.disks[0].percent - 29.0).abs() < 0.01);
    }

    #[test]
    fn long_device_names_wrap_the_mount_point() {
        let mut delta = Delta::default();
        let info = delta.apply(
            "@df\nFilesystem     1024-blocks  Used Available Capacity Mounted on\n/dev/mapper/vg-root   41152744 11048704  28206420  29%\n/\n@end",
            1.0,
        );
        assert_eq!(info.disks.len(), 1);
        assert_eq!(info.disks[0].mount, "/");
    }
}
