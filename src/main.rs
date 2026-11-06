#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ImGoSSH: the desktop GUI.
//!
//! The SSH/SFTP/SCP engine lives in the library so that `igssh`, `igscp` and
//! `igsftp` can reuse it.

use clap::Parser;
use eframe::NativeOptions;
use imgossh::config::{Config, APP_NAME, APP_VERSION};
use imgossh::icon;
use imgossh::startup::Args;
use imgossh::ui::App;

fn main() -> eframe::Result {
    env_logger::init();
    let cli = Args::parse();

    let config = Config::load();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([config.window_width, config.window_height])
        .with_min_inner_size([760.0, 420.0])
        .with_title(format!("{APP_NAME} {APP_VERSION}"))
        .with_icon(app_icon());

    let startup = cli.into_startup(&config);

    eframe::run_native(
        APP_NAME,
        NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(App::with_startup(cc, startup)))),
    )
}

/// The window and taskbar icon: the accent tile from [`icon`], which is also
/// what `build.rs` bakes into the executable as its Windows icon.
fn app_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    egui::IconData {
        rgba: icon::paint(SIZE),
        width: SIZE,
        height: SIZE,
    }
}
