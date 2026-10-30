//! ImGoSSH: an SSH / SFTP / SCP client with a tabbed terminal, an SFTP file
//! browser and tunnel management.
//!
//! The crate is split in three layers:
//!
//! * [`config`] - persisted sessions and settings (the session browser data).
//! * [`ssh`] - the SSH engine: connection actor, SFTP, transfers, tunnels.
//! * [`terminal`] - UI-agnostic VT100 emulation, key and mouse encoding.
//! * [`icon`] - the painted application icon, shared with `build.rs`.
//! * [`line_drawing`] - the DEC special graphics set, turned into box characters.
//! * [`update`] - the online updater: the newest release on GitHub, installed in place.
//! * [`ui`] - the egui/eframe front-end (tabs, sidebar, file browser, dialogs).
//!
//! The same engine powers the command line tools (`igssh`, `igscp`, `igsftp`).

pub mod cli;
pub mod config;
pub mod i18n;
pub mod icon;
pub mod line_drawing;
pub mod ssh;
pub mod terminal;
pub mod ui;
pub mod update;
pub mod util;

/// Things every binary and UI module needs.
pub mod prelude {
    pub use crate::config::{Config, Protocol, Session, Store};
    pub use crate::ssh::client::{ConnectOptions, Connection, Prompter, TermEvent, TermSink};
    pub use crate::terminal::Emulator;
    pub use crate::util::{human_size, human_time};
}
