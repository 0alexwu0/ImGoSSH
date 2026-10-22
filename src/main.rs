#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ImGoSSH: the desktop GUI.
//!
//! The SSH/SFTP/SCP engine lives in the library so that `igssh`, `igscp` and
//! `igsftp` can reuse it.

use clap::Parser;
use eframe::NativeOptions;
use imgossh::config::{Store, APP_NAME, APP_VERSION};
use imgossh::icon;
use imgossh::ui::{App, Startup};

#[derive(Parser)]
#[command(
    name = "imgossh",
    version = APP_VERSION,
    about = "SSH / SFTP / SCP workstation written in Rust"
)]
struct Cli {
    /// Open a local pseudo-console tab on startup
    #[arg(long)]
    local: bool,
    /// Open a saved session by name on startup
    #[arg(short, long)]
    session: Option<String>,
    /// Connect to [user@]host on startup
    host: Option<String>,
}

fn main() -> eframe::Result {
    env_logger::init();
    let cli = Cli::parse();

    let store = Store::load();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([store.config.window_width, store.config.window_height])
        .with_min_inner_size([760.0, 420.0])
        .with_title(format!("{APP_NAME} {APP_VERSION}"))
        .with_icon(app_icon());

    let startup = Startup {
        local: cli.local,
        session: cli.session,
        host: cli.host,
    };

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
