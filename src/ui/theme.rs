//! Design tokens: the colours, spacing and shapes every panel shares.
//!
//! Widgets are not supposed to invent a colour of their own. They ask for the
//! accent, the hairline or the muted label, so that the window reads as one
//! surface with a few deliberate highlights whichever theme is switched on.

use egui::style::{ScrollStyle, Selection};
use egui::ColorImage;
use egui::{
    Align, Color32, Context, CornerRadius, FontId, Frame, Id, Image, Margin, Rect, Response, Sense,
    Shadow, Spacing, Stroke, StrokeKind, Style, TextEdit, TextStyle, TextureHandle, TextureOptions,
    Theme as EguiTheme, ThemePreference, Ui, Vec2, Visuals, WidgetText,
};

use crate::config::{Palette, Theme};

/// Everything a frame is painted with.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    /// True when light text sits on dark surfaces.
    pub dark: bool,
    /// Panels, bars and the window behind them.
    pub surface: Color32,
    /// Cards, windows and popups: anything raised above the surface.
    pub raised: Color32,
    /// Recessed: text boxes and the terminal well.
    pub well: Color32,
    /// The hairline around a widget.
    pub border: Color32,
    /// The hairline that means "the pointer is here".
    pub border_strong: Color32,
    /// A button face at rest.
    pub button: Color32,
    /// A button face under the pointer.
    pub button_hover: Color32,
    /// A button face while pressed, and the inside of an open menu.
    pub button_active: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    /// The accent at rest: selection fills and the tab on screen.
    pub accent_dim: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The cyan of the `>` in the picture's `SSH>_` - `(37, 199, 246)`, the mean of
/// the glyph. It is the one loud colour the icon owns, so it is the one loud
/// colour the interface owns too. Text on it is the navy of the tile it was
/// lit by, which is also what the picture does with it.
const PROMPT: Color32 = rgb(0x25, 0xc7, 0xf6);
/// The green, amber and red of the three dots on the terminal in the picture -
/// `(2, 224, 81)`, `(246, 196, 48)`, `(246, 65, 82)` - each brought back from a
/// painted dot to a colour a whole line of text can be set in.
const LIVE: Color32 = rgb(0x35, 0xd0, 0x7a);
const BUSY: Color32 = rgb(0xef, 0xc1, 0x3c);
const BROKEN: Color32 = rgb(0xf2, 0x62, 0x6c);
/// A label sitting on [`PROMPT`] itself - a primary button, say - is the navy
/// of the tile the cyan is lit by, which is what the picture does with it.
const ON_PROMPT: Color32 = rgb(0x06, 0x20, 0x2e);
/// The same three for a light ground, where the dark-ground set would glare.
const LIVE_LIGHT: Color32 = rgb(0x15, 0x7a, 0x48);
const BUSY_LIGHT: Color32 = rgb(0x8a, 0x64, 0x10);
const BROKEN_LIGHT: Color32 = rgb(0xc0, 0x32, 0x3e);

/// The colours of `assets/icon.png`, measured off the picture rather than
/// invented beside it, so that the window and the tile in the taskbar are the
/// same design. The cat sleeps on a navy tile; its terminal glows blue along
/// the bottom-right rim; the prompt is cyan; and three dots - red, amber, green
/// - are the only traffic lights in the picture.
///
/// The tile's own body, `(9, 17, 30)` to `(12, 22, 38)`, is [`well`]: the
/// terminal well and the text boxes are the tile. What the app raises above it
/// steps up from there, and the bevel that catches the light along the tile's
/// top-left - `(80, 107, 153)`, `(119, 151, 199)` - is what a hairline and
/// secondary text are made of. The named colours below are those, held back a
/// little: a dot in a picture only has to survive next to navy, while a word of
/// it has to survive a whole line, and a hairline has to survive being one
/// pixel wide on a monitor that is not the one it was drawn on.
///
/// ImGo is the look built out of them. Dark and Light keep egui's familiar
/// greys but borrow the same accent and shapes, so a window of either still
/// belongs to the same cat.
///
/// [`well`]: Tokens::well
pub fn tokens(theme: Theme, palette: &Palette) -> Tokens {
    let [fr, fg, fb] = palette.foreground;
    let foreground = rgb(fr, fg, fb);
    let (good, warn, bad) = (LIVE, BUSY, BROKEN);
    match theme {
        Theme::ImGo => Tokens {
            dark: true,
            surface: rgb(0x12, 0x1c, 0x2a),
            raised: rgb(0x1b, 0x2a, 0x3e),
            well: rgb(0x0b, 0x14, 0x20),
            border: rgb(0x2c, 0x40, 0x58),
            border_strong: rgb(0x4e, 0x72, 0x97),
            button: rgb(0x22, 0x34, 0x49),
            button_hover: rgb(0x2b, 0x41, 0x5c),
            button_active: rgb(0x35, 0x53, 0x7a),
            text: foreground,
            muted: rgb(0x8f, 0xa6, 0xc2),
            accent: PROMPT,
            accent_dim: rgb(0x11, 0x3c, 0x5e),
            good,
            warn,
            bad,
        },
        Theme::Dark => Tokens {
            dark: true,
            surface: rgb(0x1b, 0x1b, 0x1b),
            raised: rgb(0x27, 0x27, 0x27),
            well: rgb(0x0e, 0x0e, 0x0e),
            border: rgb(0x47, 0x47, 0x47),
            border_strong: rgb(0x6d, 0x6d, 0x6d),
            button: rgb(0x3a, 0x3a, 0x3a),
            button_hover: rgb(0x4a, 0x4a, 0x4a),
            button_active: rgb(0x56, 0x56, 0x56),
            text: foreground,
            muted: rgb(0xa8, 0xa8, 0xa8),
            accent: PROMPT,
            accent_dim: rgb(0x24, 0x50, 0x6b),
            good,
            warn,
            bad,
        },
        Theme::Light => Tokens {
            dark: false,
            surface: rgb(0xf3, 0xf5, 0xf8),
            raised: rgb(0xff, 0xff, 0xff),
            well: rgb(0xff, 0xff, 0xff),
            border: rgb(0xd3, 0xd9, 0xe1),
            border_strong: rgb(0xa9, 0xb4, 0xc2),
            button: rgb(0xec, 0xef, 0xf3),
            button_hover: rgb(0xdf, 0xe5, 0xed),
            button_active: rgb(0xd0, 0xd9, 0xe5),
            text: rgb(0x1f, 0x23, 0x28),
            muted: rgb(0x5a, 0x6b, 0x80),
            accent: rgb(0x0b, 0x6b, 0x9c),
            accent_dim: rgb(0xd2, 0xea, 0xf6),
            good: LIVE_LIGHT,
            warn: BUSY_LIGHT,
            bad: BROKEN_LIGHT,
        },
    }
}

/// Dress the context with a theme. Idempotent: every field is written from
/// scratch, so switching themes never leaves paint from the previous one.
pub fn apply(ctx: &Context, theme: Theme, palette: &Palette, mono: f32) {
    let t = tokens(theme, palette);
    let egui_theme = if t.dark {
        EguiTheme::Dark
    } else {
        EguiTheme::Light
    };
    ctx.set_theme(if t.dark {
        ThemePreference::Dark
    } else {
        ThemePreference::Light
    });
    ctx.style_mut_of(egui_theme, |style| dress(style, theme, &t, mono));
}

fn dress(style: &mut Style, theme: Theme, t: &Tokens, mono: f32) {
    let mut visuals = match theme {
        Theme::Light => Visuals::light(),
        Theme::Dark | Theme::ImGo => Visuals::dark(),
    };
    paint(&mut visuals, t);
    style.visuals = visuals;
    style.spacing = spacing();
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Heading, FontId::proportional(21.0)),
        (TextStyle::Monospace, FontId::monospace(mono)),
    ]
    .into_iter()
    .collect();
    style.compact_menu_style = false;
    style.interaction.tooltip_delay = 0.35;
}

fn paint(v: &mut Visuals, t: &Tokens) {
    v.dark_mode = t.dark;
    v.override_text_color = Some(t.text);
    v.weak_text_color = Some(t.muted);
    v.panel_fill = t.surface;
    v.window_fill = t.raised;
    v.extreme_bg_color = t.well;
    v.text_edit_bg_color = Some(t.well);
    v.code_bg_color = t.well;
    v.faint_bg_color = if t.dark {
        Color32::from_white_alpha(7)
    } else {
        Color32::from_black_alpha(9)
    };
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.bad;
    // egui paints three things out of `selection.stroke`: the text caret, the
    // glyphs of a selected run, and the outline of a focused field. All three
    // are drawn on a well or on `accent_dim`, never on the accent itself, so
    // the stroke is the accent and the label colour for a filled button comes
    // from [`on_accent`] instead.
    v.selection = Selection {
        bg_fill: t.accent_dim,
        stroke: Stroke::new(1.0, t.accent),
    };

    v.window_corner_radius = CornerRadius::same(11);
    v.menu_corner_radius = CornerRadius::same(9);
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_shadow = shadow(4, 26);
    v.popup_shadow = shadow(2, 14);
    v.button_frame = true;
    v.collapsing_header_frame = true;
    v.striped = true;
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;

    let radius = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.raised;
    w.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = t.button;
    w.inactive.weak_bg_fill = t.button;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = t.button_hover;
    w.hovered.weak_bg_fill = t.button_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, t.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, t.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = t.button_active;
    w.active.weak_bg_fill = t.button_active;
    w.active.bg_stroke = Stroke::new(1.0, t.accent);
    w.active.fg_stroke = Stroke::new(1.0, t.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open = w.active;
}

fn shadow(offset: i8, blur: u8) -> Shadow {
    Shadow {
        offset: [0, offset],
        blur,
        spread: 0,
        color: Color32::from_black_alpha(110),
    }
}

fn spacing() -> Spacing {
    Spacing {
        item_spacing: Vec2::new(8.0, 6.0),
        window_margin: Margin::same(14),
        menu_margin: Margin::same(7),
        button_padding: Vec2::new(10.0, 5.0),
        interact_size: Vec2::new(32.0, 22.0),
        icon_width: 15.0,
        icon_width_inner: 9.0,
        icon_spacing: 6.0,
        indent: 16.0,
        slider_width: 96.0,
        combo_width: 120.0,
        text_edit_width: 140.0,
        tooltip_width: 460.0,
        menu_width: 250.0,
        scroll: ScrollStyle {
            floating: true,
            bar_width: 8.0,
            handle_min_length: 26.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Shared looks
// ---------------------------------------------------------------------------

/// The colour that means "this one".
pub fn accent(ui: &Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

/// The accent at rest: selection fills and the tab on screen.
pub fn accent_dim(ui: &Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

/// The colour a label gets when the accent is behind it rather than on it.
pub fn on_accent(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        ON_PROMPT
    } else {
        Color32::WHITE
    }
}

/// The hairline colour for this theme.
pub fn border(ui: &Ui) -> Color32 {
    ui.visuals().widgets.noninteractive.bg_stroke.color
}

/// The colour for secondary text.
pub fn muted(ui: &Ui) -> Color32 {
    let v = ui.visuals();
    v.weak_text_color.unwrap_or(v.text_color())
}

/// The four state colours: live, busy, broken, stopped.
pub fn tones(ui: &Ui) -> (Color32, Color32, Color32, Color32) {
    if ui.visuals().dark_mode {
        (LIVE, BUSY, BROKEN, rgb(0x8f, 0xa6, 0xc2))
    } else {
        (LIVE_LIGHT, BUSY_LIGHT, BROKEN_LIGHT, rgb(0x5a, 0x6b, 0x80))
    }
}
/// The colour a disabled or resting glyph should use.
pub fn faint(ui: &Ui) -> Color32 {
    ui.visuals().widgets.inactive.fg_stroke.color
}

/// A raised panel with a hairline: the box everything else sits in.
pub fn card(ui: &Ui) -> Frame {
    card_with(ui, 12)
}

/// A raised panel with a chosen inner margin.
pub fn card_with(ui: &Ui, margin: i8) -> Frame {
    let v = ui.visuals();
    Frame::new()
        .fill(v.window_fill)
        .stroke((1.0, v.widgets.noninteractive.bg_stroke.color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(margin))
}

/// The frame for a horizontal bar: menu, toolbar, status line.
pub fn bar(ui: &Ui) -> Frame {
    let v = ui.visuals();
    Frame::new()
        .fill(v.panel_fill)
        .inner_margin(Margin::symmetric(10, 5))
}

/// Padding inside a single line field. egui gives a field 2 px above and below
/// the text, which makes a whole form look pinched, so fields are their own
/// little panels instead: roomy enough that the caret and the descenders fit.
const FIELD_PADDING: Margin = Margin::symmetric(8, 6);

/// The height a single line field needs at the font size in use, so a form
/// still lines up when the user turns the text bigger.
#[must_use]
pub fn field_height(ui: &Ui) -> f32 {
    let font = ui.style().text_styles[&TextStyle::Body].clone();
    // Asking for the row height is what puts the atlas to work, so it wants
    // the mutable handle.
    ui.ctx().fonts_mut(|fonts| fonts.row_height(&font)) + 2.0 * FIELD_PADDING.top as f32
}

/// A single line field with the app padding, its text centred in it.
pub fn field<'buffer, B: egui::text_edit::TextBuffer + 'buffer>(
    text: &'buffer mut B,
) -> TextEdit<'buffer> {
    TextEdit::singleline(text)
        .margin(FIELD_PADDING)
        .vertical_align(Align::Center)
}

/// A button with no face until the pointer arrives.
pub fn ghost(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    ui.scope(|ui| {
        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
        ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
        ui.button(text)
    })
    .inner
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The one button in a dialog that should be noticed.
pub fn primary(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let accent = accent(ui);
    let lift = accent.linear_multiply(1.15);
    let face = on_accent(ui);
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.override_text_color = Some(face);
        for state in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
        ] {
            state.weak_bg_fill = accent;
            state.bg_fill = accent;
            state.bg_stroke = Stroke::new(1.0, accent);
            state.fg_stroke = Stroke::new(1.0, face);
        }
        v.widgets.hovered.weak_bg_fill = lift;
        v.widgets.hovered.bg_fill = lift;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, lift);
        ui.button(text)
    })
    .inner
}
/// The destructive button in a confirmation dialog.
pub fn danger(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let bad = tones(ui).2;
    let lift = bad.linear_multiply(1.15);
    // Both reds are dark enough that white text keeps its contrast.
    let face = Color32::WHITE;
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.override_text_color = Some(face);
        for state in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
        ] {
            state.weak_bg_fill = bad;
            state.bg_fill = bad;
            state.bg_stroke = Stroke::new(1.0, bad);
            state.fg_stroke = Stroke::new(1.0, face);
        }
        v.widgets.hovered.weak_bg_fill = lift;
        v.widgets.hovered.bg_fill = lift;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, lift);
        ui.button(text)
    })
    .inner
}
/// A small filled circle: the status light.
pub fn dot(ui: &mut Ui, color: Color32) {
    let size = 9.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size, size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0 - 1.0, color);
}

/// A pill used for counts and states.
pub fn badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    let font_id = ui.style().text_styles[&TextStyle::Small].clone();
    let galley = ui.painter().layout_no_wrap(text.to_string(), font_id, fg);
    let pad = Vec2::new(7.0, 2.5);
    let size = galley.size() + pad * 2.0;
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(9), bg);
    ui.painter().galley(rect.min + pad, galley, fg);
}

/// One read-out drawn as a pill: the number is painted on top of the bar
/// rather than inside a `ProgressBar`.
///
/// A `ProgressBar` paints its label in one colour across a fill in another, so
/// every label straddled the edge where the bar turns from empty track into
/// solid accent, green, amber or red - and one of those halves was always hard
/// to read. Here the fill keeps only the state colour's hue at a low alpha and
/// the label keeps the theme's text colour, so it reads the same on both halves.
pub fn meter(ui: &mut Ui, ratio: f32, width: f32, text: String, color: Color32) -> Response {
    let foreground = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(text, FontId::proportional(11.0), foreground);
    let height = 16.0;
    // A pill is never narrower than its own label: clipped digits read as a
    // wrong number rather than a truncated one.
    let size = Vec2::new(width.max(galley.size().x + 12.0), height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same((height / 2.0) as u8);
        let tint =
            |alpha: u8| Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
        ui.painter()
            .rect_filled(rect, radius, ui.visuals().extreme_bg_color);
        let filled = rect.width() * ratio.clamp(0.0, 1.0);
        if filled > 1.0 {
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(filled, rect.height())),
                CornerRadius::same(filled.min(height / 2.0) as u8),
                tint(110),
            );
        }
        ui.painter()
            .rect_stroke(rect, radius, (1.0, tint(90)), StrokeKind::Inside);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, foreground);
    }
    response
}

/// The application tile, drawn from the same pixels as the window icon, the
/// taskbar button and the `.ico` compiled into the executable.
///
/// The about window and the welcome card used to paint a hand-made `>_` badge,
/// which quietly drifted away from the real icon every time the icon was
/// retouched. There is now one picture and everybody borrows it.
pub fn logo(ui: &mut Ui, size: f32) {
    // Generous enough to stay sharp when the window is scaled up, and painted
    // exactly once: the texture is kept on the context between frames.
    const PAINT: u32 = 128;
    let ctx = ui.ctx().clone();
    let key = Id::new("app-logo");
    let cached = ctx.data_mut(|data| data.get_temp::<TextureHandle>(key));
    let texture = cached.unwrap_or_else(|| {
        let image = ColorImage::from_rgba_unmultiplied(
            [PAINT as usize, PAINT as usize],
            &crate::icon::paint(PAINT),
        );
        let texture = ctx.load_texture("imgossh-logo", image, TextureOptions::LINEAR);
        ctx.data_mut(|data| data.insert_temp(key, texture.clone()));
        texture
    });
    ui.add(
        Image::new(&texture)
            .fit_to_exact_size(Vec2::splat(size))
            .sense(Sense::hover()),
    );
}

/// A heading with an accent bar, used to break dialogs into sections.
pub fn section(ui: &mut Ui, title: &str) {
    let (accent, text) = (accent(ui), ui.visuals().text_color());
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, 13.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), accent);
        ui.label(egui::RichText::new(title).strong().size(12.5).color(text));
    });
    ui.add_space(3.0);
}

/// A segmented control: one recessed track with a highlighted choice.
///
/// Returns the index the user just picked, so callers keep a single `match`.
pub fn segmented(ui: &mut Ui, labels: &[&str], selected: usize) -> Option<usize> {
    let track = Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke((1.0, border(ui)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(2);
    let mut picked = None;
    track.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (index, label) in labels.iter().enumerate() {
                if ui.selectable_label(index == selected, *label).clicked() {
                    picked = Some(index);
                }
            }
        });
    });
    picked
}

/// A hairline that separates two groups of widgets.
pub fn hairline(ui: &mut Ui) {
    let color = border(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, (1.0, color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    /// The WCAG ratio between a label and the ground behind it. 4.5 is the line
    /// a body of text has to clear; 3.0 is what a border or a dot is allowed to
    /// settle for.
    fn contrast(fore: Color32, back: Color32) -> f32 {
        let lift = |color: Color32| {
            let [r, g, b, _] = color.to_normalized_gamma_f32();
            [r, g, b]
                .map(|channel| {
                    if channel <= 0.04045 {
                        channel / 12.92
                    } else {
                        ((channel + 0.055) / 1.055).powf(2.4)
                    }
                })
                .iter()
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(channel, weight)| channel * weight)
                .sum::<f32>()
        };
        let (a, b) = (lift(fore), lift(back));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn every_theme() -> [(Theme, Tokens); 3] {
        let palette = Palette::default();
        [
            (Theme::ImGo, tokens(Theme::ImGo, &palette)),
            (Theme::Dark, tokens(Theme::Dark, &palette)),
            (Theme::Light, tokens(Theme::Light, &palette)),
        ]
    }

    #[test]
    fn the_default_look_is_painted_from_the_picture() {
        // The whole point of the palette is that it is the icon's, so a change
        // here is a change to what the window and the tile in the taskbar have
        // in common. If one of these has to move, move the reasoning in the doc
        // comment on `tokens` with it.
        let t = tokens(Theme::ImGo, &Palette::default());
        assert_eq!(t.accent, PROMPT);
        assert_eq!(t.well, rgb(0x0b, 0x14, 0x20));
        assert_eq!(t.surface, rgb(0x12, 0x1c, 0x2a));
        assert_eq!(t.raised, rgb(0x1b, 0x2a, 0x3e));
        assert_eq!((t.good, t.warn, t.bad), (LIVE, BUSY, BROKEN));
        // The grounds step up towards the eye, the way the tile does: the well
        // is the picture itself, the surface floats on it, a card floats higher.
        for (a, b) in [
            (t.well, t.surface),
            (t.surface, t.raised),
            (t.raised, t.button),
        ] {
            assert!(contrast(a, b) > 1.05, "{a:?} and {b:?} read as one ground");
        }
    }

    #[test]
    fn every_label_beats_the_ground_it_is_read_on() {
        for (theme, t) in every_theme() {
            for (name, ink) in [("text", t.text), ("muted", t.muted), ("accent", t.accent)] {
                for (ground, back) in [
                    ("well", t.well),
                    ("surface", t.surface),
                    ("raised", t.raised),
                    ("button", t.button),
                ] {
                    let ratio = contrast(ink, back);
                    assert!(
                        ratio > 4.5,
                        "{theme:?} {name} on {ground} is only {ratio:.2}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn a_state_colour_survives_the_sentence_it_begins() {
        // live, busy and broken narrate a line of status text, so they hold the
        // body-text bar on every panel a line of status text can sit on. On a
        // button face they are painted as a dot or a border, where 3.0 is the
        // bar - and where pushing them brighter would cost the white label of
        // `danger` the ground under it.
        for (theme, t) in every_theme() {
            for (name, ink) in [("good", t.good), ("warn", t.warn), ("bad", t.bad)] {
                for (ground, back) in [
                    ("well", t.well),
                    ("surface", t.surface),
                    ("raised", t.raised),
                ] {
                    let ratio = contrast(ink, back);
                    assert!(
                        ratio > 4.5,
                        "{theme:?} {name} on {ground} is only {ratio:.2}:1"
                    );
                }
                let ratio = contrast(ink, t.button);
                assert!(
                    ratio > 3.0,
                    "{theme:?} {name} on a button face is only {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn the_caret_and_a_selected_word_survive_the_well_they_sit_in() {
        // egui paints the caret, the glyphs of a selection and the outline of a
        // focused field all out of `selection.stroke`, and all three appear on a
        // well or on `accent_dim`. A label colour borrowed from a filled button
        // vanishes there, which is how a field ends up with no visible caret.
        for (theme, t) in every_theme() {
            assert!(
                contrast(t.accent, t.well) > 4.5,
                "{theme:?} caret on a well is barely a scratch"
            );
            assert!(
                contrast(t.accent, t.accent_dim) > 3.0,
                "{theme:?} selection text drowns in the selection"
            );
        }
    }

    #[test]
    fn a_primary_button_writes_in_a_colour_its_own_fill_respects() {
        assert!(contrast(PROMPT, ON_PROMPT) > 4.5);
        for (theme, t) in every_theme() {
            let ink = if t.dark { ON_PROMPT } else { Color32::WHITE };
            let ratio = contrast(ink, t.accent);
            assert!(ratio > 4.5, "{theme:?} primary label is only {ratio:.2}:1");
        }
    }

    #[test]
    fn a_hairline_is_visible_without_being_shouted() {
        for (theme, t) in every_theme() {
            let quiet = contrast(t.border, t.surface);
            let insistent = contrast(t.border_strong, t.surface);
            assert!(
                quiet > 1.25 && quiet < 4.0,
                "{theme:?} hairline is {quiet:.2}:1 against the surface"
            );
            assert!(
                insistent > quiet,
                "{theme:?} the strong hairline is not the stronger one"
            );
        }
    }
}
