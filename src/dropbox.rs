use std::{io::Read, path::Path, time::Duration};

use anyhow::{anyhow, ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::{
    digest::{digest, SHA256},
    rand::{SecureRandom, SystemRandom},
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use ureq::{Agent, Request, Response};
use url::Url;
use zeroize::Zeroizing;

use crate::{t, vault};

const APP_KEY: &str = match option_env!("IMGOSSH_DROPBOX_APP_KEY") {
    Some(key) => key,
    None => "",
};
const AUTHORIZE: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN: &str = "https://api.dropboxapi.com/oauth2/token";
const CONTENT: &str = "https://content.dropboxapi.com/2/files";
const FILE: &str = "/ImGoSSH.igsync";
const METADATA_LIMIT: usize = 64 * 1024;

pub(crate) fn available() -> bool {
    !APP_KEY.trim().is_empty()
}

pub(crate) struct Authorization {
    verifier: Zeroizing<String>,
}

impl Authorization {
    pub(crate) fn begin() -> Result<(Self, String)> {
        ensure!(
            available(),
            t!("Dropbox sync is not configured in this build.")
        );
        Self::begin_with(APP_KEY.trim())
    }

    fn begin_with(client: &str) -> Result<(Self, String)> {
        let mut random = Zeroizing::new([0; 32]);
        SystemRandom::new()
            .fill(random.as_mut())
            .map_err(|_| anyhow!(t!("Could not start Dropbox authorization.")))?;
        let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_slice()));
        let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
        let mut url = Url::parse(AUTHORIZE)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", client),
            ("response_type", "code"),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
            ("token_access_type", "offline"),
            ("scope", "files.content.read files.content.write"),
        ]);
        Ok((Self { verifier }, url.into()))
    }

    pub(crate) fn exchange(self, code: &str) -> Result<Account> {
        self.exchange_with(&agent(), TOKEN, APP_KEY.trim(), code)
    }

    fn exchange_with(
        self,
        agent: &Agent,
        endpoint: &str,
        client: &str,
        code: &str,
    ) -> Result<Account> {
        let mut token = token_request(
            agent,
            endpoint,
            client,
            &[
                ("grant_type", "authorization_code"),
                ("code", code.trim()),
                ("code_verifier", &self.verifier),
            ],
        )?;
        let id = token
            .account_id
            .take()
            .filter(|id| !id.is_empty())
            .context(invalid_reply())?;
        let refresh = token
            .refresh
            .take()
            .filter(|refresh| !refresh.is_empty())
            .context(invalid_reply())?;
        Ok(Account { id, refresh })
    }
}

pub(crate) struct Account {
    pub(crate) id: String,
    refresh: Zeroizing<String>,
}

impl Account {
    pub(crate) fn remember(&self, path: &Path) -> Result<()> {
        vault::remember(path, &self.refresh).map_err(|_| credential_error())
    }
}

struct Token {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct RawToken {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

fn token_request(
    agent: &Agent,
    endpoint: &str,
    client: &str,
    form: &[(&str, &str)],
) -> Result<Token> {
    let body = Zeroizing::new(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", client)
            .extend_pairs(form.iter().copied())
            .finish(),
    );
    let reply = response(
        agent
            .post(endpoint)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_bytes(body.as_bytes()),
    )?;
    match reply.status() {
        200 => {
            let RawToken {
                access_token,
                refresh_token,
                account_id,
            } = parse_json(reply)?;
            let access = Zeroizing::new(access_token);
            let refresh = refresh_token.map(Zeroizing::new);
            ensure!(
                !access.is_empty() && access.bytes().all(|byte| byte.is_ascii_graphic()),
                invalid_reply()
            );
            Ok(Token {
                access,
                refresh,
                account_id,
            })
        }
        400 | 401 => Err(anyhow!(t!(
            "Dropbox authorization failed or expired. Authorize Dropbox again."
        ))),
        status => Err(http_error(status)),
    }
}

pub(crate) struct Dropbox {
    agent: Agent,
    content: String,
    auth: Zeroizing<String>,
}

pub(crate) struct Remote {
    pub(crate) bytes: Vec<u8>,
    pub(crate) rev: String,
}

#[derive(Deserialize)]
struct Metadata {
    rev: String,
}

impl Dropbox {
    pub(crate) fn connect(path: &Path) -> Result<Self> {
        let refresh = vault::recalled(path).map_err(|_| credential_error())?;
        Self::connect_with(agent(), TOKEN, APP_KEY.trim(), &refresh)
    }

    fn connect_with(agent: Agent, endpoint: &str, client: &str, refresh: &str) -> Result<Self> {
        let token = token_request(
            &agent,
            endpoint,
            client,
            &[("grant_type", "refresh_token"), ("refresh_token", refresh)],
        )?;
        Ok(Self {
            agent,
            content: CONTENT.into(),
            auth: Zeroizing::new(format!("Bearer {}", *token.access)),
        })
    }

    fn request(&self, route: &str, arg: Value) -> Request {
        self.agent
            .post(&format!("{}/{route}", self.content))
            .set("Authorization", &self.auth)
            .set("Dropbox-API-Arg", &arg.to_string())
    }

    pub(crate) fn download(&self, limit: usize) -> Result<Option<Remote>> {
        let reply = response(self.request("download", json!({ "path": FILE })).call())?;
        match reply.status() {
            200 => {
                let Metadata { rev } = reply
                    .header("Dropbox-API-Result")
                    .and_then(|header| serde_json::from_str(header).ok())
                    .context(invalid_reply())?;
                ensure!(!rev.is_empty(), invalid_reply());
                let bytes = read_body(reply, limit)?;
                Ok(Some(Remote { bytes, rev }))
            }
            409 if download_missing(reply) => Ok(None),
            status => Err(http_error(status)),
        }
    }

    pub(crate) fn upload(&self, rev: Option<&str>, bytes: &[u8]) -> Result<()> {
        let mode = match rev {
            Some(rev) => json!({ ".tag": "update", "update": rev }),
            None => json!("add"),
        };
        let reply = response(
            self.request(
                "upload",
                json!({
                    "path": FILE,
                    "mode": mode,
                    "autorename": false,
                    "strict_conflict": true,
                    "mute": true,
                }),
            )
            .set("Content-Type", "application/octet-stream")
            .send_bytes(bytes),
        )?;
        match reply.status() {
            200 => Ok(()),
            409 if upload_conflict(reply) => Err(anyhow!(t!(
                "The cloud snapshot changed. Pull and apply it before publishing."
            ))),
            status => Err(http_error(status)),
        }
    }
}

fn agent() -> Agent {
    ureq::AgentBuilder::new()
        .https_only(true)
        .redirects(0)
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .build()
}

fn response(result: std::result::Result<Response, ureq::Error>) -> Result<Response> {
    match result {
        Ok(reply) | Err(ureq::Error::Status(_, reply)) => Ok(reply),
        Err(_) => Err(anyhow!(t!(
            "Could not connect to Dropbox. Check your internet connection."
        ))),
    }
}

fn download_missing(reply: Response) -> bool {
    parse_json::<Value>(reply).is_ok_and(|body| {
        let error = &body["error"];
        error[".tag"] == "path" && error["path"][".tag"] == "not_found"
    })
}

fn upload_conflict(reply: Response) -> bool {
    parse_json::<Value>(reply).is_ok_and(|body| {
        let error = &body["error"];
        error[".tag"] == "path" && error["reason"][".tag"] == "conflict"
    })
}

fn parse_json<T: DeserializeOwned>(reply: Response) -> Result<T> {
    let bytes = Zeroizing::new(read_body(reply, METADATA_LIMIT)?);
    serde_json::from_slice(&bytes).map_err(|_| invalid_reply())
}

fn read_body(reply: Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reply
        .into_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_reply())?;
    ensure!(bytes.len() <= limit, t!("The sync snapshot is too large."));
    Ok(bytes)
}

fn invalid_reply() -> anyhow::Error {
    anyhow!(t!("Dropbox returned an invalid response."))
}

fn credential_error() -> anyhow::Error {
    anyhow!(t!(
        "Could not access Dropbox authorization in the system credential store."
    ))
}

fn http_error(status: u16) -> anyhow::Error {
    match status {
        401 => anyhow!(t!(
            "Dropbox authorization failed or expired. Authorize Dropbox again."
        )),
        403 => anyhow!(t!(
            "Dropbox did not allow this action. Check the account and app permissions."
        )),
        429 => anyhow!(t!("Dropbox is busy. Try again shortly.")),
        _ => anyhow!(t!("Dropbox returned HTTP {0}.", status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Store, sync};
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn authorization_uses_public_client_pkce_and_only_file_content_scopes() {
        let (authorization, url) = Authorization::begin_with("test-app").unwrap();
        let url = Url::parse(&url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(url.host_str(), Some("www.dropbox.com"));
        assert_eq!(url.path(), "/oauth2/authorize");
        assert_eq!(query["client_id"], "test-app");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["token_access_type"], "offline");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["scope"], "files.content.read files.content.write");
        assert!(!query.contains_key("redirect_uri"));
        assert!(!query.contains_key("client_secret"));
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(digest(&SHA256, authorization.verifier.as_bytes()))
        );
        assert!(!url.as_str().contains(authorization.verifier.as_str()));
    }

    struct Step {
        route: &'static str,
        mode: Option<Value>,
        form: Option<Vec<(&'static str, &'static str)>>,
        status: &'static str,
        headers: String,
        body: Vec<u8>,
    }

    fn mock_server(steps: Vec<Step>) -> (Agent, String, thread::JoinHandle<Vec<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut payloads = Vec::new();
            for step in steps {
                let deadline = std::time::Instant::now() + Duration::from_secs(6);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("No request for {}: {error}", step.route),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), format!("POST {} HTTP/1.1", step.route));
                let mut headers = std::collections::HashMap::new();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                }
                if let Some(form) = step.form {
                    assert!(!headers.contains_key("authorization"));
                    assert_eq!(headers["content-type"], "application/x-www-form-urlencoded");
                    let mut bytes = vec![0; headers["content-length"].parse().unwrap()];
                    reader.read_exact(&mut bytes).unwrap();
                    let fields: Vec<_> = url::form_urlencoded::parse(&bytes).collect();
                    assert_eq!(fields.len(), form.len());
                    for (key, value) in form {
                        assert!(fields
                            .iter()
                            .any(|field| field.0 == key && field.1 == value));
                    }
                } else {
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    let arg: Value = serde_json::from_str(&headers["dropbox-api-arg"]).unwrap();
                    assert_eq!(arg["path"], FILE);
                    if let Some(mode) = step.mode {
                        assert_eq!(arg["mode"], mode);
                        assert_eq!(arg["autorename"], false);
                        assert_eq!(arg["strict_conflict"], true);
                        assert_eq!(headers["content-type"], "application/octet-stream");
                        let mut bytes = vec![0; headers["content-length"].parse().unwrap()];
                        reader.read_exact(&mut bytes).unwrap();
                        payloads.push(bytes);
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {}\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
                    step.status,
                    step.body.len(),
                    step.headers
                )
                .unwrap();
                stream.write_all(&step.body).unwrap();
            }
            payloads
        });
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(Duration::from_secs(5))
            .try_proxy_from_env(false)
            .build();
        (agent, base, server)
    }

    fn mock(steps: Vec<Step>) -> (Dropbox, thread::JoinHandle<Vec<Vec<u8>>>) {
        let (agent, base, server) = mock_server(steps);
        (
            Dropbox {
                agent,
                content: format!("{base}/2/files"),
                auth: Zeroizing::new("Bearer test-token".into()),
            },
            server,
        )
    }

    fn oauth(form: Vec<(&'static str, &'static str)>, status: &'static str, body: &[u8]) -> Step {
        Step {
            route: "/oauth2/token",
            mode: None,
            form: Some(form),
            status,
            headers: String::new(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn authorization_code_exchange_and_refresh_use_public_client_forms() {
        let verifier = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
        let (agent, base, server) = mock_server(vec![
            oauth(
                vec![
                    ("client_id", "test-app"),
                    ("grant_type", "authorization_code"),
                    ("code", "one+time/code="),
                    ("code_verifier", verifier),
                ],
                "200 OK",
                br#"{"access_token":"first-access","refresh_token":"refresh+token/=","account_id":"dbid:test"}"#,
            ),
            oauth(
                vec![
                    ("client_id", "test-app"),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", "refresh+token/="),
                ],
                "200 OK",
                br#"{"access_token":"second-access"}"#,
            ),
        ]);
        let endpoint = format!("{base}/oauth2/token");
        let account = Authorization {
            verifier: Zeroizing::new(verifier.into()),
        }
        .exchange_with(&agent, &endpoint, "test-app", " one+time/code=\r\n")
        .unwrap();
        assert_eq!(account.id, "dbid:test");
        let dropbox =
            Dropbox::connect_with(agent, &endpoint, "test-app", &account.refresh).unwrap();
        assert_eq!(*dropbox.auth, "Bearer second-access");
        server.join().unwrap();
    }

    #[test]
    fn token_failures_do_not_surface_response_bodies() {
        let form = vec![
            ("client_id", "test-app"),
            ("grant_type", "refresh_token"),
            ("refresh_token", "test-refresh"),
        ];
        let (agent, base, server) = mock_server(vec![
            oauth(form.clone(), "400 Bad Request", b"private response details"),
            oauth(form.clone(), "200 OK", b"private response details"),
            oauth(form, "200 OK", br#"{"access_token":"private\r\ndetails"}"#),
        ]);
        for _ in 0..3 {
            let result = Dropbox::connect_with(
                agent.clone(),
                &format!("{base}/oauth2/token"),
                "test-app",
                "test-refresh",
            );
            let message = result.err().unwrap().to_string();
            assert!(!message.contains("private"));
            assert!(!message.contains("details"));
            assert!(!message.contains("test-refresh"));
        }
        server.join().unwrap();
    }

    fn download(status: &'static str, headers: &str, body: &[u8]) -> Step {
        Step {
            route: "/2/files/download",
            mode: None,
            form: None,
            status,
            headers: headers.into(),
            body: body.into(),
        }
    }

    fn upload(mode: Value, status: &'static str, body: &[u8]) -> Step {
        Step {
            route: "/2/files/upload",
            mode: Some(mode),
            form: None,
            status,
            headers: String::new(),
            body: body.into(),
        }
    }

    const PASSWORD: &str = "a unique sync password";
    const MISSING: &[u8] = br#"{"error":{".tag":"path","path":{".tag":"not_found"}}}"#;
    const CONFLICT: &[u8] = br#"{"error":{".tag":"path","reason":{".tag":"conflict","conflict":{".tag":"file"}},"upload_session_id":"test"}}"#;
    const REV: &str = "Dropbox-API-Result: {\"rev\":\"abc123\"}\r\n";

    #[test]
    fn encrypted_publish_pull_and_revision_conflicts_use_dropbox_protocol() {
        let local = tempfile::tempdir().unwrap();
        let mut source = Store::load_from(local.path(), false);
        source.config.font_size = 20.0;
        let (client, server) = mock(vec![
            download("409 Conflict", "", MISSING),
            upload(json!("add"), "200 OK", b"{}"),
        ]);
        let revision = sync::push(&client, "", PASSWORD, sync::Snapshot::capture(&source)).unwrap();
        let bytes = server.join().unwrap().pop().unwrap();
        assert!(bytes.starts_with(b"ImGoSSH-CONFIG-SYNC\0"));

        let (client, server) = mock(vec![
            download("200 OK", REV, &bytes),
            download("200 OK", REV, &bytes),
            upload(
                json!({ ".tag": "update", "update": "abc123" }),
                "409 Conflict",
                CONFLICT,
            ),
            download("200 OK", REV, &bytes),
        ]);
        let other = tempfile::tempdir().unwrap();
        let mut other = Store::load_from(other.path(), false);
        sync::pull(&client, PASSWORD)
            .unwrap()
            .apply(&mut other)
            .unwrap();
        assert_eq!(other.config.sync_revision, revision);
        assert_eq!(other.config.font_size, 20.0);
        let new_snapshot = || sync::Snapshot::capture(&source);
        let error = sync::push(&client, &revision, PASSWORD, new_snapshot()).unwrap_err();
        assert_eq!(
            error.to_string(),
            t!("The cloud snapshot changed. Pull and apply it before publishing.")
        );
        assert!(sync::push(&client, "stale", PASSWORD, new_snapshot()).is_err());
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn download_requires_metadata_and_bounds_payload_without_following_redirects() {
        let (client, server) = mock(vec![
            download("200 OK", "", b"snapshot"),
            download("200 OK", REV, b"oversize"),
            download("302 Found", "Location: https://invalid.example/\r\n", b""),
        ]);
        assert!(client.download(32).is_err());
        assert!(client.download(3).is_err());
        let Err(error) = client.download(32) else {
            panic!("redirect accepted")
        };
        assert_eq!(error.to_string(), t!("Dropbox returned HTTP {0}.", 302));
        server.join().unwrap();
    }
}
