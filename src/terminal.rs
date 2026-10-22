//! Terminal emulation: a thin, UI-agnostic layer on top of the `vt100` parser.
//!
//! Responsibilities:
//!
//! * feed remote bytes into the VT100 state machine and expose the cell grid;
//! * answer the queries `vt100` deliberately does not implement (DSR, DA, OSC 10/11);
//! * turn the DEC special graphics bytes a server prints into box-drawing
//!   characters (see [`line_drawing`]);
//! * remember the colours a server asks for through OSC 4/10/11/12;
//! * turn key / mouse events into the escape sequences a server expects;
//! * keep the window title, bell counter and scrollback offset.

use base64::Engine as _;
use vt100::{MouseProtocolEncoding, MouseProtocolMode, Screen};

use crate::config::Palette;
use crate::line_drawing::CharsetFilter;

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------

/// Captures the escape sequences that change terminal state rather than cells.
#[derive(Debug, Default, Clone)]
pub struct TermCallbacks {
    title: Option<String>,
    bell: bool,
    resize: Option<(u16, u16)>,
    clipboard: Option<String>,
    colors: Vec<ColorChange>,
}

impl vt100::Callbacks for TermCallbacks {
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bell = true;
    }

    fn visual_bell(&mut self, _: &mut Screen) {
        self.bell = true;
    }

    fn resize(&mut self, _: &mut Screen, request: (u16, u16)) {
        self.resize = Some(request);
    }

    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }

    fn set_window_icon_name(&mut self, _: &mut Screen, name: &[u8]) {
        self.title = Some(String::from_utf8_lossy(name).into_owned());
    }

    fn copy_to_clipboard(&mut self, _: &mut Screen, _ty: &[u8], data: &[u8]) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            self.clipboard = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
    }

    fn unhandled_osc(&mut self, _: &mut Screen, params: &[&[u8]]) {
        self.colors.extend(parse_color_osc(params));
    }
}

// ---------------------------------------------------------------------------
// Server colours
// ---------------------------------------------------------------------------

/// A colour the server asked the terminal to start or stop using.
///
/// `None` means "back to the palette", which is what `OSC 104` and friends ask
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChange {
    /// `OSC 4 ; index ; spec` - one entry of the 16 colour ANSI palette.
    Ansi(u8, Option<[u8; 3]>),
    /// `OSC 10 ; spec` - the default foreground.
    Foreground(Option<[u8; 3]>),
    /// `OSC 11 ; spec` - the default background.
    Background(Option<[u8; 3]>),
    /// `OSC 12 ; spec` - the cursor.
    Cursor(Option<[u8; 3]>),
}

/// Colours the server has painted over the configured palette.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerColors {
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    cursor: Option<[u8; 3]>,
    ansi: [Option<[u8; 3]>; 16],
}

impl ServerColors {
    fn apply(&mut self, changes: impl IntoIterator<Item = ColorChange>) {
        for change in changes {
            match change {
                ColorChange::Ansi(index, rgb) => {
                    if let Some(slot) = self.ansi.get_mut(index as usize) {
                        *slot = rgb;
                    }
                }
                ColorChange::Foreground(rgb) => self.foreground = rgb,
                ColorChange::Background(rgb) => self.background = rgb,
                ColorChange::Cursor(rgb) => self.cursor = rgb,
            }
        }
    }

    /// The palette to paint with: `base` with every override laid on top.
    fn resolve(&self, base: &Palette) -> Palette {
        let mut palette = base.clone();
        for (index, rgb) in self.ansi.iter().enumerate() {
            if let Some(rgb) = rgb {
                palette.ansi[index] = *rgb;
            }
        }
        if let Some(rgb) = self.foreground {
            palette.foreground = rgb;
        }
        if let Some(rgb) = self.background {
            palette.background = rgb;
        }
        if let Some(rgb) = self.cursor {
            palette.cursor = rgb;
        }
        palette
    }
}

/// Turn an OSC sequence into the colour changes it asks for.
///
/// A `?` asks us what a colour currently is; the query scanner answers those,
/// so here it means "no change".
fn parse_color_osc(params: &[&[u8]]) -> Vec<ColorChange> {
    let Some(code) = params.first().copied().and_then(decimal) else {
        return Vec::new();
    };
    let values = &params[1..];
    let one = |set: fn(Option<[u8; 3]>) -> ColorChange| match values {
        [] => vec![set(None)],
        [spec] if *spec == b"?" => Vec::new(),
        [spec] => parse_color(spec)
            .map(|rgb| vec![set(Some(rgb))])
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let reset_all = || {
        (0..16)
            .map(|index| ColorChange::Ansi(index, None))
            .collect()
    };
    match code {
        4 if values.is_empty() => reset_all(),
        4 => values
            .chunks(2)
            .filter_map(|pair| {
                let (index, spec) = (pair.first().copied()?, pair.get(1).copied()?);
                if spec == b"?" {
                    return None;
                }
                Some(ColorChange::Ansi(byte(index)?, Some(parse_color(spec)?)))
            })
            .collect(),
        10 => one(ColorChange::Foreground),
        11 => one(ColorChange::Background),
        12 => one(ColorChange::Cursor),
        104 if values.is_empty() => reset_all(),
        104 => values
            .iter()
            .copied()
            .filter_map(|index| Some(ColorChange::Ansi(byte(index)?, None)))
            .collect(),
        110 => vec![ColorChange::Foreground(None)],
        111 => vec![ColorChange::Background(None)],
        112 => vec![ColorChange::Cursor(None)],
        _ => Vec::new(),
    }
}

/// An unsigned decimal OSC parameter.
fn decimal(param: &[u8]) -> Option<u32> {
    if param.is_empty() || !param.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(param).ok()?.parse().ok()
}

/// A palette index, which has to fit in a byte to be worth applying.
fn byte(param: &[u8]) -> Option<u8> {
    u8::try_from(decimal(param)?).ok()
}

/// Read an X11 colour from an OSC parameter: `#rgb`, `#rrggbb`,
/// `#rrrrggggbbbb`, `rgb:rr/gg/bb` or `rgba:rr/gg/bb/aa`.
fn parse_color(spec: &[u8]) -> Option<[u8; 3]> {
    if let Some(hex) = spec.strip_prefix(b"#") {
        let width = match hex.len() {
            3 => 1,
            6 => 2,
            12 => 4,
            _ => return None,
        };
        let part = |i: usize| component(&hex[i * width..(i + 1) * width]);
        return Some([part(0)?, part(1)?, part(2)?]);
    }
    let body = spec
        .strip_prefix(b"rgb:")
        .or_else(|| spec.strip_prefix(b"rgba:"))?;
    let parts: Vec<&[u8]> = body.split(|c| *c == b'/').collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    Some([
        component(parts[0])?,
        component(parts[1])?,
        component(parts[2])?,
    ])
}

/// One 8-bit colour component written with 1, 2 or 4 hex digits.
fn component(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() == 3 || digits.len() > 4 {
        return None;
    }
    if !digits.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
    Some(match digits.len() {
        1 => (value * 0x11) as u8,
        2 => value as u8,
        _ => (value >> 8) as u8,
    })
}

// ---------------------------------------------------------------------------
// Query responses
// ---------------------------------------------------------------------------

/// A device query found in the outgoing byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// `CSI 6 n` - cursor position report.
    CursorPosition,
    /// `CSI 5 n` - is the terminal ready?
    Status,
    /// `CSI c` / `CSI ? c` - primary device attributes.
    PrimaryDa,
    /// `CSI > c` - secondary device attributes.
    SecondaryDa,
    /// `CSI 16 t` - pixel size of a character cell.
    CellSize,
    /// `CSI 18 t` - text area extent.
    ScreenSize,
    /// `OSC 10 ; ?` - foreground colour.
    Foreground,
    /// `OSC 11 ; ?` - background colour.
    Background,
}

/// Incremental scanner that spots the query sequences `vt100` ignores.
#[derive(Debug, Default)]
pub struct QueryScanner {
    state: ScanState,
    buf: Vec<u8>,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Normal,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

impl QueryScanner {
    /// Scan a chunk of server output, returning the queries it contains.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Query> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                ScanState::Normal => {
                    if b == 0x1b {
                        self.state = ScanState::Esc;
                        self.buf.clear();
                    }
                }
                ScanState::Esc => match b {
                    b'[' => {
                        self.state = ScanState::Csi;
                        self.buf.clear();
                    }
                    b']' => {
                        self.state = ScanState::Osc;
                        self.buf.clear();
                    }
                    _ => self.state = ScanState::Normal,
                },
                ScanState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        if let Some(q) = parse_csi(&self.buf, b) {
                            out.push(q);
                        }
                        self.state = ScanState::Normal;
                        self.buf.clear();
                    } else if self.buf.len() < 64 {
                        self.buf.push(b);
                    } else {
                        self.state = ScanState::Normal;
                        self.buf.clear();
                    }
                }
                ScanState::Osc => {
                    if b == 0x07 {
                        if let Some(q) = parse_osc(&self.buf) {
                            out.push(q);
                        }
                        self.state = ScanState::Normal;
                        self.buf.clear();
                    } else if b == 0x1b {
                        self.state = ScanState::OscEsc;
                    } else if self.buf.len() < 256 {
                        self.buf.push(b);
                    }
                }
                ScanState::OscEsc => {
                    if b == b'\\' {
                        if let Some(q) = parse_osc(&self.buf) {
                            out.push(q);
                        }
                        self.state = ScanState::Normal;
                        self.buf.clear();
                    } else {
                        self.state = ScanState::Osc;
                    }
                }
            }
        }
        out
    }

    pub fn reset(&mut self) {
        self.state = ScanState::Normal;
        self.buf.clear();
    }
}

fn parse_csi(params: &[u8], final_byte: u8) -> Option<Query> {
    let trimmed = trim_markers(params);
    match final_byte {
        b'n' if trimmed == b"6" => Some(Query::CursorPosition),
        b'n' if trimmed == b"5" => Some(Query::Status),
        b'c' if params.starts_with(b">") => Some(Query::SecondaryDa),
        b'c' => Some(Query::PrimaryDa),
        b't' if trimmed == b"16" => Some(Query::CellSize),
        b't' if trimmed == b"18" => Some(Query::ScreenSize),
        _ => None,
    }
}

fn parse_osc(body: &[u8]) -> Option<Query> {
    if let Some(rest) = body.strip_prefix(b"10;") {
        return rest.starts_with(b"?").then_some(Query::Foreground);
    }
    if let Some(rest) = body.strip_prefix(b"11;") {
        return rest.starts_with(b"?").then_some(Query::Background);
    }
    None
}

fn trim_markers(params: &[u8]) -> &[u8] {
    let markers = b"?<>=";
    let mut start = 0;
    while start < params.len() && markers.contains(&params[start]) {
        start += 1;
    }
    let mut end = params.len();
    while end > start && markers.contains(&params[end - 1]) {
        end -= 1;
    }
    &params[start..end]
}

/// Build the reply a server expects for `query`.
pub fn respond(
    query: Query,
    screen: &Screen,
    cell_px: (u16, u16),
    palette: &PaletteRgb,
) -> Vec<u8> {
    match query {
        Query::CursorPosition => {
            let (row, col) = screen.cursor_position();
            format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
        }
        Query::Status => b"\x1b[0n".to_vec(),
        // VT220 with sixel, underline, stretchy and rectangular selection.
        Query::PrimaryDa => b"\x1b[?62;1;2;6;9;15;22c".to_vec(),
        Query::SecondaryDa => b"\x1b[>1;10904000;0c".to_vec(),
        Query::CellSize => {
            let (w, h) = cell_px;
            format!("\x1b[6;{h};{w}t").into_bytes()
        }
        Query::ScreenSize => {
            let (rows, cols) = screen.size();
            format!("\x1b[8;{rows};{cols}t").into_bytes()
        }
        Query::Foreground => osc_color(10, palette.foreground),
        Query::Background => osc_color(11, palette.background),
    }
}

/// RGB triple advertised through OSC 10/11 replies.
#[derive(Debug, Clone, Copy)]
pub struct PaletteRgb {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
}

/// The reply xterm gives a colour query: the code that was asked about and
/// each component repeated into 16 bits.
fn osc_color(code: u8, rgb: [u8; 3]) -> Vec<u8> {
    let [r, g, b] = rgb;
    format!("\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07").into_bytes()
}

// ---------------------------------------------------------------------------
// Emulator
// ---------------------------------------------------------------------------

/// One terminal session's screen state.
pub struct Emulator {
    parser: vt100::Parser<TermCallbacks>,
    scanner: QueryScanner,
    charsets: CharsetFilter,
    /// Bumped on every visible change so the UI can skip idle repaints.
    pub version: u64,
    /// Incremented on `^G`; the UI flashes the tab when it changes.
    pub bell: u64,
    pub title: String,
    /// Rows scrolled up from the bottom (0 = live view).
    pub scroll_offset: usize,
    /// Set when the server asked us to resize (xterm `CSI 8 ; r ; c t`).
    pub pending_resize: Option<(u16, u16)>,
    /// Text the remote app copied via OSC 52.
    pub clipboard_request: Option<String>,
    base: PaletteRgb,
    colors: ServerColors,
}

impl Emulator {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows.max(1),
                cols.max(1),
                scrollback,
                TermCallbacks::default(),
            ),
            scanner: QueryScanner::default(),
            charsets: CharsetFilter::default(),
            version: 0,
            bell: 0,
            title: String::new(),
            scroll_offset: 0,
            pending_resize: None,
            clipboard_request: None,
            base: PaletteRgb {
                foreground: [0xd5, 0xd8, 0xdc],
                background: [0x14, 0x17, 0x1a],
            },
            colors: ServerColors::default(),
        }
    }

    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Feed server output. `responses` must be written back to the channel.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        // A server without a UTF-8 locale draws its boxes with ASCII letters;
        // turn them into box-drawing characters before the parser stores them.
        let bytes = self.charsets.translate(bytes);
        let queries = self.scanner.feed(&bytes);
        self.parser.process(&bytes);
        self.take_callbacks();
        if queries.is_empty() {
            self.version += 1;
            return Vec::new();
        }
        let cell = (8u16, 16u16);
        let palette = self.effective_rgb();
        let mut out = Vec::new();
        for q in queries {
            out.extend_from_slice(&respond(q, self.screen(), cell, &palette));
        }
        self.version += 1;
        out
    }

    fn take_callbacks(&mut self) {
        let cb = self.parser.callbacks_mut();
        if let Some(title) = cb.title.take() {
            self.title = title;
        }
        if cb.bell {
            cb.bell = false;
            self.bell += 1;
        }
        if let Some(size) = cb.resize.take() {
            self.pending_resize = Some(size);
        }
        if let Some(text) = cb.clipboard.take() {
            self.clipboard_request = Some(text);
        }
        let colors = std::mem::take(&mut cb.colors);
        self.colors.apply(colors);
    }

    /// The palette the app is configured with. Server overrides start over.
    pub fn set_palette_colors(&mut self, foreground: [u8; 3], background: [u8; 3]) {
        self.base = PaletteRgb {
            foreground,
            background,
        };
        self.colors = ServerColors::default();
    }

    /// The palette to paint with: the configured one, plus whatever the server
    /// asked for through OSC 4/10/11/12.
    pub fn effective_palette(&self, base: &Palette) -> Palette {
        self.colors.resolve(base)
    }

    /// The colours an OSC 10/11 query would be answered with, overrides
    /// included.
    fn effective_rgb(&self) -> PaletteRgb {
        PaletteRgb {
            foreground: self.colors.foreground.unwrap_or(self.base.foreground),
            background: self.colors.background.unwrap_or(self.base.background),
        }
    }
    /// Resize the grid; returns the new size actually applied.
    pub fn resize(&mut self, rows: u16, cols: u16) -> (u16, u16) {
        let (cur_rows, cur_cols) = self.parser.screen().size();
        let (rows, cols) = (rows.max(2), cols.max(2));
        if (cur_rows, cur_cols) != (rows, cols) {
            self.parser.screen_mut().set_size(rows, cols);
            self.version += 1;
        }
        (rows, cols)
    }

    /// Scroll by `rows` (positive = into history). Returns the applied offset.
    pub fn scroll(&mut self, rows: isize) -> usize {
        let target = if rows >= 0 {
            self.scroll_offset.saturating_add(rows as usize)
        } else {
            self.scroll_offset
                .saturating_sub(rows.unsigned_abs() as usize)
        };
        self.parser.screen_mut().set_scrollback(target);
        let applied = self.parser.screen().scrollback();
        if applied != self.scroll_offset {
            self.scroll_offset = applied;
            self.version += 1;
        }
        self.scroll_offset
    }

    /// Jump back to the live prompt (call before sending input).
    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset != 0 {
            self.parser.screen_mut().set_scrollback(0);
            self.scroll_offset = self.parser.screen().scrollback();
            self.version += 1;
        }
    }

    /// Plain text of the visible screen, used by "copy all to clipboard".
    pub fn visible_text(&self) -> String {
        let (rows, cols) = self.parser.screen().size();
        let mut out = String::new();
        for row in 0..rows {
            let mut line = String::new();
            for col in 0..cols {
                if let Some(cell) = self.parser.screen().cell(row, col) {
                    line.push_str(cell.contents());
                }
            }
            out.push_str(line.trim_end());
            if row + 1 < rows {
                out.push('\n');
            }
        }
        out
    }

    /// Text between two grid positions, for selection.
    pub fn text_between(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let (srow, scol) = start;
        let (erow, ecol) = end;
        let (start_row, start_col) = if (srow, scol) <= (erow, ecol) {
            (srow, scol)
        } else {
            (erow, ecol)
        };
        let (end_row, end_col) = if (srow, scol) <= (erow, ecol) {
            (erow, ecol)
        } else {
            (srow, scol)
        };
        let cols = self.parser.screen().size().1;
        let mut out = String::new();
        for row in start_row..=end_row {
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row {
                end_col.saturating_add(1).min(cols)
            } else {
                cols
            };
            let mut line = String::new();
            for col in from..to {
                if let Some(cell) = self.parser.screen().cell(row, col) {
                    line.push_str(cell.contents());
                }
            }
            if row == end_row {
                out.push_str(line.trim_end());
            } else {
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn state(&self) -> TermState {
        let s = self.parser.screen();
        TermState {
            application_cursor: s.application_cursor(),
            application_keypad: s.application_keypad(),
            bracketed_paste: s.bracketed_paste(),
            hide_cursor: s.hide_cursor(),
            mouse_mode: s.mouse_protocol_mode(),
            mouse_encoding: s.mouse_protocol_encoding(),
            alternate_screen: s.alternate_screen(),
        }
    }
}

/// The input-affecting modes of the remote application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermState {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub hide_cursor: bool,
    pub mouse_mode: MouseProtocolMode,
    pub mouse_encoding: MouseProtocolEncoding,
    pub alternate_screen: bool,
}

impl Default for TermState {
    fn default() -> Self {
        Self {
            application_cursor: false,
            application_keypad: false,
            bracketed_paste: false,
            hide_cursor: false,
            mouse_mode: MouseProtocolMode::None,
            mouse_encoding: MouseProtocolEncoding::Sgr,
            alternate_screen: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Input encoding
// ---------------------------------------------------------------------------

/// UI independent key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Escape,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Turns a key press into the bytes a PTY expects.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputEncoder(pub TermState);

impl InputEncoder {
    pub fn encode(&self, code: KeyCode, mods: Mods) -> Option<Vec<u8>> {
        let app = self.0.application_cursor;
        let seq: &[u8] = match code {
            KeyCode::Char(c) => {
                let mut out = Vec::with_capacity(6);
                if mods.ctrl {
                    if let Some(ctrl) = ctrl_byte(c) {
                        out.push(ctrl);
                    } else {
                        return None;
                    }
                } else {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                if mods.alt {
                    out.insert(0, 0x1b);
                }
                return Some(out);
            }
            KeyCode::Enter => b"\r",
            KeyCode::Tab => b"\t",
            KeyCode::BackTab => b"\x1b[Z",
            KeyCode::Backspace => b"\x7f",
            KeyCode::Escape => b"\x1b",
            KeyCode::Insert => b"\x1b[2~",
            KeyCode::Delete => b"\x1b[3~",
            KeyCode::Home => {
                if app {
                    b"\x1bOH"
                } else {
                    b"\x1b[H"
                }
            }
            KeyCode::End => {
                if app {
                    b"\x1bOF"
                } else {
                    b"\x1b[F"
                }
            }
            KeyCode::PageUp => b"\x1b[5~",
            KeyCode::PageDown => b"\x1b[6~",
            KeyCode::Up => {
                if app {
                    b"\x1bOA"
                } else {
                    b"\x1b[A"
                }
            }
            KeyCode::Down => {
                if app {
                    b"\x1bOB"
                } else {
                    b"\x1b[B"
                }
            }
            KeyCode::Right => {
                if app {
                    b"\x1bOC"
                } else {
                    b"\x1b[C"
                }
            }
            KeyCode::Left => {
                if app {
                    b"\x1bOD"
                } else {
                    b"\x1b[D"
                }
            }
            KeyCode::F(n) => function_key(n),
        };

        let mut out = Vec::with_capacity(seq.len() + 2);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(seq);
        // xterm-style modified keys (CSI 27 ; mod ; key ~) only for the arrows
        // already handled above; keep the classic encoding for compatibility.
        Some(out)
    }

    /// Wrap pasted text in bracketed-paste markers when the app asked for them.
    pub fn paste(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() + 16);
        if self.0.bracketed_paste {
            out.extend_from_slice(b"\x1b[200~");
        }
        // Terminals receive CR, not LF, for pasted newlines. A trailing newline
        // is dropped so pasting a command does not run it immediately.
        let body = text.trim_end_matches(['\r', '\n']);
        for (i, line) in body.split('\n').enumerate() {
            if i > 0 {
                out.push(b'\r');
            }
            out.extend_from_slice(line.trim_end_matches('\r').as_bytes());
        }
        if self.0.bracketed_paste {
            out.extend_from_slice(b"\x1b[201~");
        }
        out
    }
}

fn function_key(n: u8) -> &'static [u8] {
    match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        13 => b"\x1b[25~",
        14 => b"\x1b[26~",
        15 => b"\x1b[28~",
        16 => b"\x1b[29~",
        17 => b"\x1b[31~",
        18 => b"\x1b[32~",
        19 => b"\x1b[33~",
        20 => b"\x1b[34~",
        _ => b"",
    }
}

/// `^A` .. `^Z` and the control characters produced by punctuation.
fn ctrl_byte(c: char) -> Option<u8> {
    let upper = c.to_ascii_uppercase();
    if ('A'..='Z').contains(&upper) {
        Some(upper as u8 - b'@')
    } else {
        match c {
            ' ' | '@' => Some(0),
            '[' | '\\' => Some(0x1b),
            ']' => Some(0x1d),
            '^' | '_' => Some(0x1e),
            '?' | '/' => Some(0x7f),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse encoding
// ---------------------------------------------------------------------------

/// Mouse button numbers as used by the X10/SGR protocol.
pub const MOUSE_LEFT: u16 = 0;
pub const MOUSE_MIDDLE: u16 = 1;
pub const MOUSE_RIGHT: u16 = 2;
pub const MOUSE_RELEASE: u16 = 3;
pub const MOUSE_WHEEL_UP: u16 = 64;
pub const MOUSE_WHEEL_DOWN: u16 = 65;

/// Which kind of mouse report to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseReport {
    Press { button: u16 },
    Release,
    Drag { button: u16 },
    Motion,
}

/// Encode a mouse event, or `None` when the app is not listening.
pub fn encode_mouse(
    state: TermState,
    report: MouseReport,
    row: u16,
    col: u16,
    mods: Mods,
) -> Option<Vec<u8>> {
    use MouseProtocolMode as M;
    let (button_base, is_motion) = match report {
        MouseReport::Press { button } => (button, false),
        MouseReport::Release => (MOUSE_RELEASE, false),
        MouseReport::Drag { button } => (button, true),
        MouseReport::Motion => (3, true),
    };

    let enabled = match state.mouse_mode {
        M::None => return None,
        M::Press => matches!(report, MouseReport::Press { .. }),
        M::PressRelease => !is_motion,
        M::ButtonMotion | M::AnyMotion => true,
    };
    if !enabled {
        return None;
    }

    let mut button = button_base;
    if matches!(report, MouseReport::Motion) {
        button |= 32;
    }
    if mods.shift {
        button |= 4;
    }
    if mods.alt {
        button |= 8;
    }
    if mods.ctrl {
        button |= 16;
    }

    let (rows, cols) = (row.saturating_add(1), col.saturating_add(1));
    let mut out = Vec::with_capacity(24);
    match state.mouse_encoding {
        MouseProtocolEncoding::Sgr => {
            let suffix = match report {
                MouseReport::Release => 'm',
                _ => 'M',
            };
            out.extend_from_slice(format!("\x1b[<{button};{cols};{rows}{suffix}").as_bytes());
        }
        MouseProtocolEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + button as u8);
            push_utf8_coord(&mut out, cols);
            push_utf8_coord(&mut out, rows);
        }
        MouseProtocolEncoding::Default => {
            // The legacy byte encoding cannot express coordinates > 223.
            if cols > 223 || rows > 223 {
                return None;
            }
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + button as u8);
            out.push(32 + cols as u8);
            out.push(32 + rows as u8);
        }
    }
    Some(out)
}

fn push_utf8_coord(out: &mut Vec<u8>, value: u16) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(
        char::from_u32(32 + u32::from(value))
            .unwrap_or(' ')
            .encode_utf8(&mut buf)
            .as_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_and_reports_cursor_position() {
        let mut emu = Emulator::new(24, 80, 0);
        let reply = emu.feed(b"hello\x1b[6n");
        assert!(reply.starts_with(b"\x1b["), "got {reply:?}");
        assert!(reply.ends_with(b"R"));
        assert!(emu.visible_text().contains("hello"));
    }

    #[test]
    fn answers_da_and_status() {
        let mut emu = Emulator::new(24, 80, 0);
        let reply = emu.feed(b"\x1b[c\x1b[5n\x1b[>c");
        assert!(reply.contains(&b'c'));
        assert!(reply.windows(4).any(|w| w == b"\x1b[0n"));
    }

    #[test]
    fn query_split_across_chunks() {
        let mut scanner = QueryScanner::default();
        assert!(scanner.feed(b"abc\x1b[").is_empty());
        assert!(scanner.feed(b"6").is_empty());
        assert_eq!(scanner.feed(b"n"), vec![Query::CursorPosition]);
    }

    #[test]
    fn title_and_bell_callbacks() {
        let mut emu = Emulator::new(24, 80, 0);
        emu.feed(b"\x1b]0;myhost:~\x07\x07");
        assert_eq!(emu.title, "myhost:~");
        assert_eq!(emu.bell, 1);
    }

    #[test]
    fn resize_and_scrollback() {
        let mut emu = Emulator::new(10, 40, 100);
        for i in 0..40 {
            emu.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(emu.scroll(5) > 0, "scrollback should be reachable");
        emu.scroll_to_bottom();
        assert_eq!(emu.scroll_offset, 0);
        emu.resize(20, 80);
        assert_eq!(emu.size(), (20, 80));
    }

    #[test]
    fn key_encoding_respects_application_cursor() {
        let normal = InputEncoder(TermState::default());
        assert_eq!(
            normal.encode(KeyCode::Up, Mods::default()).unwrap(),
            b"\x1b[A"
        );
        let app = InputEncoder(TermState {
            application_cursor: true,
            ..Default::default()
        });
        assert_eq!(app.encode(KeyCode::Up, Mods::default()).unwrap(), b"\x1bOA");
        assert_eq!(
            normal
                .encode(
                    KeyCode::Char('c'),
                    Mods {
                        ctrl: true,
                        ..Default::default()
                    }
                )
                .unwrap(),
            vec![3]
        );
        assert_eq!(
            normal
                .encode(
                    KeyCode::Char('x'),
                    Mods {
                        alt: true,
                        ..Default::default()
                    }
                )
                .unwrap(),
            vec![0x1b, b'x']
        );
    }

    #[test]
    fn mouse_sgr_encoding() {
        let state = TermState {
            mouse_mode: MouseProtocolMode::PressRelease,
            mouse_encoding: MouseProtocolEncoding::Sgr,
            ..Default::default()
        };
        let press = encode_mouse(
            state,
            MouseReport::Press { button: MOUSE_LEFT },
            2,
            5,
            Mods::default(),
        )
        .unwrap();
        assert_eq!(press, b"\x1b[<0;6;3M");
        let release = encode_mouse(state, MouseReport::Release, 2, 5, Mods::default()).unwrap();
        assert_eq!(release, b"\x1b[<3;6;3m");
        let off = TermState::default();
        assert!(
            encode_mouse(off, MouseReport::Press { button: 0 }, 0, 0, Mods::default()).is_none()
        );
    }
}

#[cfg(test)]
mod colors {
    use super::*;

    fn feed_osc(emu: &mut Emulator, body: &[u8]) {
        let mut bytes = vec![0x1b, b']'];
        bytes.extend_from_slice(body);
        bytes.push(0x07);
        emu.feed(&bytes);
    }

    #[test]
    fn osc_colour_requests_override_the_palette() {
        let base = Palette::default();
        let mut emu = Emulator::new(4, 20, 0);
        emu.set_palette_colors(base.foreground, base.background);
        feed_osc(&mut emu, b"11;#2b2b2b");
        feed_osc(&mut emu, b"10;rgb:ff/00/00");
        feed_osc(&mut emu, b"12;#00ff00");
        feed_osc(&mut emu, b"4;1;#0000ff");
        let painted = emu.effective_palette(&base);
        assert_eq!(painted.background, [0x2b, 0x2b, 0x2b]);
        assert_eq!(painted.foreground, [0xff, 0x00, 0x00]);
        assert_eq!(painted.cursor, [0x00, 0xff, 0x00]);
        assert_eq!(painted.ansi[1], [0x00, 0x00, 0xff]);
        assert_eq!(painted.ansi[2], base.ansi[2]);
    }

    #[test]
    fn resets_return_to_the_configured_palette() {
        let base = Palette::default();
        let mut emu = Emulator::new(4, 20, 0);
        emu.set_palette_colors(base.foreground, base.background);
        feed_osc(&mut emu, b"10;#ff0000");
        feed_osc(&mut emu, b"4;1;#00ff00;2;#0000ff");
        feed_osc(&mut emu, b"110");
        feed_osc(&mut emu, b"104;1");
        let painted = emu.effective_palette(&base);
        assert_eq!(painted.foreground, base.foreground);
        assert_eq!(painted.ansi[1], base.ansi[1]);
        assert_eq!(painted.ansi[2], [0x00, 0x00, 0xff]);
    }

    #[test]
    fn colour_formats_and_junk_are_understood() {
        assert_eq!(parse_color(b"#f0a14"), None);
        assert_eq!(parse_color(b"#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_color(b"#10c850"), Some([0x10, 0xc8, 0x50]));
        assert_eq!(parse_color(b"#123456789abc"), Some([0x12, 0x56, 0x9a]));
        assert_eq!(parse_color(b"rgb:10/c8/50"), Some([0x10, 0xc8, 0x50]));
        assert_eq!(parse_color(b"rgb:1/c8/50"), Some([0x11, 0xc8, 0x50]));
        assert_eq!(parse_color(b"rgba:10/c8/50/ff"), Some([0x10, 0xc8, 0x50]));
        assert_eq!(parse_color(b"rgb:10/c8"), None);
        assert_eq!(parse_color(b"system"), None);
    }

    #[test]
    fn queries_answer_the_server_colours() {
        let mut emu = Emulator::new(4, 20, 0);
        emu.set_palette_colors([0xd5, 0xd8, 0xdc], [0x14, 0x17, 0x1a]);
        feed_osc(&mut emu, b"11;#2b2b2b");
        let reply = String::from_utf8(emu.feed(b"\x1b]11;?\x07")).unwrap();
        assert_eq!(reply, "\x1b]11;rgb:2b2b/2b2b/2b2b\x07");
        let reply = String::from_utf8(emu.feed(b"\x1b]10;?\x07")).unwrap();
        assert_eq!(reply, "\x1b]10;rgb:d5d5/d8d8/dcdc\x07");
    }

    #[test]
    fn unrelated_osc_sequences_change_nothing() {
        let base = Palette::default();
        let mut emu = Emulator::new(4, 20, 0);
        emu.set_palette_colors(base.foreground, base.background);
        feed_osc(&mut emu, b"1337;RemoteHost=user@host");
        feed_osc(&mut emu, b"10;nosuchcolour");
        assert_eq!(emu.effective_palette(&base), base);
    }
}
