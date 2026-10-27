//! Modal dialogs: session properties, credentials, host keys, tunnels, settings.

use egui::{CornerRadius, FontId, RichText, Sense, Ui, Vec2};

use crate::config::{Config, Protocol, Session, Tunnel, TunnelKind};
use crate::i18n::{tr, Lang};
use crate::ssh::client::{Challenge, HostKeyInfo, Reply, Trust};
use crate::ssh::transfer::{Phase, Queue};
use crate::t;
use crate::ui::theme;
use crate::util::{human_rate, human_size};

/// Result of the session properties dialog.
#[derive(Debug, PartialEq)]
pub enum EditorOutcome {
    Keep,
    Save(Session),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Session,
    Tunnel,
    Advanced,
}

/// The session properties window: create a new session or edit a saved one.
pub struct SessionEditor {
    pub session: Session,
    pub title: String,
    tab: EditorTab,
    show_password: bool,
    /// Mirrors "Save password"; the value is cleared on save when false.
    pub store_password: bool,
}

impl SessionEditor {
    pub fn new(session: Session, title: impl Into<String>) -> Self {
        Self {
            session,
            title: title.into(),
            tab: EditorTab::Session,
            show_password: false,
            store_password: true,
        }
    }

    /// Seed the "Save password" box from `Config::store_passwords`.
    pub fn with_store_passwords(mut self, store: bool) -> Self {
        self.store_password = store;
        self
    }

    /// Draw the dialog.
    pub fn show(&mut self, ui: &mut Ui) -> EditorOutcome {
        let mut close = false;
        let mut save = false;
        let mut open = true;
        egui::Window::new(&self.title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(Vec2::new(520.0, 420.0))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                // A segmented control: three pages of one form.
                let pages = [
                    (t!("Session"), EditorTab::Session),
                    (t!("Tunnel"), EditorTab::Tunnel),
                    (t!("Advanced SSH settings"), EditorTab::Advanced),
                ];
                let current = pages
                    .iter()
                    .position(|(_, page)| *page == self.tab)
                    .unwrap_or(0);
                let labels: [&str; 3] = [pages[0].0, pages[1].0, pages[2].0];
                if let Some(index) = theme::segmented(ui, &labels, current) {
                    self.tab = pages[index].1;
                }
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(520.0)
                    .show(ui, |ui| match self.tab {
                        EditorTab::Session => self.session_tab(ui),
                        EditorTab::Tunnel => self.tunnel_tab(ui),
                        EditorTab::Advanced => self.advanced_tab(ui),
                    });

                theme::hairline(ui);
                ui.horizontal_wrapped(|ui| {
                    if theme::primary(ui, t!("OK")).clicked() {
                        save = true;
                        close = true;
                    }
                    if ui.button(t!("Cancel")).clicked() {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost(ui, t!("Session log"))
                            .on_hover_text(t!("Write the session to the log"))
                            .clicked()
                        {
                            log::info!("session: {:?}", self.session);
                        }
                    });
                });
            });
        if save {
            if !self.store_password {
                self.session.password.clear();
            }
            EditorOutcome::Save(self.session.clone())
        } else if close || !open {
            EditorOutcome::Cancel
        } else {
            EditorOutcome::Keep
        }
    }

    fn session_tab(&mut self, ui: &mut Ui) {
        let s = &mut self.session;
        egui::Grid::new("session-basic")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label(t!("Session name"));
                ui.add_sized([300.0, theme::field_height(ui)], theme::field(&mut s.name));
                ui.end_row();

                ui.label(t!("Protocol"));
                let kinds = [
                    Protocol::Ssh,
                    Protocol::Sftp,
                    Protocol::Scp,
                    Protocol::Local,
                ];
                let names: Vec<&str> = kinds.iter().map(|kind| tr(kind.label())).collect();
                let current = kinds
                    .iter()
                    .position(|kind| *kind == s.protocol)
                    .unwrap_or(0);
                if let Some(index) = theme::segmented(ui, &names, current) {
                    s.protocol = kinds[index];
                }
                ui.end_row();

                let remote = s.protocol.is_remote();
                ui.label(t!("Remote host"));
                ui.add_enabled_ui(remote, |ui| {
                    ui.add_sized(
                        [300.0, theme::field_height(ui)],
                        theme::field(&mut s.host).hint_text(t!("example.com")),
                    );
                });
                ui.end_row();

                ui.label(t!("Port"));
                ui.add_enabled_ui(remote, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut s.port)
                            .speed(1)
                            .range(1..=u16::MAX),
                    );
                });
                ui.end_row();

                ui.label(t!("Username"));
                ui.add_enabled_ui(remote, |ui| {
                    ui.add_sized(
                        [300.0, theme::field_height(ui)],
                        theme::field(&mut s.username),
                    );
                });
                ui.end_row();

                ui.label(t!("Password"));
                ui.add_enabled_ui(remote, |ui| {
                    ui.horizontal(|ui| {
                        let field = if self.show_password {
                            theme::field(&mut s.password)
                        } else {
                            theme::field(&mut s.password).password(true)
                        };
                        ui.add_sized(
                            [238.0, theme::field_height(ui)],
                            field.hint_text(t!("leave empty to be asked on connect")),
                        );
                        ui.toggle_value(&mut self.show_password, t!("\u{1f441}"))
                            .on_hover_text(t!("Show / hide the password"));
                    });
                });
                ui.end_row();

                ui.label(t!("Save password"));
                ui.add_enabled_ui(remote, |ui| {
                    ui.checkbox(&mut self.store_password, t!("Keep it in the session list"))
                        .on_hover_text(t!("Uncheck to be asked for the password on every connect"));
                });
                ui.end_row();

                ui.label(t!("Folder"));
                ui.add_sized(
                    [300.0, theme::field_height(ui)],
                    theme::field(&mut s.folder)
                        .hint_text(t!("optional - groups sessions in the sidebar")),
                );
                ui.end_row();

                if s.protocol == Protocol::Scp {
                    ui.label(t!("SCP direction"));
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut s.scp_direction,
                            crate::config::ScpDirection::Download,
                            "Download",
                        );
                        ui.selectable_value(
                            &mut s.scp_direction,
                            crate::config::ScpDirection::Upload,
                            "Upload",
                        );
                    });
                    ui.end_row();
                    ui.label(t!("Remote path"));
                    ui.add_sized(
                        [300.0, theme::field_height(ui)],
                        theme::field(&mut s.remote_path),
                    );
                    ui.end_row();
                    ui.label(t!("Local path"));
                    ui.add_sized(
                        [300.0, theme::field_height(ui)],
                        theme::field(&mut s.local_path),
                    );
                    ui.end_row();
                }

                if s.protocol == Protocol::Sftp {
                    ui.label(t!("Remote path"));
                    ui.add_sized(
                        [300.0, theme::field_height(ui)],
                        theme::field(&mut s.remote_path),
                    );
                    ui.end_row();
                }
            });
    }

    fn tunnel_tab(&mut self, ui: &mut Ui) {
        let s = &mut self.session;
        ui.checkbox(
            &mut s.auto_tunnels,
            t!("Open the tunnels when the session starts"),
        );
        ui.separator();

        let mut remove: Option<usize> = None;
        egui::Grid::new("tunnels")
            .num_columns(6)
            .spacing([8.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for label in ["Type", "Listen port", "Remote host", "Remote port", "", ""] {
                    ui.strong(label);
                }
                ui.end_row();
                for (index, tunnel) in s.tunnels.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        for kind in [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic] {
                            if ui
                                .selectable_label(tunnel.kind == kind, tr(kind.label()))
                                .clicked()
                            {
                                tunnel.kind = kind;
                            }
                        }
                    });
                    ui.add(
                        egui::DragValue::new(&mut tunnel.listen_port)
                            .speed(1)
                            .range(1..=u16::MAX),
                    );
                    let host_enabled = tunnel.kind != TunnelKind::Dynamic;
                    ui.add_enabled_ui(host_enabled, |ui| {
                        ui.add_sized(
                            [120.0, theme::field_height(ui)],
                            theme::field(&mut tunnel.remote_host),
                        );
                    });
                    ui.add_enabled_ui(host_enabled, |ui| {
                        ui.add(
                            egui::DragValue::new(&mut tunnel.remote_port)
                                .speed(1)
                                .range(1..=u16::MAX),
                        );
                    });
                    ui.label(tunnel.describe());
                    if ui.button(t!("\u{1f5d}")).clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            s.tunnels.remove(index);
        }
        if ui.button(t!("+ Add a tunnel")).clicked() {
            s.tunnels.push(Tunnel::default());
        }
        ui.separator();
        ui.label(t!("Local (L): listen here, forward to the remote target.\nRemote (R): listen on the server, forward back here.\nDynamic (D): SOCKS5 proxy on the local port."));
    }

    fn advanced_tab(&mut self, ui: &mut Ui) {
        let s = &mut self.session;
        egui::Grid::new("advanced")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label(t!("Private key (.pem / id_*)"));
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [230.0, theme::field_height(ui)],
                        theme::field(&mut s.private_key),
                    );
                    if ui.button(t!("Browse")).clicked() {
                        if let Some(path) = pick_key_file() {
                            s.private_key = path;
                        }
                    }
                });
                ui.end_row();

                ui.label(t!("Authentication"));
                ui.checkbox(&mut s.use_agent, t!("Use ssh-agent / Pageant"));
                ui.end_row();

                ui.label(t!("Terminal type (TERM)"));
                ui.add_sized([230.0, theme::field_height(ui)], theme::field(&mut s.term));
                ui.end_row();

                ui.label(t!("Window size"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut s.cols)
                            .speed(1)
                            .range(20..=500)
                            .suffix(" cols"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut s.rows)
                            .speed(1)
                            .range(4..=200)
                            .suffix(" rows"),
                    );
                });
                ui.end_row();

                ui.label(t!("Keepalive (seconds, 0 = off)"));
                ui.add(
                    egui::DragValue::new(&mut s.keepalive)
                        .speed(1)
                        .range(0..=600),
                );
                ui.end_row();

                ui.label(t!("Run command instead of shell"));
                ui.add_sized(
                    [230.0, theme::field_height(ui)],
                    theme::field(&mut s.startup_command),
                );
                ui.end_row();

                ui.label(t!("Execute script on start"));
                ui.add_sized(
                    [230.0, theme::field_height(ui)],
                    theme::field(&mut s.startup_script),
                );
                ui.end_row();

                ui.label(t!("SFTP browser"));
                ui.checkbox(
                    &mut s.sftp_panel,
                    t!("Open the file browser with this session"),
                );
                ui.end_row();
            });
    }
}

/// Ask the user to approve a server key.
pub fn host_key_dialog(ui: &mut Ui, info: &HostKeyInfo) -> Option<Trust> {
    let mut decision = None;
    let mut open = true;
    egui::Window::new(if info.is_conflict() {
        "Security warning"
    } else {
        "Unknown host key"
    })
    .open(&mut open)
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
    .show(ui.ctx(), |ui| {
        ui.set_width(430.0);
        let (_, _, bad, muted) = theme::tones(ui);
        if info.is_conflict() {
            ui.label(
                RichText::new(t!("The host key changed since the last connection."))
                    .strong()
                    .color(bad),
            );
            ui.label(t!("This can mean a man-in-the-middle attack. Only accept the new key if you\nexpect the server to have been reinstalled."));
            ui.monospace(format!("old: {}", info.recorded.as_deref().unwrap_or("?")));
        } else {
            ui.label(t!("The server's identity has not been verified yet."));
        }
        ui.add_space(8.0);
        egui::Frame::canvas(ui.style())
            .inner_margin(10)
            .corner_radius(CornerRadius::same(8))
            .show(ui, |ui| {
                egui::Grid::new("hostkey").num_columns(2).show(ui, |ui| {
                    ui.label(RichText::new(t!("Host")).size(11.5).color(muted));
                    ui.monospace(format!("{}:{}", info.host, info.port));
                    ui.end_row();
                    ui.label(RichText::new(t!("Key type")).size(11.5).color(muted));
                    ui.monospace(&info.algorithm);
                    ui.end_row();
                    ui.label(RichText::new(t!("Fingerprint")).size(11.5).color(muted));
                    ui.monospace(&info.fingerprint);
                    ui.end_row();
                });
            });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if theme::primary(ui, t!("Accept and store")).clicked() {
                decision = Some(Trust::Always);
            }
            if ui.button(t!("Accept once")).clicked() {
                decision = Some(Trust::Once);
            }
            if ui.button(t!("Reject")).clicked() {
                decision = Some(Trust::Never);
            }
        });
    });
    if decision.is_none() && !open {
        return Some(Trust::Never);
    }
    decision
}

/// Ask for a password, a key passphrase, or 2FA answers.
pub fn challenge_dialog(
    ui: &mut Ui,
    challenge: &Challenge,
    buffer: &mut ChallengeBuffer,
) -> Option<Reply> {
    let mut answer = None;
    let mut open = true;
    let title = match challenge {
        Challenge::Password { host, .. } => t!("Password for {0}", host),
        Challenge::Passphrase { key, .. } => t!("Passphrase for {0}", key.display()),
        Challenge::KeyboardInteractive { host, .. } => t!("Two-factor authentication - {0}", host),
        Challenge::HostKey(_) => t!("Host key").to_string(),
    };
    let (_, _, bad, _) = theme::tones(ui);
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            match challenge {
                Challenge::Password {
                    host,
                    username,
                    incorrect,
                } => {
                    ui.label(format!("{username}@{host}"));
                    if *incorrect {
                        ui.label(RichText::new(t!("Access denied, try again.")).color(bad));
                    }
                    field(ui, &mut buffer.secret, false);
                }
                Challenge::Passphrase { key, incorrect } => {
                    ui.label(t!("The private key {0} is encrypted.", key.display()));
                    if *incorrect {
                        ui.label(RichText::new(t!("Wrong passphrase.")).color(bad));
                    }
                    field(ui, &mut buffer.secret, false);
                }
                Challenge::KeyboardInteractive {
                    name,
                    instructions,
                    prompts,
                    ..
                } => {
                    if !name.is_empty() {
                        ui.strong(name);
                    }
                    if !instructions.is_empty() {
                        ui.label(instructions);
                    }
                    while buffer.responses.len() < prompts.len() {
                        buffer.responses.push(String::new());
                    }
                    for (index, prompt) in prompts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(&prompt.text);
                            field(ui, &mut buffer.responses[index], prompt.echo);
                        });
                    }
                }
                Challenge::HostKey(_) => {}
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if theme::primary(ui, t!("OK")).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    answer = Some(match challenge {
                        Challenge::KeyboardInteractive { .. } => {
                            Reply::Responses(buffer.responses.clone())
                        }
                        _ => Reply::Secret(buffer.secret.clone()),
                    });
                }
                if ui.button(t!("Cancel")).clicked() {
                    answer = Some(Reply::Cancel);
                }
            });
        });
    if answer.is_none() && !open {
        return Some(Reply::Cancel);
    }
    answer
}

fn field(ui: &mut Ui, value: &mut String, echo: bool) {
    let mut edit = theme::field(value).desired_width(220.0);
    if !echo {
        edit = edit.password(true);
    }
    let response = ui.add(edit);
    // Grabbing focus on *every* frame reselects the whole value, so each
    // keystroke would replace what was typed before it.
    if !response.has_focus() {
        response.request_focus();
    }
}

/// Answers collected for the current challenge.
#[derive(Default)]
pub struct ChallengeBuffer {
    pub secret: String,
    pub responses: Vec<String>,
}

/// The settings window. Returns whether anything changed; `open` is cleared
/// when the user closes it.
pub fn settings_dialog(ui: &mut Ui, config: &mut Config, open: &mut bool) -> bool {
    *open = true;
    let mut changed = false;
    let (_, warn, _, muted) = theme::tones(ui);
    egui::Window::new(t!("Settings"))
        .open(open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(400.0, 470.0))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    theme::section(ui, t!("Language"));
                    let chosen = Lang::parse(&config.language);
                    let labels = [
                        t!("System default (auto)"),
                        Lang::En.native_name(),
                        Lang::Zh.native_name(),
                    ];
                    let current = match chosen {
                        None => 0,
                        Some(Lang::En) => 1,
                        Some(Lang::Zh) => 2,
                    };
                    if let Some(index) = theme::segmented(ui, &labels, current) {
                        config.language = ["auto", "en", "zh"][index].into();
                        changed = true;
                    }

                    theme::section(ui, t!("Terminal"));
                    egui::Grid::new("settings-terminal")
                        .num_columns(2)
                        .spacing([14.0, 7.0])
                        .show(ui, |ui| {
                            ui.label(t!("Font size"));
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut config.font_size)
                                        .speed(0.2)
                                        .range(8.0..=32.0)
                                        .suffix(" px"),
                                )
                                .on_hover_text(t!("Terminal font size"))
                                .changed();
                            ui.end_row();

                            ui.label(t!("Line spacing"));
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut config.line_spacing)
                                        .speed(0.05)
                                        .range(0.8..=2.5),
                                )
                                .on_hover_text(t!(
                                    "Extra air between terminal rows. Larger values suit Chinese."
                                ))
                                .changed();
                            ui.end_row();

                            ui.label(t!("Scrollback lines"));
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut config.scrollback)
                                        .speed(50)
                                        .range(0..=200_000),
                                )
                                .changed();
                            ui.end_row();
                        });
                    ui.add_space(2.0);
                    changed |= ui
                        .checkbox(&mut config.cursor_blink, t!("Blinking cursor"))
                        .changed();
                    changed |= ui
                        .checkbox(&mut config.copy_on_select, t!("Copy on select"))
                        .changed();
                    ui.add_space(6.0);
                    ui.label(RichText::new(t!("Colour scheme")).size(11.5).color(muted));
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 9.0;
                        for name in crate::config::Palette::names() {
                            let preset = crate::config::Palette::preset(name);
                            if palette_swatch(ui, name, &preset, config.palette == name)
                                .on_hover_text(t!("Terminal colour scheme"))
                                .clicked()
                            {
                                config.palette = name.to_string();
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(t!("Theme")).size(11.5).color(muted));
                    let looks = [
                        ("ImGo", crate::config::Theme::ImGo),
                        ("Dark", crate::config::Theme::Dark),
                        ("Light", crate::config::Theme::Light),
                    ];
                    let current = looks
                        .iter()
                        .position(|(_, look)| *look == config.theme)
                        .unwrap_or(0);
                    let names: [&str; 3] = [looks[0].0, looks[1].0, looks[2].0];
                    if let Some(index) = theme::segmented(ui, &names, current) {
                        config.theme = looks[index].1;
                        changed = true;
                    }

                    theme::section(ui, t!("Default session"));
                    egui::Grid::new("settings-default")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label(t!("Username"));
                            changed |= ui
                                .add_sized(
                                    [220.0, theme::field_height(ui)],
                                    theme::field(&mut config.default_username),
                                )
                                .changed();
                            ui.end_row();
                            ui.label(t!("Port"));
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut config.default_port)
                                        .speed(1)
                                        .range(1..=u16::MAX),
                                )
                                .changed();
                            ui.end_row();
                            ui.label(t!("TERM"));
                            changed |= ui
                                .add_sized(
                                    [220.0, theme::field_height(ui)],
                                    theme::field(&mut config.default_term),
                                )
                                .changed();
                            ui.end_row();
                            ui.label(t!("Keepalive"));
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut config.default_keepalive)
                                        .range(0..=600),
                                )
                                .changed();
                            ui.end_row();
                        });
                    changed |= ui
                        .checkbox(
                            &mut config.open_sftp_panel,
                            t!("Open the SFTP browser with new sessions"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut config.show_server_monitor,
                            t!("Show live server stats in the status bar"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut config.confirm_close,
                            t!("Ask before closing a live tab"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut config.store_passwords,
                            t!("Save passwords in sessions.toml"),
                        )
                        .changed();
                    if config.store_passwords {
                        ui.label(
                            RichText::new(t!(
                                "Passwords are stored in clear text in sessions.toml."
                            ))
                            .size(11.5)
                            .color(warn),
                        );
                    }
                });
        });
    changed
}

/// The transfer queue window.
pub fn transfers_dialog(ui: &mut Ui, queue: &Queue) -> bool {
    let mut open = true;
    egui::Window::new(t!("Transfers"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(520.0, 260.0))
        .show(ui.ctx(), |ui| {
            let jobs = queue.jobs();
            if jobs.is_empty() {
                ui.label(t!("No transfers."));
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (good, _, bad, _) = theme::tones(ui);
                    let (accent, muted) = (theme::accent(ui), theme::muted(ui));
                    for job in jobs {
                        let state = job.snapshot();
                        theme::card_with(ui, 10).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(job.direction.arrow())
                                        .size(13.0)
                                        .color(accent),
                                );
                                ui.label(RichText::new(&job.source).size(12.0).strong());
                                ui.label(RichText::new(t!("\u{2192}")).size(12.0).color(muted));
                                ui.label(RichText::new(&job.target).size(12.0).color(muted));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if !job.is_finished()
                                            && ui.small_button(t!("Cancel")).clicked()
                                        {
                                            job.cancel();
                                        }
                                    },
                                );
                            });
                            // Same on top of the bar as everywhere else: the
                            // fill is a tint, the label keeps its own colour.
                            let tone = match state.phase {
                                Phase::Done => good,
                                Phase::Failed(_) => bad,
                                Phase::Cancelled => muted,
                                _ => accent,
                            };
                            theme::meter(
                                ui,
                                state.percent() / 100.0,
                                ui.available_width(),
                                t!("{0} - {1}", state.phase.label(), state.current),
                                tone,
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} / {} - {}",
                                    human_size(state.done),
                                    human_size(state.total),
                                    human_rate(state.rate())
                                ))
                                .size(11.0)
                                .color(muted),
                            );
                        });
                        ui.add_space(6.0);
                    }
                });
            if ui.button(t!("Clear finished")).clicked() {
                queue.clear_finished();
            }
        });
    !open
}

/// Small "about" window.
pub fn about_dialog(ui: &mut Ui) -> bool {
    let mut open = true;
    egui::Window::new(t!("About ImGoSSH"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            let (muted, text) = (theme::muted(ui), ui.visuals().text_color());
            ui.set_width(360.0);
            ui.horizontal(|ui| {
                theme::logo(ui, 42.0);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(t!("ImGoSSH {0}", crate::config::APP_VERSION))
                            .size(18.0)
                            .strong()
                            .color(text),
                    );
                    ui.label(
                        RichText::new(t!("Terminal, file browser and tunnels in one native app."))
                            .size(11.5)
                            .color(muted),
                    );
                });
            });
            ui.add_space(10.0);
            theme::hairline(ui);
            ui.add_space(6.0);
            for line in [
                t!("Terminal: vt100 + ConPTY"),
                t!("Transport: russh (SSH2) + russh-sftp"),
                t!("UI: egui / eframe"),
            ] {
                ui.label(RichText::new(line).size(11.5).color(muted));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new(t!("Author: {0}", crate::config::APP_AUTHOR))
                    .size(11.5)
                    .color(text),
            );
            ui.label(
                RichText::new(t!("License: Apache License 2.0"))
                    .size(11.5)
                    .color(text),
            );
            ui.label(
                RichText::new(t!(
                    "Free to use, change and redistribute: see the LICENSE file for the full terms."
                ))
                .size(11.0)
                .color(muted),
            );
            ui.label(
                RichText::new(format!(
                    "Copyright \u{a9} 2026 {}.",
                    crate::config::APP_AUTHOR
                ))
                .size(11.0)
                .color(muted),
            );
            ui.add_space(10.0);
            let github = format!("{} {}", "\u{2197}", t!("GitHub repository"));
            if theme::primary(ui, github)
                .on_hover_text(crate::config::REPOSITORY_URL)
                .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::same_tab(crate::config::REPOSITORY_URL));
            }
            ui.label(
                RichText::new(crate::config::REPOSITORY_URL)
                    .size(10.5)
                    .color(muted),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(t!("Settings: {0}", crate::config::store_dir().display()))
                    .size(11.0)
                    .color(muted),
            );
        });
    !open
}

/// Native "open" dialog used by the private-key browser button.
pub fn pick_key_file() -> Option<String> {
    let picked = rfd::FileDialog::new()
        .set_title(t!("Select a private key"))
        .add_filter("Private keys", &["pem", "key", "ppk", "id_*"])
        .add_filter("All files", &["*"])
        .pick_file();
    picked.map(|path| path.to_string_lossy().into_owned())
}

/// A clickable preview of one terminal colour scheme.
fn palette_swatch(
    ui: &mut Ui,
    name: &str,
    preset: &crate::config::Palette,
    selected: bool,
) -> egui::Response {
    let (accent, hairline, muted) = (theme::accent(ui), theme::border(ui), theme::muted(ui));
    let size = Vec2::new(76.0, 58.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let swatch = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 32.0));
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(9);
        if selected {
            ui.painter().rect_filled(swatch.expand(2.5), radius, accent);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(swatch.expand(2.5), radius, hairline);
        }
        let bg = egui::Color32::from_rgb(
            preset.background[0],
            preset.background[1],
            preset.background[2],
        );
        ui.painter().rect_filled(swatch, radius, bg);
        let chips = [1usize, 2, 3, 6];
        let inner = swatch.shrink(7.0);
        let gap = 3.0;
        let w = (inner.width() - gap * (chips.len() as f32 - 1.0)) / chips.len() as f32;
        for (i, index) in chips.iter().enumerate() {
            let [r, g, b] = preset.ansi[*index];
            let x = inner.min.x + i as f32 * (w + gap);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, inner.min.y), Vec2::new(w, inner.height())),
                CornerRadius::same(3),
                egui::Color32::from_rgb(r, g, b),
            );
        }
        let font_id = FontId::proportional(10.5);
        let color = if selected { accent } else { muted };
        let galley = ui
            .painter()
            .layout_no_wrap(name.to_string(), font_id, color);
        ui.painter().galley(
            egui::pos2(
                swatch.center().x - galley.size().x / 2.0,
                swatch.max.y + 8.0,
            ),
            galley,
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen to lay the dialogs out on: there is no window here to ask.
    fn input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        }
    }

    // egui gives a single line field 2 px above and below the text, which is
    // what made the properties form look pinched. The app fields have to be
    // clearly roomier than that, at whatever size the text is.
    #[test]
    fn a_field_gives_its_text_room() {
        let ctx = egui::Context::default();
        let mut plain = String::from("198.51.100.234");
        let mut padded = String::from("198.51.100.234");
        let mut both = (0.0, 0.0);
        let mut output = ctx.run_ui(input(), |ui| {
            let plain = ui
                .add_sized([300.0, 0.0], egui::TextEdit::singleline(&mut plain))
                .rect
                .height();
            let padded = ui
                .add_sized([300.0, theme::field_height(ui)], theme::field(&mut padded))
                .rect
                .height();
            both = (plain, padded);
        });
        // There is no renderer here to apply the font texture to.
        output.textures_delta.clear();
        let (plain, padded) = both;
        assert!(
            padded >= plain + 6.0,
            "an app field is {padded:.1} tall against egui's {plain:.1}"
        );
    }

    #[test]
    fn every_page_of_the_properties_dialog_lays_out() {
        for tab in [EditorTab::Session, EditorTab::Tunnel, EditorTab::Advanced] {
            let ctx = egui::Context::default();
            let mut editor = SessionEditor::new(Session::default(), "properties");
            editor.tab = tab;
            let mut outcome = EditorOutcome::Keep;
            let mut output = ctx.run_ui(input(), |ui| {
                outcome = editor.show(ui);
            });
            output.textures_delta.clear();
            assert_eq!(outcome, EditorOutcome::Keep, "{tab:?} shut itself");
        }
    }
}
