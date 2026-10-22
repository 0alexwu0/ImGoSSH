//! The docked SFTP browser must never decide how wide its dock is.
//!
//! A resizable `egui::Panel` re-reads its own width from the rect its body
//! occupied on the previous frame. A browser that overflows by a pixel - a wide
//! table, a long path, a row of buttons - therefore widens the dock by a pixel
//! every frame until the terminal next to it is squeezed into a sliver.

use egui::{CentralPanel, Context, Panel, Pos2, RawInput, Rect, Vec2};
use imgossh::ssh::transfer::Queue;
use imgossh::ui::browser::Browser;
use imgossh::ui::fill_available;

const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);
/// Narrow single-pane dock through wide two-pane dock.
const DOCKS: [f32; 5] = [260.0, 360.0, 520.0, 760.0, 900.0];

/// Run `frames` frames of "dock on the right, terminal in what is left" and
/// report, per frame, (dock width, room left for the terminal, width the
/// browser actually drew itself at).
fn run(dock: f32, frames: usize) -> Vec<(f32, f32, f32)> {
    let ctx = Context::default();
    let queue = Queue::new();
    let mut browser = Browser::new();
    let mut raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        predicted_dt: 1.0 / 60.0,
        ..Default::default()
    };

    let mut seen = Vec::new();
    for index in 0..frames {
        raw.time = Some(index as f64 / 60.0);
        let mut output = ctx.run_ui(raw.clone(), |root| {
            CentralPanel::default().show(root, |ui| {
                let mut asked = 0.0;
                let panel = Panel::right("sftp-panel")
                    .resizable(true)
                    .default_size(dock)
                    .size_range(260.0..=900.0)
                    .show(ui, |ui| {
                        fill_available(ui, |ui| {
                            browser.ui(ui, None, &queue);
                            asked = ui.min_rect().width();
                        });
                    });
                seen.push((panel.response.rect.width(), ui.available_width(), asked));
            });
        });
        // There is no renderer here, so drain the font atlas by hand.
        output.textures_delta.clear();
        raw.events.clear();
    }
    seen
}

#[test]
fn dock_holds_its_width_frame_after_frame() {
    for dock in DOCKS {
        let seen = run(dock, 40);
        let widths: Vec<f32> = seen.iter().map(|(width, _, _)| *width).collect();
        let widest = widths.iter().copied().fold(f32::MIN, f32::max);
        let narrowest = widths.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(
            widest - narrowest <= 2.0,
            "a {dock} px dock drifted over 40 frames: {widths:?}"
        );

        let terminal = seen.last().unwrap().1;
        assert!(
            terminal > SCREEN.x - dock - 80.0,
            "a {dock} px dock ate into the terminal, leaving only {terminal} px"
        );
    }
}

#[test]
fn browser_fits_the_dock_it_was_given() {
    for dock in DOCKS {
        let seen = run(dock, 10);
        let (dock, _, asked) = *seen.last().unwrap();
        assert!(
            asked <= dock + 1.0,
            "browser drew itself {asked} px wide in a {dock} px dock - it is being clipped"
        );
    }
}
