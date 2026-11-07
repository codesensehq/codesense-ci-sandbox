// SPDX-License-Identifier: MIT
//! The HTTP client for the publish protocol (SW-PB-22e-2, DR-4): one method
//! per route the watcher calls (R1, R2, R3, R4, R5, R7). Every request goes to
//! `DASH_HOST` only (AC-8) and carries the publisher token (F1) as a bearer.
//! A failure is a [`CallError`] that holds a status and the registry error
//! code, never a token, a header or a response body.
//!
//! `DASH_HOST` is fixed at build with `CODESENSE_DASH_HOST`: beta uses
//! `https://share.codesense.ai`, and beta-staging uses the share host its channel config
//! names. When it is unset, the build reaches no host at all.

use std::time::Duration;

use reqwest::redirect::Policy;
use reqwest::{RequestBuilder, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use super::manifest::LiveQueries;
use super::payload::{Payload, PayloadFile};
use super::state::LiveApproved;
use crate::commands::credential_store::Secret;

/// The one host the client may call (AC-8), fixed at build. `None` when the
/// build did not name one: then no request is sent anywhere.
pub const DASH_HOST: Option<&str> = match option_env!("CODESENSE_DASH_HOST") {
    Some(host) if !host.is_empty() => Some(host),
    _ => None,
};

/// The Keychain entry for the publisher token (F1, DP-D19).
pub const TOKEN_SERVICE: &str = "ai.codesense.dashboards";
pub const TOKEN_ACCOUNT: &str = "publisher-token";

/// A request that did not succeed. `NoResponse` covers timeouts, connection
/// errors and unreadable replies. `Status` keeps the registry error code (E1 to
/// E15), which is all the publisher API says (spine `onError` rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    NoResponse,
    Status {
        status: u16,
        code: String,
    },
    /// A 2xx reply this client cannot read. Kept apart from `NoResponse`, so a
    /// contract change is not retried as if the network had failed.
    BadReply,
}

/// R3's answer: a new version to upload, or the live version, unchanged.
/// `checked_at` is set only when the reply carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Version {
    New(u64),
    Unchanged {
        version: u64,
        checked_at: Option<String>,
    },
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

#[derive(Deserialize)]
struct VersionReply {
    version: u64,
    #[serde(default)]
    unchanged: bool,
    #[serde(default, deserialize_with = "opt_unix_seconds")]
    checked_at: Option<String>,
}

#[derive(Deserialize)]
struct CommitReply {
    #[serde(deserialize_with = "unix_seconds")]
    pushed_at: String,
}

#[derive(Deserialize)]
struct CheckinReply {
    #[serde(deserialize_with = "unix_seconds")]
    checked_at: String,
}

/// R8's answer. Only the current version is read.
#[derive(Deserialize)]
struct DashboardReply {
    current_version: Option<u64>,
}

#[derive(Deserialize)]
struct MoveLinkReply {
    url: String,
}

pub use super::status::{DashboardView, Share};

#[derive(Deserialize)]
struct InventoryReply {
    dashboards: Vec<DashboardView>,
}

/// The share list in R8's and R9's answers. Other fields are not read.
#[derive(Deserialize)]
struct SharesReply {
    shares: Vec<Share>,
}

/// The Worker sends times as unix seconds (`now()` in `lib/time.ts`). The
/// record keeps them as RFC 3339 text (P2).
fn unix_seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    rfc3339(i64::deserialize(d)?)
}

pub(super) fn opt_unix_seconds<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    Option::<i64>::deserialize(d)?.map(rfc3339).transpose()
}

fn rfc3339<E: serde::de::Error>(secs: i64) -> Result<String, E> {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or_else(|| E::custom("time out of range"))
}

pub struct Client {
    http: reqwest::Client,
    base: Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeletedFiles {
    pub versions: u64,
    pub files: u64,
}

#[derive(serde::Deserialize)]
struct UnpublishReply {
    #[serde(default)]
    deleted: Option<DeletedFiles>,
}

impl Client {
    /// Refuses any base that is not `DASH_HOST` (AC-8).
    pub fn new(base: &str) -> Result<Client, String> {
        let base = Url::parse(base).map_err(|e| format!("dashboard host is not a URL: {e}"))?;
        if !host_allowed(&base) {
            return Err(format!(
                "{} is not the dashboard host",
                base.host_str().unwrap_or("(none)")
            ));
        }
        let http = reqwest::Client::builder()
            // A redirect could leave DASH_HOST, so it is never followed (AC-8).
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Client { http, base })
    }

    /// R1: create or update the dashboard row. `live_queries` is the list of
    /// the `live` block saved on the record (P2 `live_approved`, SW-DP-4b),
    /// sent as `null` when there is none, so the server never keeps a list the
    /// manifest no longer declares. `live_streams` (SW-DP-20) is the block's
    /// stream list, `null` when it declares none. `live_control` (SW-DP-23) is
    /// `"owner"` when the block says `control: owner`, else `null`.
    pub async fn put_dashboard(
        &self,
        token: &Secret,
        slug: &str,
        title: &str,
        visibility: &str,
        live: Option<&LiveApproved>,
        session_days: u16,
        presentation: Option<&super::manifest::Presentation>,
    ) -> Result<(), CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}"))?;
        let live_queries: Option<&LiveQueries> = live.map(|l| &l.queries);
        let live_streams = live.map(|l| &l.streams).filter(|s| !s.is_empty());
        // SW-DP-23 (T2 `live_control`): "owner" when the saved block allows owner control.
        let live_control = live.filter(|l| l.control).map(|_| "owner");
        let mut body = json!({
            "title": title,
            "presentation": presentation,
            "visibility": visibility,
            "session_days": session_days,
            "live_queries": live_queries,
            "live_streams": live_streams,
            "live_control": live_control,
            "live_actions": live.map(|l| l.actions.iter().map(|(name, a)| (name, serde_json::json!({"who": a.who, "verb": a.verb}))).collect::<std::collections::BTreeMap<_,_>>()).filter(|a| !a.is_empty()),
        });
        if let Some(live) = live {
            let viewers: std::collections::BTreeMap<_, _> = super::catalog::CATALOG
                .iter()
                .filter(|def| live.queries.allows(def.name))
                .map(|def| {
                    let query = LiveQueries::List(vec![def.name.to_string()]);
                    (
                        def.name,
                        super::manifest::missing_pin(&query, &live.args).is_none(),
                    )
                })
                .collect();
            body["live_viewer_allowed"] = json!(viewers);
            if let Some(scope) = &live.scope {
                use sha2::{Digest, Sha256};
                let canonical = serde_json::to_vec(&(&scope.org, &scope.team))
                    .expect("saved scope serializes");
                let fingerprint: String = Sha256::digest(canonical)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                body["live_scope"] = json!(fingerprint);
            }
        }
        if let Some(args) = live.map(|l| &l.args).filter(|args| !args.is_empty()) {
            body["live_args"] = json!(args);
        }
        self.send(self.http.put(url).json(&body), token)
            .await
            .map(drop)
    }

    /// R2: set the dashboard's `state` to `published`. Idempotent.
    pub async fn publish(&self, token: &Secret, slug: &str) -> Result<(), CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/publish"))?;
        self.send(self.http.post(url), token).await.map(drop)
    }

    /// R3: ask for a new version of `payload`, or learn that the live version
    /// already has this source.
    pub async fn create_version(
        &self,
        token: &Secret,
        slug: &str,
        payload: &Payload,
        expected_current: Option<u64>,
    ) -> Result<Version, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/versions"))?;
        let files: Vec<_> = payload
            .files
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "sha256": f.sha256,
                    "size": f.size,
                    "content_type": f.content_type,
                })
            })
            .collect();
        let body = json!({
            "source_sha256": payload.manifest_sha,
            "expected_current": expected_current,
            "files": files,
        });
        let reply: VersionReply = parse(self.send(self.http.post(url).json(&body), token).await?)?;
        Ok(if reply.unchanged {
            Version::Unchanged {
                version: reply.version,
                checked_at: reply.checked_at,
            }
        } else {
            Version::New(reply.version)
        })
    }

    /// R4: upload one file of `version`. The Worker checks `Content-SHA256`.
    pub async fn upload(
        &self,
        token: &Secret,
        slug: &str,
        version: u64,
        file: &PayloadFile,
    ) -> Result<(), CallError> {
        let mut url = self.endpoint(&format!("/api/dash/v1/d/{slug}/versions/{version}/files"))?;
        url.query_pairs_mut().append_pair("path", &file.path);
        let request = self
            .http
            .put(url)
            .header("Content-SHA256", &file.sha256)
            .header("Content-Type", file.content_type)
            .body(file.bytes.clone());
        self.send(request, token).await.map(drop)
    }

    /// R5: commit `version` if the live pointer is still `expected_current`.
    /// Returns the server's `pushed_at`, which is also its `checked_at`.
    pub async fn commit(
        &self,
        token: &Secret,
        slug: &str,
        version: u64,
        expected_current: Option<u64>,
        retain: u8,
    ) -> Result<String, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/versions/{version}/commit"))?;
        let body = json!({ "expected_current": expected_current, "retain": retain });
        let reply: CommitReply = parse(self.send(self.http.post(url).json(&body), token).await?)?;
        Ok(reply.pushed_at)
    }

    /// R6: take the dashboard down. Idempotent on the server.
    pub async fn unpublish(
        &self,
        token: &Secret,
        slug: &str,
    ) -> Result<Option<DeletedFiles>, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/unpublish"))?;
        let reply: UnpublishReply = parse(self.send(self.http.post(url), token).await?)?;
        Ok(reply.deleted)
    }

    /// R8: the server's current version for the dashboard, `None` before the
    /// first commit.
    pub async fn current_version(
        &self,
        token: &Secret,
        slug: &str,
    ) -> Result<Option<u64>, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}"))?;
        let reply: DashboardReply = parse(self.send(self.http.get(url), token).await?)?;
        Ok(reply.current_version)
    }

    /// R8: the dashboard's active share list.
    pub async fn shares(&self, token: &Secret, slug: &str) -> Result<Vec<Share>, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}"))?;
        let reply: SharesReply = parse(self.send(self.http.get(url), token).await?)?;
        Ok(reply.shares)
    }

    pub async fn view(&self, token: &Secret, slug: &str) -> Result<DashboardView, CallError> {
        parse(
            self.send(
                self.http
                    .get(self.endpoint(&format!("/api/dash/v1/d/{slug}"))?),
                token,
            )
            .await?,
        )
    }

    /// Ask the current publisher key for a browser-confirmed move link.
    pub async fn dashboard_move_link(
        &self,
        token: &Secret,
        slug: &str,
    ) -> Result<String, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/move"))?;
        let reply: MoveLinkReply = parse(self.send(self.http.post(url), token).await?)?;
        Ok(reply.url)
    }

    pub async fn list(&self, token: &Secret) -> Result<Vec<DashboardView>, CallError> {
        let reply: InventoryReply = parse(
            self.send(self.http.get(self.endpoint("/api/dash/v1/d")?), token)
                .await?,
        )?;
        Ok(reply.dashboards)
    }

    /// R9: replace the active share list. Each new address gets one invite
    /// draft on the server; nothing is sent until it is sent from the outbox.
    pub async fn put_shares(
        &self,
        token: &Secret,
        slug: &str,
        emails: &[String],
    ) -> Result<Vec<Share>, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/shares"))?;
        let body = json!({ "emails": emails });
        let reply: SharesReply = parse(self.send(self.http.put(url).json(&body), token).await?)?;
        Ok(reply.shares)
    }

    pub async fn revoke_share_sessions(
        &self,
        token: &Secret,
        slug: &str,
        share_id: &str,
    ) -> Result<(), CallError> {
        let mut url = self.endpoint(&format!("/api/dash/v1/d/{slug}/shares/"))?;
        url.path_segments_mut()
            .map_err(|_| CallError::BadReply)?
            .pop_if_empty()
            .push(share_id)
            .push("sessions")
            .push("revoke");
        self.send(self.http.post(url), token).await.map(drop)
    }

    /// R7: a heartbeat for unchanged data. Returns the server's `checked_at`.
    pub async fn checkin(&self, token: &Secret, slug: &str) -> Result<String, CallError> {
        let url = self.endpoint(&format!("/api/dash/v1/d/{slug}/checkin"))?;
        let reply: CheckinReply = parse(self.send(self.http.post(url), token).await?)?;
        Ok(reply.checked_at)
    }

    /// The URL for `path` on the base host. A path that resolves to another
    /// host is refused.
    fn endpoint(&self, path: &str) -> Result<Url, CallError> {
        let url = self.base.join(path).map_err(|_| CallError::NoResponse)?;
        if host_allowed(&url) {
            Ok(url)
        } else {
            Err(CallError::NoResponse)
        }
    }

    /// Send with the bearer token and return the body of a success. The reqwest
    /// error is dropped on purpose: it can name the URL, and nothing else from
    /// the request is kept.
    async fn send(&self, request: RequestBuilder, token: &Secret) -> Result<Vec<u8>, CallError> {
        let response = request
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|_| CallError::NoResponse)?;
        let status = response.status();
        let body = response.bytes().await.map_err(|_| CallError::NoResponse)?;
        if status.is_success() {
            return Ok(body.to_vec());
        }
        let code = serde_json::from_slice::<ErrorBody>(&body)
            .map(|b| b.error)
            .unwrap_or_default();
        Err(CallError::Status {
            status: status.as_u16(),
            code,
        })
    }
}

fn parse<T: DeserializeOwned>(body: Vec<u8>) -> Result<T, CallError> {
    serde_json::from_slice(&body).map_err(|_| CallError::BadReply)
}

/// The scheme, host and port must be `DASH_HOST`'s. Loopback is allowed in the
/// tests only, where the fake endpoint runs.
fn host_allowed(url: &Url) -> bool {
    allowed_for(url, DASH_HOST)
}

/// R22, the Mac's live link (SW-DP-4c): `DASH_HOST` with `https` mapped to
/// `wss`, at `/api/dash/v1/live`. `None` when the build names no host, so no
/// connection is attempted (DP-INV-2).
pub fn live_url() -> Option<Url> {
    live_url_for(DASH_HOST)
}

/// `live_url` for a given build host. The result passes `live_allowed_for`.
pub fn live_url_for(dash_host: Option<&str>) -> Option<Url> {
    let host = Url::parse(dash_host?).ok()?;
    if host.scheme() != "https" {
        return None;
    }
    let mut url = host.join("/api/dash/v1/live").ok()?;
    url.set_scheme("wss").ok()?;
    live_allowed_for(&url, dash_host).then_some(url)
}

/// The `allowed_for` rule for a WebSocket URL: `wss` is checked as `https`
/// (and `ws` as `http`, which no dashboard host uses). Loopback is allowed in
/// the tests only, as for HTTP.
pub fn live_allowed_for(url: &Url, dash_host: Option<&str>) -> bool {
    let mut as_http = url.clone();
    let mapped = match url.scheme() {
        "wss" => as_http.set_scheme("https"),
        "ws" => as_http.set_scheme("http"),
        _ => return false,
    };
    mapped.is_ok() && allowed_for(&as_http, dash_host)
}

/// `host_allowed` for a given build host. An unset or unparseable host allows
/// nothing outside the tests.
fn allowed_for(url: &Url, dash_host: Option<&str>) -> bool {
    if cfg!(test) && url.host_str() == Some("127.0.0.1") {
        return true;
    }
    let Some(dash) = dash_host.and_then(|h| Url::parse(h).ok()) else {
        return false;
    };
    url.scheme() == dash.scheme()
        && url.host_str() == dash.host_str()
        && url.port_or_known_default() == dash.port_or_known_default()
}

/// A hand-written HTTP endpoint for the tests: one thread-free accept loop on
/// loopback, one request per connection, the reply chosen by `route`.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// One request the fake received.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        /// The path and query, as sent.
        pub target: String,
        /// Header names in lower case.
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Seen {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        pub fn json(&self) -> serde_json::Value {
            serde_json::from_slice(&self.body).expect("request body is JSON")
        }
    }

    pub enum Reply {
        Json(u16, String),
        Redirect(String),
        /// Read the request, then close the connection with no reply.
        Drop,
    }

    pub struct Fake {
        pub base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        pub async fn start(route: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let Some(request) = read_request(&mut socket).await else {
                        continue;
                    };
                    let reply = route(&request);
                    log.lock().unwrap().push(request);
                    match reply {
                        Reply::Json(status, body) => {
                            let head = format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = socket.write_all(head.as_bytes()).await;
                            let _ = socket.write_all(body.as_bytes()).await;
                        }
                        Reply::Redirect(to) => {
                            let head = format!(
                                "HTTP/1.1 302 X\r\nLocation: {to}\r\nContent-Length: 0\r\n\
                                 Connection: close\r\n\r\n"
                            );
                            let _ = socket.write_all(head.as_bytes()).await;
                        }
                        Reply::Drop => {}
                    }
                    let _ = socket.shutdown().await;
                }
            });
            Fake { base, seen }
        }

        /// Every request received so far, in order.
        pub fn calls(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        /// The `method target` of every request, for order checks.
        pub fn routes(&self) -> Vec<String> {
            self.calls()
                .iter()
                .map(|c| format!("{} {}", c.method, c.target))
                .collect()
        }
    }

    async fn read_request(socket: &mut TcpStream) -> Option<Seen> {
        let mut buf = Vec::new();
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 1024];
            let n = socket.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let method = first.next()?.to_string();
        let target = first.next()?.to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let len = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let mut chunk = [0u8; 4096];
            let n = socket.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Some(Seen {
            method,
            target,
            headers,
            body: buf[head_end..head_end + len].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Reply};
    use super::*;

    fn token() -> Secret {
        Secret::new("csp_test_token")
    }

    #[test]
    fn dash_host_is_an_https_dashboard_origin() {
        if let Some(host) = DASH_HOST {
            let host = Url::parse(host).unwrap();
            assert_eq!(host.scheme(), "https");
            assert!(host.host_str().unwrap().ends_with("codesense.ai"));
        }
        let url = Url::parse("https://share.example.invalid").unwrap();
        assert!(allowed_for(&url, Some("https://share.example.invalid")));
        assert!(!allowed_for(&url, Some("https://share.codesense.ai")));
        assert!(!allowed_for(&url, None), "an unset host allowed a request");
    }

    #[cfg(any(cs_channel = "beta-staging", cs_channel = "beta"))]
    #[test]
    fn a_beta_channel_uses_only_its_share_host() {
        const PRODUCTION: &str = "https://share.codesense.ai";
        #[cfg(cs_channel = "beta")]
        let (own, other) = (
            PRODUCTION.to_string(),
            "https://share.example.invalid".to_string(),
        );
        #[cfg(cs_channel = "beta-staging")]
        let (own, other) = (
            super::super::staging_walk::staging_share_host()
                .expect("a staging build has its channel config"),
            PRODUCTION.to_string(),
        );
        let (own, other) = (own.as_str(), other.as_str());
        assert_eq!(DASH_HOST, Some(own));
        assert!(Client::new(own).is_ok());
        assert!(Client::new(other).is_err());
    }

    #[test]
    fn a_base_on_another_host_or_scheme_is_refused() {
        for base in [
            "https://evil.example.com",
            "https://share.codesense.ai.evil.example.com",
            "http://share.codesense.ai",
            "https://codesense.ai",
        ] {
            assert!(Client::new(base).is_err(), "{base} was accepted");
        }
        if let Some(host) = DASH_HOST {
            assert!(Client::new(host).is_ok());
        }
    }

    #[test]
    fn a_path_that_resolves_to_another_host_is_refused() {
        let client = Client {
            http: reqwest::Client::new(),
            base: Url::parse("http://127.0.0.1:1").unwrap(),
        };
        assert!(client.endpoint("//evil.example.com/api").is_err());
        assert!(client.endpoint("/api/dash/v1/d/ai-usage").is_ok());
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_to_another_host() {
        let other = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        let target = other.base.clone();
        let fake = Fake::start(move |_| Reply::Redirect(format!("{target}/x"))).await;
        let client = Client::new(&fake.base).unwrap();
        let result = client.publish(&token(), "ai-usage").await;
        assert_eq!(
            result,
            Err(CallError::Status {
                status: 302,
                code: String::new()
            })
        );
        assert!(other.calls().is_empty(), "the redirect was followed");
    }

    #[tokio::test]
    async fn dashboard_move_link_uses_the_current_key_and_returns_the_browser_url() {
        let fake = Fake::start(|seen| {
            assert_eq!(seen.method, "POST");
            assert_eq!(seen.target, "/api/dash/v1/d/weekly-metrics/move");
            Reply::Json(
                200,
                r#"{"url":"https://staging.example.invalid/account/move/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.into(),
            )
        })
        .await;
        let client = Client::new(&fake.base).unwrap();
        let url = client
            .dashboard_move_link(&token(), "weekly-metrics")
            .await
            .unwrap();
        assert_eq!(
            url,
            "https://staging.example.invalid/account/move/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let call = &fake.calls()[0];
        assert_eq!(call.header("authorization"), Some("Bearer csp_test_token"));
        assert!(call.body.is_empty());
    }

    #[tokio::test]
    async fn the_token_goes_only_in_the_bearer_header() {
        let fake = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        let client = Client::new(&fake.base).unwrap();
        client.publish(&token(), "ai-usage").await.unwrap();
        let call = &fake.calls()[0];
        assert_eq!(call.header("authorization"), Some("Bearer csp_test_token"));
        assert!(!call.body.windows(4).any(|w| w == b"csp_"));
    }

    #[tokio::test]
    async fn an_error_reply_keeps_only_the_registry_code() {
        let fake =
            Fake::start(|_| Reply::Json(409, r#"{"error":"version_conflict"}"#.into())).await;
        let client = Client::new(&fake.base).unwrap();
        assert_eq!(
            client.commit(&token(), "ai-usage", 2, Some(1), 5).await,
            Err(CallError::Status {
                status: 409,
                code: "version_conflict".into()
            })
        );
    }

    #[tokio::test]
    async fn server_times_in_unix_seconds_are_kept_as_rfc3339() {
        // The Worker's `now()` is unix seconds: these bodies are the server's shape.
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(
                    200,
                    r#"{"version":2,"pushed_at":1792141200,"url":"x"}"#.into(),
                )
            } else {
                Reply::Json(200, r#"{"checked_at":1792141500}"#.into())
            }
        })
        .await;
        let client = Client::new(&fake.base).unwrap();
        assert_eq!(
            client.commit(&token(), "ai-usage", 2, Some(1), 5).await,
            Ok("2026-10-16T09:00:00Z".to_string())
        );
        assert_eq!(
            client.checkin(&token(), "ai-usage").await,
            Ok("2026-10-16T09:05:00Z".to_string())
        );
    }

    #[tokio::test]
    async fn an_unreadable_success_reply_is_a_bad_reply_not_no_response() {
        let fake =
            Fake::start(|_| Reply::Json(200, r#"{"checked_at":"not a number"}"#.into())).await;
        let client = Client::new(&fake.base).unwrap();
        assert_eq!(
            client.checkin(&token(), "ai-usage").await,
            Err(CallError::BadReply)
        );
    }

    #[tokio::test]
    async fn the_current_version_is_read_from_r8() {
        let fake = Fake::start(|_| {
            Reply::Json(
                200,
                r#"{"slug":"ai-usage","current_version":3,"shares":[]}"#.into(),
            )
        })
        .await;
        let client = Client::new(&fake.base).unwrap();
        assert_eq!(
            client.current_version(&token(), "ai-usage").await,
            Ok(Some(3))
        );
        assert_eq!(fake.routes(), vec!["GET /api/dash/v1/d/ai-usage"]);
    }

    #[tokio::test]
    async fn a_dropped_connection_is_no_response() {
        let fake = Fake::start(|_| Reply::Drop).await;
        let client = Client::new(&fake.base).unwrap();
        assert_eq!(
            client.checkin(&token(), "ai-usage").await,
            Err(CallError::NoResponse)
        );
    }

    /// SW-DP-4 AC-10: the live link's URL comes only from the dashboard host, as `wss://`.
    #[test]
    fn the_live_url_is_the_dash_host_as_wss() {
        let own = Some("https://share.example.invalid");
        assert_eq!(
            live_url_for(own).unwrap().as_str(),
            "wss://share.example.invalid/api/dash/v1/live"
        );
        assert_eq!(live_url_for(None), None, "an unset host gave a live URL");
        assert_eq!(live_url_for(Some("http://share.codesense.ai")), None);
        assert_eq!(live_url_for(Some("not a url")), None);
        assert_eq!(live_url().is_some(), DASH_HOST.is_some());

        for (url, allowed) in [
            ("wss://share.example.invalid/api/dash/v1/live", true),
            ("ws://share.example.invalid/api/dash/v1/live", false),
            ("https://share.example.invalid/api/dash/v1/live", false),
            ("wss://share.codesense.ai/api/dash/v1/live", false),
            ("wss://evil.example.com/api/dash/v1/live", false),
            ("wss://share.example.invalid:8443/api/dash/v1/live", false),
        ] {
            let url = Url::parse(url).unwrap();
            assert_eq!(live_allowed_for(&url, own), allowed, "{url}");
            assert!(!live_allowed_for(&url, None), "{url} allowed with no host");
        }
    }
}

#[cfg(test)]
mod viewer_sign_in_tests {
    use super::fake::{Fake, Reply};
    use super::*;
    #[tokio::test]
    async fn session_days_and_session_revoke_cross_the_http_seam() {
        let server = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        let client = Client::new(&server.base).unwrap();
        let token = Secret::new("test-token");
        client
            .put_dashboard(&token, "family", "Family", "private", None, 30, None)
            .await
            .unwrap();
        client
            .revoke_share_sessions(&token, "family", "shr_123")
            .await
            .unwrap();
        let seen = server.calls();
        let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
        assert_eq!(body["session_days"], 30);
        assert_eq!(seen[1].method, "POST");
        assert_eq!(
            seen[1].target,
            "/api/dash/v1/d/family/shares/shr_123/sessions/revoke"
        );
    }
}
