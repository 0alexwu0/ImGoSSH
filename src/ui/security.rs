//! Session vault prompts; passwords stay out of serializable application settings.
use egui::{Context, RichText, Ui};
use zeroize::{Zeroize, Zeroizing};

use super::theme;
use crate::{config::Store, t};

#[derive(PartialEq)]
enum Page {
    Offer,
    Unlock,
    Manage,
}

pub struct SecurityDialog {
    page: Page,
    current: Zeroizing<String>,
    password: Zeroizing<String>,
    confirm: Zeroizing<String>,
    encrypt: bool,
    remember: bool,
    error: String,
}

impl SecurityDialog {
    fn new(page: Page, store: &Store) -> Self {
        Self {
            page,
            current: Zeroizing::new(String::new()),
            password: Zeroizing::new(String::new()),
            confirm: Zeroizing::new(String::new()),
            encrypt: store.sessions_encrypted(),
            remember: store.password_remembered(),
            error: String::new(),
        }
    }

    pub fn initial(store: &Store) -> Option<Self> {
        if store.sessions_locked() {
            Some(Self::new(Page::Unlock, store))
        } else if store.needs_encryption_prompt() {
            Some(Self::new(Page::Offer, store))
        } else {
            None
        }
    }

    pub fn manage(store: &Store) -> Self {
        Self::new(
            if store.sessions_locked() {
                Page::Unlock
            } else {
                Page::Manage
            },
            store,
        )
    }

    /// Returns true when dismissed. The first-save offer keeps plaintext off disk until answered.
    pub fn show(&mut self, ctx: &Context, store: &mut Store) -> bool {
        let mut done = false;
        let frame = egui::Frame::popup(&ctx.global_style())
            .inner_margin(28)
            .corner_radius(16);
        egui::Modal::new(egui::Id::new("session-security")).frame(frame).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 88.0).clamp(260.0, 580.0));
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            ui.spacing_mut().button_padding = egui::vec2(22.0, 10.0);
            ui.spacing_mut().interact_size.y = 38.0;
            ui.label(RichText::new(t!("Session file encryption")).size(25.0).strong());
            ui.label(RichText::new(if self.page == Page::Unlock {
                t!("Unlock your saved sessions")
            } else { t!("Protect your sessions and connection passwords") }).color(theme::tones(ui).3));
            ui.add_space(6.0);
            theme::hairline(ui);
            egui::ScrollArea::vertical()
                .id_salt("security-body")
                .auto_shrink([false, true])
                .max_height((ctx.content_rect().height() - 260.0).max(100.0))
                .show(ui, |ui| {
            ui.set_min_height((ctx.content_rect().height() - 300.0).clamp(100.0, 290.0));
            match self.page {
                Page::Offer => {
                    encryption_switch(ui, &mut self.encrypt);
                    ui.label(t!("Protect sessions.toml with a password? Existing sessions will be encrypted immediately."));
                    security_note(ui);
                }
                Page::Unlock => {
                    ui.label(t!("Enter the password to read sessions.toml."));
                    password_field(ui, t!("Password"), &mut self.current);
                    remember_option(ui, &mut self.remember);
                    ui.label(t!("Without unlocking, new sessions are temporary and will not be saved."));
                }
                Page::Manage => {
                    encryption_switch(ui, &mut self.encrypt);
                    if store.sessions_encrypted() {
                        password_field(ui, t!("Current password"), &mut self.current);
                    }
                    if self.encrypt {
                        password_field(ui, if store.sessions_encrypted() { t!("New password") } else { t!("Set password") }, &mut self.password);
                        password_field(ui, t!("Confirm password"), &mut self.confirm);
                        if store.sessions_encrypted() {
                            ui.label(RichText::new(t!("Leave the new password empty to keep the current password.")).small().color(theme::tones(ui).3));
                        }
                        remember_option(ui, &mut self.remember);
                    } else {
                        ui.label(t!("Encryption is off. Sessions will be saved without a file password after applying."));
                    }
                    security_note(ui);
                }
            }
            if !self.error.is_empty() {
                let error = match self.error.as_str() {
                    "Incorrect password or damaged session file" => t!("Incorrect password or damaged session file"),
                    "Password cannot be empty" => t!("Password cannot be empty"),
                    _ => &self.error,
                };
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            });
            ui.add_space(6.0);
            theme::hairline(ui);
            ui.horizontal_wrapped(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let primary = match self.page {
                        Page::Offer if self.encrypt => t!("Set password"),
                        Page::Offer => t!("Keep unencrypted"),
                        Page::Unlock => t!("Unlock"),
                        Page::Manage => t!("Apply"),
                    };
                    if theme::primary(ui, primary).clicked() {
                        match self.page {
                            Page::Offer if self.encrypt => { self.page = Page::Manage; self.error.clear(); }
                            Page::Offer => match store.decline_encryption() {
                                Ok(()) => done = true,
                                Err(err) => self.error = err.to_string(),
                            },
                            Page::Unlock => done = self.unlock(store),
                            Page::Manage => match self.apply(store) {
                                Ok(()) => done = true,
                                Err(err) => self.error = err.to_string(),
                            },
                        }
                    }
                    if self.page != Page::Offer && ui.button(if self.page == Page::Unlock {
                        t!("Continue without unlocking")
                    } else { t!("Cancel") }).clicked() {
                        if self.page == Page::Manage && store.needs_encryption_prompt() {
                            self.page = Page::Offer;
                            self.error.clear();
                        } else { done = true; }
                    }
                });
            });
        });
        done
    }

    fn unlock(&mut self, store: &mut Store) -> bool {
        match store.unlock(&self.current) {
            Ok(()) => {
                let result = if self.remember {
                    store.remember_password(&self.current)
                } else {
                    store.forget_password()
                };
                match result {
                    Ok(()) => return true,
                    Err(err) => {
                        self.page = Page::Manage;
                        self.encrypt = true;
                        self.error = format!(
                            "{}: {err}",
                            t!("Sessions unlocked; password preference could not be saved")
                        );
                    }
                }
            }
            Err(_) => self.error = t!("Incorrect password or damaged session file").into(),
        }
        false
    }

    fn apply(&mut self, store: &mut Store) -> anyhow::Result<()> {
        let change = self.encrypt != store.sessions_encrypted()
            || (self.encrypt && !self.password.is_empty());
        if change {
            if self.encrypt {
                anyhow::ensure!(!self.password.is_empty(), t!("Password cannot be empty"));
                anyhow::ensure!(
                    *self.password == *self.confirm,
                    t!("Passwords do not match")
                );
            }
            store.set_encryption(
                &self.current,
                self.encrypt.then_some(self.password.as_str()),
            )?;
            if self.encrypt {
                self.current = Zeroizing::new(self.password.to_string());
            }
            self.password.zeroize();
            self.confirm.zeroize();
        }
        if self.encrypt {
            if self.remember && !store.password_remembered() {
                store.remember_password(&self.current)?;
            } else if !self.remember && store.password_remembered() {
                store.forget_password()?;
            }
        } else if store.needs_encryption_prompt() {
            store.decline_encryption()?;
        }
        Ok(())
    }
}

fn password_field(ui: &mut Ui, label: &str, value: &mut String) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 7.0;
        ui.label(RichText::new(label).strong());
        ui.add_sized(
            [ui.available_width(), 42.0],
            theme::field(value).password(true),
        );
    });
}

fn remember_option(ui: &mut Ui, remember: &mut bool) {
    ui.add_space(2.0);
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.checkbox(remember, t!("Remember password in system credential store"));
    });
}

fn security_note(ui: &mut Ui) {
    ui.add_space(4.0);
    ui.label(RichText::new(t!("A forgotten password cannot be recovered. Auto-unlock is protected by your OS account."))
        .small().color(theme::tones(ui).3));
}

fn encryption_switch(ui: &mut Ui, enabled: &mut bool) {
    theme::card_with(ui, 16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(t!("Enable session encryption"))
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(if *enabled {
                        t!("Enabled")
                    } else {
                        t!("Disabled")
                    })
                    .small()
                    .color(if *enabled {
                        theme::accent(ui)
                    } else {
                        theme::tones(ui).3
                    }),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (rect, mut response) =
                    ui.allocate_exact_size(egui::vec2(50.0, 28.0), egui::Sense::click());
                if response.clicked() {
                    *enabled = !*enabled;
                    response.mark_changed();
                }
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        ui.is_enabled(),
                        *enabled,
                        t!("Enable session encryption"),
                    )
                });
                let progress = ui.ctx().animate_bool(response.id, *enabled);
                let track = if *enabled {
                    theme::accent(ui)
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                ui.painter().rect(
                    rect,
                    14,
                    track,
                    ui.style().interact(&response).bg_stroke,
                    egui::StrokeKind::Inside,
                );
                let center = egui::pos2(
                    egui::lerp((rect.left() + 14.0)..=(rect.right() - 14.0), progress),
                    rect.center().y,
                );
                ui.painter().circle_filled(
                    center,
                    10.0,
                    if *enabled {
                        theme::on_accent(ui)
                    } else {
                        ui.visuals().text_color()
                    },
                );
                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            });
        });
    });
    if !*enabled {
        ui.label(
            RichText::new(t!("Without password encryption, saved server passwords are stored in plain text in sessions.toml and may be exposed to anyone who can read the file."))
                .color(theme::tones(ui).1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_pages_render_and_validate_password_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        let mut dialog = SecurityDialog::manage(&store);
        dialog.encrypt = true;
        assert!(dialog.apply(&mut store).is_err());
        *dialog.password = "sample-password".into();
        *dialog.confirm = "different-password".into();
        assert!(dialog.apply(&mut store).is_err());
        assert!(!dir.path().join("sessions.toml").exists());
        for page in [Page::Offer, Page::Unlock, Page::Manage] {
            let ctx = Context::default();
            let mut dialog = SecurityDialog::new(page, &store);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760.0, 600.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                assert!(!dialog.show(ui.ctx(), &mut store));
            });
            output.textures_delta.clear();
        }
        *dialog.confirm = "sample-password".into();
        dialog.apply(&mut store).unwrap();
        assert!(store.sessions_encrypted());
        assert!(dialog.password.is_empty());
        assert!(dialog.confirm.is_empty());
    }
}
