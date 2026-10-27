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
    /// Text drawn on top of the accent.
    pub accent_text: Color32,
    /// The accent at rest: selection fills and the tab on screen.
    pub accent_dim: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The three built-in looks. ImGo is the designed one; Dark and Light keep
/// egui's familiar greys but borrow the same accent and shapes.
pub fn tokens(theme: Theme, palette: &Palette) -> Tokens {
    let [fr, fg, fb] = palette.foreground;
    let foreground = rgb(fr, fg, fb);
    let (good, warn, bad) = (
        rgb(0x5d, 0xc3, 0x8f),
        rgb(0xe0, 0xb2, 0x52),
        rgb(0xe5, 0x69, 0x7a),
    );
    match theme {
        Theme::ImGo => Tokens {
            dark: true,
            surface: rgb(0x17, 0x1b, 0x21),
            raised: rgb(0x1f, 0x25, 0x2e),
            well: rgb(0x11, 0x14, 0x19),
            border: rgb(0x2b, 0x33, 0x3e),
            border_strong: rgb(0x46, 0x52, 0x62),
            button: rgb(0x23, 0x2b, 0x35),
            button_hover: rgb(0x2c, 0x36, 0x42),
            button_active: rgb(0x35, 0x40, 0x4e),
            text: foreground,
            muted: rgb(0x8f, 0x9a, 0xa9),
            accent: rgb(0x4d, 0x9f, 0xe8),
            accent_text: rgb(0xdb, 0xed, 0xff),
            accent_dim: rgb(0x24, 0x40, 0x5c),
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
            muted: rgb(0x9c, 0x9c, 0x9c),
            accent: rgb(0x5b, 0x9d, 0xd9),
            accent_text: rgb(0xe8, 0xf2, 0xff),
            accent_dim: rgb(0x2f, 0x5e, 0x8c),
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
            muted: rgb(0x5d, 0x68, 0x74),
            accent: rgb(0x2f, 0x6f, 0xed),
            accent_text: rgb(0xff, 0xff, 0xff),
            accent_dim: rgb(0xd8, 0xe6, 0xfb),
            good: rgb(0x1c, 0x86, 0x55),
            warn: rgb(0x9c, 0x6b, 0x0b),
            bad: rgb(0xc0, 0x3b, 0x49),
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
    v.selection = Selection {
        bg_fill: t.accent_dim,
        stroke: Stroke::new(1.0, t.accent_text),
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

/// Text drawn on the accent.
pub fn accent_text(ui: &Ui) -> Color32 {
    ui.visuals().selection.stroke.color
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
        (
            rgb(0x5d, 0xc3, 0x8f),
            rgb(0xe0, 0xb2, 0x52),
            rgb(0xe5, 0x69, 0x7a),
            rgb(0x8f, 0x9a, 0xa9),
        )
    } else {
        (
            rgb(0x1c, 0x86, 0x55),
            rgb(0x9c, 0x6b, 0x0b),
            rgb(0xc0, 0x3b, 0x49),
            rgb(0x5d, 0x68, 0x74),
        )
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
    let face = accent_text(ui);
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
