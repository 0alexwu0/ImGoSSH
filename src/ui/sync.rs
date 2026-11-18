use std::{
    path::Path,
    sync::mpsc::{self, Receiver, TryRecvError},
};

use anyhow::{ensure, Result};
use egui::{Context, RichText, Ui};
use zeroize::Zeroizing;

use super::theme;
use crate::{config::Store, dropbox, sync, t, vault};

enum Finished {
    Connected(dropbox::Account),
    Published(String),
    Downloaded(sync::Download),
}

pub(crate) enum Action {
    Keep,
    Close,
    ChooseLocalEncryption,
}

#[derive(Default)]
pub(crate) struct SyncDialog {
    authorization: Option<dropbox::Authorization>,
    code: Zeroizing<String>,
    password: Zeroizing<String>,
    confirm: Zeroizing<String>,
    worker: Option<Receiver<Result<Finished>>>,
    download: Option<sync::Download>,
    notice: Option<String>,
    error: Option<String>,
}

impl SyncDialog {
    pub(crate) fn show(&mut self, ctx: &Context, store: &mut Store) -> Action {
        self.poll(store);
        let frame = egui::Frame::popup(&ctx.global_style())
            .inner_margin(20)
            .corner_radius(12);
        let response = egui::Modal::new(egui::Id::new("configuration-sync"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 72.0).clamp(240.0, 480.0));
                ui.heading(t!("Configuration sync"));
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 100.0).max(80.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.body(ui, store))
                    .inner
            });
        if self.worker.is_none() && response.should_close() {
            Action::Close
        } else {
            response.inner
        }
    }

    fn body(&mut self, ui: &mut Ui, store: &mut Store) -> Action {
        let action = if self.worker.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t!("Working..."));
            });
            Action::Keep
        } else if let Some(download) = &self.download {
            let preview = download.preview(store);
            ui.label(t!(
                "Sessions: {0} new, {1} matched, {2} conflicts skipped.",
                preview.added,
                preview.updated,
                preview.skipped
            ));
            ui.label(t!(
                "Applying replaces portable preferences and merges sessions. No local sessions are deleted."
            ));
            let needs_choice = preview.added > 0 && store.session_encryption_undecided();
            let primary = if needs_choice {
                ui.label(t!(
                    "Choose how to store new sessions on this computer before applying."
                ));
                t!("Choose local encryption")
            } else {
                t!("Apply")
            };
            let (apply, cancel) = ui
                .horizontal_wrapped(|ui| {
                    (
                        theme::primary(ui, primary).clicked(),
                        ui.button(t!("Cancel")).clicked(),
                    )
                })
                .inner;
            if apply && needs_choice {
                return Action::ChooseLocalEncryption;
            }
            if apply {
                match download.apply(store) {
                    Ok(()) => {
                        self.download = None;
                        self.notice = Some(t!("Synced configuration applied.").into());
                        self.error = None;
                    }
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
            } else if cancel {
                self.download = None;
                self.error = None;
            }
            Action::Keep
        } else {
            self.controls(ui, store)
        };
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                t!("Could not sync: {0}", error),
            );
        }
        action
    }

    fn controls(&mut self, ui: &mut Ui, store: &mut Store) -> Action {
        ui.label(t!(
            "Sync an encrypted snapshot through your Dropbox account. No server or shared folder is required."
        ));

        if !dropbox::available() {
            ui.label(t!("Dropbox sync is not configured in this build."));
        } else if self.authorization.is_some() {
            ui.label(t!(
                "Approve access in your browser, then paste the Dropbox authorization code here."
            ));
            password_field(ui, &mut self.code);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!self.code.trim().is_empty(), |ui| {
                    if theme::primary(ui, t!("Finish connecting")).clicked() {
                        if let Some(authorization) = self.authorization.take() {
                            let code = std::mem::take(&mut self.code);
                            self.run(ui.ctx(), move || {
                                authorization.exchange(&code).map(Finished::Connected)
                            });
                        }
                    }
                });
                if ui.button(t!("Cancel")).clicked() {
                    self.authorization = None;
                    self.code = Zeroizing::default();
                }
            });
        } else if store.config.sync_dropbox_account.is_empty() {
            if theme::primary(ui, t!("Connect Dropbox")).clicked() {
                self.begin_authorization(ui.ctx());
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.strong(t!("Dropbox connected"));
                if ui.button(t!("Authorize Dropbox again")).clicked() {
                    self.begin_authorization(ui.ctx());
                }
                if ui.button(t!("Disconnect this computer")).clicked() {
                    self.disconnect(store);
                }
            });
            if self.authorization.is_none() && !store.config.sync_dropbox_account.is_empty() {
                self.snapshot_controls(ui, store);
            }
        }
        ui.label(RichText::new(t!("Connection passwords, private keys, local paths and permissions stay on this computer.")).small().color(theme::muted(ui)));
        if ui.button(t!("Close")).clicked() {
            Action::Close
        } else {
            Action::Keep
        }
    }

    fn begin_authorization(&mut self, ctx: &Context) {
        self.error = None;
        self.notice = None;
        match dropbox::Authorization::begin() {
            Ok((auth, url)) => {
                self.authorization = Some(auth);
                ctx.open_url(egui::OpenUrl::new_tab(url));
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn snapshot_controls(&mut self, ui: &mut Ui, store: &Store) {
        if !store.sessions_writable() || store.needs_encryption_prompt() {
            ui.label(t!(
                "Unlock saved sessions or resolve the storage error before syncing."
            ));
            return;
        }
        ui.separator();
        ui.label(t!("Sync password"));
        password_field(ui, &mut self.password);
        let first_publish = store.config.sync_revision.is_empty();
        if first_publish {
            ui.label(t!("Confirm password for first publish"));
            password_field(ui, &mut self.confirm);
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.password.is_empty(), |ui| {
                if theme::primary(ui, t!("Pull and preview")).clicked() {
                    self.start(ui.ctx(), store, false);
                } else if ui.button(t!("Publish snapshot")).clicked() {
                    self.start(ui.ctx(), store, true);
                }
            });
        });
    }

    fn disconnect(&mut self, store: &mut Store) {
        self.notice = None;
        self.password = Zeroizing::default();
        self.confirm = Zeroizing::default();
        let path = store.sync_credential_path(&store.config.sync_dropbox_account);
        let mut config = store.config.clone();
        config.sync_dropbox_account.clear();
        config.sync_revision.clear();
        self.error = vault::forget(&path)
            .and_then(|()| store.save_synced_settings(config))
            .err()
            .map(|error| format!("{error:#}"));
    }

    fn start(&mut self, ctx: &Context, store: &Store, publish: bool) {
        if publish && store.config.sync_revision.is_empty() && *self.password != *self.confirm {
            self.notice = None;
            self.error = Some(t!("Passwords do not match").into());
            return;
        }
        let path = store.sync_credential_path(&store.config.sync_dropbox_account);
        let revision = store.config.sync_revision.clone();
        let snapshot = publish.then(|| sync::Snapshot::capture(store));
        let password = std::mem::take(&mut self.password);
        self.confirm = Zeroizing::default();
        self.run(ctx, move || {
            let dropbox = dropbox::Dropbox::connect(&path)?;
            match snapshot {
                Some(snapshot) => {
                    sync::push(&dropbox, &revision, &password, snapshot).map(Finished::Published)
                }
                None => sync::pull(&dropbox, &password).map(Finished::Downloaded),
            }
        });
    }

    fn run(&mut self, ctx: &Context, work: impl FnOnce() -> Result<Finished> + Send + 'static) {
        self.notice = None;
        self.error = None;
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(work());
            repaint.request_repaint();
        });
        self.worker = Some(receiver);
    }

    fn poll(&mut self, store: &mut Store) {
        let Some(worker) = &self.worker else { return };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!(t!("Sync worker stopped unexpectedly.")))
            }
        };
        self.worker = None;
        match result {
            Ok(Finished::Connected(account)) => {
                if let Err(error) =
                    save_authorization(store, &account.id, |path| account.remember(path))
                {
                    self.error = Some(format!("{error:#}"));
                }
            }
            Ok(Finished::Published(revision)) => {
                let mut config = store.config.clone();
                config.sync_revision = revision;
                self.notice =
                    Some(t!("Snapshot published. You can now pull it on another computer.").into());
                if let Err(error) = store.save_synced_settings(config) {
                    self.error = Some(format!("{error:#}"));
                }
            }
            Ok(Finished::Downloaded(download)) => self.download = Some(download),
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }
}

fn save_authorization(
    store: &mut Store,
    account_id: &str,
    remember: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let current = &store.config.sync_dropbox_account;
    ensure!(
        current.is_empty() || current == account_id,
        t!("Authorize the same Dropbox account, or disconnect this computer before switching accounts.")
    );
    let path = store.sync_credential_path(account_id);
    remember(&path)?;
    if current == account_id {
        return Ok(());
    }
    let mut config = store.config.clone();
    config.sync_dropbox_account = account_id.to_owned();
    config.sync_revision.clear();
    store.save_synced_settings(config).inspect_err(|_| {
        let _ = vault::forget(&path);
    })
}

fn password_field(ui: &mut Ui, password: &mut String) {
    let mut output = theme::field(password)
        .password(true)
        .min_size(egui::vec2(ui.available_width(), theme::field_height(ui)))
        .show(ui);
    output.state.clear_undoer();
    output.state.store(ui.ctx(), output.response.id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reauthorization_preserves_revision_and_rejects_a_different_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        let credential_path = store.sync_credential_path("test-account");
        save_authorization(&mut store, "test-account", |path| {
            assert_eq!(path, credential_path);
            Ok(())
        })
        .unwrap();
        let mut config = store.config.clone();
        config.sync_revision = "last-snapshot".into();
        store.save_synced_settings(config).unwrap();

        save_authorization(&mut store, "test-account", |path| {
            assert_eq!(path, credential_path);
            Ok(())
        })
        .unwrap();
        assert!(save_authorization(&mut store, "other-account", |_| {
            panic!("credentials for the other account must not be stored")
        })
        .is_err());
        let saved = Store::load_from(dir.path(), false).config;
        assert_eq!(saved.sync_dropbox_account, "test-account");
        assert_eq!(saved.sync_revision, "last-snapshot");
    }

    #[test]
    fn sync_modal_renders_with_and_without_a_dropbox_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load_from(dir.path(), false);
        for account in ["", "test-account"] {
            store.config.sync_dropbox_account = account.into();
            let ctx = Context::default();
            let mut dialog = SyncDialog::default();
            for _ in 0..2 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(760.0, 420.0),
                        )),
                        ..Default::default()
                    },
                    |ui| assert!(matches!(dialog.show(ui.ctx(), &mut store), Action::Keep)),
                );
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
            }
        }
    }
}
