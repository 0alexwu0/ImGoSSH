//! The session browser on the left: a searchable folder tree of saved sessions.

use std::collections::{BTreeMap, HashSet};

use egui::{Align2, CornerRadius, FontId, Pos2, Response, RichText, Sense, Ui, Vec2};

use crate::config::{Protocol, Session};
use crate::i18n::tr;
use crate::t;
use crate::ui::theme;
use crate::util::fuzzy_match;

/// What the user did to the tree; the app turns these into state changes.
#[derive(Debug, Default)]
pub struct Actions {
    pub connect: Vec<usize>,
    pub edit: Option<usize>,
    pub delete: Option<usize>,
    pub duplicate: Option<usize>,
    pub new_session: Option<(Protocol, String)>,
    pub new_folder: Option<String>,
    pub rename_folder: Option<String>,
    pub delete_folder: Option<String>,
    pub open_settings: bool,
}

/// A row the user right-clicked, so the app can show a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Session(usize),
    Folder(String),
}

#[derive(Default)]
pub struct Sidebar {
    expanded: HashSet<String>,
    filter: String,
    menu: Option<Target>,
}

/// In-memory tree built from the flat session list.
#[derive(Default)]
struct Branch {
    name: String,
    path: String,
    folders: BTreeMap<String, Branch>,
    sessions: Vec<usize>,
}

impl Branch {
    fn insert(&mut self, parts: &[&str], index: usize) {
        match parts.split_first() {
            None => self.sessions.push(index),
            Some((head, rest)) => {
                let child = self
                    .folders
                    .entry((*head).to_string())
                    .or_insert_with(|| Branch {
                        name: (*head).to_string(),
                        path: if self.path.is_empty() {
                            (*head).to_string()
                        } else {
                            format!("{}/{}", self.path, head)
                        },
                        ..Default::default()
                    });
                child.insert(rest, index);
            }
        }
    }

    fn matches(&self, filter: &str, sessions: &[Session]) -> bool {
        filter.is_empty()
            || fuzzy_match(&self.name, filter)
            || self
                .sessions
                .iter()
                .any(|i| session_matches(&sessions[*i], filter))
            || self.folders.values().any(|f| f.matches(filter, sessions))
    }
}

fn session_matches(session: &Session, filter: &str) -> bool {
    fuzzy_match(&session.name, filter)
        || fuzzy_match(&session.host, filter)
        || fuzzy_match(&session.username, filter)
}

impl Sidebar {
    /// Draw the panel and report what happened.
    pub fn ui(&mut self, ui: &mut Ui, sessions: &[Session], live: &[String]) -> Actions {
        let mut actions = Actions::default();
        let width = ui.available_width();
        let muted = theme::muted(ui);

        // Reserve the footer first so a long session list cannot push it away.
        egui::Panel::bottom("sidebar-footer")
            .frame(theme::bar(ui))
            .show(ui, |ui| {
                let cell = ((ui.available_width() - 6.0) / 2.0).max(72.0);
                let items: [(&str, Option<Protocol>); 4] = [
                    (t!("+ SSH"), Some(Protocol::Ssh)),
                    (t!("+ SFTP"), Some(Protocol::Sftp)),
                    (t!("+ SCP"), Some(Protocol::Scp)),
                    (t!("+ folder"), None),
                ];
                egui::Grid::new("sidebar-footer-grid")
                    .num_columns(2)
                    .spacing([6.0, 4.0])
                    .show(ui, |ui| {
                        for (index, (label, protocol)) in items.into_iter().enumerate() {
                            let tip: String = match protocol {
                                Some(kind) => t!("New {0} session", tr(kind.label())),
                                None => t!("New subfolder").into(),
                            };
                            let clicked = ui
                                .allocate_ui_with_layout(
                                    Vec2::new(cell, 22.0),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| theme::ghost(ui, label),
                                )
                                .inner
                                .on_hover_text(tip)
                                .clicked();
                            if clicked {
                                match protocol {
                                    Some(kind) => actions.new_session = Some((kind, String::new())),
                                    None => actions.new_folder = Some(String::new()),
                                }
                            }
                            if index % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
            });

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t!("Sessions"))
                    .size(12.5)
                    .strong()
                    .color(ui.visuals().text_color()),
            );
            let count = sessions.len().to_string();
            theme::badge(ui, &count, muted, ui.visuals().widgets.hovered.weak_bg_fill);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost(ui, t!("\u{2699}"))
                    .on_hover_text(t!("Settings"))
                    .clicked()
                {
                    actions.open_settings = true;
                }
            });
        });
        ui.add_sized(
            [width, 24.0],
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text(t!("\u{2315} Search sessions"))
                .vertical_align(egui::Align::Center),
        );
        ui.add_space(2.0);
        theme::hairline(ui);
        ui.add_space(4.0);

        let mut root = Branch::default();
        for (index, session) in sessions.iter().enumerate() {
            root.insert(&session.folder_parts(), index);
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.tree(ui, &root, sessions, live, &mut actions);
            });

        actions
    }
    fn tree(
        &mut self,
        ui: &mut Ui,
        branch: &Branch,
        sessions: &[Session],
        live: &[String],
        actions: &mut Actions,
    ) {
        if !branch.matches(&self.filter, sessions) {
            return;
        }
        for folder in branch.folders.values() {
            let open = self.expanded.contains(&folder.path) || !self.filter.is_empty();
            let glyph = if open { "\u{25be}" } else { "\u{25b8}" };
            let response = row(ui, &format!("{glyph} \u{1f4c1}"), &folder.name, "", false)
                .on_hover_text(&folder.path);
            if response.clicked() {
                if open {
                    self.expanded.remove(&folder.path);
                } else {
                    self.expanded.insert(folder.path.clone());
                }
            }
            if response.secondary_clicked() {
                self.menu = Some(Target::Folder(folder.path.clone()));
            }
            response.context_menu(|ui| folder_menu(ui, &folder.path, actions));

            if open {
                ui.indent(egui::Id::new(&folder.path), |ui| {
                    self.tree(ui, folder, sessions, live, actions);
                });
            }
        }

        for index in branch.sessions.iter().copied() {
            let session = &sessions[index];
            if !self.filter.is_empty() && !session_matches(session, &self.filter) {
                continue;
            }
            let title = session.title();
            // A session named after its own host does not need it twice.
            let note = if session.host.is_empty() {
                tr(session.protocol.label()).to_string()
            } else if title.contains(&session.host) {
                session.port.to_string()
            } else {
                format!("{}:{}", session.host, session.port)
            };
            let on = live.iter().any(|target| *target == session.target());
            let tip = format!(
                "{}\n{}:{}\n{}",
                session.target(),
                session.host,
                session.port,
                t!("Double-click to connect")
            );
            let response = row(ui, session.protocol.icon(), &title, &note, on).on_hover_text(tip);
            if response.double_clicked() {
                actions.connect.push(index);
            }
            if response.secondary_clicked() {
                self.menu = Some(Target::Session(index));
            }
            response.context_menu(|ui| session_menu(ui, index, actions));
        }
    }

    /// The row currently showing a context menu, if any.
    pub fn menu_target(&self) -> Option<&Target> {
        self.menu.as_ref()
    }
}

/// One row of the session tree.
///
/// Painted by hand so the highlight spans the whole panel: a hit target only as
/// wide as the text is maddening to aim at, and the row reads as a row.
fn row(ui: &mut Ui, glyph: &str, title: &str, note: &str, selected: bool) -> Response {
    let v = ui.visuals();
    let (hover, accent_dim, accent, muted, text) = (
        v.widgets.hovered.weak_bg_fill,
        theme::accent_dim(ui),
        theme::accent(ui),
        theme::muted(ui),
        v.text_color(),
    );
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 25.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), accent_dim);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(7), hover);
    }
    let y = rect.center().y;
    let font = FontId::proportional(12.5);
    let mark = ui.painter().text(
        Pos2::new(rect.left() + 5.0, y),
        Align2::LEFT_CENTER,
        glyph,
        font.clone(),
        if selected { accent } else { muted },
    );
    let name = ui.painter().text(
        Pos2::new(mark.right() + 6.0, y),
        Align2::LEFT_CENTER,
        title,
        font.clone(),
        text,
    );
    if !note.is_empty() {
        let room = rect.right() - name.right() - 22.0;
        if room > 36.0 {
            let shown = crate::util::truncate_chars(note, (room / 6.5) as usize);
            ui.painter().text(
                Pos2::new(rect.right() - 8.0, y),
                Align2::RIGHT_CENTER,
                shown,
                font,
                muted,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn session_menu(ui: &mut Ui, index: usize, actions: &mut Actions) {
    if ui.button(t!("Connect")).clicked() {
        actions.connect.push(index);
        ui.close();
    }
    if ui.button(t!("Edit session")).clicked() {
        actions.edit = Some(index);
        ui.close();
    }
    if ui.button(t!("Duplicate")).clicked() {
        actions.duplicate = Some(index);
        ui.close();
    }
    ui.separator();
    if ui.button(t!("Delete")).clicked() {
        actions.delete = Some(index);
        ui.close();
    }
}

fn folder_menu(ui: &mut Ui, path: &str, actions: &mut Actions) {
    if ui.button(t!("New SSH session here")).clicked() {
        actions.new_session = Some((Protocol::Ssh, path.to_string()));
        ui.close();
    }
    if ui.button(t!("New subfolder")).clicked() {
        actions.new_folder = Some(path.to_string());
        ui.close();
    }
    ui.separator();
    if ui.button(t!("Rename folder")).clicked() {
        actions.rename_folder = Some(path.to_string());
        ui.close();
    }
    if ui.button(t!("Delete folder")).clicked() {
        actions.delete_folder = Some(path.to_string());
        ui.close();
    }
}

/// Convenience used by the app when a session is dropped onto a folder.
#[allow(dead_code)]
fn is_folder_response(response: &Response) -> bool {
    response.hovered()
}
