//! The egui front-end: session sidebar, terminal tabs, SFTP browser, dialogs.
//!
//! * [`app`] - the `eframe::App` shell that owns tabs, modals and the runtime.
//! * [`chrome`] - the native window frame kept in step with the theme.
//! * [`terminal`] - the VT100 widget.
//! * [`sidebar`] - the session tree.
//! * [`browser`] - the dual-pane SFTP / local file browser.
//! * [`dialogs`] - session properties, credentials, host keys, settings.
//! * [`properties`] - what one file is, and its permission bits.
//! * [`theme`] - the design tokens every panel paints with.

pub mod app;
pub mod browser;
pub mod chrome;
pub mod dialogs;
pub mod properties;
pub mod sidebar;
pub mod terminal;
pub mod theme;

pub use app::{App, Startup};

/// Draw into the room this [`egui::Ui`] was handed, and report exactly that
/// room back - never whatever the contents happened to ask for.
///
/// A resizable [`egui::Panel`] re-reads its own width from the rect its body
/// occupied on the previous frame. So a single child that overflows by a
/// pixel - a wide table, a long path, a row of buttons - widens the dock by a
/// pixel every frame until the terminal beside it is squeezed away. Here the
/// parent decides the size once and the contents are clipped to it.
pub fn fill_available<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = ui.available_rect_before_wrap();
    let layout = *ui.layout();
    fill_rect(ui, rect, layout, add_contents)
}

/// [`fill_available`] over one box the caller picked, with a layout of its own.
///
/// `Ui::allocate_ui` inherits the parent layout, so a list dropped into a
/// horizontal row lays its header out beside its body.
pub fn fill_rect<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    layout: egui::Layout,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
    let output = add_contents(&mut child);
    ui.advance_cursor_after_rect(rect);
    output
}
