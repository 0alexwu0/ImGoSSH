//! The `eframe::App` shell: tabs, sidebar, modals and the tokio runtime.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::epaint;
use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily,
    FontId, FontTweak, Margin, Pos2, Rect, Response, Sense, StrokeKind, Ui, Vec2,
};
use tokio::sync::mpsc;

use crate::config::{Palette, Protocol, Session, Store};
use crate::i18n::{self, Lang};
use crate::ssh::client::{
    Challenge, ConnectOptions, Connection, Prompter, Reply, Request, TermEvent, TermSink,
};
use crate::ssh::monitor::{Monitor, ServerInfo, DEFAULT_INTERVAL};
use crate::ssh::transfer::Queue;
use crate::ssh::{local::LocalPty, Backend};
use crate::t;
use crate::terminal::Emulator;
use crate::ui::browser::Browser;
use crate::ui::dialogs::{self, ChallengeBuffer};
use crate::ui::sidebar::{self, Sidebar};
use crate::ui::terminal::{self as term_widget, Style as TermStyle};
use crate::ui::theme;
use crate::util::{human_rate, human_size, human_uptime};

/// How far a tab has got.
#[derive(Clone, Debug)]
pub enum Status {
    Connecting,
    Running,
    Closed(Option<u32>),
    Failed(String),
}

impl Status {
    fn label(&self) -> String {
        match self {
            Status::Connecting => t!("connecting...").into(),
            Status::Running => t!("connected").into(),
            Status::Closed(None) => t!("closed").into(),
            Status::Closed(Some(code)) => t!("exit code {0}", code),
            Status::Failed(err) => err.clone(),
        }
    }

    fn is_live(&self) -> bool {
        matches!(self, Status::Connecting | Status::Running)
    }
}

/// What a tab shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    /// An interactive shell (SSH or local).
    Terminal,
    /// A standalone SFTP browser.
    Sftp,
    /// An SCP transfer session.
    Scp,
}

/// One entry of the tab strip.
pub struct Tab {
    pub kind: TabKind,
    pub session: Session,
    pub emu: Arc<Mutex<Emulator>>,
    pub backend: Arc<Mutex<Option<Backend>>>,
    /// Filled in by the connect task, read by the browser and the status bar.
    pub conn: Arc<Mutex<Option<Connection>>>,
    pub status: Arc<Mutex<Status>>,
    /// Live server statistics for the status bar.
    pub monitor: Monitor,
    pub view: term_widget::State,
    pub browser: Option<Browser>,
    pub show_browser: bool,
    pub bell_seen: u64,
    pub flash: f64,
}

impl Tab {
    /// The name shown in the tab strip.
    ///
    /// A remote tab _is_ the session it was opened from, so the name the user
    /// gave it wins over whatever the shell reports about itself. A local
    /// shell has no saved name worth showing, so its tab follows the program
    /// running inside it.
    fn title(&self) -> String {
        if !self.session.protocol.is_remote() {
            let emu = self.emu.lock().unwrap();
            if !emu.title.is_empty() {
                return emu.title.clone();
            }
        }
        self.session.title()
    }
}

/// A question the engine asked, waiting for the user.
struct Pending {
    request: Request,
    buffer: ChallengeBuffer,
}

/// Which folder operation a name prompt belongs to.
enum NameKind {
    NewFolder,
    RenameFolder(String),
}

struct NamePrompt {
    title: String,
    value: String,
    kind: NameKind,
}

/// What to open automatically when the window appears.
#[derive(Debug, Default, Clone)]
pub struct Startup {
    /// Open a local pseudo-console tab.
    pub local: bool,
    /// Open a saved session by name.
    pub session: Option<String>,
    /// Connect to `[user@]host` in a new tab.
    pub host: Option<String>,
}

/// The application.
pub struct App {
    store: Store,
    palette: Palette,
    runtime: Arc<tokio::runtime::Runtime>,
    ctx: Option<Context>,
    tabs: Vec<Tab>,
    active: usize,
    sidebar: Sidebar,
    queue: Queue,
    prompter: Prompter,
    prompts: mpsc::UnboundedReceiver<Request>,
    pending: Option<Pending>,
    editor: Option<dialogs::SessionEditor>,
    editor_index: Option<usize>,
    confirm_close: Option<usize>,
    name_prompt: Option<NamePrompt>,
    show_settings: bool,
    show_transfers: bool,
    show_about: bool,
    term_menu: Option<(egui::Pos2, usize)>,
    /// Set on the frame the terminal menu opens, so the click that opened it
    /// cannot be mistaken for the click that dismisses it.
    term_menu_fresh: bool,
    /// The terminal grid every tab shares.
    grid: Grid,
    /// The `line_spacing` `grid` was measured against, so a change to it is
    /// noticed without measuring the grid on every frame.
    spacing: f32,
    font: FontId,
    cursor_on: bool,
    blink_at: f64,

    dirty: bool,
}

/// The grid a terminal tab paints into: what one cell costs, and the air a row
/// gets around the glyphs inside it.
#[derive(Clone, Copy, Default)]
struct Grid {
    /// The box one character cell takes, in points.
    cell: Vec2,
    /// Air `line_spacing` adds to a row, shared between its top and its bottom.
    /// Zero leaves a row exactly as tight as the glyphs that fill it.
    leading: f32,
}

impl App {
    /// Build the shell from the eframe creation context.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_startup(cc, Startup::default())
    }

    /// Build the shell and queue whatever the command line asked for.
    pub fn with_startup(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        let store = Store::load();
        i18n::set(resolve_lang(&store.config.language));
        let palette = store.config.palette();
        let font = FontId::monospace(store.config.font_size);
        setup_fonts(cc, &store.config.font_family, store.config.font_size);
        // The font atlas only exists after the first `Context::run`, so the
        // cell metrics are measured on the first frame instead of here.
        let grid = Grid::default();
        let spacing = 0.0;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(4)
                .build()
                .expect("tokio runtime"),
        );
        let (prompter, prompts) = Prompter::channel();
        let mut app = Self {
            store,
            palette,
            runtime,
            ctx: Some(cc.egui_ctx.clone()),
            tabs: Vec::new(),
            active: 0,
            sidebar: Sidebar::default(),
            queue: Queue::new(),
            prompter,
            prompts,
            pending: None,
            editor: None,
            editor_index: None,
            confirm_close: None,
            name_prompt: None,
            show_settings: false,
            show_transfers: false,
            show_about: false,
            term_menu: None,
            term_menu_fresh: false,
            grid,
            spacing,
            font,
            cursor_on: true,
            blink_at: 0.0,

            dirty: false,
        };
        app.apply_theme(&cc.egui_ctx);
        app.startup(&startup);
        cc.egui_ctx.request_repaint();
        app
    }

    fn startup(&mut self, startup: &Startup) {
        if let Some(name) = &startup.session {
            if let Some(index) = self.store.sessions.iter().position(|s| &s.name == name) {
                let session = self.store.sessions[index].clone();
                self.launch(session);
                return;
            }
            log::warn!("no saved session called {name}");
        }
        if let Some(destination) = &startup.host {
            let mut session = self.store.config.new_session(Protocol::Ssh);
            if let Some((user, host)) = destination.split_once('@') {
                session.username = user.to_string();
                session.host = host.to_string();
            } else {
                session.host = destination.clone();
            }
            session.name = session.target();
            self.launch(session);
            return;
        }
        if startup.local {
            let session = self.store.config.new_session(Protocol::Local);
            self.launch(session);
        }
    }

    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    fn new_tab(&mut self, session: Session) -> usize {
        let (rows, cols) = (session.rows.max(2), session.cols.max(2));
        let scrollback = self.store.config.scrollback;
        let mut emu = Emulator::new(rows, cols, scrollback);
        // A fresh tab starts on the configured palette; the server may repaint
        // it through OSC 4/10/11/12.
        emu.set_palette_colors(self.palette.foreground, self.palette.background);
        let tab = Tab {
            kind: match session.protocol {
                Protocol::Sftp => TabKind::Sftp,
                Protocol::Scp => TabKind::Scp,
                _ => TabKind::Terminal,
            },
            emu: Arc::new(Mutex::new(emu)),
            backend: Arc::new(Mutex::new(None)),
            conn: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(Status::Connecting)),
            monitor: Monitor::new(),
            view: term_widget::State::default(),
            browser: match session.protocol {
                Protocol::Sftp | Protocol::Scp => {
                    let mut browser = Browser::new();
                    if session.remote_path.starts_with('/') {
                        browser.path = session.remote_path.clone();
                    }
                    Some(browser)
                }
                _ => None,
            },
            show_browser: session.sftp_panel && session.protocol.is_remote(),
            bell_seen: 0,
            flash: 0.0,
            session,
        };
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Start a session exactly like a double-click in the sidebar.
    fn launch(&mut self, session: Session) {
        let index = self.new_tab(session.clone());
        match session.protocol {
            Protocol::Local => self.start_local(index),
            Protocol::Scp => self.start_scp(index),
            _ => self.start_ssh(index),
        }
    }

    fn tab_handles(
        &self,
        index: usize,
    ) -> Option<(
        Arc<Mutex<Emulator>>,
        Arc<Mutex<Option<Backend>>>,
        Arc<Mutex<Status>>,
        Arc<Mutex<Option<Connection>>>,
    )> {
        let tab = self.tabs.get(index)?;
        Some((
            tab.emu.clone(),
            tab.backend.clone(),
            tab.status.clone(),
            tab.conn.clone(),
        ))
    }

    fn sink_for(
        emu: Arc<Mutex<Emulator>>,
        backend: Arc<Mutex<Option<Backend>>>,
        status: Arc<Mutex<Status>>,
        ctx: Context,
    ) -> TermSink {
        TermSink::new(move |event| {
            match event {
                TermEvent::Data(bytes) => {
                    let responses = emu.lock().unwrap().feed(&bytes);
                    if !responses.is_empty() {
                        if let Some(backend) = backend.lock().unwrap().as_ref() {
                            backend.send(responses);
                        }
                    }
                }
                TermEvent::Closed { exit } => {
                    *status.lock().unwrap() = Status::Closed(exit);
                }
            }
            ctx.request_repaint();
        })
    }

    fn start_local(&mut self, index: usize) {
        let Some((emu, backend, status, _)) = self.tab_handles(index) else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (rows, cols) = (tab.session.rows, tab.session.cols);
        let ctx = match self.ctx.clone() {
            Some(ctx) => ctx,
            None => return,
        };
        let sink = Self::sink_for(emu, backend.clone(), status.clone(), ctx);
        match LocalPty::spawn(None, cols, rows, sink) {
            Ok(pty) => {
                *backend.lock().unwrap() = Some(Backend::Local(pty));
                *status.lock().unwrap() = Status::Running;
            }
            Err(err) => {
                *status.lock().unwrap() = Status::Failed(format!("{err:#}"));
            }
        }
    }

    /// Connect, then open a shell (SSH) or just the SFTP subsystem (SFTP).
    fn start_ssh(&mut self, index: usize) {
        let Some((emu, backend, status, conn_slot)) = self.tab_handles(index) else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let session = tab.session.clone();
        let monitor = tab.monitor.clone();
        let want_shell = matches!(session.protocol, Protocol::Ssh | Protocol::Local);
        let runtime = self.runtime.clone();
        let prompter = self.prompter.clone();
        let Some(ctx) = self.ctx.clone() else { return };

        runtime.spawn(async move {
            let opts = ConnectOptions::from_session(&session, prompter);
            let conn = match Connection::connect(opts).await {
                Ok(conn) => conn,
                Err(err) => {
                    *status.lock().unwrap() = Status::Failed(format!("{err:#}"));
                    ctx.request_repaint();
                    return;
                }
            };
            if session.auto_tunnels {
                for tunnel in session.tunnels.clone() {
                    if let Err(err) = conn.start_tunnel(&tunnel).await {
                        log::warn!("tunnel {}: {err}", tunnel.describe());
                    }
                }
            }
            *conn_slot.lock().unwrap() = Some(conn.clone());
            *status.lock().unwrap() = Status::Running;
            let repaint = ctx.clone();
            monitor.attach(conn.clone(), DEFAULT_INTERVAL, move || {
                repaint.request_repaint();
            });
            ctx.request_repaint();

            if !want_shell {
                return;
            }
            let sink = Self::sink_for(emu, backend.clone(), status.clone(), ctx.clone());
            let command = (!session.startup_command.trim().is_empty())
                .then(|| session.startup_command.clone());
            match conn
                .shell(session.cols, session.rows, &session.term, command, sink)
                .await
            {
                Ok(handle) => {
                    if let Some(script) = non_empty(&session.startup_script) {
                        let _ = handle.send(format!("{script}\r").into_bytes());
                    }
                    *backend.lock().unwrap() = Some(Backend::Ssh(handle));
                }
                Err(err) => {
                    *status.lock().unwrap() = Status::Failed(format!("{err:#}"));
                    let _ = backend;
                }
            }
            ctx.request_repaint();
        });
    }

    /// Connect and queue the configured SCP transfer.
    fn start_scp(&mut self, index: usize) {
        let Some((_, _, status, conn_slot)) = self.tab_handles(index) else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let session = tab.session.clone();
        let monitor = tab.monitor.clone();
        let runtime = self.runtime.clone();
        let prompter = self.prompter.clone();
        let queue = self.queue.clone();
        let Some(ctx) = self.ctx.clone() else { return };

        runtime.spawn(async move {
            let opts = ConnectOptions::from_session(&session, prompter);
            let conn = match Connection::connect(opts).await {
                Ok(conn) => conn,
                Err(err) => {
                    *status.lock().unwrap() = Status::Failed(format!("{err:#}"));
                    ctx.request_repaint();
                    return;
                }
            };
            *conn_slot.lock().unwrap() = Some(conn.clone());
            *status.lock().unwrap() = Status::Running;
            let repaint = ctx.clone();
            monitor.attach(conn.clone(), DEFAULT_INTERVAL, move || {
                repaint.request_repaint();
            });
            let remote = session.remote_path.clone();
            let local = PathBuf::from(&session.local_path);
            if local.as_os_str().is_empty() || remote.is_empty() {
                ctx.request_repaint();
                return;
            }
            match session.scp_direction {
                crate::config::ScpDirection::Download => {
                    let dir = if local.is_dir() {
                        local.clone()
                    } else {
                        local
                            .parent()
                            .map(PathBuf::from)
                            .unwrap_or_else(|| PathBuf::from("."))
                    };
                    queue.download(conn.clone(), remote, dir, true);
                }
                crate::config::ScpDirection::Upload => {
                    queue.upload(conn.clone(), local, remote, true);
                }
            }
            ctx.request_repaint();
        });
    }

    fn close_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.monitor.stop();
            if let Some(backend) = tab.backend.lock().unwrap().as_ref() {
                backend.close();
            }
            if let Some(conn) = tab.conn.lock().unwrap().as_ref() {
                let conn = conn.clone();
                self.runtime.spawn(async move { conn.close().await });
            }
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
// ---------------------------------------------------------------------------
// Fonts, metrics and theme
// ---------------------------------------------------------------------------

/// The ranges a terminal grid is painted from.
///
/// A face that lacks one of them hands that cell to a fallback whose advance
/// matches the terminal's only by luck, and everything right of it slides along
/// the row. The Consolas that ships with Windows has no eighth blocks and no
/// braille at all, which is enough to tear apart the boxes and bars a
/// monitoring program paints.
const GRID_RANGES: [(u32, u32); 5] = [
    (0x2500, 0x257F), // box drawing: the panels every dashboard is built from
    (0x2580, 0x259F), // blocks, eighths and shades: bars and sparklines
    (0x25A0, 0x25DF), // geometric shapes: bullets, triangles, quadrants
    (0x2190, 0x2193), // the arrows a status line points with
    (0x2800, 0x28FF), // braille: what btop and friends chart with
];

/// The grid characters outside those ranges: the middle dot, degree sign and
/// plus-minus sign that the DEC special graphics set stands in for.
const GRID_STRAYS: [char; 3] = ['\u{00b7}', '\u{00b0}', '\u{00b1}'];

/// Terminal fonts to try, the most complete first.
///
/// Cascadia Mono ships with Windows Terminal and covers the grid on its own, as
/// do Menlo on macOS and the DejaVu family on Linux. Consolas comes last: it is
/// on every Windows, but it is not a complete terminal face.
const TERMINAL_FONTS: [(&str, &str); 9] = [
    ("Cascadia Mono", r"C:\Windows\Fonts\CascadiaMono.ttf"),
    ("Cascadia Code", r"C:\Windows\Fonts\CascadiaCode.ttf"),
    ("Menlo", "/System/Library/Fonts/Menlo.ttc"),
    ("SF Mono", "/System/Library/Fonts/SFMono-Regular.otf"),
    (
        "DejaVu Sans Mono",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ),
    (
        "Liberation Mono",
        "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
    ),
    (
        "Noto Sans Mono",
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    ),
    (
        "Ubuntu Mono",
        "/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf",
    ),
    ("Consolas", r"C:\Windows\Fonts\consola.ttf"),
];

/// Build the font table: one monospace face that owns the whole grid, then the
/// symbol and East Asian fallbacks, then hand the lot to egui.
fn setup_fonts(cc: &eframe::CreationContext<'_>, family: &str, size: f32) {
    let mut fonts = FontDefinitions::default();
    install_terminal_font(&mut fonts, family);
    add_symbol_fallback(&mut fonts);
    if let Some(cjk) = add_cjk_fallback(&mut fonts) {
        tune_cjk_scale(&mut fonts, cjk, size);
    }
    cc.egui_ctx.set_fonts(fonts);
}

/// Put the first complete monospace face at the head of the terminal family.
///
/// The family named in `config.toml` is tried first, then the candidates in
/// coverage order; a face that has to borrow any grid glyph from a fallback
/// gives way to the next one. When nothing installed covers the grid, egui's own
/// embedded Hack heads the family: it needs no file, and its metrics are the
/// same on every machine, which is what a grid wants.
fn install_terminal_font(fonts: &mut FontDefinitions, family: &str) {
    let wanted = family.trim().to_lowercase();
    let preferred = TERMINAL_FONTS
        .iter()
        .find(|(name, _)| name.to_lowercase() == wanted);
    if !wanted.is_empty() && preferred.is_none() {
        log::info!("no font file for {wanted} among the terminal candidates");
    }
    let ordered = preferred.into_iter().chain(
        TERMINAL_FONTS
            .iter()
            .filter(move |face| Some(*face) != preferred),
    );
    let needed = grid_glyphs().len();
    for face in ordered {
        let (name, path) = *face;
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let data = FontData::from_owned(bytes);
        let covered = grid_coverage(name, &data);
        if covered < needed {
            let missing = needed - covered;
            // The family named in `config.toml` leads the queue, but it only gets
            // the grid if it can actually paint one - honouring an incomplete face
            // is what tears the layout apart - so say out loud why it was passed
            // over rather than leaving it to a debug line nobody enables.
            let level = if Some(face) == preferred {
                log::Level::Info
            } else {
                log::Level::Debug
            };
            log::log!(
                level,
                "{name} is missing {missing} of the {needed} grid glyphs"
            );
            continue;
        }
        fonts.font_data.insert(name.to_string(), Arc::new(data));
        let list = fonts.families.entry(FontFamily::Monospace).or_default();
        if !list.iter().any(|face| face.as_str() == name) {
            list.insert(0, name.to_string());
        }
        log::info!("terminal font: {name}");
        return;
    }
    log::info!("terminal font: Hack (embedded)");
}

/// Every character a terminal grid is painted from.
fn grid_glyphs() -> Vec<char> {
    GRID_RANGES
        .iter()
        .flat_map(|(first, last)| *first..=*last)
        .filter_map(char::from_u32)
        .chain(GRID_STRAYS)
        .collect()
}

/// The size the coverage probe measures a glyph at, in points.
const PROBE_SIZE: f32 = 16.0;

/// How many grid characters `data` paints on its own, with no fallback to lean
/// on. A face that has to borrow a glyph charges nothing for it, so a nonzero
/// advance is what "this face owns this cell" means.
///
/// `FontsView::has_glyph` cannot say. It answers by comparing the face holding
/// the glyph against the face holding the replacement glyph, and a family built
/// from a single face only ever has the second: every glyph the face can paint
/// comes back reported as missing, which once left the terminal on its embedded
/// fallback no matter what was installed.
///
/// A private atlas stands in for the one egui builds, because
/// `Context::set_fonts` only takes effect at the start of the next pass and
/// this has to be settled before the first frame is laid out.
fn grid_coverage(name: &str, data: &FontData) -> usize {
    let mut definitions = FontDefinitions::default();
    definitions.font_data.clear();
    definitions.families.clear();
    definitions
        .font_data
        .insert(name.to_string(), Arc::new(data.clone()));
    definitions
        .families
        .insert(FontFamily::Monospace, vec![name.to_string()]);
    let glyphs = grid_glyphs();
    let font = FontId::monospace(PROBE_SIZE);
    measure(&definitions, |fonts| {
        glyphs
            .iter()
            .filter(|ch| fonts.glyph_width(&font, **ch) > 0.0)
            .count()
    })
}

/// Run `probe` against a throwaway font atlas built from `definitions`.
fn measure<T>(definitions: &FontDefinitions, probe: impl FnOnce(&mut epaint::FontsView) -> T) -> T {
    let mut atlas = epaint::Fonts::new(epaint::TextOptions::default(), definitions.clone());
    probe(&mut atlas.with_pixels_per_point(1.0))
}

/// The width one character has in `definitions`, in points.
fn advance(definitions: &FontDefinitions, ch: char, size: f32) -> f32 {
    let font = FontId::monospace(size);
    measure(definitions, |fonts| fonts.glyph_width(&font, ch))
}

/// egui's bundled proportional font has no arrows, triangles or dingbats, so
/// every toolbar button using one rendered as an empty box. Segoe UI Symbol
/// covers them all in monochrome; DejaVu is the fallback on Linux.
fn add_symbol_fallback(fonts: &mut FontDefinitions) {
    let candidates: [(&str, &str); 3] = [
        ("Segoe UI Symbol", r"C:\Windows\Fonts\seguisym.ttf"),
        (
            "DejaVu Sans",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ),
        ("Apple Symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
    ];
    install_fallback(fonts, &candidates);
}

/// Chinese dates, file names and program messages need an East Asian face, and
/// its metrics have to be known before it can be kept inside the grid.
fn add_cjk_fallback(fonts: &mut FontDefinitions) -> Option<&'static str> {
    let candidates: [(&str, &str); 4] = [
        ("Microsoft YaHei", r"C:\Windows\Fonts\msyh.ttc"),
        ("SimHei", r"C:\Windows\Fonts\simhei.ttf"),
        (
            "Noto Sans CJK SC",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ),
        (
            "Hiragino Sans GB",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ),
    ];
    install_fallback(fonts, &candidates)
}

/// Keep the East Asian fallback from overflowing the two cells an ideograph is
/// charged, shrinking a face whose glyphs are too wide to fit them.
///
/// The Latin side comes from the terminal face that now heads the family, so
/// both are measured on one scale. A face that fits is left exactly as it is:
/// `FontTweak::scale` is uniform, so scaling an ideograph up to fill its pair
/// of cells stretches it taller as well as wider, and lines of Chinese end up
/// crowded both ways - the characters touching their neighbours and pressing on
/// the rows above and below. Narrower than two cells is not a problem: the pen
/// is snapped back onto the grid after every glyph anyway (see `append_cells`),
/// so the leftover space is simply left where it falls.
fn tune_cjk_scale(fonts: &mut FontDefinitions, name: &str, size: f32) {
    let latin = advance(fonts, 'M', size);

    let mut probe = fonts.clone();
    probe
        .families
        .insert(FontFamily::Monospace, vec![name.to_string()]);
    let cjk = advance(&probe, '\u{7528}', size);

    let scale = (2.0 * latin / cjk).min(1.0);
    if latin <= 0.0 || cjk <= 0.0 || scale >= 1.0 {
        return;
    }
    let Some(current) = fonts.font_data.get(name) else {
        return;
    };
    let mut tweaked = (**current).clone();
    tweaked.tweak = FontTweak {
        scale,
        ..Default::default()
    };
    fonts.font_data.insert(name.to_string(), Arc::new(tweaked));
}

/// Load the first font that exists and queue it behind the UI fonts.
/// Returns its name so callers can tweak it further.
fn install_fallback(
    fonts: &mut FontDefinitions,
    candidates: &[(&'static str, &'static str)],
) -> Option<&'static str> {
    let &(name, path) = candidates
        .iter()
        .find(|(_, path)| std::path::Path::new(path).exists())?;
    if !fonts.font_data.contains_key(name) {
        let bytes = std::fs::read(path).ok()?;
        fonts
            .font_data
            .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
    }
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        let list = fonts.families.entry(family).or_default();
        if list.iter().any(|f| f == name) {
            continue;
        }
        // Ahead of the colour-emoji fonts: the emoji-icon font hides most of its
        // shapes behind OpenType ligatures, so egui draws a box for them.
        let at = list
            .iter()
            .position(|f| f.starts_with("Noto") || f.starts_with("emoji"))
            .unwrap_or(list.len());
        list.insert(at, name.to_string());
    }
    Some(name)
}

/// The grid metrics for one terminal face at one size.
///
/// The width is the advance and nothing else: every column is placed from this
/// number, so it has to be the width the shaper itself charges for a glyph.
/// The height is the text box as `line_spacing` wants it. Latin asks for
/// little; a page of Chinese fills its box from edge to edge, and the same row
/// height reads as lines pressed against each other.
fn measure_cell(ctx: &Context, font: &FontId, line_spacing: f32) -> Grid {
    let advance = ctx.fonts_mut(|fonts| fonts.glyph_width(font, 'M'));
    let galley =
        ctx.fonts_mut(|fonts| fonts.layout_no_wrap("M".to_string(), font.clone(), Color32::WHITE));
    let size = galley.size();
    let width = if advance > 0.0 { advance } else { size.x };
    if width <= 0.0 || size.y <= 0.0 {
        return Grid {
            cell: Vec2::new(8.0, 16.0),
            leading: 0.0,
        };
    }
    let height = size.y * line_spacing.clamp(0.6, 3.0);
    Grid {
        cell: Vec2::new(width, height),
        leading: height - size.y,
    }
}

impl App {
    /// Repaint the whole shell with the tokens of the chosen theme.
    fn apply_theme(&self, ctx: &Context) {
        theme::apply(
            ctx,
            self.store.config.theme,
            &self.palette,
            self.store.config.font_size,
        );
    }

    fn sync_config(&mut self, ctx: &Context) {
        let lang = resolve_lang(&self.store.config.language);
        if lang != i18n::current() {
            i18n::set(lang);
            ctx.request_repaint();
        }
        let palette = self.store.config.palette();
        let palette_changed = palette != self.palette;
        let font = FontId::monospace(self.store.config.font_size);
        let font_changed = font != self.font;
        if palette_changed {
            for tab in &self.tabs {
                let mut emu = tab.emu.lock().unwrap();
                emu.set_palette_colors(palette.foreground, palette.background);
            }
            self.palette = palette;
        }
        // The font atlas only exists once a frame is being laid out, which is
        // why the grid is measured here rather than in `with_startup`: the
        // first pass takes it, and so does any pass where the face, its size,
        // or the requested line spacing has moved.
        let spacing = self.store.config.line_spacing;
        if font_changed || spacing != self.spacing || self.grid.cell.y <= 0.0 {
            self.grid = measure_cell(ctx, &font, spacing);
            self.spacing = spacing;
            self.font = font;
            ctx.request_repaint();
        }
        if palette_changed || font_changed {
            self.apply_theme(ctx);
            ctx.request_repaint();
        }
    }
}

// ---------------------------------------------------------------------------
// eframe::App
// ---------------------------------------------------------------------------

impl eframe::App for App {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());
        self.apply_theme(ctx);
        self.poll_prompts();
        self.poll_connections();
        if self.queue.active() > 0 && !self.show_transfers {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Browser and transfer jobs call `tokio::spawn` straight from the UI.
        let _guard = self.runtime.enter();
        let ctx = ui.ctx().clone();

        self.sync_config(&ctx);
        self.record_window_size(&ctx);
        self.tick_blink(&ctx);

        let shortcuts = self.take_shortcuts(&ctx);
        self.run_shortcuts(shortcuts, &ctx);

        // Only the session on screen is worth polling; the others keep their
        // last sample until they come back into view.
        for (index, tab) in self.tabs.iter().enumerate() {
            let idle = index != self.active
                || !self.store.config.show_statusbar
                || !self.store.config.show_server_monitor;
            tab.monitor.set_paused(idle);
        }

        self.menu_bar(ui);
        if self.store.config.show_toolbar {
            self.tool_bar(ui);
        }
        if self.store.config.show_statusbar {
            self.status_bar(ui);
        }
        if self.store.config.show_sidebar {
            let width = self.store.config.sidebar_width;
            egui::Panel::left("sidebar")
                .resizable(true)
                .min_size(160.0)
                .default_size(width)
                .show(ui, |ui| {
                    // Sessions with a live tab light up in the tree.
                    let live: Vec<String> = self
                        .tabs
                        .iter()
                        .filter(|tab| tab.status.lock().unwrap().is_live())
                        .map(|tab| tab.session.target())
                        .collect();
                    crate::ui::fill_available(ui, |ui| {
                        let actions = self.sidebar.ui(ui, &self.store.sessions, &live);
                        self.apply_sidebar_actions(actions);
                    });
                });
        }

        let surface = ui.visuals().panel_fill;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(surface).inner_margin(Margin {
                top: 0,
                bottom: 8,
                left: 8,
                right: 8,
            }))
            .show(ui, |ui| {
                self.tab_bar(ui);
                self.tab_content(ui);
            });

        self.dialogs(&ctx);
    }

    fn on_exit(&mut self) {
        if let Err(err) = self.store.save() {
            log::warn!("saving settings: {err}");
        }
    }
}
// ---------------------------------------------------------------------------
// Background state -> UI
// ---------------------------------------------------------------------------

/// An application-wide keyboard shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    NewTab,
    NewSession,
    CloseTab,
    ToggleBrowser,
    ToggleSidebar,
    Copy,
    Paste,
    NextTab,
    PrevTab,
    GotoTab(usize),
    Reconnect,
    Settings,
    Transfers,
}

/// Application shortcuts.
///
/// A shell swallows most bare `Ctrl` chords (`Ctrl+A` beginning of line,
/// `Ctrl+W` delete word, `Ctrl+T` transpose, `Ctrl+L` clear line, ...),
/// so everything the application owns is `Ctrl+Shift` or `Alt` based and
/// plain `Ctrl` chords are always forwarded to the remote side untouched.
fn match_shortcut(key: egui::Key, m: egui::Modifiers) -> Option<Shortcut> {
    use egui::Key as K;
    if m.ctrl && m.shift && !m.alt {
        return match key {
            K::C => Some(Shortcut::Copy),
            K::V => Some(Shortcut::Paste),
            K::T => Some(Shortcut::NewTab),
            K::N => Some(Shortcut::NewSession),
            K::W => Some(Shortcut::CloseTab),
            K::B => Some(Shortcut::ToggleBrowser),
            K::H => Some(Shortcut::ToggleSidebar),
            K::S => Some(Shortcut::Settings),
            K::U => Some(Shortcut::Transfers),
            K::R => Some(Shortcut::Reconnect),
            K::Tab => Some(Shortcut::PrevTab),
            _ => None,
        };
    }
    if m.ctrl && !m.shift && !m.alt {
        return match key {
            K::Tab => Some(Shortcut::NextTab),
            _ => None,
        };
    }

    if m.alt && !m.ctrl && !m.shift {
        if let Some(shortcut) = tab_number(key) {
            return Some(shortcut);
        }
        return match key {
            K::ArrowRight => Some(Shortcut::NextTab),
            K::ArrowLeft => Some(Shortcut::PrevTab),
            _ => None,
        };
    }
    None
}

/// `Alt+1` .. `Alt+9` jump straight to a tab, `Alt+0` to the tenth.
///
/// `Alt` plus a letter is deliberately absent: bash and zsh use `Alt+B`,
/// `Alt+F` and friends to move by word.
fn tab_number(key: egui::Key) -> Option<Shortcut> {
    use egui::Key as K;
    let index = match key {
        K::Num1 => 0,
        K::Num2 => 1,
        K::Num3 => 2,
        K::Num4 => 3,
        K::Num5 => 4,
        K::Num6 => 5,
        K::Num7 => 6,
        K::Num8 => 7,
        K::Num9 => 8,
        K::Num0 => 9,
        _ => return None,
    };
    Some(Shortcut::GotoTab(index))
}

impl App {
    fn poll_prompts(&mut self) {
        if self.pending.is_some() {
            return;
        }
        if let Ok(request) = self.prompts.try_recv() {
            self.pending = Some(Pending {
                request,
                buffer: ChallengeBuffer::default(),
            });
            if let Some(ctx) = self.ctx.clone() {
                ctx.request_repaint();
            }
        }
    }

    fn poll_connections(&mut self) {
        for tab in &mut self.tabs {
            let mut status = tab.status.lock().unwrap();
            if let Status::Running = *status {
                let dropped = tab
                    .conn
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|conn| !conn.is_alive());
                if dropped {
                    *status = Status::Closed(None);
                }
            }
        }
    }

    fn tick_blink(&mut self, ctx: &Context) {
        let config = &self.store.config;
        if !config.cursor_blink {
            self.cursor_on = true;
            return;
        }
        let now = ctx.input(|i| i.time);
        let interval = config.blink_ms.max(100) as f64 / 1000.0;
        if now - self.blink_at >= interval {
            self.blink_at = now;
            self.cursor_on = !self.cursor_on;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(config.blink_ms));
    }

    fn record_window_size(&mut self, ctx: &Context) {
        let rect = ctx.content_rect();
        self.store.config.window_width = rect.width().max(400.0);
        self.store.config.window_height = rect.height().max(300.0);
    }

    fn take_shortcuts(&self, ctx: &Context) -> Vec<Shortcut> {
        let mut found = Vec::new();
        ctx.input_mut(|i| {
            let mut index = 0;
            while index < i.events.len() {
                let hit = match i.events[index] {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => match_shortcut(key, modifiers),
                    _ => None,
                };
                if let Some(shortcut) = hit {
                    found.push(shortcut);
                    i.events.remove(index);
                } else {
                    index += 1;
                }
            }
        });
        found
    }

    fn run_shortcuts(&mut self, shortcuts: Vec<Shortcut>, ctx: &Context) {
        for shortcut in shortcuts {
            match shortcut {
                Shortcut::NewTab => {
                    let session = self.store.config.new_session(Protocol::Local);
                    self.launch(session);
                }
                Shortcut::NewSession => {
                    let protocol = self.store.config.default_protocol;
                    let session = self.store.config.new_session(protocol);
                    self.editor = Some(
                        dialogs::SessionEditor::new(session, t!("New session"))
                            .with_store_passwords(self.store.config.store_passwords),
                    );
                    self.editor_index = None;
                }
                Shortcut::CloseTab => self.close_active_tab(),
                Shortcut::ToggleBrowser => self.toggle_browser(),
                Shortcut::ToggleSidebar => {
                    self.store.config.show_sidebar = !self.store.config.show_sidebar;
                }
                Shortcut::Copy => self.copy_selection(ctx),
                Shortcut::Paste => self.paste_clipboard(),
                Shortcut::NextTab => self.cycle_tab(1),
                Shortcut::PrevTab => self.cycle_tab(-1),
                Shortcut::GotoTab(index) => self.goto_tab(index),
                Shortcut::Reconnect => self.reconnect_active(),
                Shortcut::Settings => self.show_settings = true,
                Shortcut::Transfers => self.show_transfers = true,
            }
        }
    }

    /// Close the tab on screen and open the same session again.
    fn reconnect_active(&mut self) {
        let Some(session) = self.tabs.get(self.active).map(|tab| tab.session.clone()) else {
            return;
        };
        let index = self.active;
        self.close_tab(index);
        self.launch(session);
    }

    fn cycle_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let count = self.tabs.len() as isize;
        self.active = (self.active as isize + delta).rem_euclid(count) as usize;
    }

    fn toggle_browser(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if tab.session.protocol.is_remote() {
                if tab.browser.is_none() {
                    tab.browser = Some(Browser::new());
                }
                tab.show_browser = !tab.show_browser;
            }
        }
    }

    fn copy_selection(&mut self, ctx: &Context) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if let Some(selection) = tab.view.selection {
            let text = tab
                .emu
                .lock()
                .unwrap()
                .text_between(selection.start, selection.end);
            if !text.is_empty() {
                ctx.copy_text(text);
            }
        }
    }

    fn paste_clipboard(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(text) = term_widget::clipboard_text() else {
            return;
        };
        let state = tab.emu.lock().unwrap().state();
        let bytes = crate::terminal::InputEncoder(state).paste(&text);
        if let Some(backend) = tab.backend.lock().unwrap().as_ref() {
            backend.send(bytes);
        }
    }

    fn close_active_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let index = self.active;
        let live = self
            .tabs
            .get(index)
            .is_some_and(|tab| tab.status.lock().unwrap().is_live());
        if live && self.store.config.confirm_close {
            self.confirm_close = Some(index);
            return;
        }
        self.close_tab(index);
    }

    /// The tab strip shortcuts, kept separate so the tests can prove that a
    /// bare `Ctrl` chord always reaches the shell.
    fn goto_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    fn apply_sidebar_actions(&mut self, actions: sidebar::Actions) {
        for index in actions.connect {
            if let Some(session) = self.store.sessions.get(index).cloned() {
                self.launch(session);
            }
        }
        if let Some(index) = actions.edit {
            if let Some(session) = self.store.sessions.get(index).cloned() {
                self.editor = Some(
                    dialogs::SessionEditor::new(session, t!("Session properties"))
                        .with_store_passwords(self.store.config.store_passwords),
                );
                self.editor_index = Some(index);
            }
        }
        if let Some(index) = actions.duplicate {
            if let Some(mut session) = self.store.sessions.get(index).cloned() {
                session.name = t!("{0} (copy)", session.name);
                self.store.sessions.push(session);
                self.dirty = true;
            }
        }
        if let Some(index) = actions.delete {
            self.store.remove(index);
            self.dirty = true;
        }
        if let Some((protocol, folder)) = actions.new_session {
            let mut session = self.store.config.new_session(protocol);
            session.folder = folder;
            self.editor = Some(
                dialogs::SessionEditor::new(session, t!("New session"))
                    .with_store_passwords(self.store.config.store_passwords),
            );
            self.editor_index = None;
        }
        if let Some(folder) = actions.new_folder {
            let title = if folder.is_empty() {
                t!("New folder").to_string()
            } else {
                t!("New folder in {0}", folder)
            };
            let value = if folder.is_empty() {
                t!("New folder").to_string()
            } else {
                format!("{folder}/{}", t!("New folder"))
            };
            self.name_prompt = Some(NamePrompt {
                title,
                value,
                kind: NameKind::NewFolder,
            });
        }
        if let Some(folder) = actions.rename_folder {
            self.name_prompt = Some(NamePrompt {
                title: t!("Rename {0}", folder),
                value: folder.clone(),
                kind: NameKind::RenameFolder(folder),
            });
        }
        if let Some(folder) = actions.delete_folder {
            self.store.delete_folder(&folder);
            self.dirty = true;
        }
        if actions.open_settings {
            self.show_settings = true;
        }
    }
}
// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

impl App {
    fn menu_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("menubar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                egui::menu::MenuButton::new(t!("Session")).ui(ui, |ui| {
                    if ui.button(t!("New local terminal\tCtrl+Shift+T")).clicked() {
                        let session = self.store.config.new_session(Protocol::Local);
                        self.launch(session);
                        ui.close();
                    }
                    if ui.button(t!("New SSH session...\tCtrl+Shift+N")).clicked() {
                        let session = self.store.config.new_session(Protocol::Ssh);
                        self.editor = Some(
                            dialogs::SessionEditor::new(session, t!("New session"))
                                .with_store_passwords(self.store.config.store_passwords),
                        );
                        self.editor_index = None;
                        ui.close();
                    }
                    if ui.button(t!("New SFTP browser")).clicked() {
                        let session = self.store.config.new_session(Protocol::Sftp);
                        self.editor = Some(
                            dialogs::SessionEditor::new(session, t!("New SFTP session"))
                                .with_store_passwords(self.store.config.store_passwords),
                        );
                        self.editor_index = None;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("Reconnect\tCtrl+Shift+R")).clicked() {
                        self.reconnect_active();
                        ui.close();
                    }
                    if ui.button(t!("Close tab\tCtrl+Shift+W")).clicked() {
                        self.close_active_tab();
                        ui.close();
                    }
                });

                egui::menu::MenuButton::new(t!("Terminal")).ui(ui, |ui| {
                    if ui.button(t!("Copy\tCtrl+Shift+C")).clicked() {
                        self.copy_selection(&ctx);
                        ui.close();
                    }
                    if ui.button(t!("Paste\tCtrl+Shift+V")).clicked() {
                        self.paste_clipboard();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("Toggle SFTP browser\tCtrl+Shift+B")).clicked() {
                        self.toggle_browser();
                        ui.close();
                    }
                    if ui.button(t!("Toggle sidebar\tCtrl+Shift+H")).clicked() {
                        self.store.config.show_sidebar = !self.store.config.show_sidebar;
                        ui.close();
                    }
                });

                egui::menu::MenuButton::new(t!("Transfers")).ui(ui, |ui| {
                    if ui.button(t!("Show transfer queue")).clicked() {
                        self.show_transfers = true;
                        ui.close();
                    }
                    if ui.button(t!("Clear finished")).clicked() {
                        self.queue.clear_finished();
                        ui.close();
                    }
                });

                egui::menu::MenuButton::new(t!("Settings")).ui(ui, |ui| {
                    if ui.button(t!("Configuration...")).clicked() {
                        self.show_settings = true;
                        ui.close();
                    }
                    for name in Palette::names() {
                        if ui
                            .selectable_label(
                                self.store.config.palette == name,
                                t!("Palette: {0}", name),
                            )
                            .clicked()
                        {
                            self.store.config.palette = name.to_string();
                            ui.close();
                        }
                    }
                });

                egui::menu::MenuButton::new(t!("Help")).ui(ui, |ui| {
                    if ui.button(t!("About ImGoSSH")).clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                    if ui.button(t!("GitHub repository")).clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::same_tab(crate::config::REPOSITORY_URL));
                        ui.close();
                    }
                    if ui.button(t!("Open settings folder")).clicked() {
                        let dir = crate::config::store_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        open_in_file_manager(&dir);
                        ui.close();
                    }
                });
            });
        });
    }

    fn tool_bar(&mut self, ui: &mut Ui) {
        let muted = theme::muted(ui);
        let accent = theme::accent(ui);
        let hover = ui.visuals().widgets.hovered.weak_bg_fill;
        let active = self.queue.active();
        egui::Panel::top("toolbar")
            .frame(theme::bar(ui))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if theme::ghost(ui, t!("\u{1f5a5} SSH"))
                        .on_hover_text(t!("New SSH session"))
                        .clicked()
                    {
                        let session = self.store.config.new_session(Protocol::Ssh);
                        self.editor = Some(
                            dialogs::SessionEditor::new(session, t!("New session"))
                                .with_store_passwords(self.store.config.store_passwords),
                        );
                        self.editor_index = None;
                    }
                    if theme::ghost(ui, t!("\u{25b6} Local"))
                        .on_hover_text(t!("New local terminal"))
                        .clicked()
                    {
                        let session = self.store.config.new_session(Protocol::Local);
                        self.launch(session);
                    }
                    if theme::ghost(ui, t!("\u{1f4c1} SFTP"))
                        .on_hover_text(t!("New SFTP browser"))
                        .clicked()
                    {
                        let session = self.store.config.new_session(Protocol::Sftp);
                        self.editor = Some(
                            dialogs::SessionEditor::new(session, t!("New SFTP session"))
                                .with_store_passwords(self.store.config.store_passwords),
                        );
                        self.editor_index = None;
                    }
                    ui.separator();
                    if theme::ghost(ui, t!("\u{1f4e4} Transfers"))
                        .on_hover_text(t!("Show transfer queue"))
                        .clicked()
                    {
                        self.show_transfers = true;
                    }
                    if active > 0 {
                        let count = t!("{0} transfer(s)", active).to_string();
                        theme::badge(ui, &count, accent, hover);
                    }
                    if theme::ghost(ui, t!("\u{2699} Settings"))
                        .on_hover_text(t!("Configuration..."))
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(t!("ImGoSSH")).size(12.0).color(muted));
                    });
                });
            });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let index = self.active;
        let bar = self.tabs.get(index).map(|tab| {
            let status = tab.status.lock().unwrap().clone();
            let (rows, cols) = tab.emu.lock().unwrap().size();
            let tunnels = tab
                .conn
                .lock()
                .unwrap()
                .as_ref()
                .map(|conn| conn.active_tunnels().len())
                .unwrap_or(0);
            SessionBar {
                status,
                size: format!("{cols} x {rows}"),
                tunnels,
                target: tab.session.target(),
                remote: tab.session.protocol.is_remote(),
                monitor: self.store.config.show_server_monitor && tab.session.protocol.is_remote(),
                info: tab.monitor.snapshot(),
            }
        });
        let muted = theme::muted(ui);
        let (good, warn, bad, idle) = theme::tones(ui);
        egui::Panel::bottom("statusbar")
            .frame(theme::bar(ui))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let Some(bar) = bar else {
                        ui.label(
                            egui::RichText::new(t!(
                                "No session - double-click a session on the left, or press Ctrl+Shift+T"
                            ))
                            .size(12.0)
                            .color(muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            version_label(ui, muted);
                        });
                        return;
                    };
                    let live = bar.status.is_live();
                    let color = match &bar.status {
                        Status::Running => good,
                        Status::Connecting => warn,
                        Status::Failed(_) => bad,
                        Status::Closed(_) => idle,
                    };
                    theme::dot(ui, color);
                    ui.label(egui::RichText::new(bar.status.label()).size(12.0).color(color));
                    ui.separator();
                    if bar.remote {
                        ui.label(egui::RichText::new(bar.target).size(12.0).color(muted));
                        ui.separator();
                    }
                    ui.label(egui::RichText::new(bar.size).size(12.0).color(muted));
                    if bar.tunnels > 0 {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(t!("{0} tunnel(s)", bar.tunnels))
                                .size(12.0)
                                .color(muted),
                        );
                    }
                    ui.separator();
                    // The monitor takes whatever room is left and clips rather than
                    // sliding under the version label.
                    let width = (ui.available_width() - 56.0).max(80.0);
                    ui.allocate_ui(Vec2::new(width, ui.available_height()), |ui| {
                        if !bar.monitor {
                            return;
                        }
                        match bar.info.as_ref() {
                            Some(info) => monitor_bar(ui, info),
                            None if live => {
                                ui.label(
                                    egui::RichText::new(t!("reading server stats\u{2026}"))
                                        .size(11.5)
                                        .color(muted),
                                );
                            }
                            None => {}
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        version_label(ui, muted);
                    });
                });
            });
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() {
            return;
        }
        let (good, warn, bad, idle) = theme::tones(ui);
        let surface = ui.visuals().panel_fill;
        let mut select = None;
        let mut close = None;
        let mut rename = None;
        egui::Frame::new()
            .fill(surface)
            .inner_margin(Margin {
                top: 7,
                bottom: 0,
                left: 0,
                right: 0,
            })
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for (index, tab) in self.tabs.iter().enumerate() {
                                let title = tab.title();
                                let short = crate::util::truncate_chars(&title, 24);
                                let label = format!("{} {}", tab.session.protocol.icon(), short);
                                let status = tab.status.lock().unwrap().clone();
                                let dot = match &status {
                                    Status::Running => good,
                                    Status::Connecting => warn,
                                    Status::Failed(_) => bad,
                                    Status::Closed(_) => idle,
                                };
                                let (response, close_button) =
                                    tab_button(ui, &label, dot, index == self.active, index);
                                if close_button.clicked() {
                                    close = Some(index);
                                } else if response.clicked() {
                                    select = Some(index);
                                }
                                let menu = |ui: &mut Ui,
                                            rename: &mut Option<usize>,
                                            close: &mut Option<usize>| {
                                    if ui.button(t!("Rename tab")).clicked() {
                                        *rename = Some(index);
                                        ui.close();
                                    }
                                    if ui.button(t!("Close tab\tCtrl+Shift+W")).clicked() {
                                        *close = Some(index);
                                        ui.close();
                                    }
                                };
                                // A right-click answers on either half of the tab.
                                close_button.context_menu(|ui| menu(ui, &mut rename, &mut close));
                                response.context_menu(|ui| menu(ui, &mut rename, &mut close));
                            }
                            if theme::ghost(ui, t!("+"))
                                .on_hover_text(t!("New local terminal"))
                                .clicked()
                            {
                                select = Some(usize::MAX);
                            }
                        });
                    });
                theme::hairline(ui);
            });

        if select == Some(usize::MAX) {
            let session = self.store.config.new_session(Protocol::Local);
            self.launch(session);
            return;
        }
        if let Some(index) = select {
            self.active = index;
        }
        if let Some(index) = rename {
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.session.name = format!("{} *", tab.session.name);
            }
        }
        if let Some(index) = close {
            self.close_tab(index);
        }
    }

    fn tab_content(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() || self.active >= self.tabs.len() {
            if self.tabs.is_empty() {
                self.welcome(ui);
                return;
            }
            self.active = 0;
        }
        let index = self.active;
        let show_browser = self.tabs[index].show_browser;
        let kind = self.tabs[index].kind;

        if show_browser && kind == TabKind::Terminal {
            let queue = self.queue.clone();
            let width = self.store.config.sftp_width;
            // The browser docks beside the terminal, on the right.
            let dock = egui::Panel::right("sftp-panel")
                .resizable(true)
                .default_size(width)
                .size_range(260.0..=900.0)
                .show(ui, |ui| {
                    // The dock keeps the width the user left it at: the browser
                    // is clipped to it instead of asking for a wider one.
                    crate::ui::fill_available(ui, |ui| self.browser_pane(ui, index, &queue));
                });
            let docked = dock.response.rect.width().round();
            if (self.store.config.sftp_width - docked).abs() > 1.0 {
                self.store.config.sftp_width = docked;
            }
        }

        match kind {
            TabKind::Terminal => {
                let palette = self.palette.clone();
                let font = self.font.clone();
                let grid = self.grid;
                let cursor_on = self.cursor_on;
                self.terminal_pane(ui, index, &palette, &font, grid, cursor_on);
            }
            TabKind::Sftp => {
                let queue = self.queue.clone();
                self.browser_pane(ui, index, &queue);
            }
            TabKind::Scp => {
                let queue = self.queue.clone();
                self.scp_pane(ui, index, &queue);
            }
        }
    }

    fn terminal_pane(
        &mut self,
        ui: &mut Ui,
        index: usize,
        palette: &Palette,
        font: &FontId,
        grid: Grid,
        cursor_on: bool,
    ) {
        // The server can repaint the palette through OSC 4/10/11/12.
        let palette = self.tabs[index]
            .emu
            .lock()
            .unwrap()
            .effective_palette(palette);
        let style = TermStyle {
            font: font.clone(),
            cell: grid.cell,
            leading: grid.leading,
            palette: &palette,
            cursor_on,
            pad: 10.0,
            radius: 11.0,
            border: theme::border(ui),
        };
        let menu_open = self.term_menu.is_some_and(|(_, tab)| tab == index);
        let tab = &mut self.tabs[index];
        let out = term_widget::show(ui, &tab.emu, &tab.backend, &mut tab.view, &style, menu_open);

        if let Some((rows, cols)) = out.resized {
            if let Some(backend) = tab.backend.lock().unwrap().as_ref() {
                backend.resize(rows, cols);
            }
        }
        if let Some(text) = out.copy {
            ui.ctx().copy_text(text);
        }
        if let Some(pos) = out.menu_at {
            self.term_menu = Some((pos, index));
            self.term_menu_fresh = true;
        }

        let (resize, osc52) = {
            let mut emu = tab.emu.lock().unwrap();
            (emu.pending_resize.take(), emu.clipboard_request.take())
        };
        if let Some((rows, cols)) = resize {
            if let Some(backend) = tab.backend.lock().unwrap().as_ref() {
                backend.resize(rows, cols);
            }
        }
        if let Some(text) = osc52 {
            ui.ctx().copy_text(text);
        }

        let bell = tab.emu.lock().unwrap().bell;
        if bell != tab.bell_seen {
            tab.bell_seen = bell;
            tab.flash = ui.input(|i| i.time);
        }
    }

    fn browser_pane(&mut self, ui: &mut Ui, index: usize, queue: &Queue) {
        let conn = self.tabs[index].conn.lock().unwrap().clone();
        let status = self.tabs[index].status.lock().unwrap().clone();
        if let Some(browser) = self.tabs[index].browser.as_mut() {
            if conn.is_none() {
                let (muted, accent) = (theme::muted(ui), theme::accent(ui));
                theme::card_with(ui, 10).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(
                            egui::RichText::new(t!("SFTP browser - {0}", status.label()))
                                .size(12.0)
                                .color(if status.is_live() { muted } else { accent }),
                        );
                    });
                });
                ui.add_space(6.0);
            }
            browser.ui(ui, conn.as_ref(), queue);
        } else {
            let mut browser = Browser::new();
            browser.ui(ui, conn.as_ref(), queue);
            self.tabs[index].browser = Some(browser);
        }
    }

    fn scp_pane(&mut self, ui: &mut Ui, index: usize, queue: &Queue) {
        let conn = self.tabs[index].conn.lock().unwrap().clone();
        let status = self.tabs[index].status.lock().unwrap().clone();
        let session = self.tabs[index].session.clone();
        let (good, warn, bad, idle) = theme::tones(ui);
        let (muted, accent) = (theme::muted(ui), theme::accent(ui));
        let color = match &status {
            Status::Running => good,
            Status::Connecting => warn,
            Status::Failed(_) => bad,
            Status::Closed(_) => idle,
        };
        theme::card_with(ui, 10).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                theme::dot(ui, color);
                ui.label(
                    egui::RichText::new(t!("SCP"))
                        .size(12.5)
                        .strong()
                        .color(accent),
                );
                ui.label(egui::RichText::new(status.label()).size(12.0).color(color));
                ui.separator();
                ui.label(
                    egui::RichText::new(session.target())
                        .size(12.0)
                        .color(muted),
                );
                ui.label(
                    egui::RichText::new(format!("{:?}", session.scp_direction))
                        .size(12.0)
                        .color(muted),
                );
                if !session.remote_path.is_empty() {
                    ui.monospace(session.remote_path);
                }
                if !session.local_path.is_empty() {
                    ui.monospace(session.local_path);
                }
            });
        });
        ui.add_space(8.0);
        let jobs = self.queue.jobs();
        if jobs.is_empty() {
            ui.weak(t!(
                "Nothing queued. Fill in the remote and local paths in the session properties."
            ));
        } else {
            ui.horizontal_wrapped(|ui| {
                for job in jobs {
                    let state = job.snapshot();
                    theme::meter(
                        ui,
                        state.percent() / 100.0,
                        190.0,
                        format!("{} {}", job.direction.arrow(), state.current),
                        accent,
                    );
                }
            });
        }
        if conn.is_none() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(13.0));
                ui.label(
                    egui::RichText::new(t!("connecting..."))
                        .size(12.0)
                        .color(theme::muted(ui)),
                );
            });
        }
        ui.separator();
        self.browser_pane(ui, index, queue);
    }
    fn welcome(&mut self, ui: &mut Ui) {
        let v = ui.visuals();
        let (raised, border, hover, muted, text) = (
            v.window_fill,
            v.widgets.noninteractive.bg_stroke.color,
            v.widgets.hovered.weak_bg_fill,
            theme::muted(ui),
            v.text_color(),
        );
        // The card is a fixed width, so it has to be pushed to the middle of
        // the panel by hand: a left-to-right layout would otherwise park it
        // against the sidebar.
        let full = ui.available_width();
        let width = full.min(600.0);
        let indent = ((full - width) / 2.0).max(0.0);

        let mut action: Option<u8> = None;
        let rows: [(&str, &str, &str); 3] = [
            (
                "\u{25b6}",
                t!("New local terminal"),
                t!("A local shell tab, ready in a blink."),
            ),
            (
                "\u{1f5a5}",
                t!("New SSH session"),
                t!("Password, key or agent - the SFTP pane follows."),
            ),
            (
                "\u{2699}",
                t!("Settings"),
                t!("Theme, colours, fonts and session defaults."),
            ),
        ];
        // The card is content-sized, so its height is known up front and the
        // whole block - card plus the two hint lines - can be centred.
        let card_h =
            36.0 + 44.0 + 18.0 + rows.len() as f32 * 58.0 + (rows.len() as f32 - 1.0) * 8.0;
        let slack = (ui.available_height() - card_h - 62.0).max(0.0) / 2.0;
        ui.add_space(slack.max(12.0));
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.allocate_ui_with_layout(
                Vec2::new(width, card_h),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    egui::Frame::new()
                        .fill(raised)
                        .stroke((1.0, border))
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(Margin::same(18))
                        .show(ui, |ui| {
                            ui.set_width(width - 36.0);
                            ui.horizontal(|ui| {
                                theme::logo(ui, 44.0);
                                ui.add_space(6.0);
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(t!("ImGoSSH"))
                                                .size(22.0)
                                                .strong()
                                                .color(text),
                                        );
                                        let version = format!("v{}", crate::config::APP_VERSION);
                                        theme::badge(ui, &version, muted, hover);
                                    });
                                    ui.label(
                                        egui::RichText::new(t!(
                                            "SSH / SFTP / SCP workstation written in Rust."
                                        ))
                                        .size(12.5)
                                        .color(muted),
                                    );
                                });
                            });
                            ui.add_space(18.0);
                            for (index, (glyph, title, detail)) in rows.iter().enumerate() {
                                if welcome_row(ui, width - 36.0, glyph, title, detail) {
                                    action = Some(index as u8);
                                }
                                if index + 1 < rows.len() {
                                    ui.add_space(8.0);
                                }
                            }
                        });
                },
            );
        });

        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            let hint = if self.store.sessions.is_empty() {
                t!("Create a session with the + buttons at the bottom of the sessions panel.").to_string()
            } else {
                t!(
                    "{0} saved session(s) - double-click one in the sidebar to connect.",
                    self.store.sessions.len()
                )
            };
            ui.label(egui::RichText::new(hint).size(12.0).color(muted));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(t!(
                    "Ctrl+Shift+T new terminal - Ctrl+Shift+N new session - Ctrl+Shift+B file browser"
                ))
                .size(11.0)
                .color(muted),
            );
        });

        match action {
            Some(0) => {
                let session = self.store.config.new_session(Protocol::Local);
                self.launch(session);
            }
            Some(1) => {
                let session = self.store.config.new_session(Protocol::Ssh);
                self.editor = Some(
                    dialogs::SessionEditor::new(session, t!("New session"))
                        .with_store_passwords(self.store.config.store_passwords),
                );
                self.editor_index = None;
            }
            Some(2) => self.show_settings = true,
            _ => {}
        }
    }
}

/// The version, kept out of the way in a corner.
fn version_label(ui: &mut Ui, color: Color32) {
    ui.label(
        egui::RichText::new(format!("v{}", crate::config::APP_VERSION))
            .size(11.0)
            .color(color),
    );
}

/// One tab in the strip: a status light, the title and a close box.
///
/// Painted by hand so the selected tab can merge into the panel below it and
/// the close box can show up only on the tab the pointer is on. The close box
/// is its own interactable: it lives inside the tab, and egui hands a click to
/// the box laid out last under the pointer, which is the only way to tell the
/// two apart. A press origin will not do - egui clears it the moment the
/// button is released, so the very frame a click is reported has no origin.
fn tab_button(
    ui: &mut Ui,
    label: &str,
    dot: Color32,
    selected: bool,
    index: usize,
) -> (Response, Response) {
    let v = ui.visuals();
    let (raised, hover, accent, muted, text) = (
        v.window_fill,
        v.widgets.hovered.weak_bg_fill,
        theme::accent(ui),
        theme::muted(ui),
        v.text_color(),
    );
    let font = FontId::proportional(12.5);
    let wanted = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), text)
        .size()
        .x;
    let width = (wanted + 58.0).clamp(112.0, 232.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 29.0), Sense::click());
    let close = Rect::from_min_size(
        Pos2::new(rect.right() - 22.0, rect.center().y - 9.0),
        Vec2::splat(17.0),
    );
    let close_button = ui.interact(close, ui.id().with(("tab-close", index)), Sense::click());
    let lit = response.hovered() || close_button.hovered();
    let top = egui::CornerRadius {
        nw: 9,
        ne: 9,
        sw: 0,
        se: 0,
    };
    if selected {
        ui.painter().rect_filled(rect, top, raised);
    } else if lit {
        ui.painter().rect_filled(rect.shrink(1.0), top, hover);
    }
    if selected {
        let bar = Rect::from_min_size(
            Pos2::new(rect.left() + 7.0, rect.bottom() - 2.5),
            Vec2::new(rect.width() - 14.0, 2.0),
        );
        ui.painter().rect_filled(bar, CornerRadius::same(1), accent);
    }
    ui.painter().circle_filled(
        Pos2::new(rect.left() + 12.0, rect.center().y - 1.0),
        3.5,
        dot,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 22.0, rect.center().y - 1.0),
        Align2::LEFT_CENTER,
        label,
        font,
        if selected { text } else { muted },
    );
    if lit || selected {
        if close_button.hovered() {
            ui.painter().circle_filled(close.center(), 8.5, hover);
        }
        ui.painter().text(
            close.center(),
            Align2::CENTER_CENTER,
            "\u{2715}",
            FontId::proportional(10.5),
            if close_button.hovered() { text } else { muted },
        );
    }
    (
        response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(label),
        close_button
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(t!("Close tab")),
    )
}

/// One row of the welcome card: a glyph tile, a title and a one-line pitch.
///
/// It is painted rather than built out of widgets so the whole row is a single
/// hit target, and the accent border only shows when the pointer is on it.
fn welcome_row(ui: &mut Ui, width: f32, glyph: &str, title: &str, detail: &str) -> bool {
    let v = ui.visuals();
    let (border, hover_border, face, hover_face, accent_dim, accent_text, muted, text) = (
        v.widgets.noninteractive.bg_stroke.color,
        theme::accent(ui),
        v.widgets.inactive.weak_bg_fill,
        v.widgets.hovered.weak_bg_fill,
        theme::accent_dim(ui),
        theme::accent_text(ui),
        theme::muted(ui),
        v.text_color(),
    );
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 58.0), Sense::click());
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        if hovered { hover_face } else { face },
        (1.0, if hovered { hover_border } else { border }),
        StrokeKind::Inside,
    );
    let tile = Rect::from_center_size(
        Pos2::new(rect.left() + 29.0, rect.center().y),
        Vec2::splat(32.0),
    );
    ui.painter()
        .rect_filled(tile, CornerRadius::same(10), accent_dim);
    ui.painter().text(
        tile.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(16.0),
        accent_text,
    );
    let x = tile.right() + 13.0;
    ui.painter().text(
        Pos2::new(x, rect.center().y - 10.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.0),
        text,
    );
    ui.painter().text(
        Pos2::new(x, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        detail,
        FontId::proportional(11.5),
        muted,
    );
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(title)
        .clicked()
}

/// Turn the stored setting into the language to use right now.
pub fn resolve_lang(setting: &str) -> Lang {
    Lang::parse(setting).unwrap_or_else(i18n::detect)
}

/// Everything the status bar needs, lifted out of the active tab up front so
/// drawing it never has to hold a borrow of the tab list.
struct SessionBar {
    status: Status,
    size: String,
    tunnels: usize,
    target: String,
    remote: bool,
    monitor: bool,
    info: Option<ServerInfo>,
}

/// The live server read-out: compact pills that stay quiet until a number
/// is worth worrying about. Host, CPU, memory, traffic, uptime, sessions
/// and disks, laid out left to right in that order.
fn monitor_bar(ui: &mut Ui, info: &ServerInfo) {
    let (good, warn, bad, dim) = theme::tones(ui);
    let accent = theme::accent(ui);
    let tone = |percent: f64| {
        if percent >= 90.0 {
            bad
        } else if percent >= 75.0 {
            warn
        } else {
            good
        }
    };
    let load = format!(
        "load {:.2} / {:.2} / {:.2}",
        info.load[0], info.load[1], info.load[2]
    );
    let note = |ui: &mut Ui, text: String| -> Response {
        ui.label(egui::RichText::new(text).size(11.5).color(dim))
    };

    egui::ScrollArea::horizontal()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if !info.hostname.is_empty() {
                    let mut hover = load.clone();
                    if !info.kernel.is_empty() {
                        hover = format!("{}{}\n{}", info.os, info.kernel, hover);
                    }
                    ui.label(
                        egui::RichText::new(format!("{} {}", "\u{1f5a5}", info.hostname))
                            .size(11.5)
                            .color(accent),
                    )
                    .on_hover_text(hover);
                    ui.separator();
                }
                theme::meter(
                    ui,
                    (info.cpu / 100.0) as f32,
                    86.0,
                    t!("CPU {0}%", format!("{:.0}", info.cpu)),
                    tone(info.cpu),
                )
                .on_hover_text(load);
                if info.mem_total > 0 {
                    let percent = info.mem_used as f64 / info.mem_total as f64 * 100.0;
                    theme::meter(
                        ui,
                        (percent / 100.0) as f32,
                        140.0,
                        format!(
                            "{} / {}",
                            human_size(info.mem_used),
                            human_size(info.mem_total)
                        ),
                        tone(percent),
                    )
                    .on_hover_text(t!("{0}% of RAM in use", format!("{:.0}", percent)));
                }
                if info.swap_total > 0 {
                    let percent = info.swap_used as f64 / info.swap_total as f64 * 100.0;
                    theme::meter(
                        ui,
                        (percent / 100.0) as f32,
                        116.0,
                        format!(
                            "swap {} / {}",
                            human_size(info.swap_used),
                            human_size(info.swap_total)
                        ),
                        tone(percent),
                    );
                }
                ui.separator();
                note(ui, format!("\u{2191} {}", human_rate(info.tx)))
                    .on_hover_text(t!("Sent since the previous sample"));
                note(ui, format!("\u{2193} {}", human_rate(info.rx)))
                    .on_hover_text(t!("Received since the previous sample"));
                ui.separator();
                note(ui, format!("\u{23f1} {}", human_uptime(info.uptime)))
                    .on_hover_text(t!("Uptime"));
                ui.separator();
                if !info.users.is_empty() {
                    note(ui, format!("\u{1f465} {}", info.users.join(" ")))
                        .on_hover_text(t!("Logged-in accounts"));
                    ui.separator();
                }
                for disk in &info.disks {
                    ui.label(
                        egui::RichText::new(format!("{} {:.0}%", disk.mount, disk.percent))
                            .size(11.5)
                            .color(tone(disk.percent)),
                    )
                    .on_hover_text(t!(
                        "{0} free of {1} ({2}% used)",
                        human_size(disk.free),
                        human_size(disk.total),
                        format!("{:.0}", disk.percent)
                    ));
                }
            });
        });
}
fn open_in_file_manager(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
// ---------------------------------------------------------------------------
// Windows and modals
// ---------------------------------------------------------------------------

impl App {
    /// Everything that floats above the panels.
    fn dialogs(&mut self, ctx: &Context) {
        let mut close_transfers = false;
        let mut close_about = false;
        egui::Area::new(egui::Id::new("dialogs"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                self.session_editor(ui);
                self.name_prompt_ui(ui);
                self.pending_ui(ui);
                self.confirm_close_ui(ui);
                self.term_menu_ui(ui);

                if self.show_settings {
                    let mut settings_open = true;
                    if dialogs::settings_dialog(ui, &mut self.store.config, &mut settings_open) {
                        self.dirty = true;
                    }
                    if !settings_open {
                        self.show_settings = false;
                    }
                }
                if self.show_transfers && dialogs::transfers_dialog(ui, &self.queue) {
                    close_transfers = true;
                }
                if self.show_about {
                    close_about = dialogs::about_dialog(ui);
                }
            });
        if close_transfers {
            self.show_transfers = false;
        }
        if close_about {
            self.show_about = false;
        }

        if self.dirty {
            if let Err(err) = self.store.save() {
                log::warn!("saving settings: {err}");
            }
            self.dirty = false;
        }
    }
    fn session_editor(&mut self, ui: &mut Ui) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        match editor.show(ui) {
            dialogs::EditorOutcome::Keep => {}
            dialogs::EditorOutcome::Save(session) => {
                match self.editor_index {
                    Some(index) => {
                        if let Some(existing) = self.store.sessions.get_mut(index) {
                            *existing = session;
                        }
                    }
                    None => self.store.sessions.push(session),
                }
                self.editor = None;
                self.editor_index = None;
                self.dirty = true;
            }
            dialogs::EditorOutcome::Cancel => {
                self.editor = None;
                self.editor_index = None;
            }
        }
    }

    fn name_prompt_ui(&mut self, ui: &mut Ui) {
        let Some(prompt) = &mut self.name_prompt else {
            return;
        };
        let title = prompt.title.clone();
        let mut ok = false;
        let mut cancel = false;
        let mut open = true;
        egui::Window::new(title)
            .id(egui::Id::new("name-prompt"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(320.0);
                ui.horizontal(|ui| {
                    ui.label(t!("Name:"));
                    let field =
                        ui.add_sized([230.0, 0.0], egui::TextEdit::singleline(&mut prompt.value));
                    if !field.has_focus() {
                        field.request_focus();
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ok = true;
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::primary(ui, t!("OK")).clicked() {
                        ok = true;
                    }
                    if ui.button(t!("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel || !open {
            self.name_prompt = None;
            return;
        }
        if !ok {
            return;
        }

        let Some(done) = self.name_prompt.take() else {
            return;
        };
        let value = done.value.trim().to_string();
        match done.kind {
            NameKind::NewFolder => {
                let mut session = self.store.config.new_session(Protocol::Ssh);
                session.folder = value;
                self.editor = Some(
                    dialogs::SessionEditor::new(session, t!("New session"))
                        .with_store_passwords(self.store.config.store_passwords),
                );
                self.editor_index = None;
            }
            NameKind::RenameFolder(old) => {
                if !value.is_empty() && value != old {
                    self.store.rename_folder(&old, &value);
                    self.dirty = true;
                }
            }
        }
    }
    /// The credential / host-key question the engine is waiting on.
    fn pending_ui(&mut self, ui: &mut Ui) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let answer = match &pending.request.challenge {
            Challenge::HostKey(info) => {
                let info = info.clone();
                dialogs::host_key_dialog(ui, &info).map(Reply::TrustKey)
            }
            other => {
                let other = other.clone();
                dialogs::challenge_dialog(ui, &other, &mut pending.buffer)
            }
        };
        if let Some(reply) = answer {
            let pending = self.pending.take().expect("pending just checked");
            let _ = pending.request.reply.send(reply);
        }
    }

    fn confirm_close_ui(&mut self, ui: &mut Ui) {
        let Some(index) = self.confirm_close else {
            return;
        };
        let mut decision: Option<bool> = None;
        let mut open = true;
        egui::Window::new(t!("Close this session?"))
            .id(egui::Id::new("confirm-close"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(300.0);
                ui.label(t!("The connection is still open. Close the tab anyway?"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::danger(ui, t!("Close")).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(t!("Keep")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(yes) = decision {
            self.confirm_close = None;
            if yes {
                self.close_tab(index);
            }
        } else if !open {
            self.confirm_close = None;
        }
    }

    fn term_menu_ui(&mut self, ui: &mut Ui) {
        let fresh = std::mem::take(&mut self.term_menu_fresh);
        let Some((pos, index)) = self.term_menu else {
            return;
        };
        let mut action: Option<u8> = None;
        let response = egui::Window::new("Terminal")
            .id(egui::Id::new("terminal-menu"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .constrain(false)
            .current_pos(pos)
            .show(ui.ctx(), |ui| {
                if ui.button(t!("Copy\tCtrl+Shift+C")).clicked() {
                    action = Some(0);
                }
                if ui.button(t!("Paste\tCtrl+Shift+V")).clicked() {
                    action = Some(1);
                }
                ui.separator();
                if ui.button(t!("Select all")).clicked() {
                    action = Some(2);
                }
                if ui.button(t!("Scroll to bottom")).clicked() {
                    action = Some(3);
                }
                ui.separator();
                if ui.button(t!("Toggle SFTP browser")).clicked() {
                    action = Some(4);
                }
                if ui.button(t!("Close tab\tCtrl+Shift+W")).clicked() {
                    action = Some(5);
                }
            });
        let Some(inner) = response else { return };
        let rect = inner.response.rect;

        // A menu that has been used, dismissed or escaped out of is a menu that
        // should be gone, and the grid wants its keyboard back: the menu button
        // is focusable, so it took the focus when the click landed on it.
        let pressed_outside = ui.input(|i| i.pointer.any_pressed())
            && ui
                .input(|i| i.pointer.press_origin())
                .is_some_and(|p| !rect.contains(p));
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if menu_is_dismissed(action, pressed_outside, escape, fresh) {
            self.term_menu = None;
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.view.want_focus = true;
            }
        }

        match action {
            Some(0) => self.copy_selection(ui.ctx()),
            Some(1) => self.paste_clipboard(),
            Some(2) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    let (rows, cols) = tab.emu.lock().unwrap().size();
                    tab.view.selection = Some(term_widget::Selection {
                        start: (0, 0),
                        end: (rows.saturating_sub(1), cols.saturating_sub(1)),
                    });
                }
            }
            Some(3) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.emu.lock().unwrap().scroll_to_bottom();
                }
            }
            Some(4) => {
                self.active = index;
                self.toggle_browser();
            }
            Some(5) => self.close_tab(index),
            _ => {}
        }
    }
}

/// Should the open terminal menu be gone by the end of this frame?
///
/// Choosing an item, clicking anywhere else and Escape all dismiss it, and the
/// dismissal happens on the press rather than the release so the menu vanishes
/// the instant the user commits to something. The frame that opened the menu is
/// exempt: a right-click pressed and released between two repaints reports its
/// origin a pixel or two away from where it landed, and the menu is placed at
/// the pointer, so that click would otherwise close the menu before it is ever
/// drawn.
fn menu_is_dismissed(
    action: Option<u8>,
    pressed_outside: bool,
    escape: bool,
    opened_this_frame: bool,
) -> bool {
    action.is_some() || (!opened_this_frame && (pressed_outside || escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The coverage probe is the whole basis for choosing a face, so it has to see
    // the glyphs a face does own. Asking `has_glyph` instead reported none of
    // them, which quietly left the terminal on the embedded fallback no matter
    // what a machine had installed.
    #[test]
    fn the_coverage_probe_sees_the_glyphs_a_face_owns() {
        // egui's own face ships with the binary, so this says nothing about
        // whatever fonts the machine running the test happens to have.
        let embedded = FontDefinitions::default();
        let hack = &embedded.font_data["Hack"];
        let covered = grid_coverage("Hack", hack);
        assert!(covered > 0, "the probe sees no glyph in the embedded face");
        assert!(
            covered < grid_glyphs().len(),
            "the probe claims the embedded face paints every grid glyph"
        );
    }

    /// A terminal face that has to borrow grid glyphs from a fallback paints
    /// them at another advance, and the columns drift. Either the face covers the
    /// whole grid, or egui's own embedded one - which is the same on every machine
    /// - is left to paint it.
    #[test]
    fn the_terminal_font_covers_the_grid_or_defers_to_the_embedded_one() {
        let mut fonts = FontDefinitions::default();
        install_terminal_font(&mut fonts, "");
        let head = fonts.families[&FontFamily::Monospace]
            .first()
            .cloned()
            .expect("the monospace family must not be empty");
        let covered = grid_coverage(&head, &fonts.font_data[&head]) == grid_glyphs().len();
        assert!(
            covered || head == "Hack",
            "{head} cannot paint the terminal grid"
        );
    }

    fn mods(ctrl: bool, shift: bool, alt: bool) -> egui::Modifiers {
        egui::Modifiers {
            ctrl,
            shift,
            alt,
            mac_cmd: false,
            command: false,
        }
    }

    /// A shell binds almost every bare `Ctrl` chord, so the application must
    /// leave them alone.
    #[test]
    fn bare_ctrl_chords_reach_the_shell() {
        for key in [
            egui::Key::T,
            egui::Key::N,
            egui::Key::W,
            egui::Key::A,
            egui::Key::B,
            egui::Key::C,
            egui::Key::H,
            egui::Key::K,
            egui::Key::L,
            egui::Key::R,
            egui::Key::U,
            egui::Key::Z,
        ] {
            assert_eq!(
                match_shortcut(key, mods(true, false, false)),
                None,
                "Ctrl+{key:?}"
            );
        }
    }

    #[test]
    fn application_shortcuts_use_ctrl_shift_or_alt() {
        use egui::Key as K;
        assert!(matches!(
            match_shortcut(K::T, mods(true, true, false)),
            Some(Shortcut::NewTab)
        ));
        assert!(matches!(
            match_shortcut(K::W, mods(true, true, false)),
            Some(Shortcut::CloseTab)
        ));
        assert!(matches!(
            match_shortcut(K::C, mods(true, true, false)),
            Some(Shortcut::Copy)
        ));
        assert!(matches!(
            match_shortcut(K::V, mods(true, true, false)),
            Some(Shortcut::Paste)
        ));
        assert!(matches!(
            match_shortcut(K::R, mods(true, true, false)),
            Some(Shortcut::Reconnect)
        ));
        assert!(matches!(
            match_shortcut(K::Tab, mods(true, false, false)),
            Some(Shortcut::NextTab)
        ));
        assert!(matches!(
            match_shortcut(K::Tab, mods(true, true, false)),
            Some(Shortcut::PrevTab)
        ));
        assert!(matches!(
            match_shortcut(K::Num3, mods(false, false, true)),
            Some(Shortcut::GotoTab(2))
        ));
        assert!(matches!(
            match_shortcut(K::Num0, mods(false, false, true)),
            Some(Shortcut::GotoTab(9))
        ));
    }

    /// Alt alone is how readline moves by words; only arrows and digits are ours.
    #[test]
    fn alt_letters_are_left_to_the_shell() {
        for key in [egui::Key::B, egui::Key::F, egui::Key::D, egui::Key::T] {
            assert_eq!(
                match_shortcut(key, mods(false, false, true)),
                None,
                "Alt+{key:?}"
            );
        }
    }

    /// The menu closes the moment it is used, and never survives a click that
    /// did not open it.
    #[test]
    fn the_terminal_menu_closes_as_soon_as_it_is_used() {
        for action in [Some(0u8), Some(5)] {
            assert!(
                menu_is_dismissed(action, false, false, true),
                "choosing an item must close the menu even on its opening frame"
            );
        }
        assert!(
            menu_is_dismissed(None, true, false, false),
            "a click outside closes the menu"
        );
        assert!(
            menu_is_dismissed(None, false, true, false),
            "Escape closes the menu"
        );
        assert!(
            !menu_is_dismissed(None, false, false, false),
            "an idle frame keeps the menu open"
        );
        assert!(
            !menu_is_dismissed(None, true, true, true),
            "the right-click that opened the menu must not close it again"
        );
    }
}
