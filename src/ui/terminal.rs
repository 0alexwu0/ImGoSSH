//! The terminal widget: paints a `vt100` screen and turns egui input into PTY
//! bytes. Transport agnostic - the same widget drives SSH shells and local
//! pseudo-consoles.

use std::sync::{Arc, Mutex};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, EventFilter, FontId, Modifiers, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
use vt100::{Color, MouseProtocolMode};

use crate::config::Palette;
use crate::ssh::Backend;
use crate::terminal::{
    encode_mouse, Emulator, InputEncoder, KeyCode, Mods, MouseReport, TermState, MOUSE_LEFT,
    MOUSE_MIDDLE, MOUSE_RELEASE, MOUSE_RIGHT, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A cell selection, inclusive on both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Selection {
    fn normalized(self) -> Self {
        if (self.start.0, self.start.1) <= (self.end.0, self.end.1) {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }

    /// True when the drag never covered a single cell.
    fn is_empty(self) -> bool {
        let s = self.normalized();
        s.start == s.end
    }

    fn contains(self, row: u16, col: u16) -> bool {
        let s = self.normalized();
        if row < s.start.0 || row > s.end.0 {
            return false;
        }
        if s.start.0 == s.end.0 {
            return col >= s.start.1 && col <= s.end.1;
        }
        if row == s.start.0 {
            col >= s.start.1
        } else if row == s.end.0 {
            col <= s.end.1
        } else {
            true
        }
    }
}

/// Per-tab widget state.
#[derive(Default)]
pub struct State {
    pub selection: Option<Selection>,
    pub dragging: bool,
    pub reported_button: Option<u16>,
    pub last_cell: Option<(u16, u16)>,
    /// Ask the grid for the keyboard as soon as it is drawn again, used to
    /// hand focus back after the right-click menu ate the click.
    pub want_focus: bool,
}

/// Colours and metrics for one frame.
pub struct Style<'a> {
    pub font: FontId,
    pub cell: Vec2,
    pub palette: &'a Palette,
    /// Cursor phase, driven by the blink timer.
    pub cursor_on: bool,
    /// Air between the well wall and the first cell, in points.
    pub pad: f32,
    /// Corner radius of the well the grid sits in.
    pub radius: f32,
    /// Hairline around the well.
    pub border: Color32,
}

/// What the widget asks of its owner after a frame.
#[derive(Default)]
pub struct Output {
    /// The grid changed size; the backend must be told.
    pub resized: Option<(u16, u16)>,
    /// Text to put on the system clipboard.
    pub copy: Option<String>,
    /// Where to open the right-click menu.
    pub menu_at: Option<Pos2>,
}

// ---------------------------------------------------------------------------
// Input plumbing
// ---------------------------------------------------------------------------

fn send(backend: &Arc<Mutex<Option<Backend>>>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    if let Some(b) = backend.lock().unwrap().as_ref() {
        b.send(bytes);
    }
}

/// Scroll back to the live prompt, then write.
fn type_into(emu: &Arc<Mutex<Emulator>>, backend: &Arc<Mutex<Option<Backend>>>, bytes: Vec<u8>) {
    emu.lock().unwrap().scroll_to_bottom();
    send(backend, bytes);
}

fn mods_of(m: &Modifiers) -> Mods {
    Mods {
        ctrl: m.ctrl,
        alt: m.alt,
        shift: m.shift,
    }
}

// ---------------------------------------------------------------------------
// Colour mapping
// ---------------------------------------------------------------------------

fn ansi(palette: &Palette, index: u8, bold: bool) -> Color32 {
    let idx = if bold && index < 8 { index + 8 } else { index };
    match idx {
        0..=15 => {
            let [r, g, b] = palette.ansi[idx as usize];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let n = idx - 16;
            let steps: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let (r, g, b) = (n / 36, (n % 36) / 6, n % 6);
            Color32::from_rgb(steps[r as usize], steps[g as usize], steps[b as usize])
        }
        _ => {
            let v = (8 + (idx as u16 - 232) * 10).min(255) as u8;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn resolve(color: Color, palette: &Palette, foreground: bool, bold: bool) -> Color32 {
    match color {
        Color::Default => {
            if foreground {
                rgb(palette.foreground)
            } else {
                rgb(palette.background)
            }
        }
        Color::Idx(index) => ansi(palette, index, bold),
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Everything that distinguishes one run of cells from the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    fg: Color32,
    bg: Color32,
    bold: bool,
    italic: bool,
    dim: bool,
    underline: bool,
    selected: bool,
}

#[allow(clippy::too_many_arguments)]
fn paint(
    painter: &Painter,
    rect: Rect,
    term: &Emulator,
    style: &Style<'_>,
    selection: Option<Selection>,
    rows: u16,
    cols: u16,
    scroll_offset: usize,
    focused: bool,
) {
    let default_bg = rgb(style.palette.background);
    let default_fg = rgb(style.palette.foreground);
    let selection_bg = rgb(style.palette.selection);
    let cell = style.cell;
    let screen = term.screen();

    for row in 0..rows {
        let y = rect.min.y + row as f32 * cell.y;
        let mut job = LayoutJob::default();
        let mut col = 0u16;
        while col < cols {
            let Some(first) = screen.cell(row, col) else {
                col += 1;
                continue;
            };
            let run = run_style(first, style.palette, selection, row, col);
            let start = col;
            let mut text = String::new();
            while col < cols {
                let Some(c) = screen.cell(row, col) else {
                    break;
                };
                if c.is_wide_continuation() {
                    // The glyph was already emitted by the cell on its left.
                    col += 1;
                    continue;
                }
                if run_style(c, style.palette, selection, row, col) != run {
                    break;
                }
                match c.contents().chars().next() {
                    Some(ch) => text.push(ch),
                    None => text.push(' '),
                }
                col += 1;
            }
            if text.is_empty() {
                continue;
            }

            let x = rect.min.x + start as f32 * cell.x;
            let width = (col - start) as f32 * cell.x;
            let box_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, cell.y));
            if run.selected {
                painter.rect_filled(box_rect, 0.0, selection_bg);
            } else if run.bg != default_bg {
                painter.rect_filled(box_rect, 0.0, run.bg);
            }
            if run.underline {
                painter.hline(x..=x + width, y + cell.y - 1.5, Stroke::new(1.0, run.fg));
            }

            let font_id = style.font.clone();
            let color = if run.selected { default_bg } else { run.fg };
            let color = if run.dim {
                color.linear_multiply(0.65)
            } else {
                color
            };
            job.append(
                &text,
                0.0,
                TextFormat {
                    font_id,
                    color,
                    italics: run.italic,
                    ..TextFormat::default()
                },
            );
        }

        if !job.text.is_empty() {
            let galley = painter.layout_job(job);
            painter.galley(Pos2::new(rect.min.x, y), galley, default_fg);
        }
    }

    // The cursor belongs to the live screen, not to scrollback.
    let state = term.state();
    if scroll_offset == 0 && !state.hide_cursor && (style.cursor_on || !focused) {
        let (crow, ccol) = term.cursor();
        if crow < rows && ccol < cols {
            let at = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + ccol as f32 * cell.x,
                    rect.min.y + crow as f32 * cell.y,
                ),
                cell,
            )
            .shrink(1.0);
            let color = rgb(style.palette.cursor);
            if focused {
                painter.rect_filled(at, 0.0, color);
            } else {
                painter.rect_stroke(at, 0.0, Stroke::new(1.0, color), StrokeKind::Middle);
            }
        }
    }
}

fn run_style(
    c: &vt100::Cell,
    palette: &Palette,
    selection: Option<Selection>,
    row: u16,
    col: u16,
) -> Run {
    let (fg_raw, bg_raw) = if c.inverse() {
        (c.bgcolor(), c.fgcolor())
    } else {
        (c.fgcolor(), c.bgcolor())
    };
    Run {
        fg: resolve(fg_raw, palette, true, c.bold()),
        bg: resolve(bg_raw, palette, false, false),
        bold: c.bold(),
        italic: c.italic(),
        dim: c.dim(),
        underline: c.underline(),
        selected: selection.is_some_and(|s| s.contains(row, col)),
    }
}
// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

/// Draw one terminal and read its input.
///
/// `menu_open` tells the grid that its right-click menu is up. The menu
/// floats over the cells, and a click on it must not be read as a click on a
/// cell - that would collapse the very selection the menu is about to copy.
pub fn show(
    ui: &mut Ui,
    emu: &Arc<Mutex<Emulator>>,
    backend: &Arc<Mutex<Option<Backend>>>,
    st: &mut State,
    style: &Style<'_>,
    menu_open: bool,
) -> Output {
    let mut out = Output::default();

    // ---- geometry -------------------------------------------------------
    let available = ui.available_size_before_wrap() - Vec2::splat(2.0 * style.pad);
    let (want_cols, want_rows) = fit(available, style.cell);
    let (rows, cols) = {
        let mut term = emu.lock().unwrap();
        if (want_cols, want_rows) != term.size() {
            out.resized = Some(term.resize(want_rows, want_cols));
        }
        term.size()
    };

    let grid = Vec2::new(cols as f32 * style.cell.x, rows as f32 * style.cell.y);
    let size = Vec2::new(
        grid.x.min(available.x).max(style.cell.x),
        grid.y.min(available.y).max(style.cell.y),
    ) + Vec2::splat(2.0 * style.pad);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let focused = response.has_focus();
    // The cells live inside a rounded well: the padding keeps the first column
    // off the border and the radius keeps the corner from cutting glyphs.
    let well = Rect::from_min_size(
        rect.min + Vec2::splat(style.pad),
        rect.size() - Vec2::splat(2.0 * style.pad),
    );
    ui.painter().rect(
        rect,
        style.radius,
        rgb(style.palette.background),
        (1.0, style.border),
        StrokeKind::Inside,
    );

    // A terminal owns its keys: Tab is shell completion, the arrows are history
    // and cursor movement, Escape cancels. Tell egui not to spend them on
    // keyboard focus navigation while this grid holds the focus.
    if focused {
        ui.memory_mut(|mem| {
            mem.set_focus_lock_filter(
                response.id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }

    {
        let term = emu.lock().unwrap();
        paint(
            &ui.painter_at(rect),
            well,
            &term,
            style,
            st.selection,
            rows,
            cols,
            term.scroll_offset,
            focused,
        );
    }
    let term_state = emu.lock().unwrap().state();

    // ---- pointer --------------------------------------------------------
    let pointer = ui.input(|i| i.pointer.clone());
    let hover = response.hover_pos();
    if response.secondary_clicked() {
        out.menu_at = hover;
    }
    if response.middle_clicked() {
        if let Some(text) = clipboard_text() {
            type_into(emu, backend, InputEncoder(term_state).paste(&text));
        }
    }

    let modifiers = ui.input(|i| i.modifiers);
    // A press belongs to us only when it started inside the grid; a release
    // belongs to us whenever we are still holding our own drag state.
    let started_inside = !menu_open && pointer.press_origin().is_some_and(|p| rect.contains(p));
    let active_cell = pointer
        .interact_pos()
        .or(hover)
        .and_then(|p| cell_at(well, style.cell, p));

    if started_inside {
        if let Some(cell) = active_cell {
            let mods = mods_of(&modifiers);
            if pointer.primary_pressed() {
                st.selection = None;
                st.reported_button = None;
                st.dragging = false;
                if term_state.mouse_mode != MouseProtocolMode::None {
                    st.reported_button = Some(MOUSE_LEFT);
                    st.last_cell = Some(cell);
                    type_report(
                        backend,
                        &term_state,
                        MouseReport::Press { button: MOUSE_LEFT },
                        cell,
                        mods,
                    );
                } else {
                    st.selection = Some(Selection {
                        start: cell,
                        end: cell,
                    });
                    st.dragging = true;
                }
            } else {
                let button = if pointer.secondary_pressed() {
                    Some(MOUSE_RIGHT)
                } else if pointer.button_pressed(egui::PointerButton::Middle) {
                    Some(MOUSE_MIDDLE)
                } else {
                    None
                };
                if let Some(button) = button {
                    if term_state.mouse_mode != MouseProtocolMode::None {
                        st.reported_button = Some(button);
                        st.last_cell = Some(cell);
                        type_report(
                            backend,
                            &term_state,
                            MouseReport::Press { button },
                            cell,
                            mods,
                        );
                    }
                }
            }
        }
    } else if st.dragging || st.reported_button.is_some() {
        // Keep tracking even if the pointer wanders off the last row.
        if let Some(cell) = active_cell {
            if let Some(button) = st.reported_button {
                if st.last_cell != Some(cell) {
                    st.last_cell = Some(cell);
                    type_report(
                        backend,
                        &term_state,
                        MouseReport::Drag { button },
                        cell,
                        mods_of(&modifiers),
                    );
                }
            } else if let Some(sel) = &mut st.selection {
                sel.end = cell;
            }
        }
    }

    if pointer.any_released() && (st.dragging || st.reported_button.is_some()) {
        let mods = mods_of(&modifiers);
        if let Some(button) = st.reported_button.take() {
            if let Some(cell) = st.last_cell.take() {
                type_report(backend, &term_state, MouseReport::Release, cell, mods);
                let _ = button;
            }
        } else if let Some(sel) = st.selection.take() {
            if sel.is_empty() {
                // A plain click: clear the old selection.
            } else {
                st.selection = Some(sel);
                let text = emu.lock().unwrap().text_between(sel.start, sel.end);
                if !text.is_empty() {
                    out.copy = Some(text);
                }
            }
        }
        st.dragging = false;
    }
    // ---- wheel ----------------------------------------------------------
    let hovered = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)));
    if hovered {
        let scroll = ui.input(|i| i.smooth_scroll_delta).y;
        if scroll.abs() > 1.0 {
            let lines = (scroll / style.cell.y).trunc() as isize;
            if lines != 0 {
                emu.lock().unwrap().scroll(lines);
                ui.ctx().request_repaint();
            }
        }
    }

    // ---- keyboard -------------------------------------------------------
    if focused {
        keyboard(ui, emu, backend, st, term_state, &mut out);
    }
    if response.clicked_by(egui::PointerButton::Primary) && !focused {
        response.request_focus();
    }
    if st.want_focus {
        st.want_focus = false;
        response.request_focus();
    }

    out
}

/// How many cells fit in `available`.
pub fn fit(available: Vec2, cell: Vec2) -> (u16, u16) {
    if cell.x <= 0.0 || cell.y <= 0.0 {
        return (80, 24);
    }
    let cols = (available.x / cell.x).floor().clamp(2.0, 1000.0) as u16;
    let rows = (available.y / cell.y).floor().clamp(2.0, 1000.0) as u16;
    (cols, rows)
}

/// Grid coordinates of a pixel position.
fn cell_at(rect: Rect, cell: Vec2, pos: Pos2) -> Option<(u16, u16)> {
    if !rect.contains(pos) || cell.x <= 0.0 || cell.y <= 0.0 {
        return None;
    }
    let col = ((pos.x - rect.min.x) / cell.x).floor();
    let row = ((pos.y - rect.min.y) / cell.y).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    Some((row as u16, col as u16))
}

fn type_report(
    backend: &Arc<Mutex<Option<Backend>>>,
    state: &TermState,
    report: MouseReport,
    (row, col): (u16, u16),
    mods: Mods,
) {
    if let Some(bytes) = encode_mouse(*state, report, row, col, mods) {
        send(backend, bytes);
    }
}

/// Read the system clipboard (used by middle-click and Shift+Insert).
pub fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut clip| clip.get_text().ok())
}

// ---------------------------------------------------------------------------
// Keyboard
// ---------------------------------------------------------------------------

fn keyboard(
    ui: &mut Ui,
    emu: &Arc<Mutex<Emulator>>,
    backend: &Arc<Mutex<Option<Backend>>>,
    st: &mut State,
    state: TermState,
    out: &mut Output,
) {
    let encoder = InputEncoder(state);
    let events = ui.input(|i| i.events.clone());
    // `Event::Text` already carries the keyboard layout and the shift state, so
    // printable keys must not be encoded a second time from their key event.
    let text_seen = events.iter().any(
        |event| matches!(event, egui::Event::Text(text) if text.chars().any(|c| !c.is_control())),
    );
    let mut keep = Vec::new();
    let mut bytes = Vec::new();
    let mut consumed = false;

    for event in events {
        match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let mods = mods_of(&modifiers);
                let copy_shortcut = (mods.ctrl && key == egui::Key::Insert)
                    || (modifiers.command && modifiers.shift && key == egui::Key::C);
                let paste_shortcut = (mods.shift && key == egui::Key::Insert)
                    || (modifiers.command && modifiers.shift && key == egui::Key::V);
                if copy_shortcut {
                    if let Some(sel) = st.selection {
                        let text = emu.lock().unwrap().text_between(sel.start, sel.end);
                        if !text.is_empty() {
                            out.copy = Some(text);
                        }
                    }
                    consumed = true;
                } else if paste_shortcut {
                    if let Some(text) = clipboard_text() {
                        bytes.extend(encoder.paste(&text));
                    }
                    consumed = true;
                } else if let Some(seq) = map_key(key, mods, text_seen, &encoder) {
                    bytes.extend(seq);
                    consumed = true;
                } else {
                    keep.push(egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        repeat: false,
                        physical_key: None,
                    });
                }
            }
            egui::Event::Text(ref text) => {
                let first = text.chars().next();
                if text.chars().count() == 1 && first.is_some_and(|c| !c.is_control()) {
                    bytes.extend(
                        encoder
                            .encode(KeyCode::Char(first.unwrap()), Mods::default())
                            .unwrap_or_default(),
                    );
                    consumed = true;
                } else if text.chars().count() > 1 {
                    bytes.extend(encoder.paste(text));
                    consumed = true;
                } else {
                    keep.push(event.clone());
                }
            }
            egui::Event::Paste(ref text) => {
                bytes.extend(encoder.paste(text));
                consumed = true;
            }
            egui::Event::Copy | egui::Event::Cut => {
                match st.selection {
                    Some(sel) => {
                        let text = emu.lock().unwrap().text_between(sel.start, sel.end);
                        if text.is_empty() {
                            bytes.push(0x03);
                        } else {
                            out.copy = Some(text);
                        }
                    }
                    None => bytes.push(0x03),
                }
                consumed = true;
            }
            _ => keep.push(event.clone()),
        }
    }

    if consumed {
        ui.input_mut(|i| i.events = keep);
        type_into(emu, backend, bytes);
    }
}

/// Map one physical key to bytes, or `None` to let `Event::Text` handle it.
fn map_key(key: egui::Key, mods: Mods, text_seen: bool, encoder: &InputEncoder) -> Option<Vec<u8>> {
    use egui::Key as K;
    let code = match key {
        K::ArrowUp => KeyCode::Up,
        K::ArrowDown => KeyCode::Down,
        K::ArrowLeft => KeyCode::Left,
        K::ArrowRight => KeyCode::Right,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::Insert => KeyCode::Insert,
        K::Delete => KeyCode::Delete,
        K::Enter => KeyCode::Enter,
        // Shift+Tab is reverse completion in zsh/fish and undo in readline-ish
        // shells; plain Tab is the classic completion key.
        K::Tab if mods.shift => KeyCode::BackTab,
        K::Tab => KeyCode::Tab,
        K::Backspace => KeyCode::Backspace,
        K::Escape => KeyCode::Escape,
        K::F1 => KeyCode::F(1),
        K::F2 => KeyCode::F(2),
        K::F3 => KeyCode::F(3),
        K::F4 => KeyCode::F(4),
        K::F5 => KeyCode::F(5),
        K::F6 => KeyCode::F(6),
        K::F7 => KeyCode::F(7),
        K::F8 => KeyCode::F(8),
        K::F9 => KeyCode::F(9),
        K::F10 => KeyCode::F(10),
        K::F11 => KeyCode::F(11),
        K::F12 => KeyCode::F(12),
        other => {
            // Printable keys are the text events' job. Only a Ctrl/Alt chord
            // needs the key event (Ctrl+A, Alt+B, ...), and even then a text
            // event for the same press wins so nothing is sent twice.
            let ch = printable_of(other)?;
            if mods.ctrl || mods.alt {
                if text_seen {
                    return None;
                }
                return encoder.encode(KeyCode::Char(ch), mods);
            }
            return None;
        }
    };
    encoder.encode(code, mods)
}

/// The unshifted character a key produces, used for Ctrl/Alt chords only.
fn printable_of(key: egui::Key) -> Option<char> {
    use egui::Key as K;
    let letter = match key {
        K::A => 'a',
        K::B => 'b',
        K::C => 'c',
        K::D => 'd',
        K::E => 'e',
        K::F => 'f',
        K::G => 'g',
        K::H => 'h',
        K::I => 'i',
        K::J => 'j',
        K::K => 'k',
        K::L => 'l',
        K::M => 'm',
        K::N => 'n',
        K::O => 'o',
        K::P => 'p',
        K::Q => 'q',
        K::R => 'r',
        K::S => 's',
        K::T => 't',
        K::U => 'u',
        K::V => 'v',
        K::W => 'w',
        K::X => 'x',
        K::Y => 'y',
        K::Z => 'z',
        K::Num0 => '0',
        K::Num1 => '1',
        K::Num2 => '2',
        K::Num3 => '3',
        K::Num4 => '4',
        K::Num5 => '5',
        K::Num6 => '6',
        K::Num7 => '7',
        K::Num8 => '8',
        K::Num9 => '9',
        K::Space => ' ',
        K::Minus => '-',
        K::Period => '.',
        K::Comma => ',',
        K::Slash => '/',
        K::Backslash => '\\',
        K::Semicolon => ';',
        K::Quote => '\'',
        K::OpenBracket => '[',
        K::CloseBracket => ']',
        K::Backtick => '`',
        K::Equals => '=',
        _ => return None,
    };
    Some(letter)
}

/// Mouse wheel buttons, re-exported for the caller's context menu.
pub const WHEEL: [u16; 2] = [MOUSE_WHEEL_UP, MOUSE_WHEEL_DOWN];
/// The release code, used by the mouse protocol.
pub const RELEASE: u16 = MOUSE_RELEASE;

#[cfg(test)]
mod tests {
    use super::*;

    // A click on the right-click menu lands inside the grid rect as far as raw
    // pointer input is concerned, and must not be read as a click on a cell -
    // that would collapse the selection the menu is about to copy.
    #[test]
    fn a_click_on_the_menu_leaves_the_selection_alone() {
        let emu = Arc::new(Mutex::new(Emulator::new(4, 20, 0)));
        let backend = Arc::new(Mutex::new(None));
        let palette = Palette::default();
        let before = Some(Selection {
            start: (0, 0),
            end: (0, 3),
        });
        let at = egui::pos2(20.0, 10.0);

        for menu_open in [true, false] {
            let mut st = State {
                selection: before,
                ..State::default()
            };
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            raw.events = vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            let style = Style {
                font: FontId::monospace(12.0),
                cell: Vec2::new(8.0, 16.0),
                palette: &palette,
                cursor_on: true,
                pad: 0.0,
                radius: 0.0,
                border: Color32::TRANSPARENT,
            };
            let mut output = ctx.run_ui(raw, |ui| {
                let _ = show(ui, &emu, &backend, &mut st, &style, menu_open);
            });
            // There is no renderer here to apply the font texture to.
            output.textures_delta.clear();

            if menu_open {
                assert_eq!(st.selection, before, "the menu click moved the selection");
                assert!(!st.dragging, "the menu click started a drag");
            } else {
                assert_ne!(st.selection, before, "a click on the grid must start one");
                assert!(st.dragging);
            }
        }
    }
}
