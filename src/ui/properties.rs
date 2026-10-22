//! The properties window: what one file is, and what it may be done to.
//!
//! The browser reports a row; this reads the rest from the server (or from
//! `std::fs` for the local pane) on a task, shows what it found, and can write
//! the permission bits back with a single SFTP `setstat`.

use std::path::{Path, PathBuf};

use egui::{Align2, RichText, Ui, Vec2};
use tokio::sync::mpsc;

use crate::ssh::client::Connection;
use crate::ssh::sftp::{self, Kind};
use crate::t;
use crate::ui::theme;
use crate::util::{human_size, human_time};

/// The nine editable `rwx` bits, owner first, exactly as they are listed.
const FLAGS: [(u32, char); 9] = [
    (0o400, 'r'),
    (0o200, 'w'),
    (0o100, 'x'),
    (0o040, 'r'),
    (0o020, 'w'),
    (0o010, 'x'),
    (0o004, 'r'),
    (0o002, 'w'),
    (0o001, 'x'),
];

/// The mode as `0644`: four digits, so the special bits stay visible.
pub fn format_mode(mode: u32) -> String {
    format!("{mode:04o}")
}

/// Read `644`, `0644` or `4755`. Anything else is not a mode.
pub fn parse_mode(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || text.len() > 4 {
        return None;
    }
    if !text.chars().all(|digit| ('0'..='7').contains(&digit)) {
        return None;
    }
    u32::from_str_radix(text, 8).ok().map(|mode| mode & 0o7777)
}

/// `rw-r--r--`, the preview shown beside the octal box.
pub fn symbol_mode(mode: u32) -> String {
    FLAGS
        .iter()
        .map(|(mask, sign)| if mode & mask != 0 { *sign } else { '-' })
        .collect()
}

/// The nine bits of a mode, as checkbox values.
fn flags_of(mode: u32) -> [bool; 9] {
    let mut out = [false; 9];
    for (index, (mask, _)) in FLAGS.iter().enumerate() {
        out[index] = mode & mask != 0;
    }
    out
}

/// Checkbox values back into a mode.
fn mode_of(flags: &[bool; 9]) -> u32 {
    FLAGS
        .iter()
        .zip(flags)
        .filter_map(|((mask, _), set)| set.then_some(*mask))
        .sum()
}
/// What one object is, as far as this window can tell.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub name: String,
    pub path: String,
    /// True when the object lives on this machine rather than on the server.
    pub local: bool,
    pub kind: Kind,
    pub size: u64,
    pub modified: i64,
    pub accessed: i64,
    pub created: i64,
    /// The permission bits. Meaningless for a local object on Windows.
    pub mode: u32,
    pub owner: String,
    pub group: String,
    /// The read-only attribute, which is what a local object has here.
    pub read_only: Option<bool>,
}

impl Info {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            Kind::Directory => t!("Folder"),
            Kind::Symlink => t!("Symbolic link"),
            Kind::File => t!("File"),
            Kind::Other => t!("Special file"),
        }
    }

    /// The folder the object sits in.
    fn parent(&self) -> String {
        if self.local {
            Path::new(&self.path)
                .parent()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            crate::util::remote_parent(&self.path)
        }
    }
}

/// What the window asked the browser to do.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Something on disk changed, so the listing should be read again.
    pub refresh: bool,
}

enum Loaded {
    /// The details of the object.
    Details(Info),
    /// The birth time, which SFTP cannot report, arrives on its own.
    Birth(i64),
    /// A permission change went through; here is the object afterwards.
    Applied(Info),
    /// Something failed; the text is ready for the window.
    Failed(String),
}

/// One open properties window.
pub struct Properties {
    title: String,
    remote: Option<String>,
    local: Option<PathBuf>,
    info: Info,
    loaded: bool,
    applying: bool,
    /// A permission change landed, so the browser should re-read the folder.
    changed: bool,
    octal: String,
    /// The setuid / setgid / sticky bits, kept while the boxes are edited.
    special: u32,
    flags: [bool; 9],
    read_only: Option<bool>,
    note: Option<String>,
    error: Option<String>,
    open: bool,
    rx: mpsc::UnboundedReceiver<Loaded>,
    tx: mpsc::UnboundedSender<Loaded>,
}
impl Properties {
    /// Open the window for a remote object and start reading it.
    pub fn remote(conn: &Connection, seed: Info) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let page = Self::new(seed.clone(), rx, tx.clone());
        let conn = conn.clone();
        let path = seed.path.clone();
        let birth_conn = conn.clone();
        let birth_path = path.clone();
        let details = tx.clone();
        tokio::spawn(async move {
            let result = async {
                let sftp = conn.sftp().await.map_err(|err| format!("{err:#}"))?;
                sftp::stat(&sftp, &path)
                    .await
                    .map_err(|err| format!("{err:#}"))
            }
            .await;
            match result {
                Ok(stat) => {
                    let mut info = seed;
                    apply_stat(&mut info, stat);
                    let _ = details.send(Loaded::Details(info));
                }
                Err(err) => {
                    let _ = details.send(Loaded::Failed(err));
                }
            }
        });
        // A birth time is not part of SFTP, so it is asked for on its own.
        let birth = tx;
        tokio::spawn(async move {
            let secs = sftp::created(&birth_conn, &birth_path).await;
            let _ = birth.send(Loaded::Birth(secs));
        });
        page
    }

    /// Open the window for an object on this machine.
    pub fn local(seed: Info) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let page = Self::new(seed.clone(), rx, tx.clone());
        let path = PathBuf::from(&seed.path);
        tokio::spawn(async move {
            let _ = tx.send(Loaded::Details(read_local(&seed, &path)));
        });
        page
    }

    fn new(
        seed: Info,
        rx: mpsc::UnboundedReceiver<Loaded>,
        tx: mpsc::UnboundedSender<Loaded>,
    ) -> Self {
        let local = seed.local;
        Self {
            title: t!("{0} - Properties", seed.name),
            remote: (!local).then(|| seed.path.clone()),
            local: local.then(|| PathBuf::from(&seed.path)),
            info: seed,
            loaded: false,
            applying: false,
            changed: false,
            octal: String::new(),
            special: 0,
            flags: [false; 9],
            read_only: None,
            note: None,
            error: None,
            open: true,
            rx,
            tx,
        }
    }

    /// True once the user has closed the window.
    pub fn finished(&self) -> bool {
        !self.open
    }

    fn poll(&mut self) {
        while let Ok(loaded) = self.rx.try_recv() {
            match loaded {
                Loaded::Details(info) => self.set_info(info),
                Loaded::Applied(info) => {
                    self.set_info(info);
                    self.applying = false;
                    self.changed = true;
                    self.note = Some(t!("Permissions updated").into());
                }
                Loaded::Birth(secs) => {
                    if secs > 0 {
                        self.info.created = secs;
                    }
                }
                Loaded::Failed(err) => {
                    self.applying = false;
                    self.error = Some(err);
                }
            }
        }
    }

    /// Show a freshly read object, keeping the birth time that arrived apart.
    fn set_info(&mut self, mut info: Info) {
        if info.created == 0 {
            info.created = self.info.created;
        }
        self.special = info.mode & !0o777;
        self.flags = flags_of(info.mode);
        self.octal = format_mode(info.mode);
        self.read_only = info.read_only;
        self.loaded = true;
        self.info = info;
    }

    /// The mode the boxes describe right now.
    fn mode(&self) -> u32 {
        parse_mode(&self.octal).unwrap_or_else(|| self.special | mode_of(&self.flags))
    }

    /// True when the boxes say something different from the object.
    fn edited(&self) -> bool {
        if !self.loaded {
            return false;
        }
        let flag = self.info.read_only.is_some() && self.read_only != self.info.read_only;
        self.mode() != self.info.mode || flag
    }

    /// Write the permissions back, then read the object again.
    fn apply(&mut self, conn: Option<&Connection>) {
        if self.applying {
            return;
        }
        let mode = self.mode();
        self.error = None;
        self.note = None;
        self.applying = true;
        if let Some(path) = self.local.clone() {
            let tx = self.tx.clone();
            let seed = self.info.clone();
            let read_only = self.read_only.unwrap_or(false);
            tokio::spawn(async move {
                let perms = permissions_for(&path, mode, read_only);
                let written = perms
                    .map(|perms| std::fs::set_permissions(&path, perms))
                    .unwrap_or_else(|| {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the object is gone",
                        ))
                    });
                match written {
                    Ok(()) => {
                        let _ = tx.send(Loaded::Applied(read_local(&seed, &path)));
                    }
                    Err(err) => {
                        let _ = tx.send(Loaded::Failed(err.to_string()));
                    }
                }
            });
            return;
        }
        let (Some(conn), Some(path)) = (conn, self.remote.clone()) else {
            self.applying = false;
            return;
        };
        let conn = conn.clone();
        let tx = self.tx.clone();
        let seed = self.info.clone();
        tokio::spawn(async move {
            let result = async {
                let sftp = conn.sftp().await.map_err(|err| format!("{err:#}"))?;
                sftp::set_mode(&sftp, &path, mode)
                    .await
                    .map_err(|err| format!("{err:#}"))?;
                sftp::stat(&sftp, &path)
                    .await
                    .map_err(|err| format!("{err:#}"))
            }
            .await;
            match result {
                Ok(stat) => {
                    let mut info = seed;
                    apply_stat(&mut info, stat);
                    let _ = tx.send(Loaded::Applied(info));
                }
                Err(err) => {
                    let _ = tx.send(Loaded::Failed(err));
                }
            }
        });
    }
    /// Draw the window. Returns what the browser should do, and whether the
    /// window has been closed.
    pub fn ui(&mut self, ui: &mut Ui, conn: Option<&Connection>) -> (Outcome, bool) {
        self.poll();
        let info = self.info.clone();
        let loaded = self.loaded;
        let applying = self.applying;
        // `Window::open` holds the flag for as long as the window lives, so the
        // window borrows locals rather than `self`, which the contents edit.
        let mut open = self.open;
        let mut closed = false;
        egui::Window::new(self.title.clone())
            .id(egui::Id::new("file-properties"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(400.0);
                if !loaded {
                    ui.label(RichText::new(t!("Loading")).size(11.5).weak());
                }
                details(ui, &info, loaded);
                theme::hairline(ui);
                self.permissions(ui, &info);
                if let Some(note) = &self.note {
                    let text = note.clone();
                    ui.label(RichText::new(text).size(11.5).color(theme::accent_text(ui)));
                }
                if let Some(error) = &self.error {
                    let text = error.clone();
                    ui.label(RichText::new(text).size(11.5).color(theme::tones(ui).2));
                }
                theme::hairline(ui);
                ui.horizontal_wrapped(|ui| {
                    let writable = self.remote.is_none() || conn.is_some();
                    let can_apply = self.loaded && !self.applying && self.edited() && writable;
                    ui.add_enabled_ui(can_apply, |ui| {
                        if theme::primary(ui, t!("Apply")).clicked() {
                            self.apply(conn);
                        }
                    });
                    if applying {
                        ui.label(RichText::new(t!("Loading")).size(11.5).weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t!("Close")).clicked() {
                            closed = true;
                        }
                    });
                });
            });
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            closed = true;
        }
        self.open = open && !closed;
        let refresh = self.changed;
        (Outcome { refresh }, !self.open)
    }

    /// The editable half of the window: the octal box and the nine bits.
    fn permissions(&mut self, ui: &mut Ui, info: &Info) {
        theme::section(ui, t!("Permissions"));
        let muted = theme::muted(ui);
        let bad = theme::tones(ui).2;
        // Windows has no permission bits of its own, only the read-only flag.
        let chmod = !info.local || cfg!(unix);
        if !chmod && info.read_only.is_none() {
            ui.label(
                RichText::new(t!("This file system has no permission bits."))
                    .size(11.5)
                    .color(muted),
            );
            return;
        }
        if chmod {
            ui.horizontal_wrapped(|ui| {
                ui.label(t!("Octal"));
                let box_response = ui.add_sized(
                    [90.0, theme::field_height(ui)],
                    theme::field(&mut self.octal),
                );
                if box_response.changed() {
                    if let Some(parsed) = parse_mode(&self.octal) {
                        self.special = parsed & !0o777;
                        self.flags = flags_of(parsed);
                    }
                }
                if parse_mode(&self.octal).is_none() {
                    ui.label(
                        RichText::new(t!("Use one to four octal digits."))
                            .size(11.0)
                            .color(bad),
                    );
                }
                ui.label(
                    RichText::new(symbol_mode(self.mode()))
                        .monospace()
                        .size(11.5)
                        .color(muted),
                );
            });
            egui::Grid::new("permissions-bits")
                .num_columns(4)
                .spacing([14.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.label("");
                    for head in ["r", "w", "x"] {
                        ui.strong(RichText::new(head).size(11.5).color(muted));
                    }
                    ui.end_row();
                    for (row, group) in [t!("Owner"), t!("Group"), t!("Others")]
                        .into_iter()
                        .enumerate()
                    {
                        ui.label(RichText::new(group).size(11.5));
                        for column in 0..3 {
                            let sign = FLAGS[row * 3 + column].1;
                            let bit = &mut self.flags[row * 3 + column];
                            if ui.checkbox(bit, sign.to_string()).changed() {
                                self.octal = format_mode(self.special | mode_of(&self.flags));
                            }
                        }
                        ui.end_row();
                    }
                });
            if self.loaded {
                ui.label(
                    RichText::new(t!(
                        "On the server it is {0} ({1})",
                        symbol_mode(info.mode),
                        format_mode(info.mode)
                    ))
                    .size(11.0)
                    .color(muted),
                );
            }
        }
        if let Some(read_only) = &mut self.read_only {
            ui.checkbox(read_only, t!("Read only"));
        }
        if !info.local {
            ui.label(
                RichText::new(t!("Permissions are set with a single SFTP request."))
                    .size(11.0)
                    .color(muted),
            );
        }
    }
}
/// The read-only half of the window: what the object is.
fn details(ui: &mut Ui, info: &Info, loaded: bool) {
    let size = if info.kind == Kind::Directory || !loaded {
        String::new()
    } else {
        format!("{} ({})", human_size(info.size), info.size)
    };
    egui::Grid::new("properties")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            line(ui, t!("Name"), info.name.clone());
            line(ui, t!("Type"), info.kind_label().to_string());
            line(ui, t!("Location"), info.parent());
            line(ui, t!("Size"), size);
            line(ui, t!("Modified"), stamp(info.modified));
            let created = stamp(info.created);
            ui.label(t!("Created"));
            ui.label(RichText::new(&created).size(11.5))
                .on_hover_text(t!("The server reports a birth time only when it can."));
            ui.end_row();
            line(ui, t!("Accessed"), stamp(info.accessed));
            if !info.owner.is_empty() {
                line(ui, t!("Owner"), info.owner.clone());
                line(ui, t!("Group"), info.group.clone());
            }
        });
}

/// One `label  value` line. The value truncates rather than widening the
/// window, and repeats itself in a tooltip so a long path can still be read.
fn line(ui: &mut Ui, label: &str, value: String) {
    let value = if value.trim().is_empty() {
        "--".to_string()
    } else {
        value
    };
    ui.label(RichText::new(label).size(11.5));
    let response = ui.add(egui::Label::new(RichText::new(&value).size(11.5)).truncate());
    response.on_hover_text(value);
    ui.end_row();
}

/// A timestamp, or `--` when nobody could say.
fn stamp(secs: i64) -> String {
    if secs > 0 {
        human_time(secs)
    } else {
        "--".to_string()
    }
}

/// Copy what the server reported onto the row the user clicked.
fn apply_stat(info: &mut Info, stat: sftp::Stat) {
    info.kind = stat.kind;
    info.size = stat.size;
    info.mode = stat.mode;
    info.modified = stat.modified;
    info.accessed = stat.accessed;
    info.owner = stat.owner;
    info.group = stat.group;
    info.read_only = None;
}

/// The same details, read from this machine.
fn read_local(seed: &Info, path: &Path) -> Info {
    let mut info = seed.clone();
    info.created = 0;
    // The link itself first, the way `ls -l` reads it; the target only
    // when the link goes nowhere.
    let meta = std::fs::symlink_metadata(path).or_else(|err| match err.kind() {
        std::io::ErrorKind::NotFound => std::fs::metadata(path),
        _ => Err(err),
    });
    let Ok(meta) = meta else {
        return info;
    };
    info.kind = if meta.is_dir() {
        Kind::Directory
    } else if meta.is_symlink() {
        Kind::Symlink
    } else {
        Kind::File
    };
    info.size = meta.len();
    info.modified = epoch(meta.modified());
    info.accessed = epoch(meta.accessed());
    info.created = epoch(meta.created());
    info.mode = local_mode(&meta);
    info.read_only = local_read_only(&meta);
    info
}

fn epoch(value: std::io::Result<std::time::SystemTime>) -> i64 {
    value
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn local_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn local_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// `None` means the `rwx` boxes are the way to edit this object.
#[cfg(unix)]
fn local_read_only(_meta: &std::fs::Metadata) -> Option<bool> {
    None
}

#[cfg(not(unix))]
fn local_read_only(meta: &std::fs::Metadata) -> Option<bool> {
    Some(meta.permissions().readonly())
}

/// The permissions to write back to a local object.
/// The permissions to write back to a local object, or `None` when there is
/// nothing there to write them to.
#[cfg(unix)]
fn permissions_for(_path: &Path, mode: u32, read_only: bool) -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    let wanted = if mode != 0 {
        mode
    } else if read_only {
        0o555
    } else {
        0o644
    };
    Some(std::fs::Permissions::from_mode(wanted))
}

#[cfg(not(unix))]
fn permissions_for(path: &Path, _mode: u32, read_only: bool) -> Option<std::fs::Permissions> {
    let mut perms = std::fs::metadata(path).ok()?.permissions();
    perms.set_readonly(read_only);
    Some(perms)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_through_text() {
        assert_eq!(parse_mode("644"), Some(0o644));
        assert_eq!(parse_mode("0644"), Some(0o644));
        assert_eq!(parse_mode(" 4755 "), Some(0o4755));
        assert_eq!(parse_mode("7777"), Some(0o7777));
        // Not a mode: empty, too long, or not octal.
        assert_eq!(parse_mode(""), None);
        assert_eq!(parse_mode("07777"), None);
        assert_eq!(parse_mode("888"), None);
        assert_eq!(parse_mode("rw-r--r--"), None);
        assert_eq!(format_mode(0o644), "0644");
        assert_eq!(format_mode(0o4755), "4755");
        assert_eq!(parse_mode(&format_mode(0o755)), Some(0o755));
    }

    #[test]
    fn checkboxes_mirror_the_mode() {
        let bits = flags_of(0o755);
        assert_eq!(
            bits,
            [true, true, true, true, false, true, true, false, true]
        );
        assert_eq!(mode_of(&flags_of(0o644)), 0o644);
        // The special bits are not the checkboxes' business.
        assert_eq!(mode_of(&flags_of(0o4755)), 0o755);
        assert_eq!(symbol_mode(0o644), "rw-r--r--");
        assert_eq!(symbol_mode(0), "---------");
    }
}
