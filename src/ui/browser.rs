//! The dual-pane file browser: remote SFTP on the left, local disk on the right.
//!
//! This is the browser that docks next to a terminal, plus a local pane so
//! uploads and downloads are one click (or one drag) away.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{Color32, CursorIcon, Pos2, Rect, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder, TableRow};
use tokio::sync::mpsc;

use crate::i18n::tr;
use crate::ssh::client::Connection;
use crate::ssh::sftp::{self, Entry, Kind};
use crate::ssh::transfer::{Direction, Job, Phase, Progress, Queue};
use crate::t;
use crate::ui::properties::{self, Info, Properties};
use crate::ui::{fill_rect, theme};
use crate::util::{human_rate, human_size, human_time, remote_join};

/// How long a finished transfer stays on the progress strip.
const KEEP_FINISHED: std::time::Duration = std::time::Duration::from_secs(2);
/// How many transfers the strip shows before it says "and N more".
const SHOWN_JOBS: usize = 2;

/// One row of the local pane.
#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// What the inline name dialog is asking for.
#[derive(Clone)]
enum Prompt {
    /// Create a folder, in whichever pane the menu was opened from.
    NewFolder { local: bool },
    /// Rename `path` (remote or local).
    Rename { path: String, local: bool },
}

/// A row the delete button was pressed on, kept until the user says so.
#[derive(Clone)]
enum PendingDelete {
    Remote(Entry),
    Local(LocalEntry),
}

enum Loaded {
    Remote {
        path: String,
        result: Result<Vec<Entry>, String>,
    },
    /// A delete, a rename or a folder created out of sight: read it again.
    Changed,
}

/// One transfer started from this browser, and the moment it stopped.
#[derive(Clone)]
struct Tracked {
    job: Job,
    finished: Option<Instant>,
}

/// Which pane a narrow dock shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Remote,
    Local,
}

/// State of one browser instance (one per tab or docked panel).
pub struct Browser {
    pub path: String,
    pub entries: Vec<Entry>,
    pub selected: Option<usize>,
    pub local_path: PathBuf,
    pub local: Vec<LocalEntry>,
    pub local_selected: Option<usize>,
    pub loading: bool,
    pub error: Option<String>,
    pub filter: String,
    pub path_buffer: String,
    /// Local text of the local pane path box.
    pub local_buffer: String,
    /// Remote folders visited so far, and the position we are standing on.
    history: Vec<String>,
    history_pos: usize,
    /// Local folders visited so far.
    local_history: Vec<PathBuf>,
    local_history_pos: usize,
    /// The first remote listing has been requested.
    booted: bool,
    /// The local pane has been read once.
    local_booted: bool,
    /// Focus state of the two path boxes on the previous frame. While one of
    /// them holds the caret its text belongs to the user, not to the pane.
    path_focused: bool,
    local_focused: bool,
    prompt: Option<(Prompt, String)>,
    pane: Pane,
    /// Transfers started from here: the pane shows their progress, then
    /// re-reads the folder they changed.
    jobs: Vec<Tracked>,
    /// The properties window, while one is open.
    props: Option<Properties>,
    /// A delete waiting for its confirmation.
    pending_delete: Option<PendingDelete>,
    /// Something changed out of sight; each pane asks once.
    dirty_remote: bool,
    dirty_local: bool,
    rx: mpsc::UnboundedReceiver<Loaded>,
    tx: mpsc::UnboundedSender<Loaded>,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            path: "/".into(),
            entries: Vec::new(),
            selected: None,
            local_path: download_dir(),
            local: Vec::new(),
            local_selected: None,
            loading: false,
            error: None,
            filter: String::new(),
            path_buffer: String::new(),
            local_buffer: String::new(),
            history: Vec::new(),
            history_pos: 0,
            local_history: Vec::new(),
            local_history_pos: 0,
            booted: false,
            local_booted: false,
            path_focused: false,
            local_focused: false,
            prompt: None,
            pane: Pane::Remote,
            jobs: Vec::new(),
            props: None,
            pending_delete: None,
            dirty_remote: false,
            dirty_local: false,
            rx,
            tx,
        }
    }

    /// Ask the server for `path`; results arrive on a later frame.
    fn load(&mut self, conn: &Connection, path: impl Into<String>) {
        let path = path.into();
        self.path_buffer = path.clone();
        self.loading = true;
        self.error = None;
        self.selected = None;
        let tx = self.tx.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            let result = async {
                let sftp = conn.sftp().await.map_err(|e| format!("{e:#}"))?;
                let resolved = sftp::canonicalize(&sftp, &path)
                    .await
                    .unwrap_or(path.clone());
                sftp::list(&sftp, &resolved)
                    .await
                    .map(|entries| (resolved, entries))
                    .map_err(|e| format!("{e:#}"))
            }
            .await;
            let _ = tx.send(match result {
                Ok((resolved, entries)) => Loaded::Remote {
                    path: resolved,
                    result: Ok(entries),
                },
                Err(err) => Loaded::Remote {
                    path,
                    result: Err(err),
                },
            });
        });
    }

    /// Go to `path`, remembering where we came from so Back works.
    pub fn navigate(&mut self, conn: &Connection, path: impl Into<String>) {
        let path = path.into();
        if self.history.is_empty() {
            self.history.push(self.path.clone());
            self.history_pos = 0;
        }
        if self.history.get(self.history_pos).map(String::as_str) != Some(path.as_str()) {
            self.history.truncate(self.history_pos + 1);
            self.history.push(path.clone());
            self.history_pos = self.history.len() - 1;
        }
        self.load(conn, path);
    }

    /// Walk the remote history; `-1` is Back, `1` is Forward.
    pub fn step_history(&mut self, conn: &Connection, delta: isize) {
        let Some(next) = self.history_pos.checked_add_signed(delta) else {
            return;
        };
        if next >= self.history.len() {
            return;
        }
        self.history_pos = next;
        let path = self.history[next].clone();
        self.load(conn, path);
    }

    /// Open a local folder, remembering where we came from so Back works.
    fn open_local(&mut self, dir: PathBuf) {
        if self.local_history.is_empty() {
            self.local_history.push(self.local_path.clone());
            self.local_history_pos = 0;
        }
        if self
            .local_history
            .get(self.local_history_pos)
            .map(PathBuf::as_path)
            != Some(dir.as_path())
        {
            self.local_history.truncate(self.local_history_pos + 1);
            self.local_history.push(dir.clone());
            self.local_history_pos = self.local_history.len() - 1;
        }
        self.local_path = dir;
        self.refresh_local();
    }

    /// Walk the local history; `-1` is Back, `1` is Forward.
    fn step_local(&mut self, delta: isize) {
        let Some(next) = self.local_history_pos.checked_add_signed(delta) else {
            return;
        };
        if next >= self.local_history.len() {
            return;
        }
        self.local_history_pos = next;
        self.local_path = self.local_history[next].clone();
        self.refresh_local();
    }

    /// Re-read the local pane (cheap, done inline).
    pub fn refresh_local(&mut self) {
        self.local = read_dir(&self.local_path);
        self.local_selected = None;
        self.local_booted = true;
        self.local_buffer = self.local_path.to_string_lossy().into_owned();
    }

    fn poll(&mut self) {
        while let Ok(loaded) = self.rx.try_recv() {
            match loaded {
                Loaded::Remote { path, result } => {
                    self.loading = false;
                    match result {
                        Ok(entries) => {
                            self.path = path.clone();
                            self.path_buffer = path;
                            self.entries = entries;
                            self.error = None;
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
                Loaded::Changed => self.dirty_remote = true,
            }
        }
    }

    /// Retire the transfers that are over, and mark whatever they changed.
    fn poll_jobs(&mut self) {
        for tracked in &mut self.jobs {
            if tracked.finished.is_some() || !tracked.job.is_finished() {
                continue;
            }
            let state = tracked.job.snapshot();
            tracked.finished = Some(Instant::now());
            if tracked.job.direction == Direction::Upload {
                self.dirty_remote = true;
            } else {
                self.dirty_local = true;
            }
            if let Phase::Failed(err) = state.phase {
                self.error = Some(if tracked.job.direction == Direction::Upload {
                    t!("upload failed: {0}", err)
                } else {
                    t!("download failed: {0}", err)
                });
            }
        }
        self.jobs.retain(|tracked| {
            tracked
                .finished
                .is_none_or(|at| at.elapsed() < KEEP_FINISHED)
        });
    }

    /// Watch one transfer: the strip shows it, and the pane re-reads itself.
    fn track(&mut self, job: Job) {
        self.jobs.push(Tracked {
            job,
            finished: None,
        });
    }

    /// Re-read both panes - the toolbar button and the blank-area menu share it.
    fn refresh(&mut self, conn: Option<&Connection>) {
        if let Some(conn) = conn {
            self.load(conn, self.path.clone());
        }
        self.refresh_local();
    }

    /// Download the selected remote row into the folder the local pane shows.
    fn download_here(&mut self, conn: Option<&Connection>, queue: &Queue) {
        let Some(entry) = self
            .selected
            .and_then(|index| self.entries.get(index).cloned())
        else {
            return;
        };
        let Some(conn) = conn else {
            return;
        };
        let into = self.local_path.clone();
        let job = queue.download(
            conn.clone(),
            entry.path,
            into,
            entry.kind == Kind::Directory,
        );
        self.track(job);
    }

    /// The blank-area "Upload file": pick local files, then send them here.
    fn pick_upload(&mut self, conn: Option<&Connection>, queue: &Queue) {
        let Some(conn) = conn else {
            return;
        };
        let Some(paths) = rfd::FileDialog::new()
            .set_title(t!("Select files to upload"))
            .set_directory(self.local_path.clone())
            .pick_files()
        else {
            return;
        };
        let into = self.path.clone();
        for path in paths {
            let is_dir = path.is_dir();
            let job = queue.upload(conn.clone(), path, into.clone(), is_dir);
            self.track(job);
        }
    }

    /// The context-menu Download: ask where the copy should go first.
    ///
    /// A folder is chosen as a folder so the tree keeps its shape; a file gets
    /// a save dialog, so its name can change on the way in.
    fn download_somewhere(&mut self, conn: Option<&Connection>, queue: &Queue, entry: &Entry) {
        let Some(conn) = conn else {
            return;
        };
        let dialog = rfd::FileDialog::new()
            .set_title(t!("Choose where to save {0}", entry.name.clone()))
            .set_directory(self.local_path.clone());
        let target = if entry.kind == Kind::Directory {
            dialog.pick_folder().map(|dir| (dir, true))
        } else {
            dialog
                .set_file_name(entry.name.clone())
                .save_file()
                .map(|file| (file, false))
        };
        let Some((target, folder)) = target else {
            return;
        };
        let path = entry.path.clone();
        let job = if folder {
            queue.download(conn.clone(), path, target, true)
        } else {
            queue.download_to_file(conn.clone(), path, target)
        };
        self.track(job);
    }

    /// A slim progress row for every transfer started from this browser.
    fn transfer_bar(&mut self, ui: &mut Ui) {
        if self.jobs.is_empty() {
            return;
        }
        let (good, _, bad, _) = theme::tones(ui);
        let (accent, muted) = (theme::accent(ui), theme::muted(ui));
        let live: Vec<(Job, Progress)> = self
            .jobs
            .iter()
            .map(|tracked| (tracked.job.clone(), tracked.job.snapshot()))
            .collect();
        let rest = live.len().saturating_sub(SHOWN_JOBS);
        for (job, state) in live.iter().take(SHOWN_JOBS) {
            let name = if state.current.is_empty() {
                job.source.clone()
            } else {
                state.current.clone()
            };
            let running = !state.phase.is_finished();
            let glyph = match state.phase {
                Phase::Done => "\u{2713}",
                Phase::Cancelled => "\u{2212}",
                Phase::Failed(_) => "\u{26a0}",
                Phase::Queued => "\u{23f8}",
                Phase::Running => "\u{2026}",
            };
            let text = t!(
                "{0} - {1} / {2} - {3}",
                state.phase.label(),
                human_size(state.done),
                human_size(state.total),
                human_rate(state.rate())
            );
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(job.direction.arrow())
                        .size(11.5)
                        .color(accent),
                );
                ui.add(egui::Label::new(RichText::new(name).size(11.5).color(muted)).truncate());
                let right =
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if running {
                            theme::ghost(ui, "\u{2715}")
                                .on_hover_text(t!("Cancel this transfer"))
                                .clicked()
                        } else {
                            ui.label(RichText::new(glyph).size(11.5).color(muted));
                            false
                        }
                    });
                cancel = right.inner;
            });
            if cancel {
                job.cancel();
            }
            // The label rides on top of the bar rather than inside it, so the
            // numbers stay readable while the fill runs underneath them.
            let tone = match state.phase {
                Phase::Done => good,
                Phase::Failed(_) => bad,
                Phase::Cancelled => muted,
                _ => accent,
            };
            theme::meter(
                ui,
                state.percent() / 100.0,
                ui.available_width(),
                text,
                tone,
            );
        }
        if rest > 0 {
            ui.label(RichText::new(t!("+{0} more", rest)).size(11.0).color(muted));
        }
    }

    fn matches_filter(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(&self.filter.to_lowercase())
    }

    /// Draw the browser. `conn` is `None` for a purely local browser.
    pub fn ui(&mut self, ui: &mut Ui, conn: Option<&Connection>, queue: &Queue) {
        self.poll();
        self.poll_jobs();
        if self.dirty_local {
            self.dirty_local = false;
            self.refresh_local();
        }
        if self.dirty_remote {
            self.dirty_remote = false;
            if let Some(conn) = conn {
                self.load(conn, self.path.clone());
            }
        }
        // A path box mirrors its pane, but never while the user is editing it.
        if !self.path_focused {
            self.path_buffer = self.path.clone();
        }
        if !self.local_focused {
            self.local_buffer = self.local_path.to_string_lossy().into_owned();
        }
        if !self.local_booted {
            self.refresh_local();
        }
        if !self.booted {
            if let Some(conn) = conn {
                self.booted = true;
                self.history = vec![self.path.clone()];
                self.history_pos = 0;
                self.load(conn, self.path.clone());
            }
        }

        // A docked browser is narrow: give one pane the whole width.
        let wide = ui.available_width() >= 760.0;
        self.toolbar(ui, conn, wide);
        if let Some(error) = &self.error {
            let text = error.clone();
            let (_, _, bad, _) = theme::tones(ui);
            ui.label(RichText::new(text).size(11.5).color(bad));
        }
        // Drawn before the panes so the lists get whatever height is left.
        self.transfer_bar(ui);

        let mut remote = RemoteCmd::default();
        let mut local = LocalCmd::default();

        // Drawn first so the file list gets whatever height is left over.
        let bar = if wide {
            PaneBar::default()
        } else {
            self.pane_bar(ui)
        };

        if wide {
            let middle = 46.0;
            // Measured here, once the toolbar and pane bar have taken their share.
            let pane = ((ui.available_width() - middle) / 2.0).max(140.0);
            let height = ui.available_height();
            let top = ui.cursor().min.y;
            let left = ui.cursor().min.x;
            let down = egui::Layout::top_down(egui::Align::Min);
            // Each pane gets a box of its own, stacked top-down: a plain
            // `allocate_ui` would hand it this row's left-to-right layout, and
            // the table header would end up sitting beside its own list.
            remote = fill_rect(
                ui,
                Rect::from_min_size(Pos2::new(left, top), Vec2::new(pane, height)),
                down,
                |ui| self.remote_table(ui, conn.is_some()),
            );
            fill_rect(
                ui,
                Rect::from_min_size(Pos2::new(left + pane, top), Vec2::new(middle, height)),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space(24.0);
                    if theme::ghost(ui, t!("\u{2192}"))
                        .on_hover_text(t!("Upload the selected local item"))
                        .clicked()
                    {
                        local.upload = true;
                    }
                    if theme::ghost(ui, t!("\u{2190}"))
                        .on_hover_text(t!("Download the selected remote item"))
                        .clicked()
                    {
                        remote.download = true;
                    }
                    ui.add_space(8.0);
                    if theme::ghost(ui, t!("\u{1f5d}"))
                        .on_hover_text(t!("Delete"))
                        .clicked()
                    {
                        remote.remove = true;
                    }
                    if theme::ghost(ui, t!("\u{270e}"))
                        .on_hover_text(t!("Rename"))
                        .clicked()
                    {
                        remote.rename = true;
                    }
                },
            );
            local = fill_rect(
                ui,
                Rect::from_min_size(
                    Pos2::new(left + pane + middle, top),
                    Vec2::new(pane, height),
                ),
                down,
                |ui| self.local_table(ui),
            );
        } else if self.pane == Pane::Remote {
            remote = self.remote_table(ui, conn.is_some());
        } else {
            local = self.local_table(ui);
        }

        // Merge what the narrow-dock buttons asked for.
        if bar.download {
            remote.download = true;
        }
        if bar.upload {
            local.upload = true;
        }
        if bar.remove {
            if self.pane == Pane::Remote {
                remote.remove = true;
            } else {
                local.remove = true;
            }
        }
        if bar.rename {
            if self.pane == Pane::Remote {
                remote.rename = true;
            } else {
                local.rename = true;
            }
        }

        // Act on what the panes asked for.
        if let Some(path) = remote.navigate {
            if let Some(conn) = conn {
                self.navigate(conn, path);
            }
        }
        if remote.refresh {
            self.refresh(conn);
        }
        if remote.new_folder {
            self.prompt = Some((Prompt::NewFolder { local: false }, t!("New folder").into()));
        }
        if remote.upload {
            self.pick_upload(conn, queue);
        }
        if let Some(entry) = remote.save_as {
            self.download_somewhere(conn, queue, &entry);
        }
        if let Some(entry) = remote.properties {
            if let Some(conn) = conn {
                self.props = Some(Properties::remote(conn, remote_info(&entry)));
            }
        }
        // A double click or the arrow button means "straight into the other
        // pane". The context menu asks where to put it instead.
        if remote.download {
            self.download_here(conn, queue);
        }
        if let Some(entry) = remote.download_entry {
            let into = self.local_path.clone();
            if let Some(conn) = conn {
                let job = queue.download(
                    conn.clone(),
                    entry.path,
                    into,
                    entry.kind == Kind::Directory,
                );
                self.track(job);
            }
        }
        if remote.remove {
            if let Some(entry) = self.selected.and_then(|i| self.entries.get(i).cloned()) {
                self.pending_delete = Some(PendingDelete::Remote(entry));
            }
        }
        if remote.rename {
            if let Some(entry) = self.selected.and_then(|i| self.entries.get(i).cloned()) {
                self.prompt = Some((
                    Prompt::Rename {
                        path: entry.path.clone(),
                        local: false,
                    },
                    entry.name,
                ));
            }
        }

        if let Some(dir) = local.open {
            self.open_local(dir);
        }
        if local.refresh {
            self.refresh(conn);
        }
        if local.new_folder {
            self.prompt = Some((Prompt::NewFolder { local: true }, t!("New folder").into()));
        }
        if let Some(entry) = local.properties {
            self.props = Some(Properties::local(local_info(&entry)));
        }
        if local.upload {
            if let (Some(conn), Some(entry)) = (
                conn,
                self.local_selected.and_then(|i| self.local.get(i).cloned()),
            ) {
                let into = self.path.clone();
                let job = queue.upload(conn.clone(), entry.path, into, entry.is_dir);
                self.track(job);
            }
        }
        if local.rename {
            if let Some(entry) = self.local_selected.and_then(|i| self.local.get(i).cloned()) {
                self.prompt = Some((
                    Prompt::Rename {
                        path: entry.path.to_string_lossy().into_owned(),
                        local: true,
                    },
                    entry.name,
                ));
            }
        }
        if local.remove {
            if let Some(entry) = self.local_selected.and_then(|i| self.local.get(i).cloned()) {
                self.pending_delete = Some(PendingDelete::Local(entry));
            }
        }

        // Files dropped from Explorer go to the remote side.
        let dropped: Vec<PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            if let Some(conn) = conn {
                let into = self.path.clone();
                for path in dropped {
                    let is_dir = path.is_dir();
                    let job = queue.upload(conn.clone(), path, into.clone(), is_dir);
                    self.track(job);
                }
            }
        }

        self.prompt_dialog(ui, conn);
        self.confirm_delete_ui(ui, conn);
        self.properties_dialog(ui, conn);
    }

    /// The properties window for one row, and the refresh it may have earned.
    fn properties_dialog(&mut self, ui: &mut Ui, conn: Option<&Connection>) {
        let mut outcome = properties::Outcome::default();
        let mut closed = false;
        if let Some(props) = &mut self.props {
            let (asked, done) = props.ui(ui, conn);
            outcome = asked;
            closed = done;
        }
        if outcome.refresh {
            self.dirty_remote = true;
            self.dirty_local = true;
        }
        if closed {
            self.props = None;
        }
    }

    /// Pane switcher and transfer buttons for the narrow right-hand dock.
    fn pane_bar(&mut self, ui: &mut Ui) -> PaneBar {
        let mut bar = PaneBar::default();
        theme::hairline(ui);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            // A segmented switch: the two panes are one control, not two buttons.
            let group = egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .stroke((1.0, theme::border(ui)))
                .corner_radius(8_u8)
                .inner_margin(2);
            group.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (label, pane) in [(t!("Remote"), Pane::Remote), (t!("Local"), Pane::Local)]
                    {
                        if ui.selectable_label(self.pane == pane, label).clicked() {
                            self.pane = pane;
                        }
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost(ui, t!("\u{1f5d}"))
                    .on_hover_text(t!("Delete the selected item"))
                    .clicked()
                {
                    bar.remove = true;
                }
                if theme::ghost(ui, t!("\u{270e}"))
                    .on_hover_text(t!("Rename the selected item"))
                    .clicked()
                {
                    bar.rename = true;
                }
                if theme::ghost(ui, t!("\u{2193}"))
                    .on_hover_text(t!("Download the selected remote item"))
                    .clicked()
                {
                    bar.download = true;
                }
                if theme::ghost(ui, t!("\u{2191}"))
                    .on_hover_text(t!("Upload the selected local item"))
                    .clicked()
                {
                    bar.upload = true;
                }
            });
        });
        ui.add_space(3.0);
        bar
    }
    /// Navigation buttons and an editable path box for each visible pane.
    fn toolbar(&mut self, ui: &mut Ui, conn: Option<&Connection>, wide: bool) {
        ui.horizontal_wrapped(|ui| {
            if theme::ghost(ui, t!("\u{21bb}"))
                .on_hover_text(t!("Refresh"))
                .clicked()
            {
                if let Some(conn) = conn {
                    self.load(conn, self.path.clone());
                }
                self.refresh_local();
            }
            if theme::ghost(ui, t!("\u{1f4c1}+"))
                .on_hover_text(t!("New remote folder"))
                .clicked()
            {
                self.prompt = Some((Prompt::NewFolder { local: false }, t!("New folder").into()));
            }
            if theme::ghost(ui, t!("\u{1f4c2}"))
                .on_hover_text(t!("Open the local folder in Explorer"))
                .clicked()
            {
                open_in_explorer(&self.local_path);
            }
        });
        ui.add_space(2.0);

        let remote = path_row(
            ui,
            "/remote/path",
            &mut self.path_buffer,
            &mut self.path_focused,
            self.history_pos > 0,
            self.history_pos + 1 < self.history.len(),
        );
        match remote {
            RowAction::Back(delta) => {
                if let Some(conn) = conn {
                    self.step_history(conn, delta);
                }
            }
            RowAction::Up => {
                if let Some(conn) = conn {
                    let parent = crate::util::remote_parent(&self.path);
                    self.navigate(conn, parent);
                }
            }
            RowAction::Go => {
                let target = self.path_buffer.trim().to_string();
                match conn {
                    Some(conn) if !target.is_empty() => self.navigate(conn, target),
                    _ => self.path_buffer = self.path.clone(),
                }
            }
            RowAction::None => {}
        }

        if wide || self.pane == Pane::Local {
            let local = path_row(
                ui,
                "C:\\path\\to\\folder",
                &mut self.local_buffer,
                &mut self.local_focused,
                self.local_history_pos > 0,
                self.local_history_pos + 1 < self.local_history.len(),
            );
            match local {
                RowAction::Back(delta) => self.step_local(delta),
                RowAction::Up => {
                    if let Some(parent) = self.local_path.parent().map(PathBuf::from) {
                        self.open_local(parent);
                    }
                }
                RowAction::Go => {
                    let raw = self.local_buffer.trim().to_string();
                    let candidate = PathBuf::from(&raw);
                    if candidate.is_dir() {
                        self.open_local(candidate);
                    } else if raw.is_empty() {
                        self.local_buffer = self.local_path.to_string_lossy().into_owned();
                    } else {
                        self.error = Some(t!("not a folder: {0}", raw));
                    }
                }
                RowAction::None => {}
            }
        }

        ui.add_sized(
            [ui.available_width(), 24.0],
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text(t!("\u{2315} Filter by name"))
                .vertical_align(egui::Align::Center),
        );
    }

    fn remote_table(&mut self, ui: &mut Ui, connected: bool) -> RemoteCmd {
        let rows: Vec<usize> = (0..self.entries.len())
            .filter(|i| self.matches_filter(&self.entries[*i].name))
            .collect();
        let mut cmd = RemoteCmd::default();
        // Walking out of a folder belongs in the list itself, not only on the
        // toolbar above it. The root has no parent to offer.
        let parent = crate::util::remote_parent(&self.path);
        let can_up = connected && parent != self.path;

        let muted = theme::muted(ui);
        let head = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).size(11.5).strong().color(muted));
        };
        let (columns, show_time) = fit_columns(ui.available_width());
        let mut table = TableBuilder::new(ui)
            .striped(true)
            // Shrink to the dock rather than asking for the full column width:
            // a side panel grows to whatever its content wants, and this table
            // used to inflate the dock a few pixels every single frame.
            .auto_shrink([false, true]);
        for column in columns {
            table = table.column(column);
        }
        table
            .header(24.0, |mut row| {
                row.col(|ui| {
                    head(ui, t!("Name"));
                });
                row.col(|ui| {
                    head(ui, t!("Size"));
                });
                if show_time {
                    row.col(|ui| {
                        head(ui, t!("Modified"));
                    });
                }
            })
            .body(|mut body| {
                if can_up {
                    body.row(23.0, |mut row| {
                        if parent_row(&mut row, "remote-parent", show_time, muted) {
                            cmd.navigate = Some(parent.clone());
                        }
                    });
                }
                for index in rows.iter().copied() {
                    body.row(23.0, |mut row| {
                        let entry = self.entries[index].clone();
                        let selected = self.selected == Some(index);
                        let glyph = match entry.kind {
                            Kind::Directory => "\u{1f4c1}",
                            Kind::Symlink => "\u{1f517}",
                            Kind::File => "\u{1f4c4}",
                            Kind::Other => "\u{2753}",
                        };
                        row.set_selected(selected);
                        // Every cell of the row answers to the same pointer, so
                        // that the empty part of a row still means that row.
                        let mut cells = Vec::new();
                        row.col(|ui| {
                            cells.push(cell(ui, "remote-name", index, |ui| {
                                ui.add(
                                    egui::Label::new(format!("{glyph} {}", entry.name))
                                        .truncate()
                                        .selectable(false),
                                );
                            }));
                        });
                        row.col(|ui| {
                            cells.push(cell(ui, "remote-size", index, |ui| {
                                let text = if entry.kind == Kind::Directory {
                                    "--".to_string()
                                } else {
                                    human_size(entry.size)
                                };
                                ui.label(RichText::new(text).size(11.5).color(muted));
                            }));
                        });
                        if show_time {
                            row.col(|ui| {
                                cells.push(cell(ui, "remote-time", index, |ui| {
                                    ui.label(
                                        RichText::new(human_time(entry.mtime))
                                            .size(11.5)
                                            .color(muted),
                                    );
                                }));
                            });
                        }
                        for response in cells {
                            let response = response.on_hover_cursor(CursorIcon::PointingHand);
                            if response.clicked() {
                                self.selected = Some(index);
                            }
                            if response.double_clicked() {
                                match entry.kind {
                                    Kind::Directory => cmd.navigate = Some(entry.path.clone()),
                                    _ => cmd.download_entry = Some(entry.clone()),
                                }
                            }
                            response.context_menu(|ui| {
                                if entry.is_dir() && ui.button(t!("Open")).clicked() {
                                    cmd.navigate = Some(entry.path.clone());
                                    self.selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Download")).clicked() {
                                    cmd.save_as = Some(entry.clone());
                                    self.selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Rename")).clicked() {
                                    cmd.rename = true;
                                    self.selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Delete")).clicked() {
                                    cmd.remove = true;
                                    self.selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Properties")).clicked() {
                                    cmd.properties = Some(entry.clone());
                                    self.selected = Some(index);
                                    ui.close();
                                }
                            });
                        }
                    });
                }
            });

        // Below the last row the pane belongs to the folder itself, so a
        // right-click there is about the folder rather than about a file.
        let blank = ui.available_rect_before_wrap();
        let blank = ui.interact(blank, ui.id().with("remote-blank"), Sense::click());
        blank.context_menu(|ui| {
            if ui.button(t!("Refresh")).clicked() {
                cmd.refresh = true;
                ui.close();
            }
            if connected && ui.button(t!("New folder")).clicked() {
                cmd.new_folder = true;
                ui.close();
            }
            if connected && ui.button(t!("Upload file")).clicked() {
                cmd.upload = true;
                ui.close();
            }
        });

        cmd
    }

    fn local_table(&mut self, ui: &mut Ui) -> LocalCmd {
        let rows: Vec<usize> = (0..self.local.len())
            .filter(|i| self.matches_filter(&self.local[*i].name))
            .collect();
        let mut cmd = LocalCmd::default();
        let parent = self.local_path.parent().map(PathBuf::from);

        let muted = theme::muted(ui);
        let head = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).size(11.5).strong().color(muted));
        };
        let (columns, show_time) = fit_columns(ui.available_width());
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .auto_shrink([false, true]);
        for column in columns {
            table = table.column(column);
        }
        table
            .header(24.0, |mut row| {
                row.col(|ui| {
                    head(ui, t!("Name"));
                });
                row.col(|ui| {
                    head(ui, t!("Size"));
                });
                if show_time {
                    row.col(|ui| {
                        head(ui, t!("Modified"));
                    });
                }
            })
            .body(|mut body| {
                if let Some(dir) = &parent {
                    body.row(23.0, |mut row| {
                        if parent_row(&mut row, "local-parent", show_time, muted) {
                            cmd.open = Some(dir.clone());
                        }
                    });
                }
                for index in rows.iter().copied() {
                    body.row(23.0, |mut row| {
                        let entry = self.local[index].clone();
                        let selected = self.local_selected == Some(index);
                        let glyph = if entry.is_dir {
                            "\u{1f4c1}"
                        } else {
                            "\u{1f4c4}"
                        };
                        row.set_selected(selected);
                        let mut cells = Vec::new();
                        row.col(|ui| {
                            cells.push(cell(ui, "local-name", index, |ui| {
                                ui.add(
                                    egui::Label::new(format!("{glyph} {}", entry.name))
                                        .truncate()
                                        .selectable(false),
                                );
                            }));
                        });
                        row.col(|ui| {
                            cells.push(cell(ui, "local-size", index, |ui| {
                                let text = if entry.is_dir {
                                    "--".to_string()
                                } else {
                                    human_size(entry.size)
                                };
                                ui.label(RichText::new(text).size(11.5).color(muted));
                            }));
                        });
                        if show_time {
                            row.col(|ui| {
                                cells.push(cell(ui, "local-time", index, |ui| {
                                    ui.label(
                                        RichText::new(human_time(entry.mtime))
                                            .size(11.5)
                                            .color(muted),
                                    );
                                }));
                            });
                        }
                        for response in cells {
                            let response = response.on_hover_cursor(CursorIcon::PointingHand);
                            if response.clicked() {
                                self.local_selected = Some(index);
                            }
                            if response.double_clicked() && entry.is_dir {
                                cmd.open = Some(entry.path.clone());
                            }
                            response.context_menu(|ui| {
                                if ui.button(t!("Upload")).clicked() {
                                    cmd.upload = true;
                                    self.local_selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Rename")).clicked() {
                                    cmd.rename = true;
                                    self.local_selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Delete")).clicked() {
                                    cmd.remove = true;
                                    self.local_selected = Some(index);
                                    ui.close();
                                }
                                if ui.button(t!("Properties")).clicked() {
                                    cmd.properties = Some(entry.clone());
                                    self.local_selected = Some(index);
                                    ui.close();
                                }
                            });
                        }
                    });
                }
            });

        // The space under the list is the folder's own.
        let blank = ui.available_rect_before_wrap();
        let blank = ui.interact(blank, ui.id().with("local-blank"), Sense::click());
        blank.context_menu(|ui| {
            if ui.button(t!("Refresh")).clicked() {
                cmd.refresh = true;
                ui.close();
            }
            if ui.button(t!("New folder")).clicked() {
                cmd.new_folder = true;
                ui.close();
            }
        });

        cmd
    }

    /// The "are you sure" window behind every delete button.
    ///
    /// Deleting is the one thing in this pane nobody can undo, and a stray
    /// click on the trash icon used to do it to a whole folder. The window
    /// names the row and says how far the damage reaches.
    fn confirm_delete_ui(&mut self, ui: &mut Ui, conn: Option<&Connection>) {
        let Some(doomed) = self.pending_delete.clone() else {
            return;
        };
        let (name, path, is_dir) = match &doomed {
            PendingDelete::Remote(entry) => {
                (entry.name.clone(), entry.path.clone(), entry.is_dir())
            }
            PendingDelete::Local(entry) => (
                entry.name.clone(),
                entry.path.to_string_lossy().into_owned(),
                entry.is_dir,
            ),
        };
        let (_, _, bad, _) = theme::tones(ui);
        let mut decision: Option<bool> = None;
        let mut open = true;
        egui::Window::new(t!("Delete this item?"))
            .id(egui::Id::new("browser-delete"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(340.0);
                ui.label(RichText::new(name.as_str()).size(12.5).strong());
                ui.label(
                    RichText::new(path.as_str())
                        .size(11.0)
                        .monospace()
                        .color(theme::muted(ui)),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(if is_dir {
                        t!(
                            "The folder and everything inside it will be deleted. This cannot be undone."
                        )
                    } else {
                        t!("The file will be deleted. This cannot be undone.")
                    })
                    .size(11.5)
                    .color(bad),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::danger(ui, t!("Delete")).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(t!("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }

        if let Some(yes) = decision {
            self.pending_delete = None;
            if yes {
                self.delete(conn, &doomed);
            }
        } else if !open {
            self.pending_delete = None;
        }
    }

    /// Carry out a confirmed delete. The remote side answers on a later frame.
    fn delete(&mut self, conn: Option<&Connection>, doomed: &PendingDelete) {
        match doomed {
            PendingDelete::Local(entry) => {
                remove_local(&entry.path);
                self.refresh_local();
            }
            PendingDelete::Remote(entry) => {
                let Some(conn) = conn else {
                    return;
                };
                let conn = conn.clone();
                let entry = entry.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    match conn.sftp().await {
                        Ok(sftp) => {
                            if let Err(err) = sftp::remove_all(&sftp, &entry).await {
                                log::warn!("delete {}: {err}", entry.path);
                            } else {
                                let _ = tx.send(Loaded::Changed);
                            }
                        }
                        Err(err) => log::warn!("delete failed: {err}"),
                    }
                });
                self.selected = None;
            }
        }
    }

    fn prompt_dialog(&mut self, ui: &mut Ui, conn: Option<&Connection>) {
        let Some((prompt, mut buffer)) = self.prompt.clone() else {
            return;
        };
        let title = match &prompt {
            Prompt::NewFolder { .. } => t!("New folder"),
            Prompt::Rename { .. } => t!("Rename"),
        };
        let mut ok = false;
        let mut cancel = false;
        let mut open = true;
        egui::Window::new(title)
            .id(egui::Id::new("browser-prompt"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    ui.label(t!("Name:"));
                    let field =
                        ui.add_sized([220.0, theme::field_height(ui)], theme::field(&mut buffer));
                    if !field.has_focus() {
                        field.request_focus();
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ok = true;
                    }
                });
                ui.horizontal(|ui| {
                    if theme::primary(ui, t!("OK")).clicked() {
                        ok = true;
                    }
                    if ui.button(t!("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }

        if cancel || !open {
            self.prompt = None;
            return;
        }
        if !ok {
            self.prompt = Some((prompt, buffer));
            return;
        }

        let name = buffer.trim().to_string();
        self.prompt = None;
        if name.is_empty() {
            return;
        }
        match prompt {
            Prompt::NewFolder { local } => {
                if local {
                    if let Err(err) = std::fs::create_dir(self.local_path.join(&name)) {
                        self.error = Some(t!("create folder failed: {0}", err.to_string()));
                    }
                    self.refresh_local();
                } else if let Some(conn) = conn {
                    let base = self.path.clone();
                    let conn = conn.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match conn.sftp().await {
                            Ok(sftp) => {
                                let target = remote_join(&base, &name);
                                match sftp::mkdir(&sftp, &target).await {
                                    Ok(()) => {
                                        let _ = tx.send(Loaded::Changed);
                                    }
                                    Err(err) => log::warn!("mkdir {target}: {err}"),
                                }
                            }
                            Err(err) => log::warn!("mkdir failed: {err}"),
                        }
                    });
                }
            }
            Prompt::Rename { path, local } => {
                if local {
                    let from = PathBuf::from(&path);
                    if let Some(parent) = from.parent() {
                        let _ = std::fs::rename(&from, parent.join(&name));
                        self.refresh_local();
                    }
                } else if let Some(conn) = conn {
                    let target = remote_join(&crate::util::remote_parent(&path), &name);
                    let conn = conn.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match conn.sftp().await {
                            Ok(sftp) => match sftp::rename(&sftp, &path, &target).await {
                                Ok(()) => {
                                    let _ = tx.send(Loaded::Changed);
                                }
                                Err(err) => log::warn!("rename {path}: {err}"),
                            },
                            Err(err) => log::warn!("rename failed: {err}"),
                        }
                    });
                }
            }
        }
    }
}

#[derive(Default)]
struct RemoteCmd {
    navigate: Option<String>,
    download_entry: Option<Entry>,
    /// Download, but ask where first.
    save_as: Option<Entry>,
    properties: Option<Entry>,
    download: bool,
    remove: bool,
    rename: bool,
    refresh: bool,
    new_folder: bool,
    /// The blank-area "Upload file": go and ask for local files.
    upload: bool,
}

#[derive(Default)]
struct LocalCmd {
    open: Option<PathBuf>,
    properties: Option<LocalEntry>,
    upload: bool,
    rename: bool,
    remove: bool,
    refresh: bool,
    new_folder: bool,
}

/// A table cell that answers to the pointer across its whole width, so the
/// blank part of a row belongs to the row instead of to the folder behind it.
fn cell(ui: &mut Ui, tag: &str, index: usize, draw: impl FnOnce(&mut Ui)) -> egui::Response {
    let rect = ui.available_rect_before_wrap();
    let response = ui.interact(rect, ui.id().with(("cell", tag, index)), Sense::click());
    draw(ui);
    response
}

/// The `..` row at the head of a listing: the folder one level up, in the same
/// column as the files. Every other file browser offers it, and here it saves
/// hunting for the up arrow on the toolbar above the path box.
///
/// It reports the request instead of navigating, so the caller keeps the
/// history of its own pane.
fn parent_row(row: &mut TableRow<'_, '_>, tag: &str, show_time: bool, muted: Color32) -> bool {
    let mut up = false;
    let mut hovered = false;
    // Every column of the row answers to the pointer, so a click anywhere on
    // the line means the same thing - not only the one under the folder glyph.
    let mut grab = |ui: &mut Ui, text: &str, tip: bool| {
        let rect = ui.available_rect_before_wrap();
        let response = ui.interact(rect, ui.id().with(tag), Sense::click());
        if !text.is_empty() {
            ui.label(RichText::new(text).size(11.5).color(muted));
        }
        let response = if tip {
            response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(t!("Parent folder"))
        } else {
            response
        };
        hovered |= response.hovered();
        up |= response.clicked();
    };
    row.col(|ui| grab(ui, "\u{1f4c1} \u{2026}", true));
    row.col(|ui| grab(ui, "--", false));
    if show_time {
        row.col(|ui| grab(ui, "", false));
    }
    row.set_hovered(hovered);
    up
}

/// The row a remote menu was opened on, as the properties window's first draft.
fn remote_info(entry: &Entry) -> Info {
    Info {
        name: entry.name.clone(),
        path: entry.path.clone(),
        local: false,
        kind: entry.kind,
        size: entry.size,
        modified: entry.mtime,
        mode: entry.mode,
        ..Default::default()
    }
}

/// The same for a local row.
fn local_info(entry: &LocalEntry) -> Info {
    Info {
        name: entry.name.clone(),
        path: entry.path.to_string_lossy().into_owned(),
        local: true,
        kind: if entry.is_dir {
            Kind::Directory
        } else {
            Kind::File
        },
        size: entry.size,
        modified: entry.mtime,
        ..Default::default()
    }
}

/// What the narrow dock's button row asked for.
#[derive(Default)]
struct PaneBar {
    download: bool,
    upload: bool,
    remove: bool,
    rename: bool,
}

/// What one navigation row asked its pane to do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowAction {
    None,
    Back(isize),
    Up,
    Go,
}

/// Columns sized to the room the table actually has.
///
/// `egui_extras` never shrinks an `initial` column, so a fixed three-column
/// listing asked for more width than the dock had - and a resizable panel
/// grows to whatever its content asks for, every frame. The name column takes
/// what is left and truncates; the timestamp only appears once it fits.
fn fit_columns(width: f32) -> (Vec<Column>, bool) {
    let show_time = width >= 290.0;
    let mut columns = vec![
        Column::remainder().at_least(80.0).clip(true),
        Column::initial(70.0).clip(true),
    ];
    if show_time {
        columns.push(Column::initial(108.0).clip(true));
    }
    (columns, show_time)
}

/// Back / Forward / Up, an editable path, a clear button and Go.
///
/// The row never navigates on its own: it reports the request so the caller
/// keeps ownership of the pane state.
fn path_row(
    ui: &mut Ui,
    hint: &'static str,
    buffer: &mut String,
    focused: &mut bool,
    can_back: bool,
    can_forward: bool,
) -> RowAction {
    let mut action = RowAction::None;
    ui.horizontal(|ui| {
        let back = ui
            .add_enabled(can_back, egui::Button::new(t!("\u{2190}")).small())
            .on_hover_text(t!("Back to the previous folder"));
        if back.clicked() {
            action = RowAction::Back(-1);
        }
        let forward = ui
            .add_enabled(can_forward, egui::Button::new(t!("\u{2192}")).small())
            .on_hover_text(t!("Forward to the next folder"));
        if forward.clicked() {
            action = RowAction::Back(1);
        }
        if theme::ghost(ui, t!("\u{2191}"))
            .on_hover_text(t!("Parent folder"))
            .clicked()
        {
            action = RowAction::Up;
        }
        let width = (ui.available_width() - 84.0).max(70.0);
        let response = ui.add_sized(
            [width, 24.0],
            egui::TextEdit::singleline(buffer)
                .hint_text(tr(hint))
                .vertical_align(egui::Align::Center)
                .cursor_at_end(true),
        );
        *focused = response.has_focus();
        if theme::ghost(ui, t!("\u{2715}"))
            .on_hover_text(t!("Clear the path"))
            .clicked()
        {
            buffer.clear();
            response.request_focus();
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (response.lost_focus() && enter) || ui.small_button(t!("Go")).clicked() {
            action = RowAction::Go;
        }
    });
    action
}
/// Where downloads land: the user's Downloads folder when there is one.
fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn remove_local(path: &Path) {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(err) = result {
        log::warn!("delete {}: {err}", path.display());
    }
}

fn open_in_explorer(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn read_dir(path: &Path) -> Vec<LocalEntry> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(path) else {
        return out;
    };
    for item in read.flatten() {
        let meta = item.metadata().ok();
        out.push(LocalEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            path: item.path(),
            is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            mtime: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}
