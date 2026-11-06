//! Desktop launch arguments, including JumpServer Client's argument templates.

use std::path::PathBuf;

use clap::{ArgGroup, Parser};

use crate::config::{Config, Protocol, Session, APP_VERSION};

/// What to open automatically when the window appears.
/// Deliberately has no Debug implementation: connection credentials are private.
#[derive(Default, Clone)]
pub struct Startup {
    pub local: bool,
    pub session: Option<String>,
    pub host: Option<String>,
    pub connection: Option<Session>,
}

#[derive(Parser)]
#[command(name = "imgossh", version = APP_VERSION,
    about = "SSH / SFTP / SCP workstation written in Rust")]
#[command(group(ArgGroup::new("destination").args(["host", "hostname"])))]
pub struct Args {
    /// Open a local pseudo-console tab on startup
    #[arg(long, conflicts_with_all = ["session", "destination"])]
    local: bool,
    /// Open a saved session by name on startup
    #[arg(short, long, conflicts_with = "destination")]
    session: Option<String>,
    /// Connect to [user@]host on startup
    host: Option<String>,
    /// Explicit host (for external client integrations)
    #[arg(long = "host")]
    hostname: Option<String>,
    /// Login user; overrides the user in [user@]host
    #[arg(short = 'l', long, requires = "destination")]
    username: Option<String>,
    /// SSH port
    #[arg(short, long, requires = "destination", value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    /// OpenSSH private key file
    #[arg(short = 'i', long, requires = "destination")]
    identity: Option<PathBuf>,
    /// Connection password or temporary token (visible in the process arguments)
    #[arg(long, requires = "destination", allow_hyphen_values = true)]
    password: Option<String>,
    /// Do not try ssh-agent / Pageant
    #[arg(long, requires = "destination")]
    no_agent: bool,
    /// Tab title
    #[arg(long, requires = "destination")]
    name: Option<String>,
    /// Open the SFTP file browser instead of an interactive SSH terminal
    #[arg(long, requires = "destination")]
    sftp: bool,
    /// Temporary JumpServer gateway connection; disables agent and monitoring
    #[arg(long, requires = "destination", conflicts_with = "identity")]
    jumpserver: bool,
}

impl Args {
    pub fn into_startup(self, config: &Config) -> Startup {
        let explicit_host = self.hostname.is_some();
        let destination = self.hostname.or(self.host);
        let connection = destination.as_ref().map(|destination| {
            let mut session = config.new_session(if self.sftp {
                Protocol::Sftp
            } else {
                Protocol::Ssh
            });
            // An explicit username keeps opaque gateway usernames intact, including '@'.
            if let Some(username) = self.username {
                session.username = username;
                session.host = if explicit_host {
                    destination.clone()
                } else {
                    destination
                        .rsplit_once('@')
                        .map_or(destination.as_str(), |(_, host)| host)
                        .into()
                };
            } else if let Some((username, host)) = destination.rsplit_once('@') {
                session.username = username.into();
                session.host = host.into();
            } else {
                session.host = destination.clone();
            }
            // Bracketed IPv6 is accepted without passing brackets to the resolver.
            if session.host.starts_with('[') && session.host.ends_with(']') {
                session.host = session.host[1..session.host.len() - 1].into();
            }
            if let Some(port) = self.port {
                session.port = port;
            }
            session.private_key = self
                .identity
                .map(|p| p.to_string_lossy().into())
                .unwrap_or_default();
            session.password = self.password.unwrap_or_default();
            session.use_agent = !self.no_agent && !self.jumpserver;
            session.jumpserver = self.jumpserver;
            if self.jumpserver {
                session.sftp_panel = self.sftp;
                session.auto_tunnels = false;
                session.tunnels.clear();
            }
            session.name = self.name.unwrap_or_else(|| {
                if self.jumpserver {
                    format!(
                        "JumpServer {} · {}:{}",
                        session.protocol.label(),
                        session.host,
                        session.port
                    )
                } else {
                    session.target()
                }
            });
            session
        });
        Startup {
            local: self.local,
            session: self.session,
            host: None,
            connection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumpserver_template_passes_gateway_and_token_without_saved_session() {
        let args = Args::try_parse_from([
            "imgossh",
            "--jumpserver",
            "--host=2001:db8::1",
            "--port=2222",
            "--username=JMS-token@domain",
            "--password=-token+/@=\"&",
        ])
        .unwrap();
        let startup = args.into_startup(&Config::default());
        assert!(startup.session.is_none());
        assert!(startup.host.is_none());
        let session = startup.connection.unwrap();
        assert_eq!(session.host, "2001:db8::1");
        assert_eq!(session.port, 2222);
        assert_eq!(session.username, "JMS-token@domain");
        assert_eq!(session.password, "-token+/@=\"&");
        assert!(session.jumpserver);
        assert!(!session.sftp_panel);
        assert!(!session.use_agent);
        assert!(!session.auto_tunnels);
        assert!(!toml::to_string(&session).unwrap().contains("jumpserver"));
    }

    #[test]
    fn jumpserver_sftp_opens_file_browser_with_temporary_credentials() {
        let session = Args::try_parse_from([
            "imgossh",
            "--jumpserver",
            "--sftp",
            "--host=gateway",
            "--username=JMS-token",
            "--password=temporary",
            "--port=2222",
        ])
        .unwrap()
        .into_startup(&Config::default())
        .connection
        .unwrap();
        assert_eq!(session.protocol, Protocol::Sftp);
        assert!(session.sftp_panel);
        assert!(session.jumpserver);
        assert!(!session.use_agent);
        assert!(!session.auto_tunnels);
        assert_eq!(session.password, "temporary");
    }

    #[test]
    fn existing_modes_and_custom_port_key_still_work() {
        let config = Config::default();
        assert!(
            Args::try_parse_from(["imgossh", "--local"])
                .unwrap()
                .into_startup(&config)
                .local
        );
        assert_eq!(
            Args::try_parse_from(["imgossh", "--session", "Production"])
                .unwrap()
                .into_startup(&config)
                .session
                .as_deref(),
            Some("Production")
        );
        let session = Args::try_parse_from([
            "imgossh",
            "user@[::1]",
            "-p",
            "2200",
            "-i",
            "key with spaces",
        ])
        .unwrap()
        .into_startup(&config)
        .connection
        .unwrap();
        assert_eq!(
            (
                session.username.as_str(),
                session.host.as_str(),
                session.port
            ),
            ("user", "::1", 2200)
        );
        assert_eq!(session.private_key, "key with spaces");
        assert!(!session.jumpserver);
        let session = Args::try_parse_from(["imgossh", "old@host", "-l", "new"])
            .unwrap()
            .into_startup(&config)
            .connection
            .unwrap();
        assert_eq!(
            (session.username.as_str(), session.host.as_str()),
            ("new", "host")
        );
    }

    #[test]
    fn rejects_ambiguous_modes_and_invalid_ports() {
        for args in [
            vec!["imgossh", "--local", "user@host"],
            vec!["imgossh", "--session", "saved", "--host=h"],
            vec!["imgossh", "h", "--host=h"],
            vec!["imgossh", "--jumpserver"],
            vec!["imgossh", "--password=token"],
            vec!["imgossh", "h", "--port=0"],
            vec!["imgossh", "h", "--port=65536"],
            vec!["imgossh", "h", "--jumpserver", "-i", "key"],
        ] {
            assert!(Args::try_parse_from(args).is_err());
        }
    }
}
