use std::collections::{HashMap, HashSet};

use anyhow::{ensure, Context, Result};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    config::{Config, Protocol, Session, Store, Theme},
    dropbox::Dropbox,
    t, vault,
};

const MAGIC: &[u8] = b"ImGoSSH-CONFIG-SYNC\0";
const VERSION: u32 = 1;
const MAX_BYTES: usize = 4 * 1024 * 1024;

macro_rules! portable_settings {
    ($($field:ident: $ty:ty),+ $(,)?) => {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Settings { $( $field: $ty ),+ }

        impl Settings {
            fn capture(config: &Config) -> Self {
                Self { $( $field: config.$field.clone() ),+ }
            }

            fn apply(&self, config: &mut Config) {
                $( config.$field = self.$field.clone(); )+
            }
        }
    };
}

portable_settings! {
    language: String,
    theme: Theme,
    palette: String,
    font_size: f32,
    line_spacing: f32,
    scrollback: usize,
    cursor_blink: bool,
    blink_ms: u64,
    audible_bell: bool,
    copy_on_select: bool,
    alt_gr_click_menu: bool,
    default_protocol: Protocol,
    default_port: u16,
    default_cols: u16,
    default_rows: u16,
    default_term: String,
    default_keepalive: u64,
    open_sftp_panel: bool,
    show_sidebar: bool,
    show_toolbar: bool,
    show_statusbar: bool,
    show_server_monitor: bool,
    confirm_close: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableSession {
    name: String,
    folder: String,
    protocol: Protocol,
    host: String,
    port: u16,
    username: String,
    term: String,
    cols: u16,
    rows: u16,
    sftp_panel: bool,
    keepalive: u64,
    sftp_path: String,
}

impl PortableSession {
    fn capture(session: &Session) -> Self {
        Self {
            name: session.name.clone(),
            folder: session.folder.clone(),
            protocol: session.protocol,
            host: session.host.clone(),
            port: session.port,
            username: session.username.clone(),
            term: session.term.clone(),
            cols: session.cols,
            rows: session.rows,
            sftp_panel: session.sftp_panel,
            keepalive: session.keepalive,
            sftp_path: if session.protocol == Protocol::Sftp {
                session.remote_path.clone()
            } else {
                String::new()
            },
        }
    }

    fn matches(&self, session: &Session) -> bool {
        self.protocol == session.protocol
            && self.host == session.host
            && self.port == session.port
            && self.username == session.username
    }

    fn apply(&self, session: &mut Session) {
        session.term.clone_from(&self.term);
        session.cols = self.cols;
        session.rows = self.rows;
        session.sftp_panel = self.sftp_panel;
        session.keepalive = self.keepalive;
        if self.protocol == Protocol::Sftp {
            session.remote_path.clone_from(&self.sftp_path);
        }
    }

    fn to_session(&self) -> Session {
        let mut session = Session {
            name: self.name.clone(),
            folder: self.folder.clone(),
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            ..Session::default()
        };
        self.apply(&mut session);
        session
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    version: u32,
    settings: Settings,
    sessions: Vec<PortableSession>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Preview {
    pub(crate) added: usize,
    pub(crate) updated: usize,
    pub(crate) skipped: usize,
}

#[derive(Default)]
struct Merge {
    preview: Preview,
    insert: Vec<usize>,
    update: Vec<(usize, usize)>,
}

impl Snapshot {
    pub(crate) fn capture(store: &Store) -> Self {
        Self {
            version: VERSION,
            settings: Settings::capture(&store.config),
            sessions: store
                .sessions
                .iter()
                .filter(|session| !session.jumpserver && session.protocol.is_remote())
                .map(PortableSession::capture)
                .collect(),
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            t!("Unsupported sync snapshot version.")
        );
        let settings = &self.settings;
        ensure!(
            (8.0..=32.0).contains(&settings.font_size)
                && (0.8..=2.5).contains(&settings.line_spacing)
                && settings.scrollback <= 200_000
                && settings.default_port > 0
                && settings.blink_ms > 0
                && (2..=500).contains(&settings.default_cols)
                && (2..=200).contains(&settings.default_rows)
                && self.sessions.iter().all(|session| {
                    session.protocol.is_remote()
                        && session.port > 0
                        && (2..=500).contains(&session.cols)
                        && (2..=200).contains(&session.rows)
                }),
            t!("Invalid sync snapshot.")
        );
        Ok(())
    }

    fn merge_plan(&self, local: &[Session]) -> Merge {
        let mut existing = HashMap::new();
        for (index, session) in local
            .iter()
            .enumerate()
            .filter(|(_, session)| !session.jumpserver)
        {
            existing
                .entry((session.folder.as_str(), session.name.as_str()))
                .and_modify(|previous| *previous = None)
                .or_insert(Some(index));
        }
        let mut seen = HashSet::new();
        let mut duplicates = HashSet::new();
        for session in &self.sessions {
            let key = (session.folder.as_str(), session.name.as_str());
            if !seen.insert(key) {
                duplicates.insert(key);
            }
        }
        let mut merge = Merge::default();
        for (index, session) in self.sessions.iter().enumerate() {
            let key = (session.folder.as_str(), session.name.as_str());
            if duplicates.contains(&key) {
                merge.preview.skipped += 1;
                continue;
            }
            match existing.get(&key) {
                None => merge.insert.push(index),
                Some(Some(local_index)) if session.matches(&local[*local_index]) => {
                    merge.update.push((*local_index, index));
                }
                Some(_) => merge.preview.skipped += 1,
            }
        }
        merge.preview.added = merge.insert.len();
        merge.preview.updated = merge.update.len();
        merge
    }
}

pub(crate) struct Download {
    snapshot: Snapshot,
    revision: String,
}

impl Download {
    pub(crate) fn preview(&self, store: &Store) -> Preview {
        self.snapshot.merge_plan(&store.sessions).preview
    }

    pub(crate) fn apply(&self, store: &mut Store) -> Result<()> {
        let merge = self.snapshot.merge_plan(&store.sessions);
        let mut config = store.config.clone();
        self.snapshot.settings.apply(&mut config);
        config.sync_revision.clone_from(&self.revision);
        let sessions = (!merge.insert.is_empty() || !merge.update.is_empty()).then(|| {
            let mut sessions = store.sessions.clone();
            for (local_index, remote_index) in merge.update {
                self.snapshot.sessions[remote_index].apply(&mut sessions[local_index]);
            }
            for remote_index in merge.insert {
                sessions.push(self.snapshot.sessions[remote_index].to_session());
            }
            sessions
        });
        store.apply_synced(config, sessions)
    }
}

pub(crate) fn pull(dropbox: &Dropbox, password: &str) -> Result<Download> {
    let remote = dropbox.download(MAX_BYTES)?.context(t!(
        "No sync snapshot in Dropbox. Publish one from another computer first."
    ))?;
    decode(&remote.bytes, password)
}

fn decode(bytes: &[u8], password: &str) -> Result<Download> {
    let revision = revision(bytes);
    let plain = open(bytes, password)?;
    let snapshot: Snapshot = serde_json::from_slice(&plain)
        .map_err(|_| anyhow::anyhow!(t!("Invalid sync snapshot.")))?;
    snapshot.validate()?;
    Ok(Download { snapshot, revision })
}

pub(crate) fn push(
    dropbox: &Dropbox,
    expected: &str,
    password: &str,
    snapshot: Snapshot,
) -> Result<String> {
    snapshot.validate()?;
    let previous = dropbox.download(MAX_BYTES)?;
    let precondition = match &previous {
        Some(remote) if revision(&remote.bytes) == expected => {
            open(&remote.bytes, password)?;
            Some(remote.rev.as_str())
        }
        None if expected.is_empty() => {
            ensure!(
                password.chars().count() >= 12,
                t!("Use at least 12 characters for the sync password.")
            );
            None
        }
        _ => anyhow::bail!(t!(
            "The cloud snapshot changed. Pull and apply it before publishing."
        )),
    };

    let sealed = encode(&snapshot, password)?;
    dropbox.upload(precondition, &sealed)?;
    Ok(revision(&sealed))
}

fn encode(snapshot: &Snapshot, password: &str) -> Result<Vec<u8>> {
    let plain = Zeroizing::new(serde_json::to_vec(&snapshot)?);
    let sealed = vault::Key::new(password)?.seal_for(MAGIC, &plain)?;
    ensure!(
        sealed.len() <= MAX_BYTES,
        t!("The sync snapshot is too large.")
    );
    Ok(sealed)
}

fn open(bytes: &[u8], password: &str) -> Result<Zeroizing<Vec<u8>>> {
    vault::Key::open_for(MAGIC, bytes, password)
        .map(|(_, plain)| plain)
        .map_err(|_| anyhow::anyhow!(t!("Incorrect sync password or unreadable snapshot.")))
}

fn revision(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Tunnel, WorkspaceConfig};

    const PASSWORD: &str = "a distinct sync password";

    fn session(name: &str, host: &str) -> Session {
        Session {
            name: name.into(),
            folder: "Servers/Production".into(),
            host: host.into(),
            username: "operator".into(),
            ..Session::new(Protocol::Ssh)
        }
    }

    #[test]
    fn sync_round_trip_keeps_private_data_and_conflicts_local() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let mut source = Store::load_from(source_dir.path(), false);
        let mut target = Store::load_from(target_dir.path(), false);
        source.config.theme = Theme::Light;
        source.config.font_size = 20.0;
        source.config.sync_dropbox_account = "source-account".into();
        source.config.font_family = "source-font".into();
        source.config.default_username = "source-default-user".into();

        let mut updated = session("Existing", "existing.example");
        updated.term = "linux".into();
        let mut added = session("New", "new.example");
        added.protocol = Protocol::Sftp;
        added.remote_path = "/remote/data".into();
        added.password = "source-password".into();
        added.private_key = "/source/key".into();
        added.local_path = "/source/downloads".into();
        added.startup_command = "source-command".into();
        added.startup_script = "source-script".into();
        added.use_agent = true;
        added.auto_tunnels = true;
        added.tunnels.push(Tunnel::default());
        added.workspace = WorkspaceConfig {
            enabled: true,
            allow_exec: true,
            root: "/source/workspace".into(),
            ..Default::default()
        };
        let mut gateway = session("Gateway", "gateway.example");
        gateway.jumpserver = true;
        source.sessions = vec![
            updated,
            added,
            session("Conflict", "other.example"),
            session("Duplicate", "first.example"),
            session("Duplicate", "second.example"),
            Session::new(Protocol::Local),
            gateway,
        ];

        target.config.font_family = "target-font".into();
        target.config.default_username = "target-default-user".into();
        target.config.window_width = 960.0;
        target.config.last_update_check = 123;
        target.config.sync_dropbox_account = "target-account".into();
        let mut existing = session("Existing", "existing.example");
        existing.password = "target-password".into();
        existing.local_path = "/target/downloads".into();
        existing.private_key = "/target/key".into();
        let conflict = session("Conflict", "original.example");
        let local_only = session("Local only", "local.example");
        target.sessions = vec![existing.clone(), conflict.clone(), local_only.clone()];
        target
            .set_encryption("", Some("target vault password"))
            .unwrap();
        let trusted_hosts = target_dir.path().join("known_hosts");
        std::fs::write(&trusted_hosts, "trusted host keys").unwrap();

        let encrypted = encode(&Snapshot::capture(&source), PASSWORD).unwrap();
        assert!(!vault::is_encrypted(&encrypted));
        assert!(!encrypted.windows(11).any(|window| window == b"new.example"));
        let plaintext = open(&encrypted, PASSWORD).unwrap();
        let text = std::str::from_utf8(&plaintext).unwrap();
        for excluded in [
            "source-",
            "target-",
            "startup_command",
            "private_key",
            "allow_exec",
            "gateway.example",
        ] {
            assert!(!text.contains(excluded), "snapshot contains {excluded}");
        }
        let downloaded = decode(&encrypted, PASSWORD).unwrap();
        let preview = downloaded.preview(&target);
        assert_eq!((preview.added, preview.updated, preview.skipped), (1, 1, 3));
        assert_eq!(target.sessions.len(), 3);
        downloaded.apply(&mut target).unwrap();
        assert_eq!(target.config.sync_revision, revision(&encrypted));
        assert_eq!(target.config.theme, Theme::Light);
        assert_eq!(target.config.font_size, 20.0);
        assert_eq!(target.config.font_family, "target-font");
        assert_eq!(target.config.default_username, "target-default-user");
        assert_eq!(target.config.window_width, 960.0);
        assert_eq!(target.config.last_update_check, 123);
        assert_eq!(target.config.sync_dropbox_account, "target-account");
        existing.term = "linux".into();
        assert_eq!(target.sessions[0], existing);
        assert_eq!(target.sessions[1], conflict);
        assert_eq!(target.sessions[2], local_only);
        assert_eq!(
            target.sessions[3],
            Session {
                name: "New".into(),
                protocol: Protocol::Sftp,
                remote_path: "/remote/data".into(),
                ..session("New", "new.example")
            }
        );
        assert_eq!(
            std::fs::read_to_string(trusted_hosts).unwrap(),
            "trusted host keys"
        );

        let mut reopened = Store::load_from(target_dir.path(), false);
        assert!(reopened.sessions_locked());
        assert!(downloaded.apply(&mut reopened).is_err());
        reopened.unlock("target vault password").unwrap();
        assert_eq!(reopened.config, target.config);
        assert_eq!(reopened.sessions, target.sessions);
    }

    #[test]
    fn rejects_wrong_passwords_and_tampering() {
        let local = tempfile::tempdir().unwrap();
        let store = Store::load_from(local.path(), false);
        let mut damaged = encode(&Snapshot::capture(&store), PASSWORD).unwrap();
        assert!(decode(&damaged, "wrong password").is_err());
        *damaged.last_mut().unwrap() ^= 1;
        assert!(decode(&damaged, PASSWORD).is_err());
    }

    #[test]
    fn failed_apply_restores_preferences_and_session_list() {
        let target_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let mut target = Store::load_from(target_dir.path(), false);
        target.config.theme = Theme::Dark;
        target.decline_encryption().unwrap();
        std::fs::create_dir(target_dir.path().join("sessions.toml")).unwrap();
        let mut source = Store::load_from(source_dir.path(), false);
        source.config.theme = Theme::Light;
        source.sessions.push(session("New", "new.example"));
        let downloaded = Download {
            snapshot: Snapshot::capture(&source),
            revision: "new-revision".into(),
        };
        assert!(downloaded.apply(&mut target).is_err());
        assert_eq!(target.config.theme, Theme::Dark);
        assert!(target.config.sync_revision.is_empty());
        assert!(target.sessions.is_empty());
        let reopened = Store::load_from(target_dir.path(), false);
        assert_eq!(reopened.config, target.config);
    }

    #[test]
    fn new_device_does_not_write_sessions_before_local_encryption_choice() {
        let local = tempfile::tempdir().unwrap();
        let mut target = Store::load_from(local.path(), false);
        let mut source = Store::load_from(local.path(), false);
        source.sessions.push(session("New", "new.example"));
        let downloaded = Download {
            snapshot: Snapshot::capture(&source),
            revision: "new-revision".into(),
        };
        assert!(downloaded.apply(&mut target).is_err());
        assert!(target.sessions.is_empty());
        assert!(!local.path().join("sessions.toml").exists());
        target.decline_encryption().unwrap();
        downloaded.apply(&mut target).unwrap();
        assert_eq!(
            Store::load_from(local.path(), false).sessions,
            source.sessions
        );
    }
}
