//! The terminal widget: paints a `vt100` screen and turns egui input into PTY
//! bytes. Transport agnostic - the same widget drives SSH shells and local
//! pseudo-consoles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Color32, Context, CursorIcon, EventFilter, FontId, Layout, Modifiers, Painter, Pos2,
    Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
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
    /// Expected glyph advances, kept so that every cell can be dropped back
    /// onto its own column.
    guesses: Guesses,
}

/// Colours and metrics for one frame.
pub struct Style<'a> {
    pub font: FontId,
    pub cell: Vec2,
    /// Air `line_spacing` adds to a row, shared between its top and its bottom.
    /// Zero leaves a row exactly as tight as the glyphs that fill it.
    pub leading: f32,
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
    /// The notice asked for the session to be started again.
    pub restart: bool,
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

/// How far a group of cells may be laid out before the pen is snapped back onto
/// the grid, in points.
const MAX_DRIFT: f32 = 0.25;

/// The advance each character is expected to cost, in points.
///
/// Only the tables inside the font know that, and a character the terminal face
/// does not own is not in them: it comes from a fallback face, or from the box
/// egui paints when no face has it at all. A guess is good enough to decide where
/// a group of cells ends, which is all it is used for - the width a group actually
/// takes is measured before the pen moves on.
#[derive(Default)]
struct Guesses {
    size: f32,
    advances: HashMap<char, f32>,
}

impl Guesses {
    /// The advance `ch` is expected to cost at `font.size`, looked up once.
    fn guess(&mut self, ctx: &Context, font: &FontId, ch: char) -> f32 {
        if self.size != font.size {
            self.size = font.size;
            self.advances.clear();
        }
        if let Some(advance) = self.advances.get(&ch) {
            return *advance;
        }
        let advance = ctx.fonts_mut(|fonts| fonts.glyph_width(font, ch));
        self.advances.insert(ch, advance);
        advance
    }
}

/// Append one run of cells, closing a group as soon as the glyphs drift off the
/// grid so that a wide fallback glyph, or a missing one, cannot take the rest of
/// the row with it.
fn append_cells(
    job: &mut LayoutJob,
    guesses: &mut Guesses,
    ctx: &Context,
    font: &FontId,
    cell_x: f32,
    cells: &[(char, u16)],
    format: TextFormat,
) {
    let mut text = String::new();
    let mut guessed = 0.0;
    let mut allotted = 0.0;
    for &(ch, count) in cells {
        let advance = guesses.guess(ctx, font, ch);
        let budget = cell_x * f32::from(count);
        // Nothing is known about a character the tables have no advance for.
        let unknown = advance <= 0.0;
        if !text.is_empty()
            && (unknown || (guessed + advance - allotted - budget).abs() > MAX_DRIFT)
        {
            close_group(job, ctx, &text, allotted, &format);
            text.clear();
            guessed = 0.0;
            allotted = 0.0;
        }
        text.push(ch);
        guessed += advance.max(0.0);
        allotted += budget;
        if unknown {
            close_group(job, ctx, &text, allotted, &format);
            text.clear();
            guessed = 0.0;
            allotted = 0.0;
        }
    }
    if !text.is_empty() {
        close_group(job, ctx, &text, allotted, &format);
    }
}

/// Lay `text` out once to learn how wide it really is, then append it with just
/// enough space in front of its neighbour to leave the pen on a cell boundary.
fn close_group(job: &mut LayoutJob, ctx: &Context, text: &str, allotted: f32, format: &TextFormat) {
    let mut measured = LayoutJob::default();
    measured.append(text, 0.0, format.clone());
    let width = ctx.fonts_mut(|fonts| fonts.layout_job(measured)).size().x;
    job.append(text, allotted - width, format.clone());
}

#[allow(clippy::too_many_arguments)]
fn paint(
    painter: &Painter,
    ctx: &Context,
    rect: Rect,
    term: &Emulator,
    style: &Style<'_>,
    guesses: &mut Guesses,
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
    // A row may be taller than the text in it. The glyphs keep their own
    // height and take the middle of the band, so leading lands half above and
    // half below rather than all of it under the baseline.
    let inset = style.leading * 0.5;
    let screen = term.screen();
    let mut cells: Vec<(char, u16)> = Vec::new();

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
            cells.clear();
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
                // A wide glyph owns its own cell and the one that follows it.
                let wide = col.checked_add(1).is_some_and(|next| {
                    screen
                        .cell(row, next)
                        .is_some_and(|following| following.is_wide_continuation())
                });
                let count = if wide { 2 } else { 1 };
                match c.contents().chars().next() {
                    Some(ch) => cells.push((ch, count)),
                    None => cells.push((' ', count)),
                }
                col += count;
            }
            if cells.is_empty() {
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
                painter.hline(
                    x..=x + width,
                    y + cell.y - inset - 1.5,
                    Stroke::new(1.0, run.fg),
                );
            }

            let font_id = style.font.clone();
            let color = if run.selected { default_bg } else { run.fg };
            let color = if run.dim {
                color.linear_multiply(0.65)
            } else {
                color
            };
            append_cells(
                &mut job,
                guesses,
                ctx,
                &style.font,
                cell.x,
                &cells,
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
            painter.galley(Pos2::new(rect.min.x, y + inset), galley, default_fg);
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

/// The strip a finished session is announced in, in points.
const STRIP: f32 = 30.0;

/// What a terminal says about a session that is no longer running.
///
/// A grid that only stops moving is easily mistaken for one that is still
/// thinking, so a dead session gets the bottom of the well to say so in, and
/// the button that starts the whole thing over.
pub struct Notice {
    /// Why the session is over: drawn in `tone`.
    pub text: String,
    /// Label of the button that opens the session again.
    pub action: Option<String>,
    /// The colour of the message: grey for a clean exit, red for a failure.
    pub tone: Color32,
}

/// Draw one terminal and read its input.
///
/// `menu_open` tells the grid that its right-click menu is up. The menu
/// floats over the cells, and a click on it must not be read as a click on a
/// cell - that would collapse the very selection the menu is about to copy.
///
/// `notice` is what the owner of the session wants said about it no longer
/// running. It gets the bottom strip of the well, and its button comes back as
/// [`Output::restart`].
pub fn show(
    ui: &mut Ui,
    emu: &Arc<Mutex<Emulator>>,
    backend: &Arc<Mutex<Option<Backend>>>,
    st: &mut State,
    style: &Style<'_>,
    menu_open: bool,
    notice: Option<&Notice>,
) -> Output {
    let mut out = Output::default();

    // ---- geometry -------------------------------------------------------
    // A session that is over gets the bottom of the well to say so in. The strip
    // comes off the grid rather than being painted over it: a reason with a row
    // of glyphs through it is worth less than a row of scrollback. A window too
    // short to spare one is a window with the notice left out rather than one
    // whose grid is squeezed to nothing.
    let room = ui.available_size_before_wrap();
    let notice = notice.filter(|_| room.y > 5.0 * STRIP);
    let strip = if notice.is_some() { STRIP } else { 0.0 };
    let available = room - Vec2::new(2.0 * style.pad, 2.0 * style.pad + strip);
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
    ) + Vec2::new(2.0 * style.pad, 2.0 * style.pad + strip);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let focused = response.has_focus();
    // The cells live inside a rounded well: the padding keeps the first column
    // off the border and the radius keeps the corner from cutting glyphs.
    let well = Rect::from_min_size(
        rect.min + Vec2::splat(style.pad),
        rect.size() - Vec2::splat(2.0 * style.pad),
    );
    // The notice takes the last `strip` points of the well and the cells stop
    // above it, so a pointer reaching for the button cannot select a cell that
    // is no longer there. The wheel still scrolls from anywhere in the widget,
    // notice included.
    let cells = Rect::from_min_size(well.min, well.size() - Vec2::new(0.0, strip));
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
            ui.ctx(),
            cells,
            &term,
            style,
            &mut st.guesses,
            st.selection,
            rows,
            cols,
            term.scroll_offset,
            focused,
        );
    }
    if let Some(notice) = notice {
        out.restart = notice_strip(ui, notice, style, well, strip);
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
        .and_then(|p| cell_at(cells, style.cell, p));

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

/// Paint the strip a finished session is announced in, and report whether its
/// button was pressed.
///
/// The reason the grid stopped moving and the way to start it again belong
/// together, so the strip lives inside the well rather than in a bar nobody
/// looks at. The message is one line and truncates: the strip is only so tall,
/// and the beginning of a long reason is more use than all of a short one
/// pushed off the edge.
fn notice_strip(ui: &mut Ui, notice: &Notice, style: &Style<'_>, well: Rect, strip: f32) -> bool {
    let band = Rect::from_min_size(
        Pos2::new(well.min.x, well.max.y - strip),
        Vec2::new(well.width(), strip),
    );
    if band.width() <= 0.0 || band.height() <= 0.0 {
        return false;
    }
    let painter = ui.painter_at(band);
    painter.hline(band.x_range(), band.min.y, Stroke::new(1.0, style.border));

    // The button is measured before the message is laid out, so the two cannot
    // land on top of one another however long either of them turns out to be.
    // The button paints its text at the same size the probe does, plus the
    // horizontal padding of a themed button: 32 pt of room is that, generously.
    let action = notice
        .action
        .as_ref()
        .map(|label| (label, label_width(&painter, label) + 32.0));
    let inset = 12.0;
    let room = (band.width() - 2.0 * inset - action.as_ref().map_or(0.0, |(_, w)| *w)).max(0.0);
    let message = Rect::from_min_size(
        Pos2::new(band.min.x + inset, band.min.y),
        Vec2::new(room, strip),
    );
    let _ = ui
        .new_child(
            UiBuilder::new()
                .id_salt("notice message")
                .max_rect(message)
                .layout(Layout::left_to_right(Align::Center)),
        )
        .add(
            egui::Label::new(
                RichText::new(notice.text.as_str())
                    .size(12.5)
                    .color(notice.tone),
            )
            .truncate()
            .selectable(false),
        );

    let Some((label, _)) = action else {
        return false;
    };
    let box_ = Rect::from_min_max(
        Pos2::new(band.min.x + inset, band.min.y),
        Pos2::new(band.max.x - inset, band.max.y),
    );
    ui.new_child(
        UiBuilder::new()
            .id_salt("notice action")
            .max_rect(box_)
            .layout(Layout::right_to_left(Align::Center)),
    )
    .add(egui::Button::new(RichText::new(label.as_str()).size(12.5)).small())
    .on_hover_cursor(CursorIcon::PointingHand)
    .clicked()
}

/// How wide the label of the notice's button is, at the size the button draws it.
fn label_width(painter: &Painter, label: &str) -> f32 {
    painter
        .layout_no_wrap(
            label.to_string(),
            FontId::proportional(12.5),
            Color32::PLACEHOLDER,
        )
        .size()
        .x
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

    // Every cell has to land on its own column. A character the terminal face
    // cannot paint arrives from a fallback face with a different advance, and left
    // to itself that glyph drags the rest of the row sideways with it.
    #[test]
    fn a_fallback_glyph_leaves_the_grid_where_it_is() {
        let ctx = egui::Context::default();
        // The font atlas only exists once a pass has begun.
        ctx.begin_pass(egui::RawInput::default());
        let font = FontId::monospace(14.0);
        let cell = ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        assert!(cell > 0.0, "the test needs a font to measure");

        // A grinning face is in none of the text faces, so an emoji fallback
        // paints it - at an advance nothing like the terminal's.
        let cells = [('a', 1), ('\u{1f600}', 1), ('b', 1)];
        let mut job = LayoutJob::default();
        let mut guesses = Guesses::default();
        append_cells(
            &mut job,
            &mut guesses,
            &ctx,
            &font,
            cell,
            &cells,
            TextFormat::default(),
        );
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let last = galley.rows[0]
            .glyphs
            .last()
            .expect("the run should have painted");
        assert!(
            (last.pos.x - 2.0 * cell).abs() < 0.5,
            "the third cell drifted to {} instead of {}",
            last.pos.x,
            2.0 * cell
        );
    }

    /// Text the face can paint on its own stays one run: snapping a grid that
    /// already lines up would only buy layout work.
    #[test]
    fn text_that_already_lines_up_stays_one_run() {
        let ctx = egui::Context::default();
        // The font atlas only exists once a pass has begun.
        ctx.begin_pass(egui::RawInput::default());
        let font = FontId::monospace(14.0);
        let cell = ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M'));
        let cells: Vec<(char, u16)> = "docker compose up".chars().map(|ch| (ch, 1)).collect();
        let mut job = LayoutJob::default();
        let mut guesses = Guesses::default();
        append_cells(
            &mut job,
            &mut guesses,
            &ctx,
            &font,
            cell,
            &cells,
            TextFormat::default(),
        );
        assert_eq!(job.sections.len(), 1, "an aligned run must not split");
    }

    // One glyph from a fallback face used to drag every cell after it off its
    // column, which is how a box painted by a monitoring program ends up torn
    // apart. Whatever a font does with a glyph, it has to start on a boundary.
    #[test]
    fn every_glyph_lands_on_a_cell_column() {
        let emu = Arc::new(Mutex::new(Emulator::new(3, 60, 0)));
        let backend = Arc::new(Mutex::new(None));
        let palette = Palette::default();
        let cell = Vec2::new(8.0, 16.0);
        // Box drawing, the shades and the eighth blocks a bar is drawn from,
        // braille, and one ideograph: egui's own faces hold none of these.
        let art = ("\u{250c}\u{2500}\u{2500} Speed \u{2500}\u{2510}\u{2591}\u{2592}\u{2593}\u{2588}\u{2581}\u{2800}\u{7528} tail\u{0d}\u{0a}").repeat(3);
        emu.lock().unwrap().feed(art.as_bytes());

        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 300.0),
            )),
            ..Default::default()
        };
        let style = Style {
            font: FontId::monospace(14.0),
            cell,
            leading: 0.0,
            palette: &palette,
            cursor_on: false,
            pad: 0.0,
            radius: 0.0,
            border: Color32::TRANSPARENT,
        };
        let mut st = State::default();
        let mut output = ctx.run_ui(raw, |ui| {
            let _ = show(ui, &emu, &backend, &mut st, &style, false, None);
        });
        // There is no renderer here to apply the font texture to.
        output.textures_delta.clear();

        let mut columns: Vec<f32> = Vec::new();
        for clipped in &output.shapes {
            let egui::Shape::Text(painted) = &clipped.shape else {
                continue;
            };
            for row in &painted.galley.rows {
                for glyph in &row.glyphs {
                    columns.push(painted.pos.x + glyph.pos.x);
                }
            }
        }
        assert!(columns.len() > 60, "the grid painted nothing to check");
        let left = columns.iter().cloned().fold(f32::INFINITY, f32::min);
        for x in columns {
            let drift = (x - left).rem_euclid(cell.x);
            // egui lands a glyph on a whole pixel, so a column can sit a pixel off
            // its neighbour; anything wider than that has slid out of its cell.
            assert!(
                drift.min(cell.x - drift) < 1.5,
                "a glyph sits {} points off its column",
                drift.min(cell.x - drift)
            );
        }
    }
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
                leading: 0.0,
                palette: &palette,
                cursor_on: true,
                pad: 0.0,
                radius: 0.0,
                border: Color32::TRANSPARENT,
            };
            let mut output = ctx.run_ui(raw, |ui| {
                let _ = show(ui, &emu, &backend, &mut st, &style, menu_open, None);
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

    /// What one laid-out frame of the widget reported.
    struct Drawn {
        /// The cost of one cell, as the widget was told to lay it out.
        cell: Vec2,
        /// The grid the screen ended up with, in cells.
        grid: Vec2,
        out: Output,
        /// A press inside the cells starts a selection drag.
        dragging: bool,
        /// Every run of text the widget painted, with the height of its top.
        text: Vec<(String, f32)>,
        /// Every box the widget painted, filled or not.
        boxes: Vec<Rect>,
    }

    /// Draw the widget on its own, once per entry of `passes`, and report the
    /// last frame.
    ///
    /// A press only reaches a widget in the frame after the one that painted it:
    /// egui hit-tests against the rectangles the previous pass left behind, so a
    /// single frame can show a notice but never answer to its button.
    ///
    /// The widget is the only thing in the pass and the well has no padding, so
    /// the grid starts at the top-left of the window and the strip reserved
    /// beneath it sits at a height a test can name without measuring anything.
    fn frames(notice: Option<&Notice>, passes: Vec<Vec<egui::Event>>) -> Drawn {
        let emu = Arc::new(Mutex::new(Emulator::new(24, 80, 0)));
        let backend = Arc::new(Mutex::new(None));
        let palette = Palette::default();
        let cell = Vec2::new(8.0, 16.0);
        let style = Style {
            font: FontId::monospace(14.0),
            cell,
            leading: 0.0,
            palette: &palette,
            cursor_on: false,
            pad: 0.0,
            radius: 0.0,
            border: Color32::TRANSPARENT,
        };
        let mut st = State::default();
        let ctx = egui::Context::default();
        let mut drawn = Vec::new();
        for events in passes {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 400.0))),
                events,
                ..Default::default()
            };
            let mut out = Output::default();
            let mut output = ctx.run_ui(raw, |ui| {
                out = show(ui, &emu, &backend, &mut st, &style, false, notice);
            });
            // There is no renderer here to apply the font texture to.
            output.textures_delta.clear();
            let (rows, cols) = emu.lock().unwrap().size();
            let text: Vec<(String, f32)> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(painted) => {
                        Some((painted.galley.text().to_string(), painted.pos.y))
                    }
                    _ => None,
                })
                .collect();
            let boxes: Vec<Rect> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(boxed) => Some(boxed.rect),
                    _ => None,
                })
                .collect();
            drawn.push(Drawn {
                cell,
                grid: Vec2::new(cols as f32, rows as f32),
                out,
                dragging: st.dragging,
                text,
                boxes,
            });
        }
        drawn
            .pop()
            .expect("a widget that is never drawn has nothing to report")
    }

    /// One frame of [`frames`].
    fn drawn(notice: Option<&Notice>, events: Vec<egui::Event>) -> Drawn {
        frames(notice, vec![events])
    }

    /// The notice a session that went away is announced with.
    fn hung_up() -> Notice {
        Notice {
            text: "the peer hung up".to_string(),
            action: Some("Reconnect".to_string()),
            tone: Color32::YELLOW,
        }
    }

    /// A grid that has merely stopped moving looks exactly like a grid that is
    /// still thinking, so the reason it stopped has to be on the screen: in the
    /// well, and below every row the grid still paints.
    #[test]
    fn a_dead_session_says_so_inside_the_well() {
        let alive = drawn(None, Vec::new());
        let dead = drawn(Some(&hung_up()), Vec::new());
        assert!(
            dead.grid.y < alive.grid.y,
            "the notice was laid over the grid instead of taking a strip off it: {} -> {}",
            alive.grid.y,
            dead.grid.y,
        );
        let strip = dead.grid.y * dead.cell.y;
        let said = dead
            .text
            .iter()
            .find(|(line, _)| line.contains("the peer hung up"))
            .expect("nothing said why the session ended");
        assert!(
            said.1 >= strip && said.1 < strip + STRIP,
            "the reason sits at {} rather than in the strip from {strip} to {}",
            said.1,
            strip + STRIP,
        );
        assert!(
            alive
                .text
                .iter()
                .all(|(line, _)| !line.contains("the peer hung up")),
            "a session that is still running has nothing to report",
        );
    }

    /// The notice is not there to be read and left: its button has to start the
    /// whole thing over again.
    #[test]
    fn the_notice_button_starts_the_session_again() {
        assert!(
            !drawn(None, Vec::new()).out.restart,
            "a session with nothing to say asked for nothing",
        );
        let dead = drawn(Some(&hung_up()), Vec::new());
        let at = dead
            .boxes
            .iter()
            .filter(|boxed| {
                let strip = dead.grid.y * dead.cell.y;
                boxed.center().y >= strip
                    && boxed.max.y <= strip + STRIP
                    && boxed.width() > 1.0
                    && boxed.height() > 1.0
            })
            .max_by(|a, b| a.max.x.total_cmp(&b.max.x))
            .expect("the notice painted no button")
            .center();
        let pressed = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let released = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        // The frame that paints the button is the frame that cannot be
        // pressed: by the time the pointer answers, the button is there.
        let down = frames(
            Some(&hung_up()),
            vec![vec![], vec![egui::Event::PointerMoved(at), pressed.clone()]],
        );
        assert!(!down.out.restart, "a press on its own starts nothing");
        let click = frames(
            Some(&hung_up()),
            vec![
                vec![],
                vec![egui::Event::PointerMoved(at), pressed, released],
            ],
        );
        assert!(
            click.out.restart,
            "the button at {at:?} did not ask for the session again",
        );
    }

    /// The strip belongs to the notice rather than to the grid: reaching the button
    /// must not copy whatever text the pointer passed over.
    #[test]
    fn a_click_on_the_strip_does_not_select_cells() {
        let dead = drawn(Some(&hung_up()), Vec::new());
        let in_the_grid = egui::pos2(4.0, 2.0 * dead.cell.y);
        let on_the_strip = egui::pos2(4.0, dead.grid.y * dead.cell.y + STRIP - 8.0);
        for (at, want) in [(in_the_grid, true), (on_the_strip, false)] {
            let drawn = drawn(
                Some(&hung_up()),
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(
                drawn.dragging, want,
                "a press at {at:?} dragged: {}",
                drawn.dragging
            );
        }
    }
}
