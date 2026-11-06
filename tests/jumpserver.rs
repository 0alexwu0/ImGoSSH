//! Offline protocol check using the real plugin template and a local SSH gateway.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use imgossh::config::{Config, Protocol};
use imgossh::ssh::client::{
    Challenge, ConnectOptions, Connection, Prompter, Reply, TermEvent, TermSink, Trust,
};
use imgossh::startup::Args;
use russh::{server, Channel, ChannelId, Pty};

const USER: &str = "JMS-7abc-test-token";
const TOKEN: &str = "-temporary+/token=42&";

#[derive(Default)]
struct Observed {
    authenticated: bool,
    pty: Option<(String, u32, u32)>,
    shell: bool,
    sftp: bool,
}

struct Gateway {
    observed: Arc<Mutex<Observed>>,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
}

impl server::Handler for Gateway {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        if username == USER && password == TOKEN {
            self.observed.lock().unwrap().authenticated = true;
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.observed.lock().unwrap().pty = Some((term.into(), cols, rows));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.observed.lock().unwrap().shell = true;
        session.channel_success(channel)?;
        session.data(channel, b"JumpServer fixture ready\r\n".to_vec())?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if !self.observed.lock().unwrap().sftp {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(name, "sftp");
        self.observed.lock().unwrap().sftp = true;
        session.channel_success(channel)?;
        let stream = self.channels.remove(&channel).unwrap().into_stream();
        tokio::spawn(async move {
            russh_sftp::server::run(stream, Files::default()).await;
        });
        Ok(())
    }
}

#[derive(Default)]
struct Files {
    bytes: Vec<u8>,
    listed: bool,
}

use russh_sftp::protocol::{
    Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        _: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        assert_eq!(filename, "/asset.txt");
        Ok(Handle {
            id,
            handle: filename,
        })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let offset = offset as usize;
        self.bytes
            .resize(self.bytes.len().max(offset + data.len()), 0);
        self.bytes[offset..offset + data.len()].copy_from_slice(&data);
        self.close(id, String::new()).await
    }
    async fn read(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let offset = offset as usize;
        if offset >= self.bytes.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: self.bytes[offset..self.bytes.len().min(offset + len as usize)].to_vec(),
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        assert_eq!(path, "/");
        self.listed = false;
        Ok(Handle { id, handle: path })
    }
    async fn readdir(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        Ok(Name {
            id,
            files: vec![File::new("asset.txt", FileAttributes::default())],
        })
    }
}

#[tokio::test]
async fn plugin_template_authenticates_gateway_and_opens_interactive_terminal() {
    let plugin: serde_json::Value = serde_json::from_str(include_str!(
        "../integrations/jumpserver/plugin/connect.json"
    ))
    .unwrap();
    gateway_roundtrip(
        plugin["launch"]["template"].as_str().unwrap(),
        Protocol::Ssh,
    )
    .await;
}

#[tokio::test]
async fn client_2_1_4_template_authenticates_gateway_and_opens_interactive_terminal() {
    let entry: serde_json::Value = serde_json::from_str(include_str!(
        "../integrations/jumpserver/legacy/terminal.json"
    ))
    .unwrap();
    assert_eq!(entry["is_default"], true);
    assert_eq!(entry["is_set"], true);
    assert_eq!(entry["is_internal"], false);
    assert_eq!(entry["protocol"], serde_json::json!(["ssh"]));
    gateway_roundtrip(entry["arg_format"].as_str().unwrap(), Protocol::Ssh).await;
}

#[tokio::test]
async fn plugin_sftp_template_lists_uploads_and_downloads_without_shell() {
    let plugin: serde_json::Value = serde_json::from_str(include_str!(
        "../integrations/jumpserver/plugin-sftp/connect.json"
    ))
    .unwrap();
    gateway_roundtrip(
        plugin["launch"]["template"].as_str().unwrap(),
        Protocol::Sftp,
    )
    .await;
}

#[tokio::test]
async fn client_2_1_4_sftp_template_lists_uploads_and_downloads_without_shell() {
    let entry: serde_json::Value = serde_json::from_str(include_str!(
        "../integrations/jumpserver/legacy/filetransfer.json"
    ))
    .unwrap();
    assert_eq!(entry["protocol"], serde_json::json!(["sftp"]));
    assert_eq!(entry["is_default"], true);
    assert_eq!(entry["is_set"], true);
    gateway_roundtrip(entry["arg_format"].as_str().unwrap(), Protocol::Sftp).await;
}

async fn gateway_roundtrip(template: &str, protocol: Protocol) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let socket = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = socket.local_addr().unwrap().port();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let gateway = Gateway {
            observed: observed.clone(),
            channels: HashMap::new(),
        };
        let mut seed = [0u8; 32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut seed).unwrap();
        let key = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed);
        let server_config = Arc::new(server::Config {
            keys: vec![russh::keys::PrivateKey::from(key)],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let server = tokio::spawn(async move {
            let (stream, _) = socket.accept().await.unwrap();
            server::run_stream(server_config, stream, gateway)
                .await
                .unwrap()
                .await
                .unwrap();
        });

        // Both v2.1.4 and v4.1.9 substitute these placeholders then split by space.
        let command = template
            .replace("{host}", "127.0.0.1")
            .replace("{port}", &port.to_string())
            .replace("{username}", USER)
            .replace("{value}", TOKEN);
        let args =
            Args::try_parse_from(std::iter::once("imgossh").chain(command.split(' '))).unwrap();
        let startup = args.into_startup(&Config::default());
        let session = startup.connection.unwrap();
        assert_eq!(session.protocol, protocol);
        let (prompter, mut prompts) = Prompter::channel();
        let prompt_task = tokio::spawn(async move {
            let request = prompts
                .recv()
                .await
                .expect("host key verification must remain enabled");
            assert!(matches!(request.challenge, Challenge::HostKey(_)));
            request.reply.send(Reply::TrustKey(Trust::Once)).unwrap();
            // No password prompt: the temporary credential must be used verbatim.
            assert!(prompts.recv().await.is_none());
        });
        let connection = Connection::connect(ConnectOptions::from_session(&session, prompter))
            .await
            .unwrap();
        if protocol == Protocol::Sftp {
            let sftp = connection.sftp().await.unwrap();
            let listing: Vec<_> = sftp.read_dir("/").await.unwrap().collect();
            assert_eq!(listing.len(), 1);
            assert_eq!(listing[0].file_name(), "asset.txt");
            sftp.write("/asset.txt", b"JumpServer file transfer")
                .await
                .unwrap();
            assert_eq!(
                sftp.read("/asset.txt").await.unwrap(),
                b"JumpServer file transfer"
            );
            {
                let observed = observed.lock().unwrap();
                assert!(observed.authenticated);
                assert!(observed.sftp);
                assert!(observed.pty.is_none());
                assert!(!observed.shell);
            }
            sftp.close().await.unwrap();
            connection.close().await;
            drop(connection);
            prompt_task.await.unwrap();
            server.await.unwrap();
            return;
        }
        let (received, mut events) = tokio::sync::mpsc::unbounded_channel();
        let sink = TermSink::new(move |event| {
            if let TermEvent::Data(bytes) = event {
                let _ = received.send(bytes);
            }
        });
        let terminal = connection
            .shell(100, 30, "xterm-256color", None, sink)
            .await
            .unwrap();
        let mut output = Vec::new();
        while !output
            .windows(b"fixture ready".len())
            .any(|w| w == b"fixture ready")
        {
            output.extend(events.recv().await.unwrap());
        }
        terminal.send(b"echo asset-command\r".to_vec()).unwrap();
        while !output
            .windows(b"asset-command".len())
            .any(|w| w == b"asset-command")
        {
            output.extend(events.recv().await.unwrap());
        }
        {
            let observed = observed.lock().unwrap();
            assert!(observed.authenticated);
            assert_eq!(observed.pty, Some(("xterm-256color".into(), 100, 30)));
            assert!(observed.shell);
        }
        terminal.close();
        connection.close().await;
        drop(connection);
        prompt_task.await.unwrap();
        server.await.unwrap();
    })
    .await
    .expect("local gateway handshake timed out");
}
