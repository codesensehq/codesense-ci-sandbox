// SPDX-License-Identifier: MIT
//! The Mac's live link (SW-DP-4c, DP-D38): one outbound WebSocket to R22 on
//! `DASH_HOST`, held while at least one published dashboard has its `live`
//! block on (saved on the record, DP-D39). Viewers see "Owner's Mac: online" while it is up.
//!
//! - **Host.** Only `client::live_url()` (`DASH_HOST` as `wss://`). No host, no link (DP-INV-2).
//! - **Token.** The publisher token from the Keychain, in the `Authorization`
//!   header of the upgrade only. A 401 on upgrade, or close 4003, writes the
//!   token's fingerprint to every published record (the 22e refusal rule); no
//!   link is opened with that token again until it is replaced.
//! - **When.** Checked every pass (5 s): connect when wanted, close with 1000
//!   when not.
//! - **Heartbeat.** Text `ping` every K14; no `pong` within 2 × K14 drops the
//!   socket and reconnects.
//! - **Backoff (K22).** 1, 2, 4 … s, capped at 300 s, ±20 % jitter, reset after
//!   60 s connected. Close 4001 (replaced) or E6 on upgrade: wait the cap.
//! - **Frames (P11).** `hello` is logged; unknown types are ignored. A `q`
//!   frame is answered once, with its own `id` (SW-DP-5a): only for a published
//!   dashboard whose manifest `live` block is the saved one and lists the
//!   query (or is `all`), and only for a catalog query. Answers are redacted
//!   (DP-INV-10), capped at K19, at most K20 run at once, and each is dropped
//!   at K17. Each answered or refused `q` is noted in `live-log.jsonl`.
//! - **Streams (SW-DP-20, P16, DP-D46).** An `s` frame opens a stream the
//!   dashboard declares in `live.streams` (checked as for a query: published,
//!   the saved block, the stream in it and in `STREAMS`); `so` answers it. Its
//!   frames (`v`, `snapshot`, `o`, `end`) are redacted (SV-PBF-286: a terminal's
//!   snapshot and output in `agents`) and go out paced (K27, K28) on the same
//!   sender as answers. `u` stops it at once, `snap` asks for a fresh snapshot,
//!   and a dropped link stops every stream (K32). Each `s` is noted in the view
//!   log too. The pass ends a stream whose block no longer declares it.
//!
//! - **Owner control (SW-DP-23, DP-D42, DP-INV-11).** `hello` names the
//!   publisher's owner account. An `i` frame types into (or resizes) the
//!   terminal of an open `terminal` stream only when the relay's `by` is that
//!   owner, the saved block says `control: owner`, the input is within K38 and
//!   the link within K39, and the session still passes SW-DP-21's team filter.
//!   It goes through the app's own write path (`commands::pty::write_session_input`,
//!   as `write_pty`); anything else is answered `ir` and written to
//!   `control-log.jsonl`, which also gets frame and byte counts per minute of
//!   control. What was typed is never logged or stored.
//!
//! The Mac sends nothing else: `ping`, `a` frames in reply to `q` frames,
//! stream frames only for a stream the relay opened and has not closed
//! (DP-INV-5 as amended by DP-D46), and `ir` for a refused `i`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{interval, sleep, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::WebSocketStream;

use super::approval;
use super::catalog::{
    self, Call, LiveCtx, QueryDef, QueryError, StreamCall, StreamDef, StreamItem, CATALOG, STREAMS,
};
use super::client::{self, DASH_HOST, TOKEN_ACCOUNT, TOKEN_SERVICE};
use super::driver::slug_lock;
use super::manifest::{self, PublishState};
use super::push::fingerprint;
use super::state::{self, LiveApproved, LocalState, PublishRecord, Scope};
use crate::commands::credential_store::{default_store, CredentialStore, Secret};

/// K14: the Mac sends `ping` this often; no `pong` within twice this drops the link.
pub const HEARTBEAT: Duration = Duration::from_secs(30);
/// How often the link checks whether it is wanted (the driver's pass).
pub const PASS: Duration = Duration::from_secs(5);
/// K22: the first reconnect delay, the cap, and how long a link must stay up
/// before the delays start again from the first.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_CAP: Duration = Duration::from_secs(300);
pub const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);
/// The largest frame or message the relay may send the Mac. P11 relay frames
/// are small (`q` args are at most 2 KB, K19); the largest is an owner's `i`
/// (SW-DP-23): K38 of control bytes escapes to about six times that in JSON. A
/// hostile or broken server cannot make the Mac buffer more.
pub const MAX_INBOUND: usize = 128 * 1024;
/// K38 (SW-DP-23): the most bytes of one `keys` or `paste` input (UTF-8).
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
/// K39 (SW-DP-23): input frames and bytes one link takes in any one second;
/// more are refused (`rate_limited`) and dropped, never queued.
pub const INPUT_FRAMES_PER_SEC: usize = 100;
pub const INPUT_BYTES_PER_SEC: usize = 64 * 1024;
/// The largest size an owner may ask for (the toolkit draws at most this).
pub const MAX_INPUT_COLS: u16 = 1000;
pub const MAX_INPUT_ROWS: u16 = 500;
/// How long a session that passed the team filter is taken as passing for
/// later input of the same stream (`resolve` is checked on every frame).
const INPUT_CHECK_FRESH: Duration = Duration::from_secs(1);
/// Accepted input waiting for the link's input worker; past it, `rate_limited`.
const INPUT_QUEUE: usize = 256;
/// How often the control log gets a line per controlled session (DP-D45).
pub const CONTROL_LOG_EVERY: Duration = Duration::from_secs(60);

/// The link's WebSocket limits (review S5).
pub fn link_ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_INBOUND))
        .max_frame_size(Some(MAX_INBOUND))
}
/// K17: a query not answered within this of getting its K20 slot is dropped on the Mac.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// A query waits at most this for a K20 slot; then it is answered `busy` (the relay retries or
/// serves its cache). The wait does not count toward K17.
pub const QUEUE_TIMEOUT: Duration = Duration::from_secs(3);
/// K19: the largest answer, as serialized JSON.
pub const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// K20: at most this many queries run at once; more wait.
pub const MAX_IN_FLIGHT: usize = 8;
/// The most `q` frames one link holds at once (running or waiting for a K20
/// slot). More are answered `busy` at once (review S5).
pub const MAX_PENDING: usize = 64;
/// K27: the largest `data` or `vt` part of one stream frame (bytes of UTF-8).
pub const STREAM_FRAME_BYTES: usize = 64 * 1024;
/// K28: a stream sends at most this many bytes in any one second ...
pub const STREAM_BYTES_PER_SEC: usize = 256 * 1024;
// A value is capped at K19 and must fit in one second's budget, or it could never be sent.
const _: () = assert!(MAX_ANSWER_BYTES <= STREAM_BYTES_PER_SEC);
/// ... and at most one frame per this interval; output in between is coalesced.
pub const STREAM_FRAME_EVERY: Duration = Duration::from_millis(50);
/// K30's ceiling for any stream's snapshot (JSON length); a larger one ends the stream (`too_large`).
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
/// K32: a stream stops within this of `u` or of the link dropping. Both stop
/// it at once here (its task is aborted); the constant is the contract.
pub const STREAM_STOP: Duration = Duration::from_secs(5);
/// The most streams one link holds open at once: twice the relay's own ceiling
/// (K31, 8 watches on each of 50 sockets), for streams lingering after a drop.
/// More are refused (`so` `busy`).
pub const MAX_OPEN_STREAMS: usize = 2 * 8 * 50;
/// The link protocol version (P11/P16 `v`): the relay sends it on `hello` and
/// its stream frames, and the Mac on its stream frames. A frame of another
/// version is ignored.
pub const LINK_PROTOCOL: u64 = 1;

/// The view log, next to the P2 records, and how long its lines are kept.
pub const VIEW_LOG: &str = "live-log.jsonl";
/// SW-DP-23 (DP-D45): owner control, by minute and by refusal, never content.
/// Next to the view log, kept as long.
pub const CONTROL_LOG: &str = "control-log.jsonl";
pub const VIEW_LOG_DAYS: i64 = 30;
/// The longest the longest text field of a view-log line may be.
const VIEW_LOG_FIELD_MAX: usize = 100;

/// The longest an upgrade may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// P11 close codes the relay sends.
const CLOSE_REPLACED: u16 = 4001;
const CLOSE_REVOKED: u16 = 4003;

/// Why an upgrade did not open a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// 401 (E1): the token is refused.
    Unauthorized,
    /// E6 `not_published`: the server has no live dashboard for this publisher yet.
    NotPublished,
    /// Anything else: no response, a TLS or protocol error, another status.
    Failed,
}

/// Opens the WebSocket. The app uses `WssConnector`; the tests use an in-memory relay.
#[async_trait]
pub trait Connect: Send + Sync {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    async fn connect(&self, token: &Secret) -> Result<WebSocketStream<Self::Stream>, ConnectError>;
}

/// The upgrade request for `url`: the token goes in the `Authorization` header
/// only, never in the URL.
fn upgrade_request(url: &str, token: &Secret) -> Result<Request, ConnectError> {
    let mut request = url
        .into_client_request()
        .map_err(|_| ConnectError::Failed)?;
    let bearer = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| ConnectError::Failed)?;
    request.headers_mut().insert(AUTHORIZATION, bearer);
    Ok(request)
}

/// Map a failed upgrade. Only the status is read. R22's only 409 is E6
/// `not_published`, so a 409 counts as E6 whether or not its body arrived with
/// the headers (tungstenite keeps only what came in the same read).
fn upgrade_error(e: tungstenite::Error) -> ConnectError {
    let tungstenite::Error::Http(response) = e else {
        return ConnectError::Failed;
    };
    match response.status().as_u16() {
        401 => ConnectError::Unauthorized,
        409 => ConnectError::NotPublished,
        _ => ConnectError::Failed,
    }
}

/// The app's connector: TLS on the ring provider with the webpki roots. No
/// redirect is followed (tungstenite reports a 3xx as a failed upgrade).
pub struct WssConnector {
    url: Url,
    tls: Arc<rustls::ClientConfig>,
}

impl WssConnector {
    /// Refuses a URL that is not `DASH_HOST`'s live route (loopback in the tests only).
    pub fn new(url: Url) -> Option<WssConnector> {
        if !client::live_allowed_for(&url, DASH_HOST) {
            return None;
        }
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Some(WssConnector {
            url,
            tls: Arc::new(tls),
        })
    }
}

#[async_trait]
impl Connect for WssConnector {
    type Stream = tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>;

    async fn connect(&self, token: &Secret) -> Result<WebSocketStream<Self::Stream>, ConnectError> {
        let request = upgrade_request(self.url.as_str(), token)?;
        let connecting = tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(link_ws_config()),
            false,
            Some(tokio_tungstenite::Connector::Rustls(self.tls.clone())),
        );
        match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Err(_) => Err(ConnectError::Failed),
            Ok(Ok((ws, _))) => Ok(ws),
            Ok(Err(e)) => Err(upgrade_error(e)),
        }
    }
}

/// The records that turn the link on: published on this Mac, with a manifest
/// whose `live` block is the saved one (SW-DP-4 AC-9).
fn live_records(data_dir: &Path, catalog: &[QueryDef]) -> Vec<PublishRecord> {
    let Ok(slugs) = state::list(data_dir) else {
        return Vec::new();
    };
    slugs
        .iter()
        .filter_map(|slug| state::load(data_dir, slug).ok())
        .filter(|record| {
            record.state == LocalState::Published
                && record
                    .folder
                    .as_deref()
                    .and_then(|f| manifest::load_with(Path::new(f), catalog).ok())
                    .is_some_and(|m| approval::is_live(record, &m))
        })
        .collect()
}

/// True when any saved record holds `token`'s fingerprint as refused (the 22e
/// rule: a refused token is not sent again, whatever the record's state).
fn refused(data_dir: &Path, token: &Secret) -> bool {
    let fp = fingerprint(Some(token));
    state::list(data_dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|slug| state::load(data_dir, slug).ok())
        .any(|r| r.refused_token.as_deref() == Some(fp.as_str()))
}

fn turn_off_signal() -> &'static tokio::sync::broadcast::Sender<String> {
    static SIGNAL: std::sync::OnceLock<tokio::sync::broadcast::Sender<String>> =
        std::sync::OnceLock::new();
    SIGNAL.get_or_init(|| tokio::sync::broadcast::channel(16).0)
}

/// SV-PBF-286 B5: end every stream of `slug` on every link now (a person
/// turned its live data off). Queries are already refused: the saved block is
/// cleared first.
pub fn stop_streams(slug: &str) {
    let _ = turn_off_signal().send(slug.to_string());
}

/// The link is wanted: a published dashboard with live on exists and the
/// current token is not refused.
pub fn wanted(data_dir: &Path, token: &Secret) -> bool {
    !live_records(data_dir, CATALOG).is_empty() && !refused(data_dir, token)
}

/// Write `token`'s fingerprint as refused to every published record, each under
/// its turn lock (SW-DP-4 AC-13). False when a record could not be saved.
async fn refuse(data_dir: &Path, token: &Secret) -> bool {
    let mut saved = true;
    let fp = fingerprint(Some(token));
    for slug in state::list(data_dir).unwrap_or_default() {
        let lock = slug_lock(&slug);
        let _turn = lock.lock().await;
        let Ok(mut record) = state::load(data_dir, &slug) else {
            continue;
        };
        if record.state != LocalState::Published || record.refused_token.as_deref() == Some(&fp) {
            continue;
        }
        record.refused_token = Some(fp.clone());
        if let Err(e) = state::save(data_dir, &record) {
            log::warn!(target: "dashboard_publish", "live link: cannot save the refusal for {slug}: {e}");
            saved = false;
        }
    }
    saved
}

/// K22 reconnect delays.
#[derive(Debug, Default)]
struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        use rand::Rng;
        let base = BACKOFF_FIRST
            .saturating_mul(1u32 << self.attempt.min(16))
            .min(BACKOFF_CAP);
        self.attempt = self.attempt.saturating_add(1);
        let jitter = rand::thread_rng().gen_range(0.8..=1.2);
        base.mul_f64(jitter).min(BACKOFF_CAP)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// How a connected session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// No live dashboard is left: the Mac closed with 1000.
    NotWanted,
    /// No `pong` within 2 × K14.
    HeartbeatLost,
    /// The relay closed the socket with this code (none: no close frame).
    Closed(Option<u16>),
    /// The socket failed or ended without a close frame.
    Dropped,
}

/// One `q` frame (P11): the dashboard, the query, its args and who asked:
/// the share it came through, or (SW-DP-22, P16 `by`) the owner's account.
#[derive(Debug, Clone)]
struct Ask {
    id: Value,
    slug: String,
    query: String,
    args: Value,
    share: Option<String>,
    /// `owner` or `viewer` from `by`; `viewer` for a relay that predates `by`.
    role: String,
    /// The owner's account id (`by.account`), for an owner's ask only.
    account: Option<String>,
}

/// One `s` frame (P16): the stream the relay opens, by its `sid`, for the
/// dashboard, with its args and who opened it.
#[derive(Debug, Clone)]
struct Open {
    sid: String,
    slug: String,
    stream: String,
    args: Value,
    by: By,
}

/// P16 `by`: who a frame is for, `{role:"owner", account}` or `{role:"viewer",
/// share}`, never an email. A relay that predates `by` sends `share` on its own
/// (queries only) and means a viewer.
#[derive(Debug, Clone)]
struct By {
    share: Option<String>,
    /// `owner` or `viewer`.
    role: String,
    /// The owner's account id, for an owner only.
    account: Option<String>,
}

fn by_of(frame: &Value) -> By {
    let by = frame.get("by");
    let by_text = |key: &str| {
        by.and_then(|b| b.get(key))
            .and_then(Value::as_str)
            .map(String::from)
    };
    let owner = by_text("role").as_deref() == Some("owner");
    By {
        share: if owner {
            None
        } else {
            by_text("share")
                .or_else(|| frame.get("share").and_then(Value::as_str).map(String::from))
        },
        role: if owner { "owner" } else { "viewer" }.to_string(),
        account: if owner { by_text("account") } else { None },
    }
}

/// What a relay frame asks of the Mac.
enum Frame {
    Ask(Ask),
    Action(Value),
    /// `s`: open a stream.
    Open(Open),
    /// `u`: stop a stream.
    Close(String),
    /// `snap`: send a stream's snapshot again.
    Snap(String),
    /// `hello` (SW-DP-23): the publisher's owner account the relay announces, or none.
    Hello(Option<String>),
    /// `i` (SW-DP-23): owner input for a stream.
    Input(Input),
    /// SW-DP-23: an `i` frame that is not JSON the Mac can read, by its sid.
    BadInput(String),
    Ignore,
}

/// The sid of `text` when it looks like an `i` frame of this version but is not
/// JSON the Mac can read: `"t":"i"` and `"sid":"…"` with a sid of at most
/// [`MAX_SID`] plain characters. Nothing else of it is read.
fn unreadable_input(text: &str) -> Option<String> {
    // The relay writes frames with JSON.stringify: no whitespace between tokens.
    let compact = text;
    if !compact.contains(r#""t":"i""#) {
        return None;
    }
    if let Some(at) = compact.find(r#""v":"#) {
        let v: String = compact[at + 4..]
            .chars()
            .take_while(char::is_ascii_digit)
            .take(20)
            .collect();
        if v.parse::<u64>().ok() != Some(LINK_PROTOCOL) {
            return None;
        }
    }
    let at = compact.find(r#""sid":""#)? + r#""sid":""#.len();
    let sid: String = compact[at..]
        .chars()
        .take_while(|c| *c != '"')
        .take(MAX_SID + 1)
        .collect();
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    (!sid.is_empty() && sid.len() <= MAX_SID && sid.chars().all(plain)).then_some(sid)
}

/// The longest `hello.owner_account` the Mac keeps (DP-D54: a self-hosted
/// relay's account ids may be long; still bounded).
pub const MAX_OWNER_ACCOUNT: usize = 512;

/// `hello.owner_account` when usable. One too long is refused with one warning
/// line naming no value, so owner control is off on this link, and says why.
fn owner_account_of(account: Option<String>) -> Option<String> {
    let account = account.filter(|a| !a.is_empty())?;
    if account.len() > MAX_OWNER_ACCOUNT {
        log::warn!(target: "dashboard_publish", "live link: the relay's owner account is longer than {MAX_OWNER_ACCOUNT} characters; owner control is refused on this link");
        return None;
    }
    Some(account)
}

/// One `i` frame (P16, SW-DP-23): input for the terminal stream `sid`, and who
/// the relay says sent it. Never logged: `data` is what the owner typed.
struct Input {
    sid: String,
    by: By,
    kind: InputKind,
}

enum InputKind {
    Keys(String),
    Paste(String),
    Resize {
        cols: u64,
        rows: u64,
    },
    /// Any other kind, or one whose fields are missing.
    Bad,
}

impl InputKind {
    /// The bytes it types (K38, K39); a resize types none.
    fn bytes(&self) -> usize {
        match self {
            InputKind::Keys(d) | InputKind::Paste(d) => d.len(),
            _ => 0,
        }
    }
}

/// The longest `sid` the Mac keeps (the relay sends UUIDs).
const MAX_SID: usize = 64;

fn read_frame(text: &str) -> Frame {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        // SW-DP-23: an `i` the Mac cannot read (a lone surrogate in `data`, say) is
        // still answered, so the owner is told it did not go.
        return unreadable_input(text).map_or(Frame::Ignore, Frame::BadInput);
    };
    // A frame of another protocol version is not read as this one. A frame with no `v`
    // is read as version 1: the P11 frames of relays from before P16 carry none.
    if frame
        .get("v")
        .is_some_and(|v| v.as_u64() != Some(LINK_PROTOCOL))
    {
        return Frame::Ignore;
    }
    let text_of = |key: &str| frame.get(key).and_then(Value::as_str).map(String::from);
    // A stream frame with no usable sid names nothing, so it is ignored.
    let sid = text_of("sid").filter(|s| !s.is_empty() && s.len() <= MAX_SID);
    match (frame.get("t").and_then(Value::as_str), sid) {
        (Some("s"), Some(sid)) => {
            return Frame::Open(Open {
                sid,
                slug: text_of("dashboard").unwrap_or_default(),
                stream: text_of("stream").unwrap_or_default(),
                args: frame.get("args").cloned().unwrap_or(Value::Null),
                by: by_of(&frame),
            })
        }
        (Some("u"), Some(sid)) => return Frame::Close(sid),
        (Some("snap"), Some(sid)) => return Frame::Snap(sid),
        (Some("i"), Some(sid)) => {
            let data = text_of("data");
            let dim = |k: &str| frame.get(k).and_then(Value::as_u64);
            let kind = match (frame.get("kind").and_then(Value::as_str), data) {
                (Some("keys"), Some(data)) => InputKind::Keys(data),
                (Some("paste"), Some(data)) => InputKind::Paste(data),
                (Some("resize"), _) => match (dim("cols"), dim("rows")) {
                    (Some(cols), Some(rows)) => InputKind::Resize { cols, rows },
                    _ => InputKind::Bad,
                },
                _ => InputKind::Bad,
            };
            return Frame::Input(Input {
                sid,
                by: by_of(&frame),
                kind,
            });
        }
        (Some("s" | "u" | "snap" | "i"), None) => return Frame::Ignore,
        _ => {}
    }
    match frame.get("t").and_then(Value::as_str) {
        Some("x")
            if frame
                .get("id")
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 100)) =>
        {
            Frame::Action(frame.clone())
        }
        Some("hello") => {
            let server_time = frame.get("server_time").cloned().unwrap_or(Value::Null);
            log::info!(target: "dashboard_publish", "live link: hello, server time {server_time}");
            // SW-DP-23: the owner account (T1 `publishers.account_id`) the relay
            // announces; the only account whose input the Mac takes on this link.
            Frame::Hello(owner_account_of(text_of("owner_account")))
        }
        // A `q` with no id cannot be answered, so it is ignored.
        Some("q") => match frame.get("id") {
            Some(id) if !id.is_null() => {
                let by = by_of(&frame);
                Frame::Ask(Ask {
                    id: id.clone(),
                    slug: text_of("dashboard").unwrap_or_default(),
                    query: text_of("query").unwrap_or_default(),
                    args: frame.get("args").cloned().unwrap_or(Value::Null),
                    share: by.share,
                    role: by.role,
                    account: by.account,
                })
            }
            _ => Frame::Ignore,
        },
        _ => Frame::Ignore,
    }
}

/// Answers `q` frames for one link (SW-DP-5a).
struct Answerer {
    data_dir: PathBuf,
    catalog: &'static [QueryDef],
    /// The streams it opens (SW-DP-20).
    streams: &'static [StreamDef],
    ctx: LiveCtx,
    /// K20. A slot is freed when its query answers or times out.
    permits: Arc<tokio::sync::Semaphore>,
    /// The bound on blocking work, running or left behind by a timed-out query (review S4): a
    /// query takes a permit before it runs and its blocking work keeps it until it ends. A
    /// query that finds none is `busy` at once.
    blocking: Arc<tokio::sync::Semaphore>,
    /// The permit of the one hosted git read that may run on after its answer timed out.
    walks: Arc<tokio::sync::Semaphore>,
    /// K17 (shorter in some tests).
    timeout: Duration,
}

impl Answerer {
    /// The saved `live` block of `slug` and its folder, only while the Mac
    /// may serve it: the record is published, the manifest is published and
    /// its `live` block is the saved one. The one check behind queries
    /// (SW-DP-5 AC-1) and streams (SW-DP-20 AC-1).
    fn saved_block(&self, slug: &str) -> Option<(LiveApproved, PathBuf)> {
        if !manifest::valid_slug(slug) {
            return None;
        }
        let record = state::load(&self.data_dir, slug).ok()?;
        if record.state != LocalState::Published {
            return None;
        }
        let folder = PathBuf::from(record.folder.as_deref()?);
        let manifest = manifest::load_with(&folder, self.catalog).ok()?;
        if manifest.state != PublishState::Published {
            return None;
        }
        // The saved block equals the manifest's (queries, fields and
        // streams), because the sha matched.
        let block = approval::approved_block(&record, &manifest)?.clone();
        Some((block, folder))
    }

    /// The catalog entry for `ask`, only when the Mac may answer it (SW-DP-5
    /// AC-1): the saved block lists the query (or is `all`).
    fn approved(
        &self,
        ask: &Ask,
    ) -> Option<(
        &'static QueryDef,
        Vec<String>,
        PathBuf,
        Option<Scope>,
        manifest::LiveArgs,
    )> {
        let (block, folder) = self.saved_block(&ask.slug)?;
        if !block.queries.allows(&ask.query) {
            return None;
        }
        let def = catalog::find(self.catalog, &ask.query)?;
        // `media` serves what `live.fields.inbox` declares (its `media` field).
        let fields_of = if def.name == "media" { "inbox" } else { def.name };
        Some((
            def,
            catalog::effective_fields(def, block.fields.get(fields_of)),
            folder,
            block.scope,
            block.args,
        ))
    }

    /// The `STREAMS` entry for a stream of `slug`, only when the Mac may open
    /// it (SW-DP-20 AC-1): the saved block lists it in `live.streams`.
    fn stream_approved(
        &self,
        slug: &str,
        stream: &str,
    ) -> Option<(
        &'static StreamDef,
        PathBuf,
        Option<Scope>,
        bool,
        Vec<String>,
    )> {
        let (block, folder) = self.saved_block(slug)?;
        if !block.streams.iter().any(|s| s == stream) {
            return None;
        }
        let def = catalog::find_stream(self.streams, stream)?;
        let fields = block.fields.get(stream).cloned().unwrap_or_default();
        Some((def, folder, block.scope, block.control, fields))
    }

    /// Opens the stream an `s` frame asks for, when the saved block declares it
    /// (read on a blocking thread), or the P16 `so` error. The opener has K17.
    async fn open_stream(self: Arc<Self>, open: &Open) -> Result<Started, &'static str> {
        let me = self.clone();
        let (slug, name) = (open.slug.clone(), open.stream.clone());
        let approved = tokio::task::spawn_blocking(move || me.stream_approved(&slug, &name))
            .await
            .ok()
            .flatten();
        let Some((def, folder, scope, control, fields)) = approved else {
            return Err("not_declared");
        };
        let snap = Arc::new(tokio::sync::Notify::new());
        let target = InputTarget {
            folder: folder.clone(),
            scope: scope.clone(),
            gate: InputGate::new(control),
            handle: (def.name == "terminal")
                .then(|| {
                    open.args
                        .get("handle")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .flatten(),
        };
        let call = StreamCall {
            args: open.args.clone(),
            fields,
            folder,
            scope,
            snap: snap.clone(),
        };
        match tokio::time::timeout(self.timeout, (def.open)(self.ctx.clone(), call)).await {
            Err(_) => {
                log::info!(target: "dashboard_publish", "live stream {} did not open in time", def.name);
                Err("failed")
            }
            Ok(Ok(items)) => Ok((def, items, snap, target)),
            Ok(Err(QueryError::InvalidArgs(_))) => Err("invalid_args"),
            Ok(Err(QueryError::StaleCursor)) => Err(crate::context_query::STALE_CURSOR),
            Ok(Err(QueryError::Refused(error))) => Err(error),
            Ok(Err(QueryError::Busy)) => Err("busy"),
            Ok(Err(QueryError::NotApproved(e))) => {
                log::info!(target: "dashboard_publish", "live stream {} not declared: {e}", def.name);
                Err("not_declared")
            }
            Ok(Err(QueryError::Failed(e))) => {
                log::info!(target: "dashboard_publish", "live stream {} failed: {e}", def.name);
                Err("failed")
            }
        }
    }

    async fn action(&self, frame: Value, owner: Option<String>) -> String {
        use super::actions;
        let slug = frame["dashboard"]
            .as_str()
            .filter(|s| manifest::valid_slug(s))
            .unwrap_or("")
            .to_string();
        let name = frame["action"]
            .as_str()
            .filter(|s| actions::valid_id(s))
            .unwrap_or("")
            .to_string();
        let mut line = action_log_line(&frame, "pending");
        let lock = super::driver::slug_lock(&slug);
        let _turn = lock.lock().await;
        let mut result: Result<Value, &'static str> = async {
            let (block, folder) = self.saved_block(&slug).ok_or("not_declared")?;
            actions::authorize(&frame["by"], owner.as_deref())?;
            if name == "ownerReact" {
                return super::inbox::react_dispatch(&self.ctx, &folder, &block, &self.data_dir, &frame, &mut line).await;
            }
            if name == "ownerMarkRead" {
                return super::inbox::mark_read_dispatch(&self.ctx, &folder, &block, &self.data_dir, &frame, &mut line).await;
            }
            if name == "ownerMessage" || block.actions.get(&name).is_some_and(|a| a.verb == "sendMessage") {
                return super::inbox::dispatch(&self.ctx, &folder, &block, &self.data_dir, &frame, &mut line).await;
            }
            let action = block.actions.get(&name).ok_or("not_declared")?;
            let args = action.args(frame["args"].clone())?;
            line["args"] = frame["args"].clone();
            let app = self.ctx.app.as_ref().ok_or("failed")?;
            let dir = self.data_dir.clone();
            let slug_read = slug.clone();
            let rows = crate::commands::await_blocking("dashboard.actions.history", move || {
                actions::read_history(&dir, &slug_read)
            })
            .await
            .map_err(|_| "failed")?;
            let running = if action.verb == "launchAgent" {
                crate::mcp::session_list::read_session_list(app, None)
                    .await
                    .map_err(|_| "failed")?
                    .into_iter()
                    .filter(|r| r.state != crate::mcp::session_list::SessionState::Exited)
                    .map(|r| r.handle)
                    .collect()
            } else {
                Vec::new()
            };
            actions::cap(action, &name, &rows, &running, chrono::Utc::now())?;
            let dir = self.data_dir.clone();
            let reservation = line.clone();
            crate::commands::await_blocking("dashboard.actions.reserve", move || {
                save_action_log(&dir, &reservation)
            })
            .await
            .map_err(|_| "failed")?;
            actions::execute(app, &folder, block.scope.as_ref(), &slug, action, &args).await
        }
        .await;
        line["outcome"] = json!(result.as_ref().map(|_| "done").unwrap_or_else(|e| *e));
        if let Ok(value) = &result {
            for k in ["handle", "run_id"] {
                if let Some(v) = value.get(k) {
                    line[k] = v.clone();
                }
            }
        }
        let dir = self.data_dir.clone();
        if crate::commands::await_blocking("dashboard.actions.audit", move || {
            save_action_log(&dir, &line)
        })
        .await
        .is_err()
        {
            result = Err("failed");
        }
        xa_frame(&frame["id"], result)
    }

    /// The `a` frame for `ask`, and a line in the view log.
    async fn answer(&self, ask: Ask) -> String {
        let asked_at = std::time::Instant::now();
        let mut ran_from: Option<std::time::Instant> = None;
        let mut timed_out = false;
        let approved = self.approved(&ask);
        let pins_allowed = match &approved {
            Some((_, _, folder, scope, pins)) => {
                catalog::pins_allow(
                    &self.ctx,
                    pins,
                    &ask.query,
                    &ask.args,
                    folder,
                    scope.as_ref(),
                )
                .await
            }
            None => true,
        };
        let result = match approved {
            None => Err("not_approved"),
            Some((def, _, _, _, ref pins))
                if !pins_allowed
                    || (ask.role == "viewer"
                        && manifest::missing_pin(
                            &manifest::LiveQueries::List(vec![def.name.into()]),
                            pins,
                        )
                        .is_some()) =>
            {
                Err("arg_not_allowed")
            }
            Some((def, fields, folder, scope, _)) => {
                match tokio::time::timeout(QUEUE_TIMEOUT, self.permits.clone().acquire_owned())
                    .await
                {
                    // Waiting for a slot is not answer time: past the queue bound it is `busy`.
                    Err(_) => Err("busy"),
                    Ok(Err(_)) => Err("failed"),
                    Ok(Ok(slot)) => {
                        ran_from = Some(std::time::Instant::now());
                        // The slot is this answer's, not the work's: the work holds a blocking
                        // permit, so a timed-out query frees its slot at the timeout while
                        // its blocking work stays counted against the bound (SV-ENG-121).
                        let Ok(work_permit) =
                            tokio::sync::Semaphore::try_acquire_owned(self.blocking.clone())
                        else {
                            drop(slot);
                            let line = query_line(&ask, asked_at, ran_from, false, "busy");
                            log::info!(target: "dashboard_publish", "{line}");
                            log_view(&self.data_dir, &ask, "busy");
                            return json!({"t": "a", "id": ask.id, "ok": false, "error": "busy"})
                                .to_string();
                        };
                        let held = Arc::new(work_permit);
                        let run = async {
                            let call = Call {
                                args: ask.args.clone(),
                                fields,
                                folder,
                                scope,
                                hold: Some(held.clone()),
                                walks: Some(self.walks.clone()),
                                walk_deadline: Some(self.timeout * ABANDONED_WALK_ANSWERS),
                            };
                            (def.run)(self.ctx.clone(), call).await
                        };
                        let outcome = tokio::time::timeout(self.timeout, run).await;
                        timed_out = Result::is_err(&outcome);
                        drop(held);
                        drop(slot);
                        match outcome {
                            Err(_) => Err("failed"),
                            Ok(Err(QueryError::InvalidArgs(_))) => Err("invalid_args"),
                            Ok(Err(QueryError::StaleCursor)) => Err(crate::context_query::STALE_CURSOR),
                            Ok(Err(QueryError::NotApproved(e))) => {
                                log::info!(target: "dashboard_publish", "live query {} not approved: {e}", def.name);
                                Err("not_approved")
                            }
                            Ok(Err(QueryError::Failed(e))) => {
                                log::info!(target: "dashboard_publish", "live query {} failed: {e}", def.name);
                                Err("failed")
                            }
                            Ok(Err(QueryError::Busy)) => Err("busy"),
                            Ok(Err(QueryError::Refused(_))) => Err("failed"),
                            Ok(Ok(answer)) => {
                                let answer = if def.name == "media" {
                                    super::inbox::media::redact(answer)
                                } else {
                                    catalog::redact(answer)
                                };
                                let size = serde_json::to_vec(&answer)
                                    .map(|b| Vec::len(&b))
                                    .unwrap_or(usize::MAX);
                                if size > MAX_ANSWER_BYTES {
                                    Err("too_large")
                                } else {
                                    Ok(answer)
                                }
                            }
                        }
                    }
                }
            }
        };
        let outcome = match &result {
            Ok(_) => "ok",
            Err(e) => e,
        };
        log::info!(
            target: "dashboard_publish",
            "{}",
            query_line(&ask, asked_at, ran_from, timed_out, outcome)
        );
        log_view(&self.data_dir, &ask, outcome);
        match result {
            Ok(answer) => json!({"t": "a", "id": ask.id, "ok": true, "answer": answer}),
            Err(error) => json!({"t": "a", "id": ask.id, "ok": false, "error": error}),
        }
        .to_string()
    }
}

/// The hosted git reads that may run on after their answer timed out (SV-ENG-121).
pub(super) const MAX_ABANDONED_WALKS: usize = 1;
/// Such a read runs on for this many answer timeouts (120 s at K17) before its cancel flag is set.
pub(super) const ABANDONED_WALK_ANSWERS: u32 = 12;

/// The `only` arrays a `storyChanges` ask names, from the fixed set; no other argument is read.
fn only_arrays(args: &Value) -> String {
    const KNOWN: [&str; 8] = [
        "tasks",
        "sessions",
        "commits",
        "prs",
        "branches",
        "worktrees",
        "actual",
        "reviews",
    ];
    let Some(list) = Option::and_then(Value::get(args, "only"), Value::as_array) else {
        return "-".to_owned();
    };
    let named: Vec<&str> = Iterator::collect(Iterator::filter(
        Iterator::filter_map(<[Value]>::iter(list), Value::as_str),
        |name| <[&str]>::contains(&KNOWN, name),
    ));
    if Vec::is_empty(&named) {
        "-".to_owned()
    } else {
        <[&str]>::join(&named, ",")
    }
}

/// SV-ENG-121: the one line per hosted live query for the app log: slug, query, the `only`
/// arrays, the wait for a slot and the run in ms, and the outcome. No argument values, ids,
/// text or paths.
fn query_line(
    ask: &Ask,
    asked_at: std::time::Instant,
    ran_from: Option<std::time::Instant>,
    timed_out: bool,
    outcome: &str,
) -> String {
    let wait = Option::map_or_else(
        ran_from,
        || std::time::Instant::elapsed(&asked_at),
        |ran| std::time::Instant::duration_since(&ran, asked_at),
    );
    let run = Option::map_or(ran_from, std::time::Duration::ZERO, |ran| {
        std::time::Instant::elapsed(&ran)
    });
    let outcome = if timed_out {
        "timeout"
    } else if outcome == "invalid_args" {
        "invalid"
    } else {
        outcome
    };
    format!(
        "live query slug={} query={} only={} wait_ms={} run_ms={} outcome={}",
        clip(&ask.slug),
        clip(&ask.query),
        only_arrays(&ask.args),
        std::time::Duration::as_millis(&wait),
        std::time::Duration::as_millis(&run),
        outcome
    )
}

fn log_path(data_dir: &Path, file: &str) -> PathBuf {
    data_dir.join("dashboard-publish").join(file)
}

#[cfg(test)]
fn view_log_path(data_dir: &Path) -> PathBuf {
    log_path(data_dir, VIEW_LOG)
}

/// Appends (to either log) are made one at a time.
static VIEW_LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clip(text: &str) -> String {
    text.chars().take(VIEW_LOG_FIELD_MAX).collect()
}

/// SW-DP-5 AC-6: one line per answered or refused `q`. Args and answers are
/// never written.
fn log_view(data_dir: &Path, ask: &Ask, outcome: &str) {
    let by = By {
        share: ask.share.clone(),
        role: ask.role.clone(),
        account: ask.account.clone(),
    };
    log_line(data_dir, &ask.slug, ("query", &ask.query), &by, outcome);
}

/// SW-DP-20: one line per `s` frame, opened or refused, the same shape with
/// `stream` for `query`. Args are never written.
fn log_stream(data_dir: &Path, open: &Open, outcome: &str) {
    log_line(
        data_dir,
        &open.slug,
        ("stream", &open.stream),
        &open.by,
        outcome,
    );
}

fn log_line(data_dir: &Path, slug: &str, (key, name): (&str, &str), by: &By, outcome: &str) {
    let line = json!({
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "slug": clip(slug),
        key: clip(name),
        "share_id": by.share.as_deref().map(clip),
        // SW-DP-22 (AC-16): who asked, by role; the owner by account id, never an email.
        "role": by.role,
        "account_id": by.account.as_deref().map(clip),
        "outcome": outcome,
    });
    append_log(data_dir, VIEW_LOG, &line);
}

/// One JSON line appended to `file` (the view log or the control log).
pub(crate) fn append_log(data_dir: &Path, file: &str, line: &Value) {
    use std::io::Write;
    let path = log_path(data_dir, file);
    let _guard = VIEW_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let written = std::fs::create_dir_all(path.parent().expect("log path has a parent"))
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(e) = written {
        log::warn!(target: "dashboard_publish", "live link: {file} not written: {e}");
    }
}

/// The `xa` answer to an action frame; a result is redacted like an answer.
fn xa_frame(id: &Value, result: Result<Value, &'static str>) -> String {
    let frame = match result {
        Ok(result) => json!({"v": LINK_PROTOCOL, "t": "xa", "id": id, "ok": true, "result": catalog::redact(result)}),
        Err(error) => json!({"v": LINK_PROTOCOL, "t": "xa", "id": id, "ok": false, "error": error}),
    };
    ToString::to_string(&frame)
}

fn action_log_line(frame: &Value, outcome: &str) -> Value {
    let by = by_of(frame);
    let identity = if by.role == "owner" {
        json!({"role":"owner","account":by.account})
    } else {
        json!({"role":"viewer","share_id":by.share})
    };
    json!({"call_id":uuid::Uuid::new_v4().to_string(),"at":now_rfc3339(),"slug":frame["dashboard"].as_str().filter(|s| manifest::valid_slug(s)).unwrap_or(""),"action":frame["action"].as_str().filter(|s| super::actions::valid_id(s)).unwrap_or(""),"args":{},"by":identity,"outcome":outcome})
}
/// One record per call. Reserve before mutation; a crash/final-write failure keeps
/// the pending record charged to the caps instead of silently permitting more work.
pub(super) fn save_action_log(data_dir: &Path, line: &Value) -> Result<(), String> {
    let path = log_path(data_dir, super::actions::LOG);
    let _guard = VIEW_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let mut rows = text
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if let Some(old) = rows.iter_mut().find(|r| r["call_id"] == line["call_id"]) {
        *old = line.clone();
    } else {
        rows.push(line.clone());
    }
    let text = rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    crate::shared::fs_atomic::write_bytes_atomic(&path, text.as_bytes())
        .map_err(|e| e.to_string())?;
    super::actions::log_changed();
    Ok(())
}

/// SW-DP-5 AC-6: drop view-log lines older than 30 days (and any line that
/// cannot be read). Run at app start. SW-DP-23: the control log too.
pub fn prune_view_log(data_dir: &Path, now: chrono::DateTime<chrono::Utc>) {
    prune_log(data_dir, super::actions::LOG, now);
    prune_log(data_dir, VIEW_LOG, now);
    prune_log(data_dir, CONTROL_LOG, now);
}

/// Drops `file`'s lines older than [`VIEW_LOG_DAYS`] (and any it cannot read).
fn prune_log(data_dir: &Path, file: &str, now: chrono::DateTime<chrono::Utc>) {
    let path = log_path(data_dir, file);
    let _guard = VIEW_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let cutoff = now - chrono::Duration::days(VIEW_LOG_DAYS);
    let kept: String = text
        .lines()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("at").and_then(Value::as_str).map(String::from))
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
                .is_some_and(|at| at >= cutoff)
        })
        .map(|line| format!("{line}\n"))
        .collect();
    let tmp = path.with_extension("jsonl.tmp");
    if let Err(e) = std::fs::write(&tmp, kept).and_then(|()| std::fs::rename(&tmp, &path)) {
        log::warn!(target: "dashboard_publish", "live link: {file} not pruned: {e}");
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// SW-DP-23 (DP-D45): one refused `i` in the control log: who, which dashboard
/// and session, why. Never what was typed.
///
/// Two kinds of line, so the lines add up exactly to the refusals:
/// - a refusal written as it happens: one refusal, plus `suppressed: n` earlier
///   ones of the same key (reason, dashboard, session, account) left out since
///   its last line (1 + n refusals);
/// - a flushed line (`flushed: true`, on the minute tick or when the link ends):
///   `count: n` refusals of that key that were left out and are on no other
///   line (n refusals, none of them its own).
fn log_refusal(data_dir: &Path, target: &Refusal, line_kind: RefusalLine) {
    let mut line = json!({
        "at": now_rfc3339(),
        "slug": target.slug.as_deref().map(clip),
        "handle": target.handle.as_deref().map(clip),
        "account": target.account.as_deref().map(clip),
        "outcome": "refused",
        "reason": target.reason,
    });
    match line_kind {
        RefusalLine::Now { suppressed: 0 } => {}
        RefusalLine::Now { suppressed } => line["suppressed"] = json!(suppressed),
        RefusalLine::Flushed { count } => {
            line["flushed"] = json!(true);
            line["count"] = json!(count);
        }
    }
    append_log(data_dir, CONTROL_LOG, &line);
}

/// Which kind of control-log refusal line ([`log_refusal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusalLine {
    Now { suppressed: u64 },
    Flushed { count: u64 },
}

/// SW-DP-23 (DP-D45): a minute of control, per (dashboard, session, account):
/// how many input frames and bytes reached the terminal. Never what was typed.
fn log_control_minute(data_dir: &Path, counts: Vec<(ControlKey, ControlTally)>) {
    for ((slug, handle, account), (frames, bytes)) in counts {
        let line = json!({
            "at": now_rfc3339(),
            "slug": clip(&slug),
            "handle": clip(&handle),
            "account": clip(&account),
            "frames": frames,
            "bytes": bytes,
        });
        append_log(data_dir, CONTROL_LOG, &line);
    }
}

/// SW-DP-23: refusal lines per link, at most one per second per key (reason,
/// dashboard, session, account), so a hostile relay cannot fill the disk; what
/// was left out is counted on the key's next line, or on a flushed line on the
/// minute tick and when the link ends ([`log_refusal`] says how lines add up).
#[derive(Clone, Default)]
struct RefusalLog(Arc<std::sync::Mutex<HashMap<RefusalKey, Suppressed>>>);

/// (reason, slug, handle, account); an overflow key has no identity.
type RefusalKey = (&'static str, Option<String>, Option<String>, Option<String>);

/// The most keys with an identity one link keeps; past it, refusals of a new
/// identity are counted under their reason's overflow key (no identity).
const MAX_REFUSAL_KEYS: usize = 64;

/// One key's throttle: when its last line went, and how many were left out since.
struct Suppressed {
    at: Instant,
    left: u64,
}

impl RefusalLog {
    /// The key `refusal` counts under. Each field is clipped (as on its log
    /// line), so a relay cannot make the Mac hold long keys.
    fn key_of(refusal: &Refusal) -> RefusalKey {
        (
            refusal.reason,
            refusal.slug.as_deref().map(clip),
            refusal.handle.as_deref().map(clip),
            refusal.account.as_deref().map(clip),
        )
    }

    /// The key `refusal` counts under (its own, or its reason's overflow key) and
    /// `Some(left out since the key's last line)` when a line may be written now,
    /// else `None` (and it is counted).
    fn admit(&self, refusal: &Refusal, now: Instant) -> (RefusalKey, Option<u64>) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut key = Self::key_of(refusal);
        if !map.contains_key(&key) {
            let named = map
                .keys()
                .filter(|k| k.1.is_some() || k.2.is_some() || k.3.is_some())
                .count();
            if named >= MAX_REFUSAL_KEYS {
                // Keys with nothing pending and no line within the second can go.
                map.retain(|_, s| s.left > 0 || now.duration_since(s.at) < SECOND);
                let named = map
                    .keys()
                    .filter(|k| k.1.is_some() || k.2.is_some() || k.3.is_some())
                    .count();
                if named >= MAX_REFUSAL_KEYS {
                    key = (refusal.reason, None, None, None);
                }
            }
        }
        let admitted = match map.get_mut(&key) {
            Some(s) if now.duration_since(s.at) < SECOND => {
                s.left += 1;
                None
            }
            Some(s) => {
                s.at = now;
                Some(std::mem::take(&mut s.left))
            }
            None => {
                map.insert(key.clone(), Suppressed { at: now, left: 0 });
                Some(0)
            }
        };
        (key, admitted)
    }

    /// The refusals left out and on no line yet, per key: written on the minute
    /// tick and when the link ends, so none is lost when no later refusal of
    /// that key comes.
    fn take_pending(&self) -> Vec<(RefusalKey, u64)> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.iter_mut()
            .filter(|(_, s)| s.left > 0)
            .map(|(k, s)| (k.clone(), std::mem::take(&mut s.left)))
            .collect()
    }

    /// Writes the pending counts (`take_pending`) off the caller's thread, as
    /// flushed lines.
    fn flush(&self, data_dir: &Path) -> Option<tokio::task::JoinHandle<()>> {
        let pending = self.take_pending();
        if pending.is_empty() {
            return None;
        }
        let data_dir = data_dir.to_path_buf();
        Some(tokio::task::spawn_blocking(move || {
            for ((reason, slug, handle, account), count) in pending {
                let refusal = Refusal {
                    slug,
                    handle,
                    account,
                    reason,
                };
                log_refusal(&data_dir, &refusal, RefusalLine::Flushed { count });
            }
        }))
    }

    /// Writes `refusal`'s line off the caller's thread when admitted (under its
    /// overflow key, without its identity, when the link has too many keys).
    fn write(&self, data_dir: &Path, refusal: Refusal) -> Option<tokio::task::JoinHandle<()>> {
        let ((reason, slug, handle, account), admitted) = self.admit(&refusal, Instant::now());
        let suppressed = admitted?;
        let refusal = Refusal {
            slug,
            handle,
            account,
            reason,
        };
        let data_dir = data_dir.to_path_buf();
        Some(tokio::task::spawn_blocking(move || {
            log_refusal(&data_dir, &refusal, RefusalLine::Now { suppressed })
        }))
    }
}

/// A refused `i`, as the control log names it.
#[derive(Debug, Clone)]
struct Refusal {
    slug: Option<String>,
    handle: Option<String>,
    account: Option<String>,
    reason: &'static str,
}

// ---- Streams (SW-DP-20) ----

/// One stream the relay opened on this link.
struct OpenStream {
    slug: String,
    name: &'static str,
    /// Notified on `snap`.
    snap: Arc<tokio::sync::Notify>,
    task: tokio::task::AbortHandle,
    /// This open's number on the link: frames it queued are dropped once it is stopped.
    generation: u64,
    /// Passes in a row that found it not declared.
    misses: u8,
    /// SW-DP-23: where an owner's input for it may go.
    input: InputTarget,
}

/// What the Mac checks an owner's `i` against (SW-DP-23): the dashboard's
/// folder and saved team, whether its saved block allows owner control (set at
/// the open, refreshed by every 5 s pass), and the terminal's handle (a
/// `terminal` stream only).
#[derive(Debug, Clone)]
struct InputTarget {
    folder: PathBuf,
    scope: Option<Scope>,
    /// Shared with every job queued for this stream, so the input worker sees a
    /// `u`, an end or control turned off before it writes a job queued earlier.
    gate: Arc<InputGate>,
    handle: Option<String>,
}

/// Whether a stream still takes input (SW-DP-23): open on the link, and its
/// saved block allows owner control. The session loop changes it; the input
/// worker reads it right before each write or resize.
#[derive(Debug)]
struct InputGate {
    open: std::sync::atomic::AtomicBool,
    control: std::sync::atomic::AtomicBool,
}

impl InputGate {
    fn new(control: bool) -> Arc<InputGate> {
        Arc::new(InputGate {
            open: std::sync::atomic::AtomicBool::new(true),
            control: std::sync::atomic::AtomicBool::new(control),
        })
    }

    fn control(&self) -> bool {
        self.control.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn set_control(&self, on: bool) {
        self.control.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    fn close(&self) {
        self.open.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// The refusal reason for a job of this stream now, or none.
    fn refusal(&self) -> Option<&'static str> {
        if !self.open.load(std::sync::atomic::Ordering::SeqCst) {
            Some("not_open")
        } else if !self.control() {
            Some("control_off")
        } else {
            None
        }
    }
}

/// What a stream's pump sends: a frame of the stream open with this generation.
type Outgoing = (Option<u64>, String);

/// The session's stream table and the generations it stopped, whose frames still
/// waiting on the sender are not sent (DP-INV-5: no frame after `u`).
#[derive(Default)]
struct StreamTable {
    open: HashMap<String, OpenStream>,
    next_generation: u64,
    retired: std::collections::HashSet<u64>,
    /// The order generations were retired in, so the oldest are forgotten first.
    retired_order: VecDeque<u64>,
    /// Streams that ended on their own, by sid, whose last frames (`end`) may still be
    /// queued. A new `s` for that sid retires them, so the old `end` cannot end the new open.
    finished: HashMap<String, u64>,
    /// The order sids finished in (with the generation), so the oldest are forgotten
    /// first; an entry whose sid finished again since, or was stopped, is skipped.
    finished_order: VecDeque<(String, u64)>,
}

impl StreamTable {
    /// False for a frame queued by a stream stopped since: it is not sent.
    fn deliverable(&self, generation: Option<u64>) -> bool {
        generation.is_none_or(|g| !self.retired.contains(&g))
    }

    /// Removes and stops a stream; its queued frames are dropped from now on. A
    /// stream of that sid that ended on its own has its queued frames dropped too.
    fn stop(&mut self, sid: &str) -> Option<OpenStream> {
        if let Some(generation) = self.finished.remove(sid) {
            self.retire(generation);
        }
        let stream = self.open.remove(sid)?;
        stream.task.abort();
        // SW-DP-23: input queued for it is not written after this.
        stream.input.gate.close();
        self.retire(stream.generation);
        Some(stream)
    }

    /// A stream that ended on its own leaves the table; its queued frames still go.
    fn finish(&mut self, sid: &str) {
        if let Some(stream) = self.open.remove(sid) {
            stream.input.gate.close();
            // Bounded like `retired`, oldest first: an entry is only needed while its frames drain.
            while self.finished_order.len() >= 4096 {
                if let Some((oldest, generation)) = self.finished_order.pop_front() {
                    if self.finished.get(&oldest) == Some(&generation) {
                        self.finished.remove(&oldest);
                    }
                }
            }
            self.finished.insert(sid.to_string(), stream.generation);
            self.finished_order
                .push_back((sid.to_string(), stream.generation));
        }
    }

    fn retire(&mut self, generation: u64) {
        // Bounded: far more than the sender holds at once (MAX_PENDING), oldest first.
        if self.retired_order.len() >= 4096 {
            if let Some(oldest) = self.retired_order.pop_front() {
                self.retired.remove(&oldest);
            }
        }
        self.retired.insert(generation);
        self.retired_order.push_back(generation);
    }
}

/// One frame waiting to be sent for a stream.
#[derive(Debug)]
enum Out {
    /// The value and its JSON length.
    Value(Value, usize),
    Part {
        vt: String,
        /// `vt`'s JSON length, worked out once (the pacing reads it per output chunk).
        size: usize,
        part: usize,
        last: bool,
        cols: Option<u16>,
        rows: Option<u16>,
    },
    Bytes(String),
    /// P16 `rs` (SW-DP-21): the terminal's new size.
    Resized {
        cols: u16,
        rows: u16,
    },
    Gap,
    End(&'static str),
}

/// The K27 and K28 rules for one stream: frames go at most every 50 ms, at
/// most 256 KB in any second, no `data` or `vt` part over 64 KB. Output that
/// waits is coalesced; more than one second of it is dropped for a `gap` and a
/// fresh snapshot.
struct Pacer {
    sid: String,
    /// P16 `seq`: one per value, snapshot part and output frame.
    seq: u64,
    queue: VecDeque<Out>,
    /// Output bytes waiting in `queue`.
    queued: usize,
    /// After a gap, output is dropped until the fresh snapshot arrives (it holds it).
    awaiting_snapshot: bool,
    /// An overflow while a snapshot's parts were still queued: its gap and the
    /// fresh snapshot wait until that snapshot has gone, so it always completes.
    gap_after_parts: bool,
    /// A fresh snapshot is due: the pump notifies the stream.
    want_snapshot: bool,
    /// When each frame of the last second went, and its size.
    sent: VecDeque<(Instant, usize)>,
    last_frame: Option<Instant>,
}

const SECOND: Duration = Duration::from_secs(1);

/// How many bytes `c` takes inside a JSON string: K27 and K28 count what goes on
/// the wire, where a terminal's control bytes (ESC) become `\u001b`, six bytes.
pub(crate) fn json_char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// `text`'s length inside a JSON string.
pub(crate) fn json_len(text: &str) -> usize {
    text.chars().map(json_char_len).sum()
}

/// The byte index of the longest prefix of `text` whose JSON length is at most `max`.
fn json_cut(text: &str, max: usize) -> usize {
    let mut size = 0;
    for (i, c) in text.char_indices() {
        size += json_char_len(c);
        if size > max {
            return i;
        }
    }
    text.len()
}

/// `text` split at char boundaries into parts of at most `max` bytes as JSON
/// (one empty part for empty text).
fn split_parts(text: &str, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let cut = json_cut(rest, max).max(rest.chars().next().map_or(0, char::len_utf8));
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

impl Pacer {
    fn new(sid: String) -> Pacer {
        Pacer {
            sid,
            seq: 0,
            queue: VecDeque::new(),
            queued: 0,
            awaiting_snapshot: false,
            gap_after_parts: false,
            want_snapshot: false,
            sent: VecDeque::new(),
            last_frame: None,
        }
    }

    /// Queue one item from the stream. True when it ended the stream.
    fn take(&mut self, item: StreamItem, snap: &tokio::sync::Notify) -> bool {
        match item {
            StreamItem::Value(value) => {
                let size = value.to_string().len();
                if size > MAX_ANSWER_BYTES {
                    // K19, as for an answer: never sent.
                    self.queue.push_back(Out::End("too_large"));
                    return true;
                }
                // The newest value replaces one that has not gone yet.
                match self
                    .queue
                    .iter_mut()
                    .rev()
                    .find(|o| matches!(o, Out::Value(..)))
                {
                    Some(Out::Value(waiting, waiting_size)) => {
                        *waiting = value;
                        *waiting_size = size;
                    }
                    _ => self.queue.push_back(Out::Value(value, size)),
                }
            }
            StreamItem::Snapshot { vt, cols, rows } => {
                if json_len(&vt) > MAX_SNAPSHOT_BYTES {
                    // K30's ceiling for any stream: a larger snapshot is never sent.
                    self.queue.push_back(Out::End("too_large"));
                    return true;
                }
                if self.awaiting_snapshot {
                    // The gap's snapshot: everything before it is in it.
                    self.awaiting_snapshot = false;
                }
                // An older snapshot's parts that have not gone are superseded by this one.
                self.queue.retain(|o| !matches!(o, Out::Part { .. }));
                if std::mem::take(&mut self.gap_after_parts) {
                    // Its pending gap goes first, so watchers that were live take this one.
                    self.queue.push_back(Out::Gap);
                }
                let parts = split_parts(&vt, STREAM_FRAME_BYTES);
                let count = parts.len();
                for (part, vt) in parts.into_iter().enumerate() {
                    self.queue.push_back(Out::Part {
                        size: json_len(&vt),
                        vt,
                        part,
                        last: part + 1 == count,
                        cols,
                        rows,
                    });
                }
            }
            StreamItem::Bytes(data) => {
                if self.awaiting_snapshot || data.is_empty() {
                    return false;
                }
                // Output may wait behind a snapshot still going out, up to that snapshot's size
                // on top of a second's worth, so steady output under a long snapshot never gaps.
                // As sent: JSON length (an ESC is six bytes), like the pacing that drains it.
                let size = json_len(&data);
                let parts: usize = self
                    .queue
                    .iter()
                    .map(|o| match o {
                        Out::Part { size, .. } => *size,
                        _ => 0,
                    })
                    .sum();
                if self.queued + size > STREAM_BYTES_PER_SEC + parts {
                    // K28: more than a second of output is waiting. It is dropped,
                    // watchers are told, and a fresh snapshot follows.
                    // Resizes go with the output: the fresh snapshot carries the size.
                    self.queue
                        .retain(|o| !matches!(o, Out::Bytes(_) | Out::Resized { .. }));
                    self.queued = 0;
                    self.awaiting_snapshot = true;
                    if self.queue.iter().any(|o| matches!(o, Out::Part { .. })) {
                        // A snapshot is still going out: dropping it for a newer one could
                        // repeat for ever under steady output. It finishes first.
                        self.gap_after_parts = true;
                        return false;
                    }
                    self.queue.push_back(Out::Gap);
                    snap.notify_one();
                    return false;
                }
                self.queued += size;
                match self.queue.back_mut() {
                    Some(Out::Bytes(waiting)) => waiting.push_str(&data),
                    _ => self.queue.push_back(Out::Bytes(data)),
                }
            }
            StreamItem::Resized { cols, rows } => {
                // After a gap the fresh snapshot carries the size.
                if !self.awaiting_snapshot {
                    self.queue.push_back(Out::Resized { cols, rows });
                }
            }
            StreamItem::End(reason) => {
                self.queue.push_back(Out::End(reason));
                return true;
            }
        }
        false
    }

    /// The bytes the front frame needs from the one-second window.
    fn need(&self) -> Option<usize> {
        Some(match self.queue.front()? {
            Out::Value(_, size) => *size,
            Out::Part { size, .. } => *size,
            // At least its first character; it is cut to what the window allows.
            Out::Bytes(data) => data.chars().next().map_or(0, json_char_len),
            Out::Resized { .. } | Out::Gap | Out::End(_) => 0,
        })
    }

    fn used(&self, at: Instant) -> usize {
        self.sent
            .iter()
            .filter(|(when, _)| *when + SECOND > at)
            .map(|(_, n)| n)
            .sum()
    }

    /// When the next frame may go, if one is waiting.
    fn next_at(&self, now: Instant) -> Option<Instant> {
        let need = self.need()?;
        let mut at = self
            .last_frame
            .map_or(now, |last| last + STREAM_FRAME_EVERY)
            .max(now);
        while self.used(at) + need > STREAM_BYTES_PER_SEC {
            // Wait until the oldest frame still in the window leaves it.
            let oldest = self
                .sent
                .iter()
                .find(|(when, _)| *when + SECOND > at)
                .map(|(when, _)| *when)?;
            at = oldest + SECOND;
        }
        Some(at)
    }

    /// The next frame as P16 text, sent at `now`.
    fn send_next(&mut self, now: Instant) -> Option<String> {
        let allowed = STREAM_BYTES_PER_SEC.saturating_sub(self.used(now));
        let out = match self.queue.pop_front()? {
            Out::Bytes(mut data) => {
                let max = allowed.min(STREAM_FRAME_BYTES);
                if json_len(&data) > max {
                    let cut =
                        json_cut(&data, max).max(data.chars().next().map_or(0, char::len_utf8));
                    let rest = data.split_off(cut);
                    self.queue.push_front(Out::Bytes(rest));
                }
                self.queued = self.queued.saturating_sub(json_len(&data));
                Out::Bytes(data)
            }
            other => other,
        };
        let size = match &out {
            Out::Value(_, size) => *size,
            Out::Part { size, .. } => *size,
            Out::Bytes(data) => json_len(data),
            Out::Resized { .. } | Out::Gap | Out::End(_) => 0,
        };
        while self
            .sent
            .front()
            .is_some_and(|(when, _)| *when + SECOND <= now)
        {
            self.sent.pop_front();
        }
        self.sent.push_back((now, size));
        self.last_frame = Some(now);
        if self.gap_after_parts && !self.queue.iter().any(|o| matches!(o, Out::Part { .. })) {
            // The snapshot in progress has gone: now the gap, and a fresh one.
            self.gap_after_parts = false;
            self.queue.push_back(Out::Gap);
            self.want_snapshot = true;
        }
        let sid = &self.sid;
        let frame = match out {
            Out::Value(value, _) => {
                self.seq += 1;
                json!({"t": "v", "v": LINK_PROTOCOL, "sid": sid, "seq": self.seq, "value": value})
            }
            Out::Part {
                vt,
                part,
                last,
                cols,
                rows,
                ..
            } => {
                self.seq += 1;
                let mut frame = json!({"t": "snapshot", "v": LINK_PROTOCOL, "sid": sid, "seq": self.seq, "part": part, "last": last, "vt": vt});
                if let (Some(cols), Some(rows)) = (cols, rows) {
                    frame["cols"] = cols.into();
                    frame["rows"] = rows.into();
                }
                frame
            }
            Out::Bytes(data) => {
                self.seq += 1;
                json!({"t": "o", "v": LINK_PROTOCOL, "sid": sid, "seq": self.seq, "data": data})
            }
            Out::Resized { cols, rows } => {
                json!({"t": "rs", "v": LINK_PROTOCOL, "sid": sid, "cols": cols, "rows": rows})
            }
            Out::Gap => json!({"t": "o", "v": LINK_PROTOCOL, "sid": sid, "gap": true}),
            Out::End(reason) => {
                json!({"t": "end", "v": LINK_PROTOCOL, "sid": sid, "reason": reason})
            }
        };
        Some(frame.to_string())
    }
}

/// Runs one open stream: reads its items, paces them (K27, K28) and sends
/// its frames on the link's sender. Ends after its `end` frame, or when the
/// link is gone. Returns its sid.
async fn pump(
    generation: u64,
    sid: String,
    mut items: futures_util::stream::BoxStream<'static, StreamItem>,
    snap: Arc<tokio::sync::Notify>,
    tx: tokio::sync::mpsc::Sender<Outgoing>,
) -> String {
    let mut pacer = Pacer::new(sid);
    let mut open = true;
    loop {
        let wake = pacer.next_at(Instant::now());
        if !open && wake.is_none() {
            return pacer.sid;
        }
        let deadline = wake.unwrap_or_else(|| Instant::now() + SECOND);
        tokio::select! {
            item = items.next(), if open => {
                let item = item.unwrap_or(StreamItem::End("closed"));
                if pacer.take(item, &snap) {
                    open = false;
                }
            }
            _ = tokio::time::sleep_until(deadline), if wake.is_some() => {
                let frame = pacer.send_next(Instant::now());
                if std::mem::take(&mut pacer.want_snapshot) {
                    snap.notify_one();
                }
                if let Some(frame) = frame {
                    if tx.send((Some(generation), frame)).await.is_err() {
                        return pacer.sid;
                    }
                }
            }
        }
    }
}

/// The `so` answer to an `s` frame.
fn opened(sid: &str, error: Option<&str>) -> String {
    match error {
        None => json!({"t": "so", "v": LINK_PROTOCOL, "sid": sid, "ok": true}),
        Some(error) => {
            json!({"t": "so", "v": LINK_PROTOCOL, "sid": sid, "ok": false, "error": error})
        }
    }
    .to_string()
}

/// The link for one Mac: its records, its connector and the Keychain.
pub struct LiveLink<C: Connect> {
    data_dir: PathBuf,
    connector: C,
    store: Arc<dyn CredentialStore>,
    answerer: Arc<Answerer>,
}

/// Prune the view log, then run the app's link for ever, or return at once
/// when the build names no dashboard host (DP-INV-2).
pub async fn run(data_dir: &Path, app: Option<tauri::AppHandle>) {
    prune_view_log(data_dir, chrono::Utc::now());
    let Some(link) = LiveLink::open(data_dir, client::live_url()) else {
        log::info!(target: "dashboard_publish", "live link: no dashboard host in this build");
        return;
    };
    link.with_ctx(LiveCtx {
        app,
        ..Default::default()
    })
    .run()
    .await
}

impl LiveLink<WssConnector> {
    /// The app's link for `url` (`client::live_url()`), or `None` without one.
    pub fn open(data_dir: &Path, url: Option<Url>) -> Option<LiveLink<WssConnector>> {
        let connector = WssConnector::new(url?)?;
        let store: Arc<dyn CredentialStore> = Arc::from(default_store(TOKEN_SERVICE));
        Some(LiveLink::new(data_dir, connector, store))
    }
}

impl<C: Connect> LiveLink<C> {
    pub fn new(data_dir: &Path, connector: C, store: Arc<dyn CredentialStore>) -> LiveLink<C> {
        LiveLink {
            data_dir: data_dir.into(),
            connector,
            store,
            answerer: Arc::new(Answerer {
                data_dir: data_dir.into(),
                catalog: CATALOG,
                streams: STREAMS,
                ctx: LiveCtx::default(),
                permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
                blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
                walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
                timeout: ANSWER_TIMEOUT,
            }),
        }
    }

    /// The same link answering from `catalog` (the tests pass a fixture catalog).
    pub fn with_catalog(mut self, catalog: &'static [QueryDef]) -> LiveLink<C> {
        self.answerer = Arc::new(Answerer {
            data_dir: self.data_dir.clone(),
            catalog,
            streams: self.answerer.streams,
            ctx: self.answerer.ctx.clone(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        });
        self
    }

    /// The same link with the app context its queries need.
    pub fn with_ctx(mut self, ctx: LiveCtx) -> LiveLink<C> {
        self.answerer = Arc::new(Answerer {
            data_dir: self.data_dir.clone(),
            catalog: self.answerer.catalog,
            streams: self.answerer.streams,
            ctx,
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        });
        self
    }

    fn catalog(&self) -> &'static [QueryDef] {
        self.answerer.catalog
    }

    async fn read_token(&self) -> Result<Option<Secret>, String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.get(TOKEN_ACCOUNT))
            .await
            .map_err(|e| e.to_string())?
    }

    /// The connection loop. It never returns.
    pub async fn run(self) {
        let mut backoff = Backoff::default();
        // A token refused in this run. It is never dialled again, even when the
        // refusal could not be saved to the records (review C6).
        let mut refused_here: Option<String> = None;
        loop {
            if live_records(&self.data_dir, self.catalog()).is_empty() {
                sleep(PASS).await;
                continue;
            }
            let token = match self.read_token().await {
                Ok(Some(token)) => token,
                Ok(None) => {
                    log::info!(target: "dashboard_publish", "live link: no publisher token saved");
                    sleep(PASS).await;
                    continue;
                }
                Err(e) => {
                    log::warn!(target: "dashboard_publish", "live link: publisher token unavailable: {e}");
                    sleep(PASS).await;
                    continue;
                }
            };
            if refused_here.as_deref() == Some(fingerprint(Some(&token)).as_str())
                || refused(&self.data_dir, &token)
            {
                sleep(PASS).await;
                continue;
            }
            let ws = match self.connector.connect(&token).await {
                Ok(ws) => ws,
                Err(ConnectError::Unauthorized) => {
                    log::warn!(target: "dashboard_publish", "live link: the publisher token was refused");
                    refused_here = Some(fingerprint(Some(&token)));
                    if !refuse(&self.data_dir, &token).await {
                        log::warn!(target: "dashboard_publish", "live link: refusal not saved; the token is not dialled again in this run");
                    }
                    sleep(PASS).await;
                    continue;
                }
                Err(ConnectError::NotPublished) => {
                    sleep(BACKOFF_CAP).await;
                    continue;
                }
                Err(ConnectError::Failed) => {
                    sleep(backoff.next()).await;
                    continue;
                }
            };
            log::info!(target: "dashboard_publish", "live link: connected");
            let started = Instant::now();
            let end = self.session(ws).await;
            log::info!(target: "dashboard_publish", "live link: ended ({end:?})");
            if started.elapsed() >= BACKOFF_RESET_AFTER {
                backoff.reset();
            }
            match end {
                End::NotWanted => backoff.reset(),
                End::Closed(Some(CLOSE_REPLACED)) => sleep(BACKOFF_CAP).await,
                End::Closed(Some(CLOSE_REVOKED)) => {
                    refused_here = Some(fingerprint(Some(&token)));
                    refuse(&self.data_dir, &token).await;
                    sleep(PASS).await;
                }
                End::Closed(_) | End::HeartbeatLost | End::Dropped => sleep(backoff.next()).await,
            }
        }
    }

    /// Starts answering an `s` frame (SW-DP-20 AC-1) off the session loop: the
    /// saved block is read on a blocking thread and the stream's opener may read
    /// the app (SW-DP-21), so the link goes on meanwhile; the `so` goes when the
    /// open is done. A repeated `s` for a sid replaces that stream (and an open
    /// still in progress). Returns the `so` only for an open refused at once.
    fn begin_open(
        &self,
        open: Open,
        streams: &mut StreamTable,
        opens: &mut Opens,
    ) -> Option<String> {
        streams.stop(&open.sid);
        opens.cancel(&open.sid);
        if streams.open.len() + opens.pending.len() >= MAX_OPEN_STREAMS {
            let data_dir = self.data_dir.clone();
            let sid = open.sid.clone();
            tokio::task::spawn_blocking(move || log_stream(&data_dir, &open, "busy"));
            return Some(opened(&sid, Some("busy")));
        }
        opens.next += 1;
        let token = opens.next;
        let sid = open.sid.clone();
        let slug = open.slug.clone();
        let answerer = self.answerer.clone();
        let data_dir = self.data_dir.clone();
        let task = opens.set.spawn(async move {
            let result = answerer.open_stream(&open).await;
            let outcome = match &result {
                Ok(_) => "ok",
                Err(error) => error,
            };
            let logged = open.clone();
            let _ =
                tokio::task::spawn_blocking(move || log_stream(&data_dir, &logged, outcome)).await;
            Opened {
                token,
                open,
                result,
            }
        });
        opens.pending.insert(sid, (token, task, slug));
        None
    }

    /// An open that finished: its stream starts and the `so` is returned, unless a
    /// later `s` or a `u` for its sid came since (then it is dropped, unsent).
    fn finish_open(
        &self,
        done: Opened,
        streams: &mut StreamTable,
        opens: &mut Opens,
        tasks: &mut tokio::task::JoinSet<String>,
        tx: &tokio::sync::mpsc::Sender<Outgoing>,
    ) -> Option<String> {
        let sid = done.open.sid.clone();
        if opens.pending.get(&sid).map(|(token, _, _)| *token) != Some(done.token) {
            return None;
        }
        opens.pending.remove(&sid);
        let (def, items, snap, input) = match done.result {
            Ok(started) => started,
            Err(error) => return Some(opened(&sid, Some(error))),
        };
        streams.next_generation += 1;
        let generation = streams.next_generation;
        let task = tasks.spawn(pump(
            generation,
            sid.clone(),
            items,
            snap.clone(),
            tx.clone(),
        ));
        streams.open.insert(
            sid.clone(),
            OpenStream {
                slug: done.open.slug,
                name: def.name,
                snap,
                task,
                generation,
                misses: 0,
                input,
            },
        );
        Some(opened(&sid, None))
    }

    /// One connected socket, until it ends. SW-DP-23: control counted on it is
    /// written to the control log when it ends, as at each minute.
    async fn session<S>(&self, ws: WebSocketStream<S>) -> End
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let counts = ControlCounts::default();
        let refusals = RefusalLog::default();
        let mut workers = tokio::task::JoinSet::new();
        let end = self
            .session_inner(ws, &counts, &refusals, &mut workers)
            .await;
        // The input worker stops (and its refusal writes end) before the last flush,
        // so no refusal it counts is left after the flush has taken the counts.
        workers.shutdown().await;
        // Refusals left out since their key's last line are not lost with the link.
        if let Some(flushed) = refusals.flush(&self.data_dir) {
            let _ = flushed.await;
        }
        let taken = counts.take();
        if !taken.is_empty() {
            let data_dir = self.data_dir.clone();
            let _ = tokio::task::spawn_blocking(move || log_control_minute(&data_dir, taken)).await;
        }
        end
    }

    /// SW-DP-23: the Mac's own checks on an owner's `i` (DP-D42: it trusts the
    /// relay's `by` and nothing else, never the page): the owner the relay
    /// announced in `hello`, a `terminal` stream open on this link, a saved block
    /// with `control: owner`, a valid kind within K38, and K39 for the link. The
    /// team filter runs in the link's input worker. Cheap: no disk, no await.
    fn check_input(
        &self,
        input: Input,
        owner: Option<&str>,
        streams: &StreamTable,
        rate: &mut InputRate,
    ) -> Result<InputJob, (Refusal, &'static str)> {
        let stream = streams.open.get(&input.sid);
        let refuse = |reason: &'static str, error: &'static str| {
            (
                Refusal {
                    slug: stream.map(|s| s.slug.clone()),
                    handle: stream.and_then(|s| s.input.handle.clone()),
                    account: input.by.account.clone(),
                    reason,
                },
                error,
            )
        };
        let account = match (owner, input.by.account.as_deref()) {
            (Some(owner), Some(account)) if input.by.role == "owner" && account == owner => {
                account.to_string()
            }
            _ => return Err(refuse("not_owner", "refused")),
        };
        let Some(stream) = stream else {
            return Err(refuse("not_open", "refused"));
        };
        let Some(handle) = stream
            .input
            .handle
            .clone()
            .filter(|_| stream.name == "terminal")
        else {
            return Err(refuse("not_terminal", "refused"));
        };
        if !stream.input.gate.control() {
            return Err(refuse("control_off", "refused"));
        }
        let kind = match input.kind {
            InputKind::Bad => return Err(refuse("bad_input", "refused")),
            // Nothing to type: the relay and the shell refuse it too.
            InputKind::Keys(d) if d.is_empty() => {
                return Err(refuse("bad_input", "refused"));
            }
            // The size first: the marker scan below then only ever reads K38 bytes.
            InputKind::Keys(d) | InputKind::Paste(d) if d.len() > MAX_INPUT_BYTES => {
                return Err(refuse("too_large", "too_large"));
            }
            // A paste of nothing but bracketed-paste markers types nothing either.
            InputKind::Paste(d) if crate::commands::pty::bracketed_paste(&d).is_empty() => {
                return Err(refuse("bad_input", "refused"));
            }
            InputKind::Resize { cols, rows }
                if !(1..=u64::from(MAX_INPUT_COLS)).contains(&cols)
                    || !(1..=u64::from(MAX_INPUT_ROWS)).contains(&rows) =>
            {
                return Err(refuse("bad_input", "refused"));
            }
            kind => kind,
        };
        if !rate.take(Instant::now(), kind.bytes()) {
            return Err(refuse("rate_limited", "rate_limited"));
        }
        Ok(InputJob {
            sid: input.sid,
            slug: stream.slug.clone(),
            handle,
            account,
            target: stream.input.clone(),
            kind,
        })
    }

    async fn session_inner<S>(
        &self,
        mut ws: WebSocketStream<S>,
        counts: &ControlCounts,
        refusals: &RefusalLog,
        workers: &mut tokio::task::JoinSet<()>,
    ) -> End
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        // No `pong` within 2 x K14 of the last one drops the link, on its own
        // deadline rather than at the next ping (review C2).
        let pong_deadline = sleep(HEARTBEAT * 2);
        tokio::pin!(pong_deadline);
        let mut beat = interval(HEARTBEAT);
        beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut pass = interval(PASS);
        pass.set_missed_tick_behavior(MissedTickBehavior::Delay);
        pass.tick().await;
        // Answers and stream frames come back from their tasks on two channels, so
        // a query's answer never waits behind stream frames (SW-DP-20 follow-up).
        // The tasks end with the session: dropping a set aborts any still running,
        // so a dropped link stops every stream at once (K32).
        let (answers_tx, mut answers) = tokio::sync::mpsc::channel::<String>(MAX_PENDING);
        let (frames_tx, mut frames) = tokio::sync::mpsc::channel::<Outgoing>(MAX_PENDING);
        let mut asks = tokio::task::JoinSet::new();
        let mut streams = StreamTable::default();
        let mut opens = Opens::default();
        let mut stream_tasks: tokio::task::JoinSet<String> = tokio::task::JoinSet::new();
        // The 5 s pass reads the records off the loop (SW-DP-20 follow-up); one at a time.
        let mut passes: tokio::task::JoinSet<PassRead> = tokio::task::JoinSet::new();
        // SW-DP-23: the owner the relay names in this link's `hello` (none until then:
        // every `i` is refused), K39 for this link, and its one input worker.
        let mut owner_account: Option<String> = None;
        let mut rate = InputRate::default();
        let (input_tx, input_rx) = tokio::sync::mpsc::channel::<InputJob>(INPUT_QUEUE);
        workers.spawn(input_worker(
            self.answerer.ctx.clone(),
            self.data_dir.clone(),
            input_rx,
            answers_tx.clone(),
            counts.clone(),
            refusals.clone(),
        ));
        let mut minute = interval(CONTROL_LOG_EVERY);
        minute.set_missed_tick_behavior(MissedTickBehavior::Delay);
        minute.tick().await;
        let mut turned_off = turn_off_signal().subscribe();
        loop {
            tokio::select! {
                // Frames first, so a close the relay sent is read before a ping is written.
                biased;
                message = ws.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        if text.as_str() == "pong" {
                            pong_deadline.as_mut().reset(Instant::now() + HEARTBEAT * 2);
                            continue;
                        }
                        match read_frame(text.as_str()) {
                            Frame::Ask(ask) => {
                                if asks.len() >= MAX_PENDING {
                                    // Too many at once: refused now, not queued.
                                    let busy = json!({"t": "a", "id": ask.id, "ok": false, "error": "busy"});
                                    let data_dir = self.data_dir.clone();
                                    tokio::task::spawn_blocking(move || log_view(&data_dir, &ask, "busy"));
                                    if ws.send(Message::text(busy.to_string())).await.is_err() {
                                        return End::Dropped;
                                    }
                                    continue;
                                }
                                let answerer = self.answerer.clone();
                                let tx = answers_tx.clone();
                                asks.spawn(async move {
                                    let _ = tx.send(answerer.answer(ask).await).await;
                                });
                            }
                            Frame::Action(frame) => {
                                let answerer = self.answerer.clone();
                                let tx = answers_tx.clone();
                                let owner = owner_account.clone();
                                // The action task finishes its audit even when its socket disconnects.
                                if let Ok(permit) = answerer.permits.clone().try_acquire_owned() {
                                    tokio::spawn(async move { let _permit = permit; let _ = tx.send(answerer.action(frame, owner).await).await; });
                                } else {
                                    let dir=self.data_dir.clone(); let line=action_log_line(&frame,"busy");
                                    let _=crate::commands::await_blocking("dashboard.actions.busy",move || save_action_log(&dir,&line)).await;
                                    let reply = json!({"v": LINK_PROTOCOL, "t": "xa", "id": frame["id"], "ok": false, "error": "busy"});
                                    if ws.send(Message::text(reply.to_string())).await.is_err() { return End::Dropped; }
                                }
                            }
                            Frame::Open(open) => {
                                if let Some(reply) = self.begin_open(open, &mut streams, &mut opens) {
                                    if ws.send(Message::text(reply)).await.is_err() {
                                        return End::Dropped;
                                    }
                                }
                            }
                            Frame::Close(sid) => {
                                opens.cancel(&sid);
                                streams.stop(&sid);
                            }
                            Frame::Snap(sid) => {
                                if let Some(stream) = streams.open.get(&sid) {
                                    stream.snap.notify_one();
                                }
                            }
                            Frame::Hello(owner) => owner_account = owner,
                            Frame::BadInput(sid) => {
                                let stream = streams.open.get(&sid);
                                refusals.write(&self.data_dir, Refusal {
                                    slug: stream.map(|s| s.slug.clone()),
                                    handle: stream.and_then(|s| s.input.handle.clone()),
                                    account: None,
                                    reason: "bad_input",
                                });
                                if ws.send(Message::text(input_refused(&sid, "refused"))).await.is_err() {
                                    return End::Dropped;
                                }
                            }
                            Frame::Input(input) => {
                                let sid = input.sid.clone();
                                let job = self.check_input(input, owner_account.as_deref(), &streams, &mut rate);
                                let refused = match job {
                                    Ok(job) => enqueue_input(&mut rate, &input_tx, job),
                                    Err(refused) => Some(refused),
                                };
                                if let Some((refusal, error)) = refused {
                                    // At most one line a second per reason (RefusalLog); every `ir` still goes.
                                    refusals.write(&self.data_dir, refusal);
                                    if ws.send(Message::text(input_refused(&sid, error))).await.is_err() {
                                        return End::Dropped;
                                    }
                                }
                            }
                            Frame::Ignore => {}
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return End::Closed(frame.map(|f| u16::from(f.code)));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return End::Dropped,
                },
                _ = &mut pong_deadline => return End::HeartbeatLost,
                // The timers before the channels: busy streams keep them always ready, and the
                // ping (and its pong) must still go, or the link drops as lost.
                // SV-PBF-286 B5: a person turned live data off. Its streams end and its
                // control closes now, not after the two-pass debounce.
                off = turned_off.recv() => {
                    let slugs: std::collections::BTreeSet<String> = match off {
                        Ok(slug) => std::iter::once(slug).collect(),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            streams
                                .open
                                .values()
                                .map(|s| s.slug.clone())
                                .chain(opens.pending.values().map(|(_, _, slug)| slug.clone()))
                                .collect()
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => continue,
                    };
                    let mut ended: Vec<String> = Vec::new();
                    for (sid, s) in streams.open.iter_mut() {
                        if slugs.contains(&s.slug) {
                            s.input.gate.set_control(false);
                            ended.push(sid.clone());
                        }
                    }
                    for sid in ended {
                        streams.stop(&sid);
                        let end = json!({"t": "end", "v": LINK_PROTOCOL, "sid": sid, "reason": "not_declared"});
                        if ws.send(Message::text(end.to_string())).await.is_err() {
                            return End::Dropped;
                        }
                    }
                    // Opens that passed their check just before the turn off end too.
                    for sid in opens.stop_slugs(&slugs) {
                        if ws.send(Message::text(opened(&sid, Some("not_declared")))).await.is_err() {
                            return End::Dropped;
                        }
                    }
                }
                _ = beat.tick() => {
                    if ws.send(Message::text("ping")).await.is_err() {
                        return End::Dropped;
                    }
                }
                _ = minute.tick() => {
                    // Refusal counts left out with no later line of their reason.
                    refusals.flush(&self.data_dir);
                    // DP-D45: one line per controlled session per minute of control.
                    let taken = counts.take();
                    if !taken.is_empty() {
                        let data_dir = self.data_dir.clone();
                        tokio::task::spawn_blocking(move || log_control_minute(&data_dir, taken));
                    }
                }
                _ = pass.tick() => {
                    if passes.is_empty() {
                        let slugs: std::collections::BTreeSet<String> =
                            streams.open.values().map(|s| s.slug.clone()).collect();
                        let answerer = self.answerer.clone();
                        let data_dir = self.data_dir.clone();
                        let catalog = self.catalog();
                        passes.spawn_blocking(move || PassRead {
                            // The block is read once per dashboard per pass, not once per stream.
                            declared: slugs
                                .into_iter()
                                .map(|slug| {
                                    let names = answerer.saved_block(&slug).map(|(block, _)| (block.streams, block.control));
                                    (slug, names)
                                })
                                .collect(),
                            wanted: !live_records(&data_dir, catalog).is_empty(),
                        });
                    }
                }
                Some(read) = passes.join_next(), if !passes.is_empty() => {
                    let Ok(read) = read else { continue };
                    // A stream the saved block no longer declares ends, at the second pass in a row
                    // that finds it so: one manifest read caught mid-save does not end it. A stream
                    // opened after the read began is left to the next pass.
                    let mut undeclared: Vec<String> = Vec::new();
                    for (sid, s) in streams.open.iter_mut() {
                        let Some(names) = read.declared.get(&s.slug) else { continue };
                        // SW-DP-23: owner control follows the saved block within a pass.
                        s.input.gate.set_control(names.as_ref().is_some_and(|(_, control)| *control));
                        if names.as_ref().is_some_and(|(names, _)| names.iter().any(|n| n == s.name)) {
                            s.misses = 0;
                            continue;
                        }
                        s.misses += 1;
                        if s.misses >= 2 {
                            undeclared.push(sid.clone());
                        }
                    }
                    for sid in undeclared {
                        streams.stop(&sid);
                        let end = json!({"t": "end", "v": LINK_PROTOCOL, "sid": sid, "reason": "not_declared"});
                        if ws.send(Message::text(end.to_string())).await.is_err() {
                            return End::Dropped;
                        }
                    }
                    if !read.wanted {
                        let _ = ws
                            .close(Some(CloseFrame {
                                code: CloseCode::Normal,
                                reason: "".into(),
                            }))
                            .await;
                        return End::NotWanted;
                    }
                }
                Some(_) = asks.join_next(), if !asks.is_empty() => {}
                Some(done) = opens.set.join_next_with_id(), if !opens.set.is_empty() => {
                    let reply = match done {
                        Ok((_, done)) => self.finish_open(done, &mut streams, &mut opens, &mut stream_tasks, &frames_tx),
                        Err(e) if e.is_cancelled() => None,
                        // The opener panicked: its sid is answered `failed`.
                        Err(e) => opens.failed(e.id()).map(|sid| opened(&sid, Some("failed"))),
                    };
                    if let Some(reply) = reply {
                        if ws.send(Message::text(reply)).await.is_err() {
                            return End::Dropped;
                        }
                    }
                }
                Some(done) = stream_tasks.join_next_with_id(), if !stream_tasks.is_empty() => {
                    // A stream that ended on its own leaves the table (not one already
                    // replaced under the same sid). One whose task failed is ended too.
                    let (id, failed) = match done {
                        Ok((id, _)) => (id, false),
                        Err(e) if e.is_cancelled() => continue,
                        Err(e) => (e.id(), true),
                    };
                    let Some(sid) = streams
                        .open
                        .iter()
                        .find(|(_, s)| s.task.id() == id)
                        .map(|(sid, _)| sid.clone())
                    else {
                        continue;
                    };
                    if failed {
                        // Retired before its `end`: frames it had queued are not sent after it.
                        streams.stop(&sid);
                    } else {
                        streams.finish(&sid);
                    }
                    if failed {
                        log::warn!(target: "dashboard_publish", "live stream task failed");
                        // `failed`, not `closed`: the page can tell a fault here from a normal end.
                        let end = json!({"t": "end", "v": LINK_PROTOCOL, "sid": sid, "reason": "failed"});
                        if ws.send(Message::text(end.to_string())).await.is_err() {
                            return End::Dropped;
                        }
                    }
                }
                // Answers before stream frames: a query never waits behind a busy stream.
                Some(answer) = answers.recv() => {
                    if ws.send(Message::text(answer)).await.is_err() {
                        return End::Dropped;
                    }
                }
                // The frames last: tasks that finished are reaped first, so busy streams do not
                // leave done asks counted against MAX_PENDING or hold back a failed stream's `end`.
                Some((generation, frame)) = frames.recv() => {
                    // A frame of a stream stopped since it was queued is not sent.
                    if !streams.deliverable(generation) {
                        continue;
                    }
                    if ws.send(Message::text(frame)).await.is_err() {
                        return End::Dropped;
                    }
                }
            }
        }
    }
}

/// What one 5 s pass read of the records: each open stream's dashboard's saved
/// streams (`None`: no longer served), and whether any dashboard still wants the link.
struct PassRead {
    /// Per dashboard: its saved streams and whether it allows owner control.
    declared: HashMap<String, Option<(Vec<String>, bool)>>,
    wanted: bool,
}

/// A finished open: the stream's entry, its items and its `snap`, or the `so` error.
type Started = (
    &'static StreamDef,
    catalog::Items,
    Arc<tokio::sync::Notify>,
    InputTarget,
);

struct Opened {
    token: u64,
    open: Open,
    result: Result<Started, &'static str>,
}

/// The link's one input worker (SW-DP-23): takes accepted `i` frames in the
/// order they came, checks the session still passes SW-DP-21's team filter,
/// and types or resizes through the app's own path ([`AgentHost::write`],
/// [`AgentHost::resize`]). A refusal or a failed write is answered `ir`.
async fn input_worker(
    ctx: LiveCtx,
    data_dir: PathBuf,
    mut jobs: tokio::sync::mpsc::Receiver<InputJob>,
    replies: tokio::sync::mpsc::Sender<String>,
    counts: ControlCounts,
    refusals: RefusalLog,
) {
    // Per sid: the session that last passed the team filter, and when.
    let mut passed: HashMap<String, (String, Instant)> = HashMap::new();
    while let Some(job) = jobs.recv().await {
        let refusal = |reason: &'static str| Refusal {
            slug: Some(job.slug.clone()),
            handle: Some(job.handle.clone()),
            account: Some(job.account.clone()),
            reason,
        };
        let outcome: Result<(), (&'static str, &'static str)> = async {
            let host = super::agents::host_of(&ctx).map_err(|_| ("failed", "failed"))?;
            let fresh = passed
                .get(&job.sid)
                .filter(|(session, at)| {
                    at.elapsed() < INPUT_CHECK_FRESH
                        && host.resolve(&job.handle).as_deref() == Some(session.as_str())
                })
                .map(|(session, _)| session.clone());
            let session = match fresh {
                Some(session) => session,
                None => {
                    passed.remove(&job.sid);
                    let session = super::agents::input_session(
                        &host,
                        &job.target.folder,
                        job.target.scope.as_ref(),
                        &job.handle,
                    )
                    .await
                    .map_err(|_| ("not_in_team", "refused"))?;
                    if passed.len() >= MAX_OPEN_STREAMS {
                        passed.clear();
                    }
                    passed.insert(job.sid.clone(), (session.clone(), Instant::now()));
                    session
                }
            };
            // Right before the write: a `u`, an end or control turned off since this
            // job was queued refuses it (and every job queued behind it).
            if let Some(reason) = job.target.gate.refusal() {
                return Err((reason, "refused"));
            }
            let bytes = job.kind.bytes() as u64;
            // Not bounded by a timeout on purpose: the write runs on a blocking thread that cannot be
            // cancelled, so giving up on it would let later input overtake it (or land twice after a
            // "failed"). A write that hangs holds the link's queue; frames past its 256 slots are refused
            // rate_limited, which the owner sees.
            let written = match &job.kind {
                InputKind::Keys(data) => host.write(session, data.clone()).await,
                InputKind::Paste(data) => {
                    host.write(session, crate::commands::pty::bracketed_paste(data))
                        .await
                }
                InputKind::Resize { cols, rows } => {
                    // Checked within MAX_INPUT_COLS and MAX_INPUT_ROWS, so they fit.
                    host.resize(session, *cols as u16, *rows as u16).await
                }
                InputKind::Bad => Err(String::new()),
            };
            // The error text is not logged: a failed write's error never carries the
            // bytes, but nothing here risks it.
            written.map_err(|_| ("write_failed", "failed"))?;
            // DP-D45: keystroke frames and bytes. A resize types nothing, so it is
            // not counted (it is still refused and logged like any other input).
            if matches!(job.kind, InputKind::Keys(_) | InputKind::Paste(_)) {
                counts.add(&job.slug, &job.handle, &job.account, bytes);
            }
            Ok(())
        }
        .await;
        if let Err((reason, error)) = outcome {
            if let Some(written) = refusals.write(&data_dir, refusal(reason)) {
                let _ = written.await;
            }
            if replies.send(input_refused(&job.sid, error)).await.is_err() {
                return;
            }
        }
    }
}

/// The `ir` refusal of an `i` (P16, SW-DP-23).
fn input_refused(sid: &str, error: &str) -> String {
    json!({"t": "ir", "v": LINK_PROTOCOL, "sid": sid, "error": error}).to_string()
}

/// An accepted `i`, for the link's input worker.
struct InputJob {
    sid: String,
    slug: String,
    handle: String,
    account: String,
    target: InputTarget,
    kind: InputKind,
}

/// K39 for one link: the frames and bytes of the last second.
#[derive(Default)]
struct InputRate {
    recent: VecDeque<(Instant, usize)>,
    bytes: usize,
}

/// Hands an accepted `i` to the link's input worker. A full queue refuses it
/// (`rate_limited`) and gives its K39 slot back, so a frame that never goes
/// costs none of the second's budget.
fn enqueue_input(
    rate: &mut InputRate,
    tx: &tokio::sync::mpsc::Sender<InputJob>,
    job: InputJob,
) -> Option<(Refusal, &'static str)> {
    let refusal = |job: InputJob, reason: &'static str| Refusal {
        slug: Some(job.slug),
        handle: Some(job.handle),
        account: Some(job.account),
        reason,
    };
    match tx.try_send(job) {
        Ok(()) => None,
        Err(tokio::sync::mpsc::error::TrySendError::Full(job)) => {
            rate.give_back(job.kind.bytes());
            Some((refusal(job, "rate_limited"), "rate_limited"))
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(job)) => {
            rate.give_back(job.kind.bytes());
            Some((refusal(job, "write_failed"), "failed"))
        }
    }
}

impl InputRate {
    /// Undoes the last `take` (of `bytes`): that frame did not go.
    fn give_back(&mut self, bytes: usize) {
        if self.recent.back().is_some_and(|&(_, n)| n == bytes) {
            self.recent.pop_back();
            self.bytes -= bytes;
        }
    }

    /// Counts a frame of `bytes` at `now`, or refuses it (counted nowhere) when
    /// the second already holds K39's frames or bytes.
    fn take(&mut self, now: Instant, bytes: usize) -> bool {
        while let Some(&(at, n)) = self.recent.front() {
            if now.duration_since(at) < SECOND {
                break;
            }
            self.recent.pop_front();
            self.bytes -= n;
        }
        if self.recent.len() >= INPUT_FRAMES_PER_SEC || self.bytes + bytes > INPUT_BYTES_PER_SEC {
            return false;
        }
        self.recent.push_back((now, bytes));
        self.bytes += bytes;
        true
    }
}

/// Input that reached a terminal since the last control-log line, per
/// (dashboard, session handle, account): frames and bytes (DP-D45).
#[derive(Clone, Default)]
struct ControlCounts(Arc<std::sync::Mutex<HashMap<ControlKey, ControlTally>>>);

/// (dashboard slug, session handle, owner account).
type ControlKey = (String, String, String);
/// (input frames, input bytes).
type ControlTally = (u64, u64);

impl ControlCounts {
    fn add(&self, slug: &str, handle: &str, account: &str, bytes: u64) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .entry((slug.to_string(), handle.to_string(), account.to_string()))
            .or_default();
        entry.0 += 1;
        entry.1 += bytes;
    }

    fn take(&self) -> Vec<(ControlKey, ControlTally)> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *map).into_iter().collect()
    }
}

/// The opens in progress on one link, by sid, with the token of the latest one.
#[derive(Default)]
struct Opens {
    set: tokio::task::JoinSet<Opened>,
    pending: HashMap<String, (u64, tokio::task::AbortHandle, String)>,
    next: u64,
}

impl Opens {
    /// A `u` or a repeated `s`: the open in progress for `sid` is dropped.
    fn cancel(&mut self, sid: &str) {
        if let Some((_, task, _)) = self.pending.remove(sid) {
            task.abort();
        }
    }

    /// SV-PBF-286 L2: a person turned live data off for `slugs`. Their opens
    /// still in progress are dropped; the sids are returned so each gets its `so`.
    fn stop_slugs(&mut self, slugs: &std::collections::BTreeSet<String>) -> Vec<String> {
        let sids: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, (_, _, slug))| slugs.contains(slug))
            .map(|(sid, _)| sid.clone())
            .collect();
        for sid in &sids {
            self.cancel(sid);
        }
        sids
    }

    /// The sid whose open task `id` failed, which is no longer pending.
    fn failed(&mut self, id: tokio::task::Id) -> Option<String> {
        let sid = self
            .pending
            .iter()
            .find(|(_, (_, task, _))| task.id() == id)
            .map(|(sid, _)| sid.clone())?;
        self.pending.remove(&sid);
        Some(sid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::credential_store::FakeStore;
    use crate::dashboard_publish::manifest::{LiveQueries, MANIFEST_FILE};
    use crate::dashboard_publish::push::tests::{token_store, TOKEN};
    use crate::dashboard_publish::state::{Event, LiveApproved};
    use std::fs;
    use std::sync::Mutex;
    use tokio::io::DuplexStream;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::handshake::server::{
        ErrorResponse, Request as ServerRequest, Response,
    };
    use tokio_tungstenite::tungstenite::http;

    /// How the in-memory relay treats the next upgrade.
    #[derive(Debug, Clone)]
    enum Mode {
        /// Accept, send `hello`, answer every `ping` with `pong`.
        Answer,
        /// Accept and send `hello`, then never answer.
        Silent,
        /// Answer the first `ping` one second late, then never again.
        PongOnceLate,
        /// Refuse the upgrade with this status and body.
        Reject(u16, String),
        /// Accept, send `hello`, then close with this code.
        CloseWith(u16),
    }

    #[derive(Default)]
    struct Seen {
        /// When each upgrade was asked for.
        attempts: Vec<Instant>,
        /// Upgrades accepted.
        connections: usize,
        /// The `Authorization` header and request target of each upgrade.
        upgrades: Vec<(Option<String>, String)>,
        /// Every text frame the Mac sent, in order, and when it arrived.
        frames: Vec<String>,
        times: Vec<Instant>,
        /// Close codes the Mac sent.
        closes: Vec<Option<u16>>,
    }

    /// A relay in memory (`tokio::io::duplex`), so the paused clock is exact.
    #[derive(Clone)]
    struct Relay {
        mode: Arc<Mutex<Mode>>,
        seen: Arc<Mutex<Seen>>,
        /// Frames to push to the Mac on the current connection.
        to_mac: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,
        /// SW-DP-23: the `owner_account` the next `hello` names (none: left out).
        owner: Arc<Mutex<Option<String>>>,
    }

    impl Relay {
        fn new(mode: Mode) -> Relay {
            Relay {
                mode: Arc::new(Mutex::new(mode)),
                seen: Default::default(),
                to_mac: Default::default(),
                owner: Default::default(),
            }
        }

        fn set_owner(&self, owner: Option<&str>) {
            *self.owner.lock().unwrap() = owner.map(String::from);
        }

        fn set_mode(&self, mode: Mode) {
            *self.mode.lock().unwrap() = mode;
        }

        fn connections(&self) -> usize {
            self.seen.lock().unwrap().connections
        }

        fn attempts(&self) -> Vec<Instant> {
            self.seen.lock().unwrap().attempts.clone()
        }

        fn frames(&self) -> Vec<String> {
            self.seen.lock().unwrap().frames.clone()
        }

        fn timed_frames(&self) -> Vec<(Instant, String)> {
            let seen = self.seen.lock().unwrap();
            seen.times
                .iter()
                .copied()
                .zip(seen.frames.iter().cloned())
                .collect()
        }

        fn closes(&self) -> Vec<Option<u16>> {
            self.seen.lock().unwrap().closes.clone()
        }

        fn send(&self, frame: Message) {
            if let Some(tx) = self.to_mac.lock().unwrap().as_ref() {
                let _ = tx.send(frame);
            }
        }
    }

    #[async_trait]
    impl Connect for Relay {
        type Stream = DuplexStream;

        async fn connect(
            &self,
            token: &Secret,
        ) -> Result<WebSocketStream<DuplexStream>, ConnectError> {
            self.seen.lock().unwrap().attempts.push(Instant::now());
            let mode = self.mode.lock().unwrap().clone();
            let (client, server) = tokio::io::duplex(1 << 16);
            let seen = self.seen.clone();
            let hello = match self.owner.lock().unwrap().as_deref() {
                Some(owner) => {
                    json!({"t": "hello", "v": 1, "server_time": 1792141200, "owner_account": owner})
                        .to_string()
                }
                None => r#"{"t":"hello","server_time":1792141200}"#.to_string(),
            };
            let (tx, mut rx) = mpsc::unbounded_channel();
            *self.to_mac.lock().unwrap() = Some(tx);
            tokio::spawn(async move {
                let record = seen.clone();
                let reject = match &mode {
                    Mode::Reject(status, body) => Some((*status, body.clone())),
                    _ => None,
                };
                let callback =
                    move |req: &ServerRequest, resp: Response| -> Result<Response, ErrorResponse> {
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        record
                            .lock()
                            .unwrap()
                            .upgrades
                            .push((auth, req.uri().to_string()));
                        match reject {
                            Some((status, body)) => Err(http::Response::builder()
                                .status(status)
                                .body(Some(body))
                                .unwrap()),
                            None => Ok(resp),
                        }
                    };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(server, callback).await else {
                    return;
                };
                seen.lock().unwrap().connections += 1;
                let _ = ws.send(Message::text(hello)).await;
                if let Mode::CloseWith(code) = mode {
                    let _ = ws
                        .close(Some(CloseFrame {
                            code: CloseCode::from(code),
                            reason: "".into(),
                        }))
                        .await;
                }
                loop {
                    tokio::select! {
                        push = rx.recv() => match push {
                            Some(frame) => { let _ = ws.send(frame).await; }
                            None => {}
                        },
                        message = ws.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                let text = text.to_string();
                                {
                                    let mut seen = seen.lock().unwrap();
                                    seen.frames.push(text.clone());
                                    seen.times.push(Instant::now());
                                }
                                let pings = seen.lock().unwrap().frames.iter().filter(|f| *f == "ping").count();
                                if text == "ping" && matches!(mode, Mode::Answer) {
                                    let _ = ws.send(Message::text("pong")).await;
                                } else if text == "ping" && matches!(mode, Mode::PongOnceLate) && pings == 1 {
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    let _ = ws.send(Message::text("pong")).await;
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                seen.lock().unwrap().closes.push(frame.map(|f| u16::from(f.code)));
                                return;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(_)) | None => return,
                        },
                    }
                }
            });
            let request = upgrade_request("ws://relay.test/api/dash/v1/live", token)?;
            tokio_tungstenite::client_async_with_config(request, client, Some(link_ws_config()))
                .await
                .map(|(ws, _)| ws)
                .map_err(upgrade_error)
        }
    }

    /// A published dashboard on disk with `live_block`; `saved` is the block saved on its record.
    fn live_dashboard(
        data: &Path,
        live_block: &str,
        saved: Option<LiveQueries>,
    ) -> tempfile::TempDir {
        live_dashboard_as(data, "ai-usage", live_block, saved)
    }

    /// `live_dashboard` under `slug`. The refusal tests use their own slug, because
    /// the turn lock is shared by every test in the process.
    fn live_dashboard_as(
        data: &Path,
        slug: &str,
        live_block: &str,
        saved: Option<LiveQueries>,
    ) -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("index.html"), "<h1>AI usage</h1>").unwrap();
        fs::write(
            folder.path().join(MANIFEST_FILE),
            format!("slug: {slug}\ntitle: AI usage\npublish:\n  state: published\n  visibility: private\n  files:\n    - index.html\n{live_block}"),
        )
        .unwrap();
        let mut record = PublishRecord::new(slug);
        record.apply(Event::Publish).unwrap();
        record.apply(Event::FirstCommitOk).unwrap();
        record.folder = Some(folder.path().display().to_string());
        record.live_approved = saved.map(|q| LiveApproved {
            scope: None,
            sha: q.sha(),
            queries: q,
            // Canonical, as `approval::entry` saves it: `widens` compares folders as saved.
            folder: state::folder_key(folder.path()),
            fields: Default::default(),
            args: Default::default(),
            streams: Vec::new(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            approved_at: "2026-10-17T10:00:00Z".into(),
            confirmed: true,
        });
        state::save(data, &record).unwrap();
        folder
    }

    fn save_live(data: &Path, queries: LiveQueries) {
        let mut record = state::load(data, "ai-usage").unwrap();
        record.live_approved = Some(LiveApproved {
            scope: None,
            sha: queries.sha(),
            queries,
            folder: state::folder_key(Path::new(record.folder.as_deref().unwrap_or_default())),
            fields: Default::default(),
            args: Default::default(),
            streams: Vec::new(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            approved_at: "2026-10-17T10:00:00Z".into(),
            confirmed: true,
        });
        state::save(data, &record).unwrap();
    }

    /// A person confirms the block the watcher holds for `slug`, as `confirm`
    /// stores it (`approval::settle`, bound to what was shown). The R1 is left
    /// to the watcher, as when the confirm's own R1 fails.
    fn confirm_held(data: &Path, slug: &str) {
        use crate::dashboard_publish::approval::{settle, Bound, Initiator, Shown};
        let record = state::load(data, slug).unwrap();
        let folder = PathBuf::from(record.folder.clone().unwrap());
        let m = manifest::load(&folder).unwrap();
        let request = record.live_request.clone().expect("an ask is held");
        let shown = Shown::of(&request, m.visibility.as_str());
        let settled = settle(
            &record,
            &m,
            &folder,
            None,
            m.visibility,
            Initiator::Human,
            Some(&Bound::Request(shown)),
        )
        .unwrap();
        state::set_live_approved(data, slug, &folder, settled.gated.applied).unwrap();
        state::set_live_request(data, slug, settled.request).unwrap();
    }

    /// The watcher held the manifest's block for a person: an ask waits, the
    /// saved block keeps `queries`, and no R1 is pending.
    fn assert_held(data: &Path, slug: &str, queries: Option<LiveQueries>) {
        let record = state::load(data, slug).unwrap();
        assert!(
            record
                .live_request
                .as_ref()
                .is_some_and(|r| r.block_waiting().is_some()),
            "the edited block was not held: {:?}",
            record.live_request
        );
        assert_eq!(record.live_approved.map(|a| a.queries), queries);
        assert!(!record.live_pending, "a held block marked R1 pending");
    }

    fn start(data: &Path, relay: &Relay, store: Arc<FakeStore>) -> tokio::task::JoinHandle<()> {
        let store: Arc<dyn CredentialStore> = store;
        let link = LiveLink::new(data, relay.clone(), store);
        tokio::spawn(link.run())
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// SW-DP-4 AC-10: with no host there is no link and nothing is dialled.
    #[tokio::test]
    async fn without_a_host_no_connection_is_attempted() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert!(LiveLink::open(data.path(), client::live_url_for(None)).is_none());
        // A URL on another host is refused before any socket is opened.
        let other = Url::parse("wss://evil.example.com/api/dash/v1/live").unwrap();
        assert!(LiveLink::open(data.path(), Some(other)).is_none());
        if DASH_HOST.is_none() {
            tokio::time::timeout(secs(1), run(data.path(), None))
                .await
                .expect("run returned at once with no host");
        }
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "a connection reached the loopback listener"
        );
    }

    /// SW-DP-4 AC-13 (first half), through the app's connector on loopback: the
    /// token is only in the `Authorization` header; a 401 and an E6 are told apart.
    #[tokio::test]
    async fn the_app_connector_sends_the_token_only_as_a_bearer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<(Option<String>, String)>>> = Default::default();
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in [
                (None, None),
                (Some(401u16), None),
                (Some(409), Some(r#"{"error":"not_published"}"#.to_string())),
                (Some(409), None),
            ] {
                let (socket, _) = listener.accept().await.unwrap();
                let log = log.clone();
                let callback =
                    move |req: &ServerRequest, resp: Response| -> Result<Response, ErrorResponse> {
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        log.lock().unwrap().push((auth, req.uri().to_string()));
                        match status {
                            None => Ok(resp),
                            Some(s) => Err(http::Response::builder()
                                .status(s)
                                .body(body.clone())
                                .unwrap()),
                        }
                    };
                if let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(socket, callback).await {
                    let _ = ws.close(None).await;
                }
            }
        });
        let url = Url::parse(&format!("ws://127.0.0.1:{port}/api/dash/v1/live")).unwrap();
        let connector = WssConnector::new(url).expect("loopback is allowed in the tests");
        let token = Secret::new(TOKEN);
        assert!(connector.connect(&token).await.is_ok());
        assert_eq!(
            connector.connect(&token).await.err(),
            Some(ConnectError::Unauthorized)
        );
        assert_eq!(
            connector.connect(&token).await.err(),
            Some(ConnectError::NotPublished)
        );
        // Review C5: a 409 whose body is not in hand is still E6.
        assert_eq!(
            connector.connect(&token).await.err(),
            Some(ConnectError::NotPublished)
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 4);
        for (auth, target) in seen {
            assert_eq!(auth.as_deref(), Some("Bearer csp_test_token"));
            assert_eq!(target, "/api/dash/v1/live");
        }
    }

    /// SW-DP-4 AC-11: no link while no dashboard has live on; one when a
    /// dashboard has; closed with 1000 within one pass when its block is edited
    /// or it is unpublished.
    #[tokio::test(start_paused = true)]
    async fn the_link_follows_the_dashboards_with_live_on() {
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard(data.path(), "live:\n  queries: all\n", None);
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));

        sleep(secs(30)).await;
        assert_eq!(
            relay.attempts().len(),
            0,
            "a link opened with nothing approved"
        );

        save_live(data.path(), LiveQueries::All);
        sleep(PASS + secs(1)).await;
        assert_eq!(relay.connections(), 1);

        // The block is edited without a new publish: closed within one pass.
        let manifest_path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&manifest_path).unwrap();
        fs::write(&manifest_path, text.replace("queries: all", "queries: []")).unwrap();
        sleep(PASS).await;
        assert_eq!(relay.closes(), vec![Some(1000)]);
        sleep(secs(60)).await;
        assert_eq!(
            relay.connections(),
            1,
            "the link reopened for an edited block"
        );

        // Approved again, then unpublished on this Mac: closed again.
        save_live(data.path(), LiveQueries::List(vec![]));
        sleep(PASS + secs(1)).await;
        assert_eq!(relay.connections(), 2);
        let mut record = state::load(data.path(), "ai-usage").unwrap();
        record.apply(Event::Unpublish).unwrap();
        state::save(data.path(), &record).unwrap();
        sleep(PASS).await;
        assert_eq!(relay.closes(), vec![Some(1000), Some(1000)]);
        link.abort();
    }

    /// SW-DP-4 AC-12: `ping` every K14; a relay that stops answering is left
    /// after 2 × K14.
    #[tokio::test(start_paused = true)]
    async fn the_heartbeat_pings_every_k14_and_a_silent_relay_is_left() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(95)).await;
        assert_eq!(relay.connections(), 1);
        assert_eq!(
            relay.frames(),
            vec!["ping"; 4],
            "pings at 0, 30, 60 and 90 s"
        );
        link.abort();

        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Silent);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(70)).await;
        let attempts = relay.attempts();
        assert_eq!(attempts.len(), 2, "no reconnect after the missed pongs");
        let gap = attempts[1] - attempts[0];
        // Dropped at 2 × K14, then the first K22 delay (1 s ± 20 %).
        assert!(
            gap >= secs(60) + Duration::from_millis(800)
                && gap <= secs(60) + Duration::from_millis(1200),
            "{gap:?}"
        );
        link.abort();
    }

    /// SW-DP-4 AC-12: reconnect delays double from 1 s (±20 %), reset after
    /// 60 s connected; close 4001 waits the cap.
    #[tokio::test(start_paused = true)]
    async fn reconnects_follow_k22() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Reject(503, String::new()));
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(20)).await;
        let attempts = relay.attempts();
        assert!(attempts.len() >= 5, "{attempts:?}");
        for (i, pair) in attempts.windows(2).take(4).enumerate() {
            let base = 1u64 << i;
            let gap = (pair[1] - pair[0]).as_secs_f64();
            assert!(
                gap >= base as f64 * 0.8 - 0.001 && gap <= base as f64 * 1.2 + 0.001,
                "delay {i}: {gap}"
            );
        }

        // Up for more than 60 s, then dropped by the relay: the delays start again at 1 s.
        relay.set_mode(Mode::Answer);
        sleep(secs(100)).await;
        let before = relay.attempts().len();
        assert_eq!(relay.connections(), 1);
        relay.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        })));
        sleep(secs(2)).await;
        assert_eq!(
            relay.attempts().len(),
            before + 1,
            "the delay was not reset"
        );

        // Replaced by another link of this publisher: wait the cap.
        relay.set_mode(Mode::CloseWith(4001));
        relay.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        })));
        sleep(secs(3)).await;
        let replaced_at = *relay.attempts().last().unwrap();
        let count = relay.attempts().len();
        tokio::time::sleep_until(replaced_at + BACKOFF_CAP - secs(1)).await;
        assert_eq!(
            relay.attempts().len(),
            count,
            "reconnected before the cap after 4001: {:?}",
            relay.attempts()
        );
        sleep(secs(2)).await;
        let next = relay.attempts()[count];
        assert_eq!(next - replaced_at, BACKOFF_CAP);
        link.abort();
    }

    /// SW-DP-4 AC-13: a 401 on upgrade writes the token's fingerprint to every
    /// published record and stops the link; a new token resumes it.
    #[tokio::test(start_paused = true)]
    async fn a_refused_token_stops_the_link_until_it_is_replaced() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard_as(
            data.path(),
            "live-refused",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Reject(401, r#"{"error":"unauthenticated"}"#.into()));
        let store = token_store(TOKEN);
        let link = start(data.path(), &relay, store.clone());
        sleep(secs(60)).await;
        assert_eq!(relay.attempts().len(), 1, "a refused token was sent again");
        let record = state::load(data.path(), "live-refused").unwrap();
        assert_eq!(
            record.refused_token,
            Some(fingerprint(Some(&Secret::new(TOKEN))))
        );

        relay.set_mode(Mode::Answer);
        store
            .set(TOKEN_ACCOUNT, &Secret::new("csp_new_token"))
            .unwrap();
        sleep(PASS + secs(1)).await;
        assert_eq!(
            relay.connections(),
            1,
            "a new token did not resume the link"
        );
        let upgrades = relay.seen.lock().unwrap().upgrades.clone();
        assert_eq!(
            upgrades.last().unwrap().0.as_deref(),
            Some("Bearer csp_new_token")
        );
        assert!(upgrades.iter().all(|(_, target)| !target.contains("csp_")));
        link.abort();
    }

    /// SW-DP-4 AC-13: close 4003 (publisher revoked) is a refusal too.
    #[tokio::test(start_paused = true)]
    async fn close_4003_refuses_the_token() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard_as(
            data.path(),
            "live-revoked",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::CloseWith(4003));
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(120)).await;
        assert_eq!(relay.attempts().len(), 1);
        let record = state::load(data.path(), "live-revoked").unwrap();
        assert_eq!(
            record.refused_token,
            Some(fingerprint(Some(&Secret::new(TOKEN))))
        );
        link.abort();
    }

    /// SW-DP-4 AC-14: the Mac sends only `ping` and `a` frames with `ok:false`.
    #[tokio::test(start_paused = true)]
    async fn only_pings_and_refusals_cross_the_link() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        relay.send(Message::text(
            r#"{"t":"q","id":"q1","dashboard":"ai-usage","query":"notInTheCatalog","args":{},"share":"s1"}"#,
        ));
        relay.send(Message::text(r#"{"t":"later","x":1}"#));
        relay.send(Message::text("not json"));
        sleep(secs(300)).await;
        let frames = relay.frames();
        let mut answers = 0;
        for frame in &frames {
            if frame == "ping" {
                continue;
            }
            let a: Value = serde_json::from_str(frame).unwrap();
            assert_eq!(
                a,
                json!({"t":"a","id":"q1","ok":false,"error":"not_approved"})
            );
            answers += 1;
        }
        assert_eq!(answers, 1);
        assert_eq!(
            frames.iter().filter(|f| *f == "ping").count(),
            11,
            "{frames:?}"
        );
        link.abort();
    }

    /// SW-DP-22 AC-16: `by` names the owner by account and a viewer by share; never an email.
    #[test]
    fn a_q_frame_names_the_owner_by_account_and_a_viewer_by_share() {
        let read = |text: &str| match read_frame(text) {
            Frame::Ask(ask) => ask,
            _ => panic!("not an ask: {text}"),
        };
        let owner = read(
            r#"{"t":"q","id":"q1","dashboard":"d","query":"runs","args":{},"share":null,"by":{"role":"owner","account":"acc_1"}}"#,
        );
        assert_eq!(
            (
                owner.role.as_str(),
                owner.account.as_deref(),
                owner.share.as_deref()
            ),
            ("owner", Some("acc_1"), None)
        );
        let viewer = read(
            r#"{"t":"q","id":"q2","dashboard":"d","query":"runs","args":{},"share":"s1","by":{"role":"viewer","share":"s1"}}"#,
        );
        assert_eq!(
            (
                viewer.role.as_str(),
                viewer.account.as_deref(),
                viewer.share.as_deref()
            ),
            ("viewer", None, Some("s1"))
        );
        // A relay that predates `by`.
        let legacy =
            read(r#"{"t":"q","id":"q3","dashboard":"d","query":"runs","args":{},"share":"s2"}"#);
        assert_eq!(
            (legacy.role.as_str(), legacy.share.as_deref()),
            ("viewer", Some("s2"))
        );

        let data = tempfile::tempdir().unwrap();
        log_view(data.path(), &owner, "ok");
        log_view(data.path(), &viewer, "ok");
        let lines = view_log(data.path());
        assert_eq!(lines[0]["role"], "owner");
        assert_eq!(lines[0]["account_id"], "acc_1");
        assert!(lines[0]["share_id"].is_null());
        assert_eq!(lines[1]["role"], "viewer");
        assert_eq!(lines[1]["share_id"], "s1");
        assert!(lines[1]["account_id"].is_null());
    }

    #[test]
    fn the_link_module_names_no_other_host_or_listener() {
        let code = non_test_code();
        assert!(
            code.contains("fn input_worker("),
            "the scan covers the input code"
        );
        for needle in [
            "TcpListener",
            "accept_async",
            "bind(",
            "http://",
            "https://",
        ] {
            assert!(!code.contains(needle), "live.rs names {needle}");
        }
    }

    /// Review C2: a relay that stops after one `pong` is left 2 x K14 after that
    /// `pong`, not at a later ping.
    #[tokio::test(start_paused = true)]
    async fn a_missing_pong_is_caught_on_its_own_deadline() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::PongOnceLate);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(75)).await;
        let attempts = relay.attempts();
        assert_eq!(attempts.len(), 2, "{attempts:?}");
        let gap = attempts[1] - attempts[0];
        // The pong came at 1 s: dropped at 61 s, then the first K22 delay.
        assert!(
            gap >= secs(61) + Duration::from_millis(800)
                && gap <= secs(61) + Duration::from_millis(1200),
            "{gap:?}"
        );
        link.abort();
    }

    /// Review S5: a relay frame over the inbound limit ends the link instead of
    /// being buffered.
    #[tokio::test(start_paused = true)]
    async fn an_oversized_relay_frame_is_refused() {
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard(
            data.path(),
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        relay.send(Message::text("x".repeat(MAX_INBOUND + 1)));
        sleep(secs(3)).await;
        assert_eq!(
            relay.attempts().len(),
            2,
            "the link kept an oversized frame"
        );
        link.abort();
    }

    // ---- SW-DP-5a: answers ----

    use crate::dashboard_publish::catalog::{strict, QueryError as QErr};
    use futures_util::future::BoxFuture;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn echo(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async move {
            Ok(json!({
                "args": call.args,
                "fields": call.fields,
                "owner": "me",
                "leak": "publisher token csp_live_0123456789abcdef",
                "key": "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123"
            }))
        })
    }

    fn big(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async { Ok(json!({"blob": "x".repeat(MAX_ANSWER_BYTES)})) })
    }

    fn with_limit(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async move {
            let params: crate::mcp::run_tools::ListRunsParams = strict(call.args, &["limit"])?;
            Ok(json!({"limit": params.limit}))
        })
    }

    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    static MOST_RUNNING: AtomicUsize = AtomicUsize::new(0);

    fn busy(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async {
            let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
            MOST_RUNNING.fetch_max(now, Ordering::SeqCst);
            sleep(secs(1)).await;
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            Ok(json!({"done": true}))
        })
    }

    static SLOW_DROPPED: AtomicBool = AtomicBool::new(false);

    struct SetOnDrop(&'static AtomicBool);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn slow(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async {
            let _guard = SetOnDrop(&SLOW_DROPPED);
            sleep(secs(60)).await;
            Ok(json!({"late": true}))
        })
    }

    fn wait(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async {
            sleep(secs(30)).await;
            Ok(json!({}))
        })
    }

    fn two_seconds(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async {
            sleep(secs(2)).await;
            Ok(json!({}))
        })
    }

    fn nine_seconds(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async {
            sleep(secs(9)).await;
            Ok(json!({"done": true}))
        })
    }

    static HELD_RELEASE: Mutex<Option<std::sync::mpsc::Receiver<()>>> = Mutex::new(None);

    /// Blocking work that runs until the test releases it.
    fn held(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async move {
            catalog::blocking(call.hold, "test.held", || {
                if let Some(release) = HELD_RELEASE.lock().unwrap().take() {
                    let _ = release.recv();
                }
                Ok(json!({}))
            })
            .await
            .map_err(QErr::Failed)
        })
    }

    #[tokio::test]
    async fn selector_queries_resolve_the_current_scoped_index_and_fail_closed() {
        use crate::context_query::{
            index::IndexRoot, model::Layer, service::Footprint, ContextIndexService,
        };
        use std::sync::{mpsc, Mutex};
        static PIN_CATALOG: &[QueryDef] = &[QueryDef {
            name: "storyChanges",
            args: &["storyId", "limit", "cursor"],
            describe: "fixture",
            shares: "fixture",
            fields: catalog::Fields::Named(&[]),
            run: pinned_echo,
        }];
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard_as(
            data.path(),
            "selectors",
            "live: {queries: [storyChanges], args: {storyChanges: {storyId: [{epic: SV-CDE}]}}}\n",
            None,
        );
        let family = tempfile::tempdir().unwrap();
        let write_story = |path: PathBuf, id: &str, team: &str, epic: &str| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!("---\ntype: story\nstory_id: {id}\nteam: {team}\n{epic}\n---\nStory\n"),
            )
            .unwrap();
        };
        write_story(
            folder.path().join("front.md"),
            "SV-CDE-1",
            "riverside",
            "parent_epic: SV-CDE",
        );
        write_story(
            folder.path().join("SV-CDE/folder.md"),
            "SV-CDE-2",
            "riverside",
            "",
        );
        write_story(
            folder.path().join("other.md"),
            "SV-CDE-3",
            "riverside",
            "parent_epic: OTHER",
        );
        write_story(
            folder.path().join("alias.md"),
            "SV-CDE-4",
            "riverside",
            "epic: SV-CDE",
        );
        write_story(
            family.path().join("foreign.md"),
            "SV-CDE-99",
            "family",
            "parent_epic: FAMILY",
        );
        let roots = Arc::new(Mutex::new(vec![
            IndexRoot {
                dir: folder.path().to_path_buf(),
                org: Some("acme".into()),
                team: Some("riverside".into()),
                layer: Layer::Team,
            },
            IndexRoot {
                dir: family.path().to_path_buf(),
                org: Some("acme".into()),
                team: Some("family".into()),
                layer: Layer::Team,
            },
        ]));
        let service = ContextIndexService::new();
        let roots_for_service = roots.clone();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        service.start_observed(
            Arc::new(move || {
                Some(Footprint {
                    owner: 1,
                    roots: roots_for_service.lock().unwrap().clone(),
                    summary_headings: vec![],
                })
            }),
            Arc::new(|_| {}),
            Arc::new(move |_| {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        rx.recv_timeout(secs(10)).unwrap();
        let head = fs::read_to_string(folder.path().join(MANIFEST_FILE))
            .unwrap()
            .split("live:")
            .next()
            .unwrap()
            .to_string();
        let mut record = state::load(data.path(), "selectors").unwrap();
        let save_pin = |pin: &str, record: &mut PublishRecord| {
            fs::write(folder.path().join(MANIFEST_FILE), format!("{head}live: {{queries: [storyChanges], args: {{storyChanges: {{storyId: {pin}}}}}}}\n")).unwrap();
            let m = manifest::load(folder.path()).unwrap();
            record.live_approved = approval::decide(
                &m,
                folder.path(),
                record.live_approved.as_ref(),
                Some(("acme", "riverside")),
            )
            .approved;
            state::save(data.path(), record).unwrap();
        };
        let a = Answerer {
            data_dir: data.path().into(),
            catalog: PIN_CATALOG,
            streams: STREAMS,
            ctx: LiveCtx {
                context_index: Some(service.clone()),
                ..Default::default()
            },
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let request = |id: &str| Ask {
            id: json!("selectors"),
            slug: "selectors".into(),
            query: "storyChanges".into(),
            args: json!({"storyId":id}),
            share: None,
            role: "viewer".into(),
            account: None,
        };
        let response = |text: String| serde_json::from_str::<Value>(&text).unwrap();
        for (pin, allowed) in [
            ("[{epic: SV-CDE}]", vec!["SV-CDE-1", "SV-CDE-2", "SV-CDE-4"]),
            (
                "[SV-CDE-3, {epic: SV-CDE}]",
                vec!["SV-CDE-1", "SV-CDE-2", "SV-CDE-3", "SV-CDE-4"],
            ),
            (
                "[{team: riverside}]",
                vec!["SV-CDE-1", "SV-CDE-2", "SV-CDE-3", "SV-CDE-4"],
            ),
            ("[{epic: FAMILY}]", vec![]),
        ] {
            save_pin(pin, &mut record);
            for id in [
                "SV-CDE-1",
                "SV-CDE-2",
                "SV-CDE-3",
                "SV-CDE-4",
                "SV-CDE-99",
                "SV-CDE-404",
            ] {
                let _ = fs::remove_file(folder.path().join("query-ran"));
                let result = response(a.answer(request(id)).await);
                assert_eq!(result["ok"], allowed.contains(&id), "{pin}/{id}: {result}");
                assert_eq!(
                    folder.path().join("query-ran").exists(),
                    allowed.contains(&id)
                );
                if !allowed.contains(&id) {
                    assert_eq!(result["error"], "arg_not_allowed");
                }
            }
        }
        save_pin("[{epic: SV-CDE}]", &mut record);
        for epic in ["OTHER", "SV-CDE"] {
            write_story(
                folder.path().join("front.md"),
                "SV-CDE-1",
                "riverside",
                &format!("parent_epic: {epic}"),
            );
            service.note_paths([folder.path().join("front.md")]);
            rx.recv_timeout(secs(10)).unwrap();
            assert_eq!(
                response(a.answer(request("SV-CDE-1")).await)["ok"],
                epic == "SV-CDE"
            );
        }
        let unready = ContextIndexService::new();
        assert!(!unready.snapshot().ready);
        let startup = Answerer {
            data_dir: data.path().into(),
            catalog: PIN_CATALOG,
            streams: STREAMS,
            ctx: LiveCtx {
                context_index: Some(unready),
                ..Default::default()
            },
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        assert_eq!(
            response(startup.answer(request("SV-CDE-1")).await)["error"],
            "arg_not_allowed"
        );
        // The same saved block is refused after the folder moves to a different team.
        roots.lock().unwrap()[0].team = Some("family".into());
        service.request_reconcile();
        rx.recv_timeout(secs(10)).unwrap();
        assert_eq!(
            response(a.answer(request("SV-CDE-1")).await)["error"],
            "arg_not_allowed"
        );
        service.reset();
        assert_eq!(
            response(a.answer(request("SV-CDE-1")).await)["error"],
            "arg_not_allowed"
        );
    }

    fn pinned_echo(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        // An answer proves dispatch ran; the on-disk marker counts disallowed dispatches too.
        fs::write(call.folder.join("query-ran"), "yes").unwrap();
        Box::pin(async move { Ok(call.args) })
    }

    #[tokio::test]
    async fn pinned_queries_enforce_values_roles_follow_and_restart() {
        use crate::dashboard_publish::client::fake::Fake;
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::tests::healthy;
        static PIN_CATALOG: &[QueryDef] = &[
            QueryDef {
                name: "storyChanges",
                args: &["storyId", "limit", "cursor"],
                describe: "fixture",
                shares: "args",
                fields: catalog::Fields::Named(&[]),
                run: pinned_echo,
            },
            QueryDef {
                name: "whyLine",
                args: &["repo", "rev", "path", "start", "end"],
                describe: "fixture",
                shares: "args",
                fields: catalog::Fields::Named(&[]),
                run: pinned_echo,
            },
        ];
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard_as(
            data.path(),
            "pins",
            "live: {queries: all}\n",
            Some(LiveQueries::All),
        );
        let head = fs::read_to_string(folder.path().join(MANIFEST_FILE))
            .unwrap()
            .split("live:")
            .next()
            .unwrap()
            .to_string();
        let fake = Fake::start(healthy).await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let fresh = |catalog| Answerer {
            data_dir: data.path().into(),
            catalog,
            streams: STREAMS,
            ctx: LiveCtx::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let a = fresh(PIN_CATALOG);
        let request = |query: &str, args: Value, role: &str| Ask {
            id: json!("pins"),
            slug: "pins".into(),
            query: query.into(),
            args,
            share: None,
            role: role.into(),
            account: None,
        };
        let response = |text: String| serde_json::from_str::<Value>(&text).unwrap();
        for (query, key) in [("storyChanges", "storyId"), ("whyLine", "repo")] {
            assert_eq!(
                response(a.answer(request(query, json!({key:"a"}), "viewer")).await)["error"],
                "arg_not_allowed"
            );
            assert_eq!(
                response(a.answer(request(query, json!({key:"a"}), "owner")).await)["ok"],
                true
            );
        }
        let t0 = std::time::Instant::now();
        for (n, values) in ["[a]", "[a, b]", "[b]", "[a, b]", "[a]"]
            .into_iter()
            .enumerate()
        {
            let block = format!("live: {{queries: all, args: {{storyChanges: {{storyId: {values}}}, whyLine: {{repo: {values}, start: [5], rev: [HEAD]}}}}}}\n");
            fs::write(folder.path().join(MANIFEST_FILE), format!("{head}{block}")).unwrap();
            assert_eq!(
                response(
                    a.answer(request("storyChanges", json!({"storyId":"a"}), "owner"))
                        .await
                )["error"],
                "not_approved"
            );
            let pins = |record: &PublishRecord| {
                serde_json::to_value(record.live_approved.as_ref().unwrap()).unwrap()["args"]
                    ["storyChanges"]["storyId"]
                    .as_array()
                    .map(Vec::len)
            };
            driver.tick(t0 + secs((n as u64 + 1) * 5)).await;
            // SV-PBF-286: a narrower pin list is followed at once; a wider one is
            // held, the confirmed pins kept, until a person confirms it.
            if n > 0 && values.contains(',') {
                let held = state::load(data.path(), "pins").unwrap();
                assert!(
                    held.live_request
                        .as_ref()
                        .is_some_and(|r| r.block_waiting().is_some()),
                    "{values}: the wider pins were not held"
                );
                assert_eq!(
                    pins(&held),
                    Some(1),
                    "{values}: the wider pins were applied"
                );
                confirm_held(data.path(), "pins");
                driver.tick(t0 + secs((n as u64 + 1) * 5 + 1)).await;
            }
            let saved = state::load(data.path(), "pins").unwrap();
            assert_eq!(
                pins(&saved),
                Some(if values.contains(',') { 2 } else { 1 }),
                "{values}"
            );
            for (query, key) in [("storyChanges", "storyId"), ("whyLine", "repo")] {
                for role in ["owner", "viewer"] {
                    for value in ["a", "b", "outside"] {
                        let _ = fs::remove_file(folder.path().join("query-ran"));
                        let args = if query == "whyLine" {
                            json!({key:value,"start":5,"rev":"HEAD"})
                        } else {
                            json!({key:value})
                        };
                        let result =
                            response(fresh(PIN_CATALOG).answer(request(query, args, role)).await);
                        let allowed = values.contains(value);
                        assert_eq!(result["ok"], allowed, "{values}: {query}/{role}/{value}");
                        assert_eq!(folder.path().join("query-ran").exists(), allowed);
                        if !allowed {
                            assert_eq!(result["error"], "arg_not_allowed");
                        }
                    }
                }
            }
            for args in [
                json!({}),
                json!({"repo":"a","start":"5","rev":"HEAD"}),
                json!({"repo":"a","start":5}),
            ] {
                let _ = fs::remove_file(folder.path().join("query-ran"));
                assert_eq!(
                    response(a.answer(request("whyLine", args.clone(), "owner")).await)["error"],
                    "arg_not_allowed"
                );
                assert!(!folder.path().join("query-ran").exists());
                // The production entry refuses at the same gate, before its repository read.
                assert_eq!(
                    response(
                        fresh(CATALOG)
                            .answer(request("whyLine", args, "viewer"))
                            .await
                    )["error"],
                    "arg_not_allowed"
                );
            }
            assert_eq!(
                fake.calls()
                    .iter()
                    .filter(|c| c.method == "PUT")
                    .last()
                    .unwrap()
                    .json()["live_args"],
                serde_json::to_value(saved.live_approved.unwrap()).unwrap()["args"]
            );
            // An unchanged follow is a no-op, including after the store has been reloaded.
            driver.tick(t0 + secs((n as u64 + 1) * 5 + 2)).await;
        }
        let logs = view_log(data.path());
        assert!(logs.iter().any(|l| l["outcome"] == "arg_not_allowed"));
        assert!(logs.iter().all(|l| l.get("args").is_none()));
    }

    /// The fixture catalog the link answers from in these tests.
    static TEST_CATALOG: &[QueryDef] = &[
        QueryDef {
            name: "fixtureEcho",
            args: &[],
            describe: "Echoes its args",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: echo,
        },
        QueryDef {
            name: "fixtureBig",
            args: &[],
            describe: "An answer over K19",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: big,
        },
        QueryDef {
            name: "fixtureLimit",
            args: &["limit"],
            describe: "Takes {limit?}",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: with_limit,
        },
        QueryDef {
            name: "fixtureBusy",
            args: &[],
            describe: "Takes a second",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: busy,
        },
        QueryDef {
            name: "fixtureSlow",
            args: &[],
            describe: "Takes a minute",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: slow,
        },
        QueryDef {
            name: "fixtureWait",
            args: &[],
            describe: "Takes half a minute",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: wait,
        },
        QueryDef {
            name: "fixtureTwoSeconds",
            args: &[],
            describe: "Takes two seconds",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: two_seconds,
        },
        QueryDef {
            name: "fixtureNineSeconds",
            args: &[],
            describe: "Takes nine seconds",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: nine_seconds,
        },
        QueryDef {
            name: "fixtureHeld",
            args: &[],
            describe: "Blocks until released",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: held,
        },
        QueryDef {
            name: "fixtureExtra",
            args: &[],
            describe: "The entry AC-7 adds",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: echo,
        },
        QueryDef {
            name: "fixtureStale",
            args: &[],
            describe: "A cursor page whose snapshot is gone",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: stale_cursor,
        },
    ];

    fn stale_cursor(_ctx: LiveCtx, _call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        Box::pin(async { Err(QErr::StaleCursor) })
    }

    /// SV-PBF-287: a twin's `StaleCursor` reaches the relay as `stale_cursor`,
    /// the exact code the relay's `MAC_ERRORS` passes on as a 409.
    #[tokio::test(start_paused = true)]
    async fn a_stale_cursor_is_answered_as_the_relays_stale_cursor() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        ask(&relay, "stale", "dash-a", "fixtureStale", json!({}));
        sleep(secs(1)).await;
        let got = answers(&relay);
        assert_eq!(got["stale"]["ok"], false);
        assert_eq!(got["stale"]["error"], "stale_cursor");
        #[cfg(cs_site)]
        {
            let mac_errors = include_str!("../../../site/worker/live/relay.ts")
                .lines()
                .find(|l| l.starts_with("const MAC_ERRORS"))
                .unwrap();
            assert!(mac_errors.contains("\"stale_cursor\""), "{mac_errors}");
        }
        link.abort();
    }

    fn start_fixture(data: &Path, relay: &Relay) -> tokio::task::JoinHandle<()> {
        let store: Arc<dyn CredentialStore> = token_store(TOKEN);
        let link = LiveLink::new(data, relay.clone(), store).with_catalog(TEST_CATALOG);
        tokio::spawn(link.run())
    }

    fn ask(relay: &Relay, id: &str, slug: &str, query: &str, args: Value) {
        relay.send(Message::text(
            json!({"t": "q", "id": id, "dashboard": slug, "query": query, "args": args, "share": "share-1"})
                .to_string(),
        ));
    }

    /// The `a` frames the relay received, by id. Each id must be answered once.
    fn answers(relay: &Relay) -> std::collections::HashMap<String, Value> {
        let mut out = std::collections::HashMap::new();
        for frame in relay.frames() {
            if frame == "ping" {
                continue;
            }
            let a: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(a["t"], "a", "{frame}");
            let id = a["id"].as_str().unwrap().to_string();
            assert!(
                out.insert(id.clone(), a).is_none(),
                "{id} was answered twice"
            );
        }
        out
    }

    fn view_log(data: &Path) -> Vec<Value> {
        fs::read_to_string(view_log_path(data))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn list(names: &[&str]) -> LiveQueries {
        LiveQueries::list(names.iter().map(|n| n.to_string()))
    }

    /// SW-DP-5 AC-1, AC-5, AC-6: only a declared catalog query, saved on the record, on a
    /// published dashboard is answered; every `q` gets one answer with its id;
    /// each is logged without its args or answer.
    #[tokio::test(start_paused = true)]
    async fn only_saved_declared_queries_are_answered() {
        let data = tempfile::tempdir().unwrap();
        let block = "live:\n  queries: [fixtureEcho]\n";
        let _a = live_dashboard_as(data.path(), "dash-a", block, Some(list(&["fixtureEcho"])));
        // Declared, but no block saved on the record.
        let _b = live_dashboard_as(data.path(), "dash-b", block, None);
        // Saved, then the block was edited and not yet followed.
        let _c = live_dashboard_as(
            data.path(),
            "dash-c",
            "live:\n  queries: [fixtureEcho, fixtureBig]\n",
            Some(list(&["fixtureEcho"])),
        );
        // Saved, then unpublished on this Mac.
        let _d = live_dashboard_as(data.path(), "dash-d", block, Some(list(&["fixtureEcho"])));
        let mut d = state::load(data.path(), "dash-d").unwrap();
        d.apply(Event::Unpublish).unwrap();
        state::save(data.path(), &d).unwrap();

        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        assert_eq!(relay.connections(), 1);
        let secret_args = json!({"note": "csp_args_never_logged"});
        ask(
            &relay,
            "live-on",
            "dash-a",
            "fixtureEcho",
            secret_args.clone(),
        );
        ask(&relay, "undeclared", "dash-a", "fixtureBig", json!({}));
        ask(&relay, "declined", "dash-b", "fixtureEcho", json!({}));
        ask(&relay, "edited", "dash-c", "fixtureEcho", json!({}));
        ask(&relay, "unpublished", "dash-d", "fixtureEcho", json!({}));
        ask(
            &relay,
            "no-such-dashboard",
            "../etc",
            "fixtureEcho",
            json!({}),
        );
        ask(
            &relay,
            "not-in-catalog",
            "dash-a",
            "sessionUsage",
            json!({}),
        );
        sleep(secs(2)).await;

        let got = answers(&relay);
        assert_eq!(got.len(), 7, "{got:?}");
        assert_eq!(got["live-on"]["ok"], true);
        assert_eq!(got["live-on"]["answer"]["owner"], "me");
        for id in [
            "undeclared",
            "declined",
            "edited",
            "unpublished",
            "no-such-dashboard",
            "not-in-catalog",
        ] {
            assert_eq!(
                got[id],
                json!({"t": "a", "id": id, "ok": false, "error": "not_approved"}),
                "{id}"
            );
        }

        // AC-3 through the link: the answer is redacted.
        let answered = got["live-on"].to_string();
        assert!(!answered.contains("csp_"), "{answered}");
        assert!(!answered.contains("sk-ant-"), "{answered}");

        // AC-6: one line per q, args and answers never written.
        let lines = view_log(data.path());
        assert_eq!(lines.len(), 7);
        let first = lines
            .iter()
            .find(|l| l["slug"] == "dash-a" && l["query"] == "fixtureEcho")
            .unwrap();
        assert_eq!(first["outcome"], "ok");
        assert_eq!(first["share_id"], "share-1");
        let keys: Vec<_> = first.as_object().unwrap().keys().cloned().collect();
        assert_eq!(first["role"], "viewer");
        assert_eq!(keys.len(), 7, "{keys:?}");
        assert!(
            lines
                .iter()
                .filter(|l| l["outcome"] == "not_approved")
                .count()
                == 6
        );
        let log_text = fs::read_to_string(view_log_path(data.path())).unwrap();
        assert!(!log_text.contains("csp_args_never_logged") && !log_text.contains("owner"));
        link.abort();
    }

    /// SW-DP-5 AC-2: args that do not parse into the params are `invalid_args`.
    #[tokio::test(start_paused = true)]
    async fn args_that_do_not_parse_are_invalid_args() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        ask(
            &relay,
            "wrong-type",
            "dash-a",
            "fixtureLimit",
            json!({"limit": "ten"}),
        );
        ask(
            &relay,
            "unknown-key",
            "dash-a",
            "fixtureLimit",
            json!({"limit": 1, "all": true}),
        );
        ask(
            &relay,
            "not-an-object",
            "dash-a",
            "fixtureLimit",
            json!([1]),
        );
        ask(
            &relay,
            "good",
            "dash-a",
            "fixtureLimit",
            json!({"limit": 3}),
        );
        sleep(secs(2)).await;
        let got = answers(&relay);
        for id in ["wrong-type", "unknown-key", "not-an-object"] {
            assert_eq!(got[id]["error"], "invalid_args", "{id}");
        }
        assert_eq!(got["good"]["answer"], json!({"limit": 3}));
        link.abort();
    }

    /// SW-DP-5 AC-4: an answer over K19 is `too_large`; at most K20 run at
    /// once; a query running past K17 is dropped and answered `failed`.
    #[tokio::test(start_paused = true)]
    async fn size_concurrency_and_time_are_capped() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;

        ask(&relay, "big", "dash-a", "fixtureBig", json!({}));
        for i in 0..9 {
            ask(
                &relay,
                &format!("busy-{i}"),
                "dash-a",
                "fixtureBusy",
                json!({}),
            );
        }
        sleep(secs(3)).await;
        let got = answers(&relay);
        assert_eq!(
            got["big"],
            json!({"t": "a", "id": "big", "ok": false, "error": "too_large"})
        );
        assert!(
            !relay.frames().iter().any(|f| f.contains("xxxxxxxx")),
            "an oversized answer was sent"
        );
        for i in 0..9 {
            assert_eq!(got[&format!("busy-{i}")]["ok"], true, "busy-{i}");
        }
        assert_eq!(
            MOST_RUNNING.load(Ordering::SeqCst),
            MAX_IN_FLIGHT,
            "more than K20 ran at once"
        );

        ask(&relay, "slow", "dash-a", "fixtureSlow", json!({}));
        sleep(ANSWER_TIMEOUT - Duration::from_millis(100)).await;
        assert!(!answers(&relay).contains_key("slow"), "answered before K17");
        assert!(!SLOW_DROPPED.load(Ordering::SeqCst));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(answers(&relay)["slow"]["error"], "failed");
        assert!(
            SLOW_DROPPED.load(Ordering::SeqCst),
            "the slow query was not dropped"
        );
        link.abort();
    }

    /// SW-DP-5 AC-5 (DP-INV-5): with no `q` frames, only `ping` is sent.
    #[tokio::test(start_paused = true)]
    async fn with_no_query_only_pings_are_sent() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(300)).await;
        let frames = relay.frames();
        assert!(!frames.is_empty());
        assert!(frames.iter().all(|f| f == "ping"), "{frames:?}");
        assert!(view_log(data.path()).is_empty());
        link.abort();
    }

    /// SW-DP-5 AC-6: lines older than 30 days (and unreadable ones) are dropped.
    #[test]
    fn the_view_log_keeps_30_days() {
        let data = tempfile::tempdir().unwrap();
        let path = view_log_path(data.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = |at: &str| {
            json!({"at": at, "slug": "dash-a", "query": "runs", "share_id": "s", "outcome": "ok"})
                .to_string()
        };
        fs::write(
            &path,
            format!(
                "{}\n{}\nnot json\n{}\n",
                line("2026-08-01T00:00:00Z"),
                line("2026-09-18T00:00:01Z"),
                line("2026-10-17T09:00:00Z")
            ),
        )
        .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-18T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        prune_view_log(data.path(), now);
        let kept = view_log(data.path());
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["at"], "2026-09-18T00:00:01Z");
        assert_eq!(kept[1]["at"], "2026-10-17T09:00:00Z");
    }

    /// SW-DP-5 AC-7: one catalog entry is enough: valid in a manifest, listed in
    /// the contract, and answered.
    #[tokio::test(start_paused = true)]
    async fn one_catalog_entry_is_valid_listed_and_answered() {
        let data = tempfile::tempdir().unwrap();
        let block = "live:\n  queries: [fixtureExtra]\n";
        let folder = live_dashboard_as(data.path(), "dash-a", block, Some(list(&["fixtureExtra"])));

        let m = manifest::load_with(folder.path(), TEST_CATALOG).unwrap();
        assert_eq!(m.live, Some(list(&["fixtureExtra"])));
        assert!(
            manifest::load(folder.path()).is_err(),
            "the app catalog has no fixtureExtra"
        );

        let hosted = crate::mcp::live_data_authoring::hosted_section(TEST_CATALOG).to_string();
        assert!(hosted.contains("fixtureExtra") && hosted.contains("The entry AC-7 adds"));

        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        ask(&relay, "extra", "dash-a", "fixtureExtra", json!({}));
        sleep(secs(1)).await;
        assert_eq!(answers(&relay)["extra"]["ok"], true);
        link.abort();
    }

    #[test]
    fn action_accounting_keeps_one_record_and_refuses_unavailable_storage() {
        let dir=tempfile::tempdir().unwrap();
        let mut line=action_log_line(&json!({"dashboard":"triage","action":"fix","by":{"role":"owner","account":"o"}}),"pending");
        save_action_log(dir.path(),&line).unwrap();
        line["outcome"]="done".into(); line["handle"]="h1".into();
        save_action_log(dir.path(),&line).unwrap();
        let rows=super::super::actions::read_history(dir.path(),"triage").unwrap();
        assert_eq!(rows.len(),1); assert_eq!(rows[0]["handle"],"h1");
        fs::write(log_path(dir.path(),super::super::actions::LOG),"corrupt").unwrap();
        assert!(super::super::actions::read_history(dir.path(),"triage").is_err());
        assert!(save_action_log(dir.path(),&line).is_err());
    }

    #[tokio::test]
    async fn action_frames_are_refused_and_logged_before_execution() {
        let data = tempfile::tempdir().unwrap();
        let folder=live_dashboard_as(data.path(),"action-gates","actions:\n  close:\n    verb: setFields\n    who: owner\n    fields: {status: wontfix}\n",Some(LiveQueries::List(vec![])));
        let m = manifest::load(folder.path()).unwrap();
        let mut record = state::load(data.path(), "action-gates").unwrap();
        record.live_approved =
            approval::decide(&m, folder.path(), None, Some(("org", "team"))).approved;
        state::save(data.path(), &record).unwrap();
        let answerer = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: LiveCtx::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: secs(10),
        };
        let owner = json!({"role":"owner","account":"owner"});
        for (n, (name, by, args, error)) in [
            (
                "missing",
                owner.clone(),
                json!({"id":"BUG-1"}),
                "not_declared",
            ),
            (
                "close",
                json!({"role":"viewer","share":"s1"}),
                json!({"id":"BUG-1"}),
                "not_allowed",
            ),
            (
                "close",
                json!({"role":"owner","account":"wrong"}),
                json!({"id":"BUG-1"}),
                "not_allowed",
            ),
            (
                "close",
                owner.clone(),
                json!({"id":"../bug"}),
                "invalid_args",
            ),
            (
                "close",
                owner.clone(),
                json!({"id":"BUG/1"}),
                "invalid_args",
            ),
            (
                "close",
                owner.clone(),
                json!({"id":"BUG-1","choice":"free"}),
                "invalid_args",
            ),
            (
                "close",
                owner,
                json!({"id":"BUG-1","prompt":"private text"}),
                "invalid_args",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let frame = json!({"v":1,"t":"x","id":format!("x{n}"),"dashboard":"action-gates","action":name,"by":by,"args":args});
            let reply: Value =
                serde_json::from_str(&answerer.action(frame, Some("owner".into())).await).unwrap();
            assert_eq!(reply["error"], error);
        }
        let history = super::super::actions::history(data.path(), "action-gates");
        assert_eq!(history.len(), 7);
        assert!(!serde_json::to_string(&history)
            .unwrap()
            .contains("private text"));
        assert_eq!(
            fs::read_to_string(folder.path().join("index.html")).unwrap(),
            "<h1>AI usage</h1>"
        );
    }

    static BOUND_LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static BOUND_PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static BOUND_RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Lets the blocking fixtures end when the test ends, even by a failed assert.
    struct ReleaseBound;
    impl Drop for ReleaseBound {
        fn drop(&mut self) {
            BOUND_RELEASE.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Blocking work that runs until `BOUND_RELEASE`, counting how many run at once.
    fn counted(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        use std::sync::atomic::Ordering::SeqCst;
        Box::pin(async move {
            catalog::blocking(call.hold, "test.counted", || {
                let now = BOUND_LIVE.fetch_add(1, SeqCst) + 1;
                BOUND_PEAK.fetch_max(now, SeqCst);
                while !BOUND_RELEASE.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                BOUND_LIVE.fetch_sub(1, SeqCst);
                Ok(json!({}))
            })
            .await
            .map_err(QErr::Failed)
        })
    }

    static STUCK_RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Lets the stuck fake walk end when the test ends, even by a failed assert.
    struct ReleaseStuck;
    impl Drop for ReleaseStuck {
        fn drop(&mut self) {
            STUCK_RELEASE.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The hosted `storyChanges` entry with a fake git read. `SV-SLOW` and `SV-STUCK` are
    /// blocking walks that run until cancelled (SLOW checks the cancel flag, STUCK ignores it
    /// until `STUCK_RELEASE`); any other story takes 2 s.
    fn changes(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QErr>> {
        use std::sync::atomic::Ordering::SeqCst;
        Box::pin(catalog::story_changes_via(
            call,
            |params, _scope, cancel| {
                Box::pin(async move {
                    let cooperates = params.story_id == "SV-SLOW";
                    if !cooperates && params.story_id != "SV-STUCK" {
                        sleep(Duration::from_millis(20)).await;
                        return Ok(json!({}));
                    }
                    crate::commands::await_blocking("test.walk", move || {
                        let began = std::time::Instant::now();
                        while began.elapsed() < Duration::from_secs(30) {
                            if cooperates && cancel.load(SeqCst) {
                                return Err("cancelled".to_owned());
                            }
                            if !cooperates && STUCK_RELEASE.load(SeqCst) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Ok(json!({}))
                    })
                    .await
                })
            },
        ))
    }

    static BOUND_CATALOG: &[QueryDef] = &[
        QueryDef {
            name: "fixtureCounted",
            args: &[],
            describe: "Blocks until released",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: counted,
        },
        QueryDef {
            name: "fixtureChanges",
            args: &["storyId", "only"],
            describe: "The hosted storyChanges with a fake read",
            shares: "fixture data",
            fields: catalog::Fields::Named(&[]),
            run: changes,
        },
    ];

    fn bound_answerer(data: &Path, timeout: Duration) -> Answerer {
        Answerer {
            data_dir: data.into(),
            catalog: BOUND_CATALOG,
            streams: STREAMS,
            ctx: LiveCtx::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout,
        }
    }

    fn bound_ask(id: String, query: &str, args: Value) -> Ask {
        Ask {
            id: json!(id),
            slug: "dash-bound".into(),
            query: query.into(),
            args,
            share: None,
            role: "viewer".into(),
            account: None,
        }
    }

    fn changes_ask(id: &str, story: &str, only: &str) -> Ask {
        bound_ask(
            id.into(),
            "fixtureChanges",
            json!({"storyId": story, "only": [only]}),
        )
    }

    async fn frame(answerer: &Answerer, ask: Ask) -> Value {
        serde_json::from_str(&answerer.answer(ask).await).unwrap()
    }

    /// SV-ENG-121 (was review S4): a query dropped at K17 frees its K20 slot at the timeout, but
    /// its blocking work keeps a permit of the blocking bound: under repeated timeouts no more
    /// than MAX_IN_FLIGHT jobs run, and a new ask past the bound is `busy` at once.
    #[tokio::test]
    async fn a_timed_out_query_frees_its_slot_but_its_work_stays_bounded() {
        use std::sync::atomic::Ordering::SeqCst;
        let _release = ReleaseBound;
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-bound",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let answerer = bound_answerer(data.path(), Duration::from_millis(150));
        let mut errors = Vec::new();
        for n in 0..MAX_IN_FLIGHT + 4 {
            let reply = frame(
                &answerer,
                bound_ask(format!("c{n}"), "fixtureCounted", Value::Null),
            )
            .await;
            errors.push(reply["error"].as_str().unwrap_or("").to_owned());
            assert_eq!(
                answerer.permits.available_permits(),
                MAX_IN_FLIGHT,
                "the slot was freed at the timeout"
            );
        }
        assert!(
            errors[..MAX_IN_FLIGHT].iter().all(|e| e == "failed"),
            "{errors:?}"
        );
        assert!(
            errors[MAX_IN_FLIGHT..].iter().all(|e| e == "busy"),
            "{errors:?}"
        );
        assert!(
            BOUND_PEAK.load(SeqCst) <= MAX_IN_FLIGHT,
            "more blocking jobs than the bound"
        );
        BOUND_RELEASE.store(true, SeqCst);
        for _ in 0..200 {
            if answerer.blocking.available_permits() == MAX_IN_FLIGHT {
                let reply = frame(
                    &answerer,
                    bound_ask("after".into(), "fixtureCounted", Value::Null),
                )
                .await;
                assert_eq!(reply["ok"], true, "{reply}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the blocking permits did not come back when the work ended");
    }

    /// Waits (real time, driving the runtime) until `done` holds.
    async fn until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..1500 {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// SV-ENG-121, through the shipped `storyChanges` entry: git reads inside their answer time
    /// run together; one that outlives it is abandoned and caps others `busy` (a read with no
    /// git is not capped); at its deadline its cancel flag stops the walk and only then do its
    /// permits return.
    #[tokio::test]
    async fn an_abandoned_git_read_caps_others_busy_until_its_walk_is_cancelled() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-bound",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let answerer = bound_answerer(data.path(), Duration::from_millis(500));
        let together = futures_util::future::join_all([
            frame(&answerer, changes_ask("a", "SV-1", "commits")),
            frame(&answerer, changes_ask("b", "SV-2", "commits")),
        ])
        .await;
        assert!(together.iter().all(|r| r["ok"] == true), "{together:?}");

        let slow = frame(&answerer, changes_ask("slow", "SV-SLOW", "commits")).await;
        assert_eq!(slow["error"], "failed", "{slow}");
        assert_eq!(
            answerer.walks.available_permits(),
            0,
            "the abandoned read holds the permit"
        );
        let capped = frame(&answerer, changes_ask("capped", "SV-1", "commits")).await;
        assert_eq!(capped["error"], "busy", "{capped}");
        let light = frame(&answerer, changes_ask("light", "SV-1", "tasks")).await;
        assert_eq!(
            light["ok"], true,
            "a read with no git is not capped: {light}"
        );

        assert_eq!(
            answerer.walks.available_permits(),
            0,
            "still inside the deadline"
        );
        assert_eq!(answerer.blocking.available_permits(), MAX_IN_FLIGHT - 1);
        assert!(
            until(|| answerer.walks.available_permits() == MAX_ABANDONED_WALKS).await,
            "the cancelled walk did not return"
        );
        assert_eq!(answerer.blocking.available_permits(), MAX_IN_FLIGHT);
        let again = frame(&answerer, changes_ask("again", "SV-1", "commits")).await;
        assert_eq!(again["ok"], true, "{again}");
    }

    /// SV-ENG-121: a blocking walk that ignores its cancel flag keeps both permits past the
    /// deadline, until its closure returns.
    #[tokio::test]
    async fn a_walk_that_ignores_cancel_keeps_its_permits_until_it_returns() {
        let _release = ReleaseStuck;
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-bound",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let answerer = bound_answerer(data.path(), Duration::from_millis(500));
        let stuck = frame(&answerer, changes_ask("stuck", "SV-STUCK", "commits")).await;
        assert_eq!(stuck["error"], "failed", "{stuck}");
        sleep(Duration::from_millis(500) * (ABANDONED_WALK_ANSWERS + 4)).await;
        assert_eq!(
            answerer.walks.available_permits(),
            0,
            "released while the walk ran"
        );
        assert_eq!(answerer.blocking.available_permits(), MAX_IN_FLIGHT - 1);
        STUCK_RELEASE.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            until(|| answerer.walks.available_permits() == MAX_ABANDONED_WALKS
                && answerer.blocking.available_permits() == MAX_IN_FLIGHT)
            .await,
            "the permits did not return when the walk did"
        );
    }

    /// SV-ENG-121: one info line per query with its slug, query, `only` arrays, wait, run and
    /// outcome, and none of its argument values.
    #[test]
    fn a_query_line_names_the_query_and_no_argument_values() {
        let ask = Ask {
            id: json!("secret-id"),
            slug: "river".into(),
            query: "storyChanges".into(),
            args: json!({"storyId": "SV-SECRET-9", "only": ["commits", "prs", "/etc/passwd"], "cursor": "tok"}),
            share: Some("share-secret".into()),
            role: "viewer".into(),
            account: Some("acct-secret".into()),
        };
        let at = std::time::Instant::now();
        let line = query_line(&ask, at, Some(at), true, "failed");
        assert!(
            line.starts_with("live query slug=river query=storyChanges only=commits,prs "),
            "{line}"
        );
        assert!(
            line.contains(" wait_ms=") && line.contains(" run_ms="),
            "{line}"
        );
        assert!(line.ends_with("outcome=timeout"), "{line}");
        for secret in [
            "SV-SECRET-9",
            "passwd",
            "tok",
            "secret-id",
            "share-secret",
            "acct-secret",
        ] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
        assert!(query_line(&ask, at, None, false, "busy").ends_with("outcome=busy"));
        assert!(query_line(&ask, at, Some(at), false, "invalid_args").ends_with("outcome=invalid"));
    }

    /// Review S5: more than MAX_PENDING `q` frames at once are refused at once
    /// (`busy`), not queued.
    #[tokio::test(start_paused = true)]
    async fn pending_queries_are_bounded() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        for i in 0..MAX_PENDING + 6 {
            ask(&relay, &format!("w{i}"), "dash-a", "fixtureWait", json!({}));
        }
        sleep(secs(1)).await;
        let early = answers(&relay);
        assert_eq!(early.len(), 6, "{early:?}");
        assert!(early.values().all(|a| a["error"] == "busy"));
        sleep(ANSWER_TIMEOUT + secs(1)).await;
        assert_eq!(answers(&relay).len(), MAX_PENDING + 6);
        link.abort();
    }

    /// SV-PBF-287 case 6: on a saturated link the 9th query waits for a slot; past the queue
    /// bound it is answered `busy`, never `failed`.
    #[tokio::test(start_paused = true)]
    async fn a_saturated_link_answers_busy_past_the_queue_bound_never_failed() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        for i in 0..MAX_IN_FLIGHT {
            ask(&relay, &format!("hold-{i}"), "dash-a", "fixtureWait", json!({}));
        }
        sleep(Duration::from_millis(100)).await;
        ask(&relay, "ninth", "dash-a", "fixtureBusy", json!({}));
        sleep(QUEUE_TIMEOUT - Duration::from_millis(200)).await;
        assert!(
            !answers(&relay).contains_key("ninth"),
            "the ninth query did not wait for a slot"
        );
        sleep(Duration::from_millis(400)).await;
        let got = answers(&relay);
        assert_eq!(
            got["ninth"],
            json!({"t": "a", "id": "ninth", "ok": false, "error": "busy"})
        );
        assert_eq!(got.len(), 1, "a slot holder was answered early: {got:?}");
        link.abort();
    }

    /// SV-PBF-287 I4: the wait for a slot does not count toward K17. A query that waits two
    /// seconds for a slot and then runs nine still answers (before, it failed at ten seconds).
    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_slot_does_not_spend_the_answer_time() {
        let data = tempfile::tempdir().unwrap();
        let _a = live_dashboard_as(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n",
            Some(LiveQueries::All),
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        for i in 0..MAX_IN_FLIGHT {
            ask(&relay, &format!("hold-{i}"), "dash-a", "fixtureTwoSeconds", json!({}));
        }
        sleep(Duration::from_millis(100)).await;
        ask(&relay, "ninth", "dash-a", "fixtureNineSeconds", json!({}));
        sleep(ANSWER_TIMEOUT + Duration::from_millis(500)).await;
        assert!(
            !answers(&relay).contains_key("ninth"),
            "the ninth query was answered before it could have run nine seconds"
        );
        sleep(secs(2)).await;
        let got = answers(&relay);
        assert_eq!(got["ninth"]["ok"], true, "{:?}", got["ninth"]);
        assert_eq!(got["ninth"]["answer"], json!({"done": true}));
        for i in 0..MAX_IN_FLIGHT {
            assert_eq!(got[&format!("hold-{i}")]["ok"], true, "hold-{i}");
        }
        link.abort();
    }

    /// SW-DP-19: a published dashboard's edited live block takes effect with no
    /// publish, and a narrower one with no prompt. The watcher saves it on the record and sends R1
    /// with the new list; the Mac then answers with the new field. A removed
    /// block turns live off (R1 null, nothing answered). SV-PBF-286: a wider
    /// block is held, unanswered and unsent, until a person confirms it.
    #[tokio::test]
    async fn the_watcher_follows_an_edited_live_block() {
        use crate::dashboard_publish::client::fake::Fake;
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::tests::healthy;
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard(
            data.path(),
            "live:\n  queries: [sessionUsage]\n",
            Some(list(&["sessionUsage"])),
        );
        let fake = Fake::start(healthy).await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let answerer = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: LiveCtx::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let ask = |query: &str| Ask {
            id: json!("q"),
            slug: "ai-usage".into(),
            query: query.into(),
            args: Value::Null,
            share: None,
            role: "viewer".into(),
            account: None,
        };
        let r1s = || -> Vec<Value> {
            fake.calls()
                .iter()
                .filter(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/ai-usage")
                .map(|c| c.json()["live_queries"].clone())
                .collect()
        };
        let set_block = |block: &str| {
            let path = folder.path().join(MANIFEST_FILE);
            let text = fs::read_to_string(&path).unwrap();
            let head = &text[..text.find("live:").unwrap_or(text.len())];
            fs::write(&path, format!("{head}{block}")).unwrap();
        };

        // The saved block is the manifest's: nothing is sent.
        let manifest_text = || fs::read_to_string(folder.path().join(MANIFEST_FILE)).unwrap();
        let t0 = std::time::Instant::now();
        driver.tick(t0).await;
        assert!(r1s().is_empty(), "R1 sent for an unchanged block");
        let (_, fields, _, _, _) = answerer.approved(&ask("sessionUsage")).unwrap();
        assert!(!fields.contains(&"story".to_string()), "{fields:?}");

        // Fields added: held for a person, so nothing is sent or answered.
        set_block("live:\n  queries: [sessionUsage]\n  fields:\n    sessionUsage: [story]\n");
        driver.tick(t0 + secs(5)).await;
        assert!(r1s().is_empty(), "R1 sent for a held block");
        assert_held(data.path(), "ai-usage", Some(list(&["sessionUsage"])));
        assert!(
            answerer.approved(&ask("sessionUsage")).is_none(),
            "answered while held"
        );

        // Confirmed: saved, R1 sent, and the new field answered.
        confirm_held(data.path(), "ai-usage");
        driver.tick(t0 + secs(6)).await;
        assert_eq!(r1s(), vec![json!(["sessionUsage"])]);
        let sent = fake
            .calls()
            .into_iter()
            .find(|c| c.method == "PUT")
            .unwrap()
            .json();
        assert_eq!(
            (sent["title"].as_str(), sent["visibility"].as_str()),
            (Some("AI usage"), Some("private"))
        );
        let before = manifest_text();
        driver.tick(t0 + secs(7)).await;
        assert_eq!(manifest_text(), before, "the watcher wrote dashboard.yml");
        let record = state::load(data.path(), "ai-usage").unwrap();
        assert!(
            !record.live_pending,
            "R1 succeeded but the block is still pending"
        );
        let (_, fields, _, _, _) = answerer
            .approved(&ask("sessionUsage"))
            .expect("the edited block is answered");
        assert!(fields.contains(&"story".to_string()), "{fields:?}");
        assert!(answerer.approved(&ask("runs")).is_none());

        // A query added: held, then once confirmed answered, and R1 carries the new list once.
        set_block("live:\n  queries: [sessionUsage, runs]\n  fields:\n    sessionUsage: [story]\n");
        driver.tick(t0 + secs(10)).await;
        assert_eq!(r1s().len(), 1, "R1 sent for a held block");
        assert_held(data.path(), "ai-usage", Some(list(&["sessionUsage"])));
        assert!(
            answerer.approved(&ask("runs")).is_none(),
            "answered while held"
        );
        confirm_held(data.path(), "ai-usage");
        driver.tick(t0 + secs(15)).await;
        driver.tick(t0 + secs(16)).await;
        assert_eq!(r1s().len(), 2, "R1 sent again for an unchanged block");
        assert_eq!(r1s()[1], json!(["runs", "sessionUsage"]));
        assert!(answerer.approved(&ask("runs")).is_some());

        // Block removed: live off, R1 sends null, nothing answered.
        set_block("");
        driver.tick(t0 + secs(20)).await;
        assert_eq!(r1s().last(), Some(&Value::Null));
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().live_approved,
            None
        );
        assert!(answerer.approved(&ask("sessionUsage")).is_none());
        assert!(!wanted(data.path(), &Secret::new(TOKEN)));
    }

    /// SW-DP-19: an R1 that fails leaves the edited block pending; the
    /// watcher retries it after a minute, not on every pass, and the block is
    /// answered locally meanwhile. SV-PBF-286: the wider block is held, with no
    /// R1, until a person confirms it.
    #[tokio::test]
    async fn a_failed_r1_for_an_edited_block_is_retried_a_minute_later() {
        use crate::dashboard_publish::client::fake::{Fake, Reply};
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::tests::healthy;
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard(
            data.path(),
            "live:\n  queries: [runs]\n",
            Some(list(&["runs"])),
        );
        let first_put = AtomicBool::new(true);
        let fake = Fake::start(move |seen| {
            if seen.method == "PUT" && first_put.swap(false, Ordering::SeqCst) {
                Reply::Json(500, r#"{"error":"server_error"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("queries: [runs]", "queries: [runs, sessions]"),
        )
        .unwrap();
        let t0 = std::time::Instant::now();
        driver.tick(t0).await;
        assert!(
            fake.routes().is_empty(),
            "sent for a held block: {:?}",
            fake.routes()
        );
        assert_held(data.path(), "ai-usage", Some(list(&["runs"])));
        confirm_held(data.path(), "ai-usage");
        driver.tick(t0 + secs(1)).await;
        assert_eq!(fake.routes(), vec!["PUT /api/dash/v1/d/ai-usage"]);
        let record = state::load(data.path(), "ai-usage").unwrap();
        assert!(record.live_pending);
        assert_eq!(
            record.live_approved.unwrap().queries,
            list(&["runs", "sessions"])
        );
        driver.tick(t0 + secs(6)).await;
        assert_eq!(
            fake.routes(),
            vec!["PUT /api/dash/v1/d/ai-usage"],
            "R1 retried before the minute"
        );
        driver.tick(t0 + secs(62)).await;
        // The confirmed record rebuilt the publisher at t0 + 1 s, which scheduled a
        // debounced content push; its own R1 precedes /versions.
        assert_eq!(
            fake.routes(),
            vec![
                "PUT /api/dash/v1/d/ai-usage",
                "PUT /api/dash/v1/d/ai-usage",
                "PUT /api/dash/v1/d/ai-usage",
                "POST /api/dash/v1/d/ai-usage/versions",
                "PUT /api/dash/v1/d/ai-usage/versions/2/files?path=index.html",
                "POST /api/dash/v1/d/ai-usage/versions/2/commit",
            ],
            "expected one live retry followed by the debounced push"
        );
    }

    /// SW-DP-19 review: a re-publish that changes the block marks it pending,
    /// so an R1 that failed there is sent by the watcher later.
    #[tokio::test]
    async fn a_republished_block_whose_r1_failed_is_sent_by_the_watcher() {
        use crate::dashboard_publish::approval::{decide, LiveResult};
        use crate::dashboard_publish::client::fake::{Fake, Reply};
        use crate::dashboard_publish::client::Client;
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::{tests::healthy, Publisher};
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard_as(
            data.path(),
            "dash-repub",
            "live:\n  queries: [runs]\n",
            Some(list(&["runs"])),
        );
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("queries: [runs]", "queries: [runs, sessions]"),
        )
        .unwrap();

        // What publish_dashboard does on a published dashboard, with R1 down.
        let m = manifest::load(folder.path()).unwrap();
        let record = state::load(data.path(), "dash-repub").unwrap();
        let d = decide(&m, folder.path(), record.live_approved.as_ref(), None);
        assert_eq!(d.result, LiveResult::On);
        let saved =
            state::set_live_approved(data.path(), "dash-repub", folder.path(), d.approved).unwrap();
        assert!(saved.live_pending);
        let down = Fake::start(|_| Reply::Json(500, r#"{"error":"server_error"}"#.into())).await;
        let store: Arc<dyn CredentialStore> = token_store(TOKEN);
        let now = std::time::Instant::now();
        let mut p = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&down.base).unwrap(),
            store,
            now,
        )
        .unwrap();
        assert!(p.update_live(manifest::Visibility::Private).await.is_err());
        assert!(state::load(data.path(), "dash-repub").unwrap().live_pending);

        // The watcher sends it.
        let up = Fake::start(healthy).await;
        let mut driver = Driver::new(data.path(), Some(&up.base), token_store(TOKEN));
        driver.tick(std::time::Instant::now()).await;
        let r1: Vec<Value> = up
            .calls()
            .iter()
            .filter(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/dash-repub")
            .map(|c| c.json()["live_queries"].clone())
            .collect();
        assert_eq!(r1, vec![json!(["runs", "sessions"])]);
        assert!(!state::load(data.path(), "dash-repub").unwrap().live_pending);
    }

    /// SW-DP-19 review: while the token is refused, the watcher sends no R1
    /// for an edited block; the block stays pending. SV-PBF-286: the wider block
    /// is held until a person confirms it, then pends like any other.
    #[tokio::test]
    async fn no_r1_is_sent_while_the_token_is_refused() {
        use crate::dashboard_publish::client::fake::Fake;
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::{fingerprint, tests::healthy};
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard_as(
            data.path(),
            "dash-refused",
            "live:\n  queries: [runs]\n",
            Some(list(&["runs"])),
        );
        let mut record = state::load(data.path(), "dash-refused").unwrap();
        record.refused_token = Some(fingerprint(Some(&Secret::new(TOKEN))));
        state::save(data.path(), &record).unwrap();
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("queries: [runs]", "queries: [runs, sessions]"),
        )
        .unwrap();

        let fake = Fake::start(healthy).await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let t0 = std::time::Instant::now();
        driver.tick(t0).await;
        assert_held(data.path(), "dash-refused", Some(list(&["runs"])));
        confirm_held(data.path(), "dash-refused");
        driver.tick(t0 + secs(1)).await;
        driver.tick(t0 + secs(62)).await;
        assert!(
            fake.calls().is_empty(),
            "a refused token was sent: {:?}",
            fake.routes()
        );
        let saved = state::load(data.path(), "dash-refused").unwrap();
        assert!(saved.live_pending);
        assert_eq!(
            saved.live_approved.unwrap().queries,
            list(&["runs", "sessions"])
        );
    }

    /// SW-DP-19 review: an R1 the server refuses (409 slug_taken) is not
    /// retried; the pending mark is dropped. SV-PBF-286: the wider block is held,
    /// with no R1, until a person confirms it.
    #[tokio::test]
    async fn a_refused_r1_for_an_edited_block_is_not_retried() {
        use crate::dashboard_publish::client::fake::{Fake, Reply};
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::tests::healthy;
        let data = tempfile::tempdir().unwrap();
        let folder = live_dashboard_as(
            data.path(),
            "dash-taken",
            "live:\n  queries: [runs]\n",
            Some(list(&["runs"])),
        );
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("queries: [runs]", "queries: [runs, sessions]"),
        )
        .unwrap();
        let first_put = AtomicBool::new(true);
        let fake = Fake::start(move |seen| {
            if seen.method == "PUT" && first_put.swap(false, Ordering::SeqCst) {
                Reply::Json(409, r#"{"error":"slug_taken"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let t0 = std::time::Instant::now();
        driver.tick(t0).await;
        assert!(
            fake.routes().is_empty(),
            "sent for a held block: {:?}",
            fake.routes()
        );
        assert_held(data.path(), "dash-taken", Some(list(&["runs"])));
        confirm_held(data.path(), "dash-taken");
        driver.tick(t0 + secs(1)).await;
        assert_eq!(fake.routes(), vec!["PUT /api/dash/v1/d/dash-taken"]);
        assert!(!state::load(data.path(), "dash-taken").unwrap().live_pending);
        driver.tick(t0 + secs(62)).await;
        // The confirmed record rebuilt the publisher at t0 + 1 s, which scheduled a
        // debounced content push; its own R1 precedes /versions.
        let routes = fake.routes();
        assert_eq!(
            routes,
            vec![
                "PUT /api/dash/v1/d/dash-taken",
                "PUT /api/dash/v1/d/dash-taken",
                "POST /api/dash/v1/d/dash-taken/versions",
                "PUT /api/dash/v1/d/dash-taken/versions/2/files?path=index.html",
                "POST /api/dash/v1/d/dash-taken/versions/2/commit",
            ],
            "a refused R1 was retried outside the debounced push"
        );
        driver.tick(t0 + secs(123)).await;
        assert_eq!(fake.routes(), routes, "more requests after the push");
    }

    /// SW-DP-19 review: a published dashboard with no block saved (one added
    /// after publishing, or one declined in the old dialog before the upgrade)
    /// is turned on through the watcher: saved, sent with R1, and answered.
    /// SV-PBF-286: the watcher holds it, unsent and unanswered, until a person
    /// confirms it.
    #[tokio::test]
    async fn the_watcher_turns_on_a_block_with_none_saved() {
        use crate::dashboard_publish::client::fake::Fake;
        use crate::dashboard_publish::driver::Driver;
        use crate::dashboard_publish::push::tests::healthy;
        let data = tempfile::tempdir().unwrap();
        let _folder = live_dashboard_as(
            data.path(),
            "dash-added",
            "live:\n  queries: [runs]\n",
            None,
        );
        let answerer = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: LiveCtx::default(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let ask = Ask {
            id: json!("q"),
            slug: "dash-added".into(),
            query: "runs".into(),
            args: Value::Null,
            share: None,
            role: "viewer".into(),
            account: None,
        };
        assert!(
            answerer.approved(&ask).is_none(),
            "answered before it was saved"
        );

        let fake = Fake::start(healthy).await;
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let t0 = std::time::Instant::now();
        let r1 = || -> Vec<Value> {
            fake.calls()
                .iter()
                .filter(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/dash-added")
                .map(|c| c.json()["live_queries"].clone())
                .collect()
        };
        driver.tick(t0).await;
        assert!(r1().is_empty(), "R1 sent for a held block");
        assert_held(data.path(), "dash-added", None);
        assert!(answerer.approved(&ask).is_none(), "answered while held");

        confirm_held(data.path(), "dash-added");
        driver.tick(t0 + secs(1)).await;
        assert_eq!(r1(), vec![json!(["runs"])]);
        let record = state::load(data.path(), "dash-added").unwrap();
        assert_eq!(record.live_approved.unwrap().queries, list(&["runs"]));
        assert!(
            !record.live_pending,
            "R1 succeeded but the block is still pending"
        );
        assert!(
            answerer.approved(&ask).is_some(),
            "the added block is not answered"
        );
    }

    #[tokio::test]
    async fn inbox_dispatch_checks_owner_declaration_caps_and_logs_no_body() {
        let data = tempfile::tempdir().unwrap();
        let folder = stream_dashboard(data.path(), "inbox-test", "live:\n  streams: [inbox]\nactions:\n  askTeam:\n    verb: sendMessage\n    who: viewers\n    to: [human]\n    max_per_hour: 1\n");
        let (_, ctx, call) = super::super::inbox::tests::fixture();
        let mut record = state::load(data.path(), "inbox-test").unwrap();
        record.live_approved.as_mut().unwrap().scope = call.scope.clone();
        state::save(data.path(), &record).unwrap();
        let a = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: ctx.clone(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let marker = "BODY_STAYS_ONLY_IN_INBOX_26";
        let frame = json!({"id":"x", "dashboard":"inbox-test", "action":"ownerMessage", "args":{"to":"human", "text":marker}, "by":{"role":"owner", "account":"account"}});
        let run = |f| async { serde_json::from_str::<Value>(&a.action(f, Some("account".into())).await).unwrap() };
        let mut wrong = frame.clone(); wrong["by"]["account"] = "wrong".into();
        assert_eq!(run(wrong).await["error"], "not_allowed");
        let mut viewer = frame.clone(); viewer["by"] = json!({"role":"viewer", "share":"share"}); viewer["action"] = "askTeam".into();
        assert_eq!(run(viewer).await["error"], "not_allowed");
        let sent = run(frame.clone()).await; assert_eq!(sent["ok"], true, "{sent}");
        let id = sent["result"]["messageId"].clone();
        let log = fs::read_to_string(data.path().join("dashboard-publish/action-log.jsonl")).unwrap();
        assert!(!log.contains(marker));
        let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(line["to"], "human"); assert_eq!(line["message_id"], id); assert_eq!(line["length"], marker.len());
        let view = super::super::inbox::query(ctx, Call { folder: folder.path().into(), fields: vec!["text".into()], ..call }).await.unwrap();
        assert!(view["messages"].as_array().unwrap().iter().any(|m| m["text"] == marker));
        let mut declared = frame.clone(); declared["action"] = "askTeam".into(); declared["args"]["to"] = "role:triage".into();
        assert_eq!(run(declared.clone()).await["error"], "invalid_args");
        declared["args"]["to"] = "human".into();
        assert_eq!(run(declared.clone()).await["ok"], true);
        assert_eq!(run(declared).await["error"], "rate_limited");
        for _ in 0..58 { assert_eq!(run(frame.clone()).await["ok"], true); }
        assert_eq!(run(frame.clone()).await["error"], "rate_limited");
        let _second = stream_dashboard(data.path(), "inbox-second", "live:\n  streams: [inbox]\n");
        let mut record = state::load(data.path(), "inbox-second").unwrap();
        record.live_approved.as_mut().unwrap().scope = state::load(data.path(), "inbox-test").unwrap().live_approved.unwrap().scope;
        state::save(data.path(), &record).unwrap();
        let mut other_dashboard = frame; other_dashboard["dashboard"] = "inbox-second".into();
        assert_eq!(run(other_dashboard).await["error"], "rate_limited");
    }

    #[tokio::test]
    async fn owner_react_is_owner_only_team_scoped_allowlisted_toggling_and_capped() {
        let data = tempfile::tempdir().unwrap();
        let folder = stream_dashboard(data.path(), "react-test", "live:\n  streams: [inbox]\n");
        let (host, ctx, call) = super::super::inbox::tests::fixture();
        let mut record = state::load(data.path(), "react-test").unwrap();
        record.live_approved.as_mut().unwrap().scope = call.scope.clone();
        state::save(data.path(), &record).unwrap();
        let mine = host.insert("emea", "human", "mine");
        for _ in 0..3 { host.insert("other", "human", "foreign"); }
        let foreign = host.insert("other", "human", "foreign");
        assert!(foreign > mine, "the foreign id must not exist in the dashboard team");
        let a = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: ctx.clone(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let frame = |args: Value| json!({"id":"x", "dashboard":"react-test", "action":"ownerReact", "args":args, "by":{"role":"owner", "account":"account"}});
        let run = |f: Value| { let a = &a; async move { serde_json::from_str::<Value>(&a.action(f, Some("account".into())).await).unwrap() } };
        let react = |args: Value| run(frame(args));
        let reactions = |fields: &[&str]| {
            let (ctx, call, folder) = (ctx.clone(), call.clone(), folder.path().to_path_buf());
            let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
            async move { super::super::inbox::query(ctx, Call { folder, fields, ..call }).await.unwrap()["messages"][0]["reactions"].clone() }
        };

        // Owner only: a viewer, and another account's owner frame, are refused on the Mac too.
        let mut viewer = frame(json!({"messageId": mine, "emoji": "👍"})); viewer["by"] = json!({"role":"viewer", "share":"share"});
        assert_eq!(run(viewer).await["error"], "not_allowed");
        let mut wrong = frame(json!({"messageId": mine, "emoji": "👍"})); wrong["by"]["account"] = "wrong".into();
        assert_eq!(run(wrong).await["error"], "not_allowed");
        assert_eq!(reactions(&["reactions"]).await, json!([]));

        // Free text, multi-grapheme, non-allowlisted, missing or extra args never reach the store.
        for bad in [
            json!({"messageId": mine, "emoji": "🐙"}),
            json!({"messageId": mine, "emoji": "👍👍"}),
            json!({"messageId": mine, "emoji": "👍 "}),
            json!({"messageId": mine, "emoji": "x"}),
            json!({"messageId": mine, "emoji": ""}),
            json!({"messageId": mine, "emoji": "\u{2764}"}),
            json!({"messageId": 0, "emoji": "👍"}),
            json!({"messageId": -1, "emoji": "👍"}),
            json!({"messageId": mine}),
            json!({"emoji": "👍"}),
            json!({"messageId": mine, "emoji": "👍", "reactor": "agent"}),
            json!({"messageId": mine, "emoji": "👍", "remove": "yes"}),
        ] {
            assert_eq!(react(bad.clone()).await["error"], "invalid_args", "{bad}");
        }
        // Another team's message, and one that does not exist, are not found.
        assert_eq!(react(json!({"messageId": foreign, "emoji": "👍"})).await["error"], "not_found");
        assert_eq!(react(json!({"messageId": 9999, "emoji": "👍"})).await["error"], "not_found");
        assert_eq!(reactions(&["reactions"]).await, json!([]));

        // Toggle semantics: no `remove` toggles; explicit add and remove are idempotent.
        let ok = react(json!({"messageId": mine, "emoji": "👍"})).await;
        assert_eq!((ok["ok"].clone(), ok["result"]["on"].clone()), (json!(true), json!(true)), "{ok}");
        assert_eq!(reactions(&["reactions"]).await, json!([{"emoji":"👍", "count":1, "mine":true}]));
        assert_eq!(react(json!({"messageId": mine, "emoji": "👍", "remove": false})).await["result"]["on"], true);
        assert_eq!(react(json!({"messageId": mine, "emoji": "👍"})).await["result"]["on"], false);
        assert_eq!(react(json!({"messageId": mine, "emoji": "👍", "remove": true})).await["result"]["on"], false);
        assert_eq!(reactions(&["reactions"]).await, json!([]));
        for emoji in super::super::inbox::QUICK_REACTIONS {
            assert_eq!(react(json!({"messageId": mine, "emoji": emoji})).await["result"]["on"], true, "{emoji}");
        }
        assert_eq!(reactions(&["reactions"]).await.as_array().unwrap().len(), 6);
        assert_eq!(reactions(&[]).await, Value::Null);

        // The activity log keeps ids, never an email or the emoji's reactor.
        let log = fs::read_to_string(data.path().join("dashboard-publish/action-log.jsonl")).unwrap();
        let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!((line["action"].clone(), line["kind"].clone(), line["message_id"].clone(), line["outcome"].clone()), (json!("ownerReact"), json!("react"), json!(mine), json!("done")));
        assert!(!log.contains('@'), "{log}");

        // 120 an hour, on a cap of its own: sends still work, and the refusal is not charged.
        let used = log.lines().filter(|l| l.contains("\"react\"") && l.contains("\"done\"")).count();
        for _ in used..120 { assert_eq!(react(json!({"messageId": mine, "emoji": "😂"})).await["ok"], true); }
        assert_eq!(react(json!({"messageId": mine, "emoji": "😂"})).await["error"], "rate_limited");
        let sent = run(json!({"id":"x", "dashboard":"react-test", "action":"ownerMessage", "args":{"to":"human", "text":"still works"}, "by":{"role":"owner", "account":"account"}})).await;
        assert_eq!(sent["ok"], true, "{sent}");
    }

    #[tokio::test]
    async fn owner_mark_read_is_owner_only_team_scoped_human_mail_only_idempotent_and_capped() {
        let data = tempfile::tempdir().unwrap();
        let folder = stream_dashboard(data.path(), "mark-test", "live:\n  streams: [inbox]\n");
        let (host, ctx, call) = super::super::inbox::tests::fixture();
        let mut record = state::load(data.path(), "mark-test").unwrap();
        record.live_approved.as_mut().unwrap().scope = call.scope.clone();
        state::save(data.path(), &record).unwrap();
        let a_id = host.insert("emea", "human", "one");
        let b_id = host.insert("emea", "human", "two");
        let c_id = host.insert("emea", "human", "three");
        let agent_mail = host.insert("emea", "agent", "session mail");
        for _ in 0..6 { host.insert("other", "human", "foreign"); }
        let foreign = host.insert("other", "human", "foreign");
        assert!(foreign > agent_mail, "the foreign id must not exist in the dashboard team");
        let a = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx: ctx.clone(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let frame = |args: Value| json!({"id":"x", "dashboard":"mark-test", "action":"ownerMarkRead", "args":args, "by":{"role":"owner", "account":"account"}});
        let run = |f: Value| { let a = &a; async move { serde_json::from_str::<Value>(&a.action(f, Some("account".into())).await).unwrap() } };
        let mark = |ids: Value| run(frame(json!({"messageIds": ids})));
        let unread = || {
            let (ctx, call, folder) = (ctx.clone(), call.clone(), folder.path().to_path_buf());
            async move { super::super::inbox::query(ctx, Call { folder, ..call }).await.unwrap()["humanUnread"].clone() }
        };
        assert_eq!(unread().await, 3);

        // Owner only, on the Mac too: a viewer and another account's owner frame are refused.
        let mut viewer = frame(json!({"messageIds": [a_id]})); viewer["by"] = json!({"role":"viewer", "share":"share"});
        assert_eq!(run(viewer).await["error"], "not_allowed");
        let mut wrong = frame(json!({"messageIds": [a_id]})); wrong["by"]["account"] = "wrong".into();
        assert_eq!(run(wrong).await["error"], "not_allowed");
        assert_eq!(unread().await, 3);

        // Shape: 1 to 200 positive safe ids, nothing else.
        let too_many: Vec<i64> = (1..=201).collect();
        for bad in [
            json!({}), json!({"messageIds": []}), json!({"messageIds": too_many}), json!({"messageIds": [0]}), json!({"messageIds": [-1]}),
            json!({"messageIds": ["1"]}), json!({"messageIds": 1}), json!({"messageIds": [9007199254740993i64]}),
            json!({"messageIds": [a_id], "reader": "agent"}), json!({"messageId": a_id}),
        ] {
            assert_eq!(run(frame(bad.clone())).await["error"], "invalid_args", "{bad}");
        }
        assert_eq!(unread().await, 3);

        // Another team's id, an agent's mail and a missing id refuse the whole call: nothing is marked.
        for bad in [json!([foreign]), json!([a_id, foreign]), json!([agent_mail]), json!([a_id, agent_mail]), json!([9999])] {
            assert_eq!(mark(bad.clone()).await["error"], "not_found", "{bad}");
        }
        assert_eq!(unread().await, 3);

        // Marking is the app's own path: it changes the human's unread count, and repeats are idempotent.
        let ok = mark(json!([a_id, b_id])).await;
        assert_eq!((ok["ok"].clone(), ok["result"]["marked"].clone()), (json!(true), json!(2)), "{ok}");
        assert_eq!(unread().await, 1);
        assert_eq!(mark(json!([a_id, b_id])).await["result"]["marked"], 0);
        assert_eq!(mark(json!([c_id, c_id])).await["result"]["marked"], 1);
        assert_eq!(unread().await, 0);

        // The activity log keeps a count and ids, never an email.
        let log = fs::read_to_string(data.path().join("dashboard-publish/action-log.jsonl")).unwrap();
        let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!((line["action"].clone(), line["kind"].clone(), line["count"].clone(), line["marked"].clone(), line["outcome"].clone()), (json!("ownerMarkRead"), json!("mark_read"), json!(1), json!(1), json!("done")));
        assert!(!log.contains('@'), "{log}");

        // 600 calls an hour, on a cap of its own: a refusal is not charged, and sends and reactions keep theirs.
        let used = log.lines().filter(|l| l.contains("\"mark_read\"") && l.contains("\"done\"")).count();
        for _ in used..super::super::inbox::MARK_READ_PER_HOUR as usize { assert_eq!(mark(json!([a_id])).await["ok"], true); }
        assert_eq!(mark(json!([a_id])).await["error"], "rate_limited");
        let sent = run(json!({"id":"x", "dashboard":"mark-test", "action":"ownerMessage", "args":{"to":"human", "text":"still works"}, "by":{"role":"owner", "account":"account"}})).await;
        assert_eq!(sent["ok"], true, "{sent}");
    }

    /// The production dispatcher, over saved manifests: `media` answers only with the
    /// query listed and `media` in `live.fields.inbox`, and its base64 survives redaction.
    #[tokio::test]
    async fn media_is_answered_only_when_the_query_and_inbox_media_field_are_declared() {
        use base64::Engine as _;
        use crate::dashboard_publish::inbox::tests::{fixture, gif, image};
        let data = tempfile::tempdir().unwrap();
        let (host, ctx, call) = fixture();
        // Image bytes whose base64 holds an AWS-key lookalike, which the secret scan would mask.
        let mut bytes = b"GIF89a\x01\x00\x01\x00\x00\x00".to_vec();
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode("AKIAABCDEFGHIJKLMNOP")
                .unwrap(),
        );
        bytes.extend(gif(32));
        let sha = host.cache_media(&bytes);
        let id = host.insert("emea", "human", "hi");
        host.attach("emea", id, &[image(&sha, "cached")]);
        let answerer = Answerer {
            data_dir: data.path().into(),
            catalog: CATALOG,
            streams: STREAMS,
            ctx,
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
            walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)),
            timeout: ANSWER_TIMEOUT,
        };
        let ask = |slug: &str, query: &str, args: Value| Ask {
            id: json!("q"),
            slug: slug.into(),
            query: query.into(),
            args,
            share: None,
            role: "viewer".into(),
            account: None,
        };
        let publish = |slug: &str, block: &str| {
            let folder = stream_dashboard(data.path(), slug, block);
            let mut record = state::load(data.path(), slug).unwrap();
            record.live_approved.as_mut().unwrap().scope = call.scope.clone();
            state::save(data.path(), &record).unwrap();
            folder
        };
        let args = json!({"messageId": id, "sha256": sha});
        let reply = |frame: String| serde_json::from_str::<Value>(&frame).unwrap();

        let _no_query = publish("media-no-query", "live:\n  queries: [inbox]\n  fields:\n    inbox: [media]\n");
        let r = reply(answerer.answer(ask("media-no-query", "media", args.clone())).await);
        assert_eq!((r["ok"].clone(), r["error"].clone()), (json!(false), json!("not_approved")));

        let _no_field = publish("media-no-field", "live:\n  queries: [inbox, media]\n  fields:\n    inbox: [text, meta]\n");
        let r = reply(answerer.answer(ask("media-no-field", "media", args.clone())).await);
        assert_eq!((r["ok"].clone(), r["error"].clone()), (json!(false), json!("not_approved")));
        let r = reply(answerer.answer(ask("media-no-field", "inbox", json!({}))).await);
        assert!(!r.to_string().contains("attachments"), "{r}");

        let _declared = publish("media-ok", "live:\n  queries: [inbox, media]\n  fields:\n    inbox: [media]\n");
        let r = reply(answerer.answer(ask("media-ok", "media", args)).await);
        assert_eq!(r["ok"], true, "{r}");
        let data_b64 = r["answer"]["data"].as_str().unwrap();
        assert!(data_b64.contains("AKIAABCDEFGHIJKLMNOP"), "redaction changed the bytes");
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(data_b64).unwrap(), bytes);
        let r = reply(answerer.answer(ask("media-ok", "inbox", json!({}))).await);
        assert_eq!(r["answer"]["messages"][0]["attachments"][0]["sha256"], sha);
    }

    // ---- SW-DP-20a: streams ----

    /// A published dashboard whose `live` block is saved as publish saves it
    /// (`approval::decide`), streams included.
    fn stream_dashboard(data: &Path, slug: &str, block: &str) -> tempfile::TempDir {
        let folder = live_dashboard_as(data, slug, block, None);
        let m = manifest::load(folder.path()).unwrap();
        let decided = approval::decide(&m, folder.path(), None, None);
        state::set_live_approved(data, slug, folder.path(), decided.approved).unwrap();
        folder
    }

    fn open_stream(relay: &Relay, sid: &str, slug: &str, stream: &str, args: Value) {
        relay.send(Message::text(
            json!({"t": "s", "v": 1, "sid": sid, "dashboard": slug, "stream": stream, "args": args, "by": {"role": "viewer", "share": "share-1"}})
                .to_string(),
        ));
    }

    fn stream_frame(relay: &Relay, t: &str, sid: &str) {
        relay.send(Message::text(json!({"t": t, "sid": sid}).to_string()));
    }

    /// The JSON frames the Mac sent for `sid`, with when each arrived.
    fn frames_of(relay: &Relay, sid: &str) -> Vec<(Instant, Value)> {
        relay
            .timed_frames()
            .into_iter()
            .filter(|(_, f)| f != "ping")
            .map(|(at, f)| (at, serde_json::from_str::<Value>(&f).unwrap()))
            .filter(|(_, f)| f["sid"] == sid)
            .collect()
    }

    /// The bytes a stream frame carries against K27 and K28 (`data`, `vt`, or the value).
    fn payload(frame: &Value) -> usize {
        match frame["t"].as_str() {
            Some("o") => frame["data"].as_str().map_or(0, json_len),
            Some("snapshot") => json_len(frame["vt"].as_str().unwrap()),
            Some("v") => frame["value"].to_string().len(),
            _ => 0,
        }
    }

    /// SW-DP-20 AC-1: only a stream the saved block declares, in `STREAMS`, on
    /// a published dashboard opens; everything else is `not_declared`. Each `s`
    /// is in the view log, without its args.
    #[tokio::test(start_paused = true)]
    async fn only_declared_streams_open() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        // Live, but declares no streams.
        let _b = stream_dashboard(data.path(), "dash-b", "live:\n  queries: []\n");
        // Saved, then the block was edited and not yet followed.
        let c = stream_dashboard(data.path(), "dash-c", "live:\n  queries: []\n");
        let path = c.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("queries: []", "queries: []\n  streams: [tick]"),
        )
        .unwrap();
        // Saved, then unpublished on this Mac.
        let _d = stream_dashboard(data.path(), "dash-d", "live:\n  streams: [tick]\n");
        let mut d = state::load(data.path(), "dash-d").unwrap();
        d.apply(Event::Unpublish).unwrap();
        state::save(data.path(), &d).unwrap();

        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        let args = json!({"every_ms": 500, "note": "csp_args_never_logged"});
        open_stream(
            &relay,
            "declared",
            "dash-a",
            "tick",
            json!({"every_ms": 500}),
        );
        open_stream(&relay, "bad-args", "dash-a", "tick", args);
        open_stream(&relay, "undeclared", "dash-b", "tick", json!({}));
        open_stream(&relay, "edited", "dash-c", "tick", json!({}));
        open_stream(&relay, "unknown", "dash-a", "terminal", json!({}));
        open_stream(&relay, "unpublished", "dash-d", "tick", json!({}));
        open_stream(&relay, "no-dashboard", "../etc", "tick", json!({}));
        // Another protocol version is not read as this one.
        relay.send(Message::text(
            json!({"t": "s", "v": 2, "sid": "v2", "dashboard": "dash-a", "stream": "tick", "args": {}})
                .to_string(),
        ));
        sleep(secs(2)).await;
        assert!(frames_of(&relay, "v2").is_empty());

        let open = frames_of(&relay, "declared");
        assert_eq!(
            open[0].1,
            json!({"t": "so", "v": 1, "sid": "declared", "ok": true})
        );
        assert_eq!(open[1].1["t"], "snapshot");
        assert_eq!(
            (open[1].1["part"].clone(), open[1].1["last"].clone()),
            (json!(0), json!(true))
        );
        assert!(open[2..].iter().all(|(_, f)| f["t"] == "o"), "{open:?}");
        assert!(open.len() >= 5, "{open:?}");
        assert_eq!(
            frames_of(&relay, "bad-args")
                .iter()
                .map(|(_, f)| f.clone())
                .collect::<Vec<_>>(),
            vec![
                json!({"t": "so", "v": 1, "sid": "bad-args", "ok": false, "error": "invalid_args"})
            ]
        );
        for sid in [
            "undeclared",
            "edited",
            "unknown",
            "unpublished",
            "no-dashboard",
        ] {
            let got: Vec<Value> = frames_of(&relay, sid).into_iter().map(|(_, f)| f).collect();
            assert_eq!(
                got,
                vec![json!({"t": "so", "v": 1, "sid": sid, "ok": false, "error": "not_declared"})],
                "{sid}"
            );
        }

        let lines = view_log(data.path());
        assert_eq!(lines.len(), 7, "{lines:?}");
        let ok = lines.iter().find(|l| l["outcome"] == "ok").unwrap();
        assert_eq!(
            (ok["slug"].as_str(), ok["stream"].as_str()),
            (Some("dash-a"), Some("tick"))
        );
        assert_eq!(ok["share_id"], "share-1");
        assert_eq!(ok["role"], "viewer");
        // The same seven keys as a query's line, with `stream` for `query`.
        assert_eq!(ok.as_object().unwrap().len(), 7);
        let log_text = fs::read_to_string(view_log_path(data.path())).unwrap();
        assert!(!log_text.contains("csp_args_never_logged"));
        link.abort();
    }

    /// SW-DP-20 AC-1, failure modes: `snap` sends the snapshot again; a value
    /// stream sends its value; editing the block to drop the stream ends it
    /// (`end` `not_declared`) at the next pass.
    #[tokio::test(start_paused = true)]
    async fn snap_value_and_a_dropped_declaration() {
        let data = tempfile::tempdir().unwrap();
        let folder = stream_dashboard(
            data.path(),
            "dash-a",
            "live:\n  streams: [tick, tickValue]\n",
        );
        // Another live dashboard keeps the link up when dash-a's block changes.
        let _b = stream_dashboard(data.path(), "dash-b", "live:\n  queries: []\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(&relay, "t", "dash-a", "tick", json!({"every_ms": 1000}));
        open_stream(
            &relay,
            "v",
            "dash-a",
            "tickValue",
            json!({"every_ms": 1000}),
        );
        sleep(Duration::from_millis(2500)).await;
        stream_frame(&relay, "snap", "t");
        sleep(Duration::from_millis(100)).await;
        let t = frames_of(&relay, "t");
        let kinds: Vec<&str> = t.iter().map(|(_, f)| f["t"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["so", "snapshot", "o", "o", "snapshot"], "{t:?}");
        let seqs: Vec<u64> = t[1..]
            .iter()
            .map(|(_, f)| f["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [1, 2, 3, 4], "seq grows by one per frame");
        let values: Vec<Value> = frames_of(&relay, "v")[1..]
            .iter()
            .map(|(_, f)| f["value"].clone())
            .collect();
        assert_eq!(values, [json!({"n": 0}), json!({"n": 1}), json!({"n": 2})]);

        // The block drops tickValue: both streams' saved block no longer matches.
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("streams: [tick, tickValue]", "streams: [tick]"),
        )
        .unwrap();
        // Two passes in a row find it undeclared (one read caught mid-save is not enough).
        sleep(PASS * 2 + secs(1)).await;
        for sid in ["t", "v"] {
            let last = frames_of(&relay, sid).last().unwrap().1.clone();
            assert_eq!(
                last,
                json!({"t": "end", "v": 1, "sid": sid, "reason": "not_declared"}),
                "{sid}"
            );
        }
        let count = relay.frames().len();
        sleep(secs(10)).await;
        assert!(relay.frames()[count..]
            .iter()
            .all(|f| f == "ping" || !f.contains("\"sid\"")));
        link.abort();
    }

    /// SW-DP-20 AC-2 (DP-INV-5 as amended): with no stream open only `ping`
    /// crosses the link; after `u`, or when the link drops, no frame of that
    /// stream comes within K32 or later.
    #[tokio::test(start_paused = true)]
    async fn stream_frames_only_while_open() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(60)).await;
        assert!(
            relay.frames().iter().all(|f| f == "ping"),
            "a frame with no stream open"
        );

        open_stream(&relay, "s1", "dash-a", "tick", json!({"every_ms": 200}));
        sleep(secs(60)).await;
        stream_frame(&relay, "u", "s1");
        let closed_at = Instant::now();
        sleep(secs(120)).await;
        let s1 = frames_of(&relay, "s1");
        assert!(s1.len() > 100, "the stream did not run: {}", s1.len());
        assert!(
            s1.iter().all(|(at, _)| *at <= closed_at + STREAM_STOP),
            "a frame after u + K32"
        );

        // A second stream, then the link drops: nothing of it on the next link.
        open_stream(&relay, "s2", "dash-a", "tick", json!({"every_ms": 200}));
        sleep(secs(5)).await;
        relay.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        })));
        let dropped_at = Instant::now();
        sleep(secs(55)).await;
        assert_eq!(relay.connections(), 2, "the link did not come back");
        assert!(frames_of(&relay, "s2")
            .iter()
            .all(|(at, _)| *at <= dropped_at + STREAM_STOP));
        let after: Vec<String> = relay
            .timed_frames()
            .into_iter()
            .filter(|(at, _)| *at > dropped_at + STREAM_STOP)
            .map(|(_, f)| f)
            .collect();
        assert!(
            !after.is_empty() && after.iter().all(|f| f == "ping"),
            "{after:?}"
        );
        link.abort();
    }

    /// Asserts K27 and K28 over every frame of a stream: no part over 64 KB, at
    /// least 50 ms apart, at most 256 KB in any one second.
    fn assert_paced(frames: &[(Instant, Value)]) {
        for (at, f) in frames {
            assert!(
                payload(f) <= STREAM_FRAME_BYTES,
                "a frame over K27 at {at:?}"
            );
        }
        let sent: Vec<&(Instant, Value)> = frames.iter().filter(|(_, f)| f["t"] != "so").collect();
        for pair in sent.windows(2) {
            assert!(
                pair[1].0 - pair[0].0 >= STREAM_FRAME_EVERY,
                "frames closer than 50 ms"
            );
        }
        for (i, (start, _)) in sent.iter().enumerate() {
            let in_second: usize = sent[i..]
                .iter()
                .take_while(|(at, _)| *at < *start + SECOND)
                .map(|(_, f)| payload(f))
                .sum();
            assert!(
                in_second <= STREAM_BYTES_PER_SEC,
                "{in_second} bytes in one second"
            );
        }
    }

    /// SW-DP-20 AC-3: a burst under K28 is coalesced and delivered whole; a
    /// 2 MB second is paced, then dropped for a `gap` and a fresh snapshot.
    #[tokio::test(start_paused = true)]
    async fn streams_are_paced_and_a_flood_gaps() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;

        // 200 KB in 4 ms: coalesced into a few frames, nothing dropped.
        open_stream(
            &relay,
            "burst",
            "dash-a",
            "tick",
            json!({"chunk": 50_000, "every_ms": 1, "items": 4}),
        );
        sleep(secs(5)).await;
        let burst = frames_of(&relay, "burst");
        assert_paced(&burst);
        let output: usize = burst
            .iter()
            .filter(|(_, f)| f["t"] == "o")
            .map(|(_, f)| payload(f))
            .sum();
        assert_eq!(output, 200_000);
        assert!(burst.iter().filter(|(_, f)| f["t"] == "o").count() <= 4);
        assert!(!burst.iter().any(|(_, f)| f["gap"] == true));

        // 2 MB in about one second.
        open_stream(
            &relay,
            "flood",
            "dash-a",
            "tick",
            json!({"chunk": 65_536, "every_ms": 31, "items": 32}),
        );
        sleep(secs(10)).await;
        let flood = frames_of(&relay, "flood");
        assert_paced(&flood);
        let gap = flood
            .iter()
            .position(|(_, f)| f["gap"] == true)
            .expect("no gap frame");
        assert_eq!(
            flood[gap].1,
            json!({"t": "o", "v": 1, "sid": "flood", "gap": true})
        );
        let next = flood[gap + 1..]
            .iter()
            .find(|(_, f)| f["t"] != "o" || f["gap"] != true)
            .unwrap();
        assert_eq!(
            (next.1["t"].as_str(), next.1["part"].as_u64()),
            (Some("snapshot"), Some(0))
        );
        let output: usize = flood
            .iter()
            .filter(|(_, f)| f["t"] == "o")
            .map(|(_, f)| payload(f))
            .sum();
        assert!(output < 2 * 1024 * 1024, "nothing was dropped");
        let seqs: Vec<u64> = flood
            .iter()
            .filter_map(|(_, f)| f["seq"].as_u64())
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "seq is not one per frame"
        );
        link.abort();
    }

    /// SW-DP-20 AC-4: one `STREAMS` entry is enough: valid in a manifest
    /// (above), listed in the contract, and opened (the tests above) with no
    /// other change.
    #[test]
    fn one_stream_entry_is_valid_listed_and_openable() {
        let hosted = crate::mcp::live_data_authoring::local_contract_payload();
        let available = hosted["hosted"]["streams"]["available"]
            .as_array()
            .unwrap()
            .clone();
        let names: Vec<&str> = available
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "actions",
                "workspaces",
                "terminals",
                "terminal",
                "inbox",
                "tick",
                "tickValue"
            ]
        );
        assert_eq!(
            available.iter().find(|s| s["name"] == "tick").unwrap()["kind"],
            "bytes"
        );
        assert_eq!(
            available.iter().find(|s| s["name"] == "tickValue").unwrap()["describe"],

            "Test stream: a counter value on a timer"
        );
        assert!(catalog::find_stream(STREAMS, "tick").is_some());
    }

    /// Review round 1: a value over K19 ends the stream (`too_large`) and is never
    /// sent; past MAX_OPEN_STREAMS an `s` is refused `busy`.
    #[tokio::test(start_paused = true)]
    async fn stream_limits_too_large_and_busy() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(
            data.path(),
            "dash-a",
            "live:\n  streams: [tick, tickValue]\n",
        );
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(
            &relay,
            "huge",
            "dash-a",
            "tickValue",
            json!({"pad": MAX_ANSWER_BYTES}),
        );
        sleep(secs(1)).await;
        let huge: Vec<Value> = frames_of(&relay, "huge")
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        assert_eq!(
            huge,
            vec![
                json!({"t": "so", "v": 1, "sid": "huge", "ok": true}),
                json!({"t": "end", "v": 1, "sid": "huge", "reason": "too_large"})
            ]
        );

        for i in 0..MAX_OPEN_STREAMS {
            open_stream(
                &relay,
                &format!("s{i}"),
                "dash-a",
                "tick",
                json!({"every_ms": 60_000}),
            );
        }
        open_stream(&relay, "over", "dash-a", "tick", json!({}));
        sleep(secs(1)).await;
        assert_eq!(frames_of(&relay, "s0")[0].1["ok"], true);
        assert_eq!(
            frames_of(&relay, "over")[0].1,
            json!({"t": "so", "v": 1, "sid": "over", "ok": false, "error": "busy"})
        );
        link.abort();
    }

    /// Review round 3: a stream task that fails ends with `end failed` and leaves
    /// the table (a new `s` for the same sid opens again).
    #[tokio::test(start_paused = true)]
    async fn a_failed_stream_task_ends_failed() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(
            &relay,
            "p",
            "dash-a",
            "tick",
            json!({"every_ms": 100, "panic_after": 1}),
        );
        sleep(secs(1)).await;
        let last = frames_of(&relay, "p").last().unwrap().1.clone();
        assert_eq!(
            last,
            json!({"t": "end", "v": 1, "sid": "p", "reason": "failed"})
        );
        open_stream(&relay, "p", "dash-a", "tick", json!({"every_ms": 100}));
        sleep(secs(1)).await;
        let oks = frames_of(&relay, "p")
            .iter()
            .filter(|(_, f)| f["t"] == "so" && f["ok"] == true)
            .count();
        assert_eq!(oks, 2);
        assert_eq!(relay.connections(), 1, "the link survived the failed task");
        link.abort();
    }

    /// Review round 4: a repeated `s` for an open sid replaces that stream:
    /// the first one's frames stop, and one stream sends from then on.
    #[tokio::test(start_paused = true)]
    async fn a_repeated_s_replaces_the_open_stream() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(
            &relay,
            "r",
            "dash-a",
            "tick",
            json!({"every_ms": 100, "chunk": 1}),
        );
        sleep(secs(1)).await;
        open_stream(
            &relay,
            "r",
            "dash-a",
            "tick",
            json!({"every_ms": 100, "chunk": 2}),
        );
        sleep(secs(2)).await;
        let frames = frames_of(&relay, "r");
        let second = frames
            .iter()
            .rposition(|(_, f)| f["t"] == "so")
            .expect("a second so");
        // After the second `so`: the new stream's snapshot, then only its 2-byte outputs.
        let after: Vec<&Value> = frames[second + 1..].iter().map(|(_, f)| f).collect();
        assert_eq!(after[0]["t"], "snapshot");
        assert!(after[1..].len() >= 10);
        assert!(after[1..]
            .iter()
            .all(|f| f["t"] == "o" && f["data"].as_str().unwrap().len() % 2 == 0));
        link.abort();
    }

    /// Steady output (below K28) while a large snapshot goes out waits behind it
    /// and is sent after it: no gap, no new snapshot.
    #[test]
    fn steady_output_behind_a_long_snapshot_does_not_gap() {
        let snap = tokio::sync::Notify::new();
        let mut pacer = Pacer::new("s".into());
        pacer.take(
            StreamItem::Snapshot {
                vt: "a".repeat(STREAM_FRAME_BYTES * 16),
                cols: None,
                rows: None,
            },
            &snap,
        );
        // 64 KB/s for the 4 s the 1 MB snapshot takes.
        for _ in 0..4 {
            pacer.take(StreamItem::Bytes("x".repeat(64 * 1024)), &snap);
        }
        let mut at = Instant::now();
        let mut gaps = 0;
        let mut output = 0;
        while let Some(frame) = pacer.send_next(at) {
            let f: Value = serde_json::from_str(&frame).unwrap();
            if f["gap"] == true {
                gaps += 1;
            }
            output += f["data"].as_str().map_or(0, str::len);
            at += SECOND;
        }
        assert_eq!((gaps, output), (0, 4 * 64 * 1024));
        assert!(!pacer.want_snapshot);
    }

    /// Review round 5: a flood while a large snapshot is still going out does not
    /// drop it for a newer one (which could repeat for ever): it finishes, then
    /// the gap goes out and a fresh snapshot is asked for.
    #[test]
    fn a_flood_during_a_snapshot_lets_it_finish_first() {
        let snap = tokio::sync::Notify::new();
        let mut pacer = Pacer::new("s".into());
        pacer.take(
            StreamItem::Snapshot {
                vt: "a".repeat(STREAM_FRAME_BYTES * 3),
                cols: None,
                rows: None,
            },
            &snap,
        );
        // More than a second's worth on top of the snapshot still queued: a flood.
        let flood = "x".repeat(STREAM_BYTES_PER_SEC / 2 + 1);
        for _ in 0..5 {
            pacer.take(StreamItem::Bytes(flood.clone()), &snap);
        }
        let mut at = Instant::now();
        let mut kinds = Vec::new();
        while let Some(frame) = pacer.send_next(at) {
            let f: Value = serde_json::from_str(&frame).unwrap();
            kinds.push(if f["gap"] == true {
                "gap".to_string()
            } else {
                f["t"].as_str().unwrap().to_string()
            });
            at += SECOND;
        }
        assert_eq!(kinds, ["snapshot", "snapshot", "snapshot", "gap"]);
        assert!(pacer.want_snapshot, "no fresh snapshot was asked for");
    }

    /// One pass that cannot confirm the declaration (a manifest caught mid-save)
    /// does not end a stream; the next good read resets the count.
    #[tokio::test(start_paused = true)]
    async fn one_missed_pass_does_not_end_a_stream() {
        let data = tempfile::tempdir().unwrap();
        let folder = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        // Another live dashboard keeps the link up while dash-a's manifest is unreadable.
        let _b = stream_dashboard(data.path(), "dash-b", "live:\n  queries: []\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(&relay, "m", "dash-a", "tick", json!({"every_ms": 500}));
        sleep(secs(1)).await;
        let path = folder.path().join(MANIFEST_FILE);
        let good = fs::read_to_string(&path).unwrap();
        for _ in 0..3 {
            // Unreadable for one pass, then back.
            fs::write(&path, "not: [valid").unwrap();
            sleep(PASS).await;
            fs::write(&path, &good).unwrap();
            sleep(PASS).await;
        }
        assert!(
            !frames_of(&relay, "m").iter().any(|(_, f)| f["t"] == "end"),
            "one missed pass ended the stream"
        );
        let count = frames_of(&relay, "m").len();
        sleep(secs(2)).await;
        assert!(frames_of(&relay, "m").len() > count, "the stream stopped");
        link.abort();
    }

    /// DP-INV-5 as amended: a frame queued by a stream that was stopped since
    /// (`u`, a repeated `s`, undeclared) is not delivered; answers always are.
    #[test]
    fn frames_of_a_stopped_stream_are_not_delivered() {
        let snap = Arc::new(tokio::sync::Notify::new());
        let mut table = StreamTable::default();
        let handle = |table: &mut StreamTable, sid: &str, generation: u64| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let task = rt.block_on(async { tokio::spawn(async {}).abort_handle() });
            table.open.insert(
                sid.into(),
                OpenStream {
                    slug: "dash-a".into(),
                    name: "tick",
                    snap: snap.clone(),
                    task,
                    generation,
                    misses: 0,
                    input: InputTarget {
                        folder: PathBuf::new(),
                        scope: None,
                        gate: InputGate::new(false),
                        handle: None,
                    },
                },
            );
        };
        handle(&mut table, "a", 1);
        handle(&mut table, "b", 2);
        assert!(
            table.deliverable(Some(1)) && table.deliverable(Some(2)) && table.deliverable(None)
        );
        table.stop("a");
        assert!(
            !table.deliverable(Some(1)),
            "a stopped stream's queued frame went out"
        );
        assert!(table.deliverable(Some(2)) && table.deliverable(None));
        for g in 3..5000 {
            table.retired.insert(g);
            table.retired_order.push_back(g);
        }
        table.stop("b");
        assert!(
            !table.deliverable(Some(2)),
            "the newest retired generation was forgotten"
        );
        // A stream that ended on its own keeps its queued `end`; a new `s` for its
        // sid drops it, so it cannot end the new open.
        handle(&mut table, "c", 6000);
        table.finish("c");
        assert!(
            table.deliverable(Some(6000)),
            "a finished stream's `end` was dropped"
        );
        table.stop("c");
        assert!(
            !table.deliverable(Some(6000)),
            "an old `end` would reach the new open"
        );
        // Bounded oldest first: a sid that finished, was re-opened and finished again has
        // two queued entries; popping the older one must not drop the newer generation.
        let mut table = StreamTable::default();
        handle(&mut table, "x", 100);
        table.finish("x");
        table.stop("x"); // the re-open: generation 100 retired
        handle(&mut table, "x", 200);
        table.finish("x");
        for i in 0..4095u64 {
            handle(&mut table, &format!("f{i}"), 1000 + i);
            table.finish(&format!("f{i}"));
        }
        assert_eq!(
            table.finished.get("x"),
            Some(&200),
            "the newer generation was dropped"
        );
        assert!(table.finished.len() <= 4096);
        table.stop("x");
        assert!(
            !table.deliverable(Some(200)),
            "an old `end` would reach a re-open"
        );
    }

    /// The link's stream cap is twice the relay's own K31 ceiling (lingering streams).
    #[cfg(cs_site)]
    #[test]
    fn the_open_stream_cap_follows_the_relays_k31() {
        const RELAY: &str = include_str!("../../../site/worker/live/relay.ts");
        let number = |name: &str| -> usize {
            let re = regex::Regex::new(&format!(r"export const {name} = (\d+);")).unwrap();
            re.captures(RELAY).unwrap_or_else(|| panic!("{name}"))[1]
                .parse()
                .unwrap()
        };
        assert_eq!(
            MAX_OPEN_STREAMS,
            2 * number("MAX_WATCHES") * number("MAX_VIEWER_SOCKETS")
        );
    }

    /// Review round 1: a newer snapshot supersedes the queued parts of an older one.
    #[test]
    fn a_newer_snapshot_drops_older_queued_parts() {
        let snap = tokio::sync::Notify::new();
        let mut pacer = Pacer::new("s".into());
        let big = |c: &str| StreamItem::Snapshot {
            vt: c.repeat(STREAM_FRAME_BYTES * 2),
            cols: None,
            rows: None,
        };
        pacer.take(big("a"), &snap);
        pacer.take(big("b"), &snap);
        assert_eq!(pacer.queue.len(), 2);
        assert!(pacer
            .queue
            .iter()
            .all(|o| matches!(o, Out::Part { vt, .. } if vt.starts_with('b'))));
    }

    /// K27/K28 count the JSON on the wire: output full of ESC bytes (6 bytes
    /// each as JSON) is cut so no frame's data is over 64 KB as JSON.
    #[test]
    fn k27_counts_escaped_bytes() {
        let snap = tokio::sync::Notify::new();
        let mut pacer = Pacer::new("s".into());
        pacer.take(StreamItem::Bytes("\u{1b}[0m".repeat(20_000)), &snap);
        let mut at = Instant::now();
        let mut total = 0;
        while let Some(frame) = pacer.send_next(at) {
            let f: Value = serde_json::from_str(&frame).unwrap();
            let data = f["data"].as_str().unwrap();
            assert!(serde_json::to_string(data).unwrap().len() - 2 <= STREAM_FRAME_BYTES);
            total += data.len();
            at += SECOND;
        }
        assert_eq!(total, 4 * 20_000);
        assert_eq!(json_len("\u{1b}a\"\n"), 6 + 1 + 2 + 2);
        let parts = split_parts(&"\u{1b}".repeat(STREAM_FRAME_BYTES), STREAM_FRAME_BYTES);
        assert!(parts.iter().all(|p| json_len(p) <= STREAM_FRAME_BYTES));
        assert_eq!(parts.concat().len(), STREAM_FRAME_BYTES);
    }

    /// K28's one-second backlog is counted as sent: ESC-heavy output gaps once
    /// its JSON passes a second's worth, not six times later.
    #[test]
    fn k28_backlog_counts_escaped_bytes() {
        let snap = tokio::sync::Notify::new();
        let mut pacer = Pacer::new("s".into());
        // 50 000 ESC bytes are 300 000 bytes as JSON: past 256 KB.
        pacer.take(StreamItem::Bytes("\u{1b}".repeat(25_000)), &snap);
        pacer.take(StreamItem::Bytes("\u{1b}".repeat(25_000)), &snap);
        assert!(pacer.queue.iter().any(|o| matches!(o, Out::Gap)));
    }

    #[test]
    fn snapshots_split_at_k27_on_char_boundaries() {
        let text = "é".repeat(STREAM_FRAME_BYTES);
        let parts = split_parts(&text, STREAM_FRAME_BYTES);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.len() <= STREAM_FRAME_BYTES));
        assert_eq!(parts.concat(), text);
        assert_eq!(split_parts("", STREAM_FRAME_BYTES), vec![String::new()]);
    }

    // ---- SW-DP-21: the agent streams on the link ----

    /// `stream_dashboard` published while CodeSense knew its team.
    fn stream_dashboard_in(
        data: &Path,
        slug: &str,
        block: &str,
        team: (&str, &str),
    ) -> tempfile::TempDir {
        let folder = live_dashboard_as(data, slug, block, None);
        let m = manifest::load(folder.path()).unwrap();
        let decided = approval::decide(&m, folder.path(), None, Some(team));
        state::set_live_approved(data, slug, folder.path(), decided.approved).unwrap();
        folder
    }

    fn start_with_agents(
        data: &Path,
        relay: &Relay,
        host: &Arc<crate::dashboard_publish::agents::tests::FakeHost>,
    ) -> tokio::task::JoinHandle<()> {
        let store: Arc<dyn CredentialStore> = token_store(TOKEN);
        let agents: Arc<dyn crate::dashboard_publish::agents::AgentHost> = host.clone();
        let link = LiveLink::new(data, relay.clone(), store)
            .with_catalog(TEST_CATALOG)
            .with_ctx(LiveCtx {
                agents: Some(agents),
                ..Default::default()
            });
        tokio::spawn(link.run())
    }

    /// SW-DP-21 AC-3, AC-5, AC-6 on the link: `so`, the snapshot with the
    /// terminal's size, ordered output with a seq per frame, a resize (`rs`), and
    /// on exit `end exited`; a second terminal's `u` unsubscribes within K32. The
    /// host saw subscribes and unsubscribes only. An unknown handle is `so`
    /// `not_found`; a folder in no team, `no_team`.
    #[tokio::test(start_paused = true)]
    async fn a_terminal_streams_over_the_link() {
        use crate::dashboard_publish::agents::tests::{Call as HostCall, FakeHost};
        use crate::dashboard_publish::agents::TermEvent;
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard_in(
            data.path(),
            "dash-a",
            "live:\n  streams: [terminal, terminals]\n",
            ("acme", "emea"),
        );
        let host = Arc::new(FakeHost::new());
        host.launch("a1", Some(("acme", "emea")), "SW-1", Some("SW-1"));
        host.launch("a2", Some(("acme", "emea")), "ops", None);
        let relay = Relay::new(Mode::Answer);
        let link = start_with_agents(data.path(), &relay, &host);
        sleep(secs(1)).await;
        open_stream(&relay, "t1", "dash-a", "terminal", json!({"handle": "a1"}));
        open_stream(
            &relay,
            "nope",
            "dash-a",
            "terminal",
            json!({"handle": "b9"}),
        );
        sleep(Duration::from_millis(200)).await;
        host.emit(TermEvent::Output {
            data: "one\r\n".into(),
            seq: 110,
        });
        sleep(Duration::from_millis(100)).await;
        host.emit(TermEvent::Resized {
            cols: 100,
            rows: 40,
        });
        host.emit(TermEvent::Output {
            data: "two\r\n".into(),
            seq: 120,
        });
        sleep(Duration::from_millis(200)).await;
        host.emit(TermEvent::Exited);
        sleep(Duration::from_millis(200)).await;
        let got: Vec<Value> = frames_of(&relay, "t1")
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        let kinds: Vec<&str> = got.iter().map(|f| f["t"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["so", "snapshot", "o", "rs", "o", "end"], "{got:?}");
        assert_eq!(got[1]["vt"], "$ claude\r\nhello\r\n");
        assert_eq!(
            (got[1]["cols"].clone(), got[1]["rows"].clone()),
            (json!(120), json!(30))
        );
        assert_eq!(got[2]["data"], "one\r\n");
        assert_eq!(
            got[3],
            json!({"t": "rs", "v": 1, "sid": "t1", "cols": 100, "rows": 40})
        );
        assert_eq!(got[5]["reason"], "exited");
        let seqs: Vec<u64> = [1, 2, 4]
            .iter()
            .map(|&i| got[i]["seq"].as_u64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
        assert_eq!(
            frames_of(&relay, "nope")[0].1,
            json!({"t": "so", "v": 1, "sid": "nope", "ok": false, "error": "not_found"})
        );
        assert_eq!(
            host.calls(),
            [
                HostCall::Subscribe("sess-a1".into()),
                HostCall::Unsubscribe(1)
            ]
        );

        open_stream(&relay, "t2", "dash-a", "terminal", json!({"handle": "a2"}));
        sleep(Duration::from_millis(200)).await;
        stream_frame(&relay, "u", "t2");
        sleep(STREAM_STOP).await;
        assert_eq!(
            host.calls()[2..],
            [
                HostCall::Subscribe("sess-a2".into()),
                HostCall::Unsubscribe(2)
            ]
        );

        *host.team.lock().unwrap() = None;
        open_stream(&relay, "lists", "dash-a", "terminals", json!({}));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            frames_of(&relay, "lists")[0].1,
            json!({"t": "so", "v": 1, "sid": "lists", "ok": false, "error": "no_team"})
        );
        link.abort();
    }

    /// SV-PBF-286 row 3: a fake key across a K27 frame cut never reaches the
    /// socket; the output is still cut into frames within K27.
    #[tokio::test(start_paused = true)]
    async fn a_secret_across_a_frame_cut_never_reaches_the_link() {
        use crate::dashboard_publish::agents::tests::{FakeHost, FAKE_KEY};
        use crate::dashboard_publish::agents::TermEvent;
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard_in(
            data.path(),
            "dash-a",
            "live:\n  streams: [terminal]\n",
            ("acme", "emea"),
        );
        let host = Arc::new(FakeHost::new());
        host.launch("a1", Some(("acme", "emea")), "SW-1", Some("SW-1"));
        let relay = Relay::new(Mode::Answer);
        let link = start_with_agents(data.path(), &relay, &host);
        sleep(secs(1)).await;
        open_stream(&relay, "t1", "dash-a", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(200)).await;
        // The key starts just before the 64 KB cut, in one event and split over two.
        let pad = "x".repeat(STREAM_FRAME_BYTES - 10);
        let (head, tail) = FAKE_KEY.split_at(20);
        for (seq, data) in [
            (110, format!("{pad}{FAKE_KEY}\r\n")),
            (120, format!("{pad}{head}")),
            (130, format!("{tail}\r\n")),
        ] {
            host.emit(TermEvent::Output { data, seq });
        }
        sleep(secs(2)).await;
        let frames: Vec<Value> = frames_of(&relay, "t1")
            .into_iter()
            .map(|(_, f)| f)
            .filter(|f| f["t"] == "o")
            .collect();
        assert!(frames.len() > 2, "{} frames", frames.len());
        assert!(frames.iter().all(|f| payload(f) <= STREAM_FRAME_BYTES));
        let out: String = frames.iter().map(|f| f["data"].as_str().unwrap()).collect();
        assert_eq!(out, format!("{pad}[redacted]\r\n{pad}[redacted]\r\n"));
        link.abort();
    }

    /// SV-PBF-286 row 6: an action's result goes out in its `xa` frame
    /// redacted like an answer, through the production action path: an
    /// owner's message to a role whose name holds a fake key answers it masked.
    #[tokio::test]
    async fn action_results_are_redacted() {
        use crate::dashboard_publish::agents::tests::FAKE_KEY;
        let data = tempfile::tempdir().unwrap();
        let _folder = stream_dashboard(data.path(), "xa-test", "live:\n  streams: [inbox]\n");
        let (_, ctx, call) = super::super::inbox::tests::fixture();
        let mut record = state::load(data.path(), "xa-test").unwrap();
        record.live_approved.as_mut().unwrap().scope = call.scope.clone();
        state::save(data.path(), &record).unwrap();
        let a = Answerer { data_dir: data.path().into(), catalog: CATALOG, streams: STREAMS, ctx, permits: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)), blocking: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)), walks: Arc::new(tokio::sync::Semaphore::new(MAX_ABANDONED_WALKS)), timeout: ANSWER_TIMEOUT };
        let frame = json!({"id": "x1", "dashboard": "xa-test", "action": "ownerMessage", "args": {"to": format!("role:{FAKE_KEY}"), "text": "hi"}, "by": {"role": "owner", "account": "account"}});
        let sent: Value = serde_json::from_str(&a.action(frame, Some("account".into())).await).unwrap();
        assert_eq!((sent["t"].clone(), sent["ok"].clone()), (json!("xa"), json!(true)), "{sent}");
        assert_eq!(sent["result"]["to"], "role:[redacted]", "{sent}");
        assert!(!sent.to_string().contains("FAKE"));
    }

    // ---- SW-DP-23: owner control on the link ----

    /// live.rs up to its test module (the real `#[cfg(test)] mod tests`, not an
    /// earlier test-only item).
    fn non_test_code() -> &'static str {
        let src = include_str!("live.rs");
        let marker = ["#[cfg(test)]", "mod tests {"].join("\n");
        &src[..src.find(&marker).expect("the test module")]
    }

    /// Rewrites dashboard `slug`'s manifest in `folder` with `block` and saves
    /// its block as publish does (`approval::decide`), team acme/emea.
    fn resave(data: &Path, folder: &Path, slug: &str, block: &str) {
        fs::write(
            folder.join(MANIFEST_FILE),
            format!("slug: {slug}\ntitle: AI usage\npublish:\n  state: published\n  visibility: private\n  files:\n    - index.html\n{block}"),
        )
        .unwrap();
        let m = manifest::load(folder).unwrap();
        let decided = approval::decide(&m, folder, None, Some(("acme", "emea")));
        state::set_live_approved(data, slug, folder, decided.approved).unwrap();
    }

    const OWNER: &str = "acc_owner";
    const CONTROL_BLOCK: &str = "live:\n  streams: [terminal, terminals]\n  control: owner\n";

    fn send_input(relay: &Relay, sid: &str, by: Value, kind: Value) {
        let mut frame = json!({"t": "i", "v": 1, "sid": sid, "by": by});
        for (k, v) in kind.as_object().unwrap() {
            frame[k] = v.clone();
        }
        relay.send(Message::text(frame.to_string()));
    }

    fn owner_by() -> Value {
        json!({"role": "owner", "account": OWNER})
    }

    fn keys(data: &str) -> Value {
        json!({"kind": "keys", "data": data})
    }

    fn control_log(data: &Path) -> Vec<Value> {
        fs::read_to_string(log_path(data, CONTROL_LOG))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The control log's lines are written off the loop: waits (real time) until
    /// `pred` holds of them, up to two seconds.
    async fn control_log_until(data: &Path, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        for _ in 0..200 {
            let lines = control_log(data);
            if pred(&lines) {
                return lines;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(10));
        }
        control_log(data)
    }

    fn refusals_of(relay: &Relay, sid: &str) -> Vec<Value> {
        frames_of(relay, sid)
            .into_iter()
            .map(|(_, f)| f)
            .filter(|f| f["t"] == "ir")
            .collect()
    }

    fn writes(
        host: &crate::dashboard_publish::agents::tests::FakeHost,
    ) -> Vec<crate::dashboard_publish::agents::tests::Call> {
        use crate::dashboard_publish::agents::tests::Call;
        host.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Write(..) | Call::Resize(..)))
            .collect()
    }

    /// A link with `dash-c` (control: owner) and `dash-n` (streams, no control),
    /// both in acme/emea, with `a1` running there, the relay naming `owner`.
    async fn control_fixture(
        owner: Option<&str>,
    ) -> (
        tempfile::TempDir,
        Vec<tempfile::TempDir>,
        Relay,
        Arc<crate::dashboard_publish::agents::tests::FakeHost>,
        tokio::task::JoinHandle<()>,
    ) {
        use crate::dashboard_publish::agents::tests::FakeHost;
        let data = tempfile::tempdir().unwrap();
        let folders = vec![
            stream_dashboard_in(data.path(), "dash-c", CONTROL_BLOCK, ("acme", "emea")),
            stream_dashboard_in(
                data.path(),
                "dash-n",
                "live:\n  streams: [terminal, terminals]\n",
                ("acme", "emea"),
            ),
        ];
        let host = Arc::new(FakeHost::new());
        host.launch("a1", Some(("acme", "emea")), "SW-1", Some("SW-1"));
        let relay = Relay::new(Mode::Answer);
        relay.set_owner(owner);
        let link = start_with_agents(data.path(), &relay, &host);
        sleep(secs(1)).await;
        (data, folders, relay, host, link)
    }

    /// SW-DP-23 AC-2 (DP-D42, DP-INV-11): the Mac refuses what the relay should
    /// not have sent. Input from a viewer, from an account that is not the owner,
    /// for a stream that is not open, for a stream that is not a terminal, for a
    /// dashboard without `live.control`, and for a session outside the team is
    /// answered `ir refused`, written to the control log as refused, and never
    /// reaches the terminal. After `u`, the sid's input is refused too.
    #[tokio::test(start_paused = true)]
    async fn the_mac_refuses_input_the_relay_should_not_have_sent() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        // The fixture host feeds one terminal at a time: dash-n's goes first.
        open_stream(&relay, "n1", "dash-n", "terminal", json!({"handle": "a1"}));
        open_stream(&relay, "list", "dash-c", "terminals", json!({}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "n1", owner_by(), keys("x"));
        send_input(&relay, "list", owner_by(), keys("x"));
        send_input(&relay, "ghost", owner_by(), keys("x"));
        sleep(Duration::from_millis(200)).await;
        stream_frame(&relay, "u", "n1");
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;

        send_input(
            &relay,
            "t1",
            json!({"role": "viewer", "share": "share-1"}),
            keys("x"),
        );
        send_input(
            &relay,
            "t1",
            json!({"role": "owner", "account": "acc_other"}),
            keys("x"),
        );
        send_input(
            &relay,
            "t1",
            json!({"role": "viewer", "share": "s", "account": OWNER}),
            keys("x"),
        );
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "launch", "data": "x"}),
        );
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "resize", "cols": 0, "rows": 40}),
        );
        sleep(Duration::from_millis(200)).await;
        // Outside the team: the dashboard's folder is now in another team.
        *host.team.lock().unwrap() = Some(crate::dashboard_publish::agents::Team {
            org: "acme".into(),
            team: "apac".into(),
        });
        send_input(&relay, "t1", owner_by(), keys("x"));
        sleep(Duration::from_millis(300)).await;

        let refused = |sid: &str| -> Vec<Value> { refusals_of(&relay, sid) };
        assert_eq!(refused("t1").len(), 6, "{:?}", refused("t1"));
        for sid in ["ghost", "list", "n1"] {
            assert_eq!(
                refused(sid),
                [json!({"t": "ir", "v": 1, "sid": sid, "error": "refused"})]
            );
        }
        assert!(refused("t1").iter().all(|f| f["error"] == "refused"));
        assert_eq!(writes(&host), [], "nothing reached the terminal");
        let lines = control_log_until(data.path(), |l| l.len() >= 7).await;
        // Written off the loop, so in any order; one line a second per (reason, dashboard,
        // session, account): the viewer's two refusals share one, acc_other gets its own.
        let mut reasons: Vec<&str> = lines
            .iter()
            .map(|l| l["reason"].as_str().unwrap())
            .collect();
        reasons.sort();
        assert_eq!(
            reasons,
            [
                "bad_input",
                "control_off",
                "not_in_team",
                "not_open",
                "not_owner",
                "not_owner",
                "not_terminal"
            ]
        );
        assert!(lines.iter().all(|l| l["outcome"] == "refused"));
        let by = |reason: &str| {
            lines
                .iter()
                .find(|l| l["reason"] == reason)
                .unwrap()
                .clone()
        };
        assert_eq!(by("control_off")["slug"], "dash-n");
        assert_eq!(
            by("not_open")["slug"],
            Value::Null,
            "an unknown sid names no dashboard"
        );
        assert_eq!(by("not_in_team")["handle"], "a1");
        assert_eq!(by("not_in_team")["account"], OWNER);

        // Back in the team, the owner's input goes; after `u`, it is refused.
        *host.team.lock().unwrap() = Some(crate::dashboard_publish::agents::tests::acme());
        send_input(&relay, "t1", owner_by(), keys("ok"));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(writes(&host).len(), 1);
        stream_frame(&relay, "u", "t1");
        sleep(Duration::from_millis(100)).await;
        send_input(&relay, "t1", owner_by(), keys("late"));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(writes(&host).len(), 1, "no input after u");
        assert_eq!(refused("t1").len(), 7);
        link.abort();
    }

    /// SW-DP-23 AC-2: with no `owner_account` in `hello` (a relay before
    /// SW-DP-23, or a publisher with no owner account), every `i` is refused.
    #[tokio::test(start_paused = true)]
    async fn with_no_owner_in_hello_every_input_is_refused() {
        let (_data, _folders, relay, host, link) = control_fixture(None).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t1", owner_by(), keys("ls\r"));
        send_input(
            &relay,
            "t1",
            json!({"role": "owner", "account": null}),
            keys("ls\r"),
        );
        sleep(Duration::from_millis(200)).await;
        assert_eq!(refusals_of(&relay, "t1").len(), 2);
        assert_eq!(writes(&host), []);
        link.abort();
    }

    /// SW-DP-23: an owner frame replayed on a later link whose `hello` names
    /// another owner (the dashboard's publisher was relinked) is refused: the
    /// owner is per link and comes only from the relay's `hello`.
    #[tokio::test(start_paused = true)]
    async fn a_replayed_owner_frame_on_a_new_link_with_another_owner_is_refused() {
        let (_data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t1", owner_by(), keys("ok"));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(writes(&host).len(), 1);

        relay.set_owner(Some("acc_new_owner"));
        relay.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        })));
        sleep(secs(10)).await;
        assert_eq!(relay.connections(), 2);
        open_stream(&relay, "t2", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t2", owner_by(), keys("replayed"));
        sleep(Duration::from_millis(200)).await;
        assert_eq!(refusals_of(&relay, "t2").len(), 1);
        assert_eq!(
            writes(&host).len(),
            1,
            "the replay never reached the terminal"
        );
        link.abort();
    }

    /// SW-DP-23 AC-3: the owner's `keys` reach the session's terminal through
    /// the app's write path, a three-line paste arrives bracketed, and a resize
    /// goes as the `dashboard` viewer, in the order they came. Nothing is sent back.
    #[tokio::test(start_paused = true)]
    async fn owner_input_reaches_the_terminal_in_order() {
        use crate::dashboard_publish::agents::tests::Call;
        let (_data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t1", owner_by(), keys("ls\r"));
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": "one\ntwo\nthree"}),
        );
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "resize", "cols": 120, "rows": 40}),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            writes(&host),
            [
                Call::Write("sess-a1".into(), "ls\r".into()),
                Call::Write("sess-a1".into(), "\x1b[200~one\ntwo\nthree\x1b[201~".into()),
                Call::Resize("sess-a1".into(), 120, 40),
            ]
        );
        assert_eq!(refusals_of(&relay, "t1"), Vec::<Value>::new());
        link.abort();
    }

    /// SW-DP-23 AC-6: K38 and K39 on the Mac. An input over 16 KB is `too_large`;
    /// past 100 frames, or 64 KB, in a second the excess is `rate_limited` and
    /// dropped, never reaching the terminal (not queued for later either).
    #[tokio::test(start_paused = true)]
    async fn input_limits_are_enforced_on_the_mac() {
        let (_data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(
            &relay,
            "t1",
            owner_by(),
            keys(&"a".repeat(MAX_INPUT_BYTES + 1)),
        );
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "too_large"})]
        );
        assert_eq!(writes(&host), []);

        for _ in 0..150 {
            send_input(&relay, "t1", owner_by(), keys("x"));
        }
        sleep(Duration::from_millis(500)).await;
        assert_eq!(writes(&host).len(), INPUT_FRAMES_PER_SEC);
        let limited = refusals_of(&relay, "t1")
            .iter()
            .filter(|f| f["error"] == "rate_limited")
            .count();
        assert_eq!(limited, 50);
        sleep(secs(3)).await;
        assert_eq!(
            writes(&host).len(),
            INPUT_FRAMES_PER_SEC,
            "the excess was dropped, not queued"
        );

        // Bytes: four 16 KB pastes fill the second's 64 KB; the fifth is refused.
        for _ in 0..5 {
            send_input(
                &relay,
                "t1",
                owner_by(),
                json!({"kind": "paste", "data": "b".repeat(MAX_INPUT_BYTES)}),
            );
        }
        sleep(Duration::from_millis(500)).await;
        assert_eq!(writes(&host).len(), INPUT_FRAMES_PER_SEC + 4);
        link.abort();
    }

    /// SW-DP-23 AC-7 (DP-D45): control is logged by who, which dashboard and
    /// session, and frame and byte counts per minute, never by what was typed. A
    /// marker typed (accepted and refused) is in no control-log or view-log line.
    #[tokio::test(start_paused = true)]
    async fn the_control_log_counts_input_and_never_holds_it() {
        const MARKER: &str = "MARKER-7f3a";
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t1", owner_by(), keys(MARKER));
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": format!("{MARKER}\n{MARKER}")}),
        );
        send_input(
            &relay,
            "t1",
            json!({"role": "viewer", "share": "share-1"}),
            keys(MARKER),
        );
        send_input(
            &relay,
            "t1",
            owner_by(),
            keys(&format!("{MARKER}{}", "z".repeat(MAX_INPUT_BYTES))),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host).len(), 2);
        sleep(CONTROL_LOG_EVERY + secs(1)).await;
        let lines =
            control_log_until(data.path(), |l| l.iter().any(|x| x.get("frames").is_some())).await;
        let minute = lines
            .iter()
            .find(|l| l.get("frames").is_some())
            .expect("a minute line");
        assert_eq!(minute["slug"], "dash-c");
        assert_eq!(minute["handle"], "a1");
        assert_eq!(minute["account"], OWNER);
        assert_eq!(minute["frames"], 2);
        let pasted = crate::commands::pty::bracketed_paste(&format!("{MARKER}\n{MARKER}"));
        assert_eq!(
            minute["bytes"],
            json!(MARKER.len() + format!("{MARKER}\n{MARKER}").len())
        );
        assert!(pasted.len() > MARKER.len());
        let keys_of: Vec<_> = minute.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys_of.len(), 6, "{keys_of:?}");
        assert_eq!(
            lines.iter().filter(|l| l["outcome"] == "refused").count(),
            2
        );
        let control_text = fs::read_to_string(log_path(data.path(), CONTROL_LOG)).unwrap();
        let view_text = fs::read_to_string(view_log_path(data.path())).unwrap_or_default();
        assert!(!control_text.contains(MARKER), "{control_text}");
        assert!(!view_text.contains(MARKER), "{view_text}");
        link.abort();
    }

    /// SW-DP-23 review 2: control turned off in the saved block while jobs wait
    /// behind a slow write: only the write in flight lands; the rest are refused
    /// (`control_off`) by the gate the pass turns off.
    #[tokio::test(start_paused = true)]
    async fn input_queued_when_a_pass_turns_control_off_is_not_written() {
        let (data, folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        // In flight past the next pass: the write takes 9 s, longer than PASS (5 s),
        // so the pass turns control off while it is in flight.
        let delay = Duration::from_secs(9);
        assert!(PASS < delay);
        *host.write_delay.lock().unwrap() = Some(delay);
        for k in ["one", "two", "three"] {
            send_input(&relay, "t1", owner_by(), keys(k));
        }
        sleep(Duration::from_millis(100)).await;
        assert_eq!(writes(&host).len(), 1, "the first write is in flight");
        resave(
            data.path(),
            folders[0].path(),
            "dash-c",
            "live:\n  streams: [terminal, terminals]\n",
        );
        sleep(PASS * 5).await;
        assert_eq!(writes(&host).len(), 1, "queued jobs never reach the host");
        assert_eq!(refusals_of(&relay, "t1").len(), 2);
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "control_off");
        link.abort();
    }

    /// SW-DP-23 review 2: the terminal ends on its own (its session exits) while
    /// jobs wait behind a slow write: the rest are refused (`not_open`).
    #[tokio::test(start_paused = true)]
    async fn input_queued_when_the_stream_ends_is_not_written() {
        use crate::dashboard_publish::agents::TermEvent;
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        *host.write_delay.lock().unwrap() = Some(secs(2));
        for k in ["one", "two", "three"] {
            send_input(&relay, "t1", owner_by(), keys(k));
        }
        sleep(Duration::from_millis(100)).await;
        host.emit(TermEvent::Exited);
        sleep(secs(5)).await;
        assert!(
            frames_of(&relay, "t1").iter().any(|(_, f)| f["t"] == "end"),
            "the stream ended on its own"
        );
        assert_eq!(writes(&host).len(), 1, "queued jobs never reach the host");
        assert_eq!(refusals_of(&relay, "t1").len(), 2);
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "not_open");
        link.abort();
    }

    /// SW-DP-23 review 2: an `i` that is not JSON the Mac can read (a lone
    /// surrogate in `data`) is answered `ir refused` and logged `bad_input`,
    /// not dropped in silence; the link stays up.
    #[tokio::test(start_paused = true)]
    async fn an_unreadable_input_frame_is_refused_not_dropped() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        relay.send(Message::text(format!(
            r#"{{"t":"i","v":1,"sid":"t1","by":{{"role":"owner","account":"{OWNER}"}},"kind":"keys","data":"\udc00"}}"#
        )));
        // Not an `i`, or another version: ignored as before.
        relay.send(Message::text(r#"{"t":"q","sid":"t1","data":"\udc00"}"#));
        relay.send(Message::text(
            r#"{"t":"i","v":2,"sid":"t1","data":"\udc00"}"#,
        ));
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"})]
        );
        assert_eq!(writes(&host), []);
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "bad_input");
        assert_eq!(lines[0]["slug"], "dash-c");
        assert_eq!(relay.connections(), 1);
        link.abort();
    }

    #[test]
    fn unreadable_input_reads_only_a_plain_bounded_sid() {
        assert_eq!(
            unreadable_input(r#"{"t":"i","sid":"abc-1","data":"\udc00"}"#),
            Some("abc-1".into())
        );
        assert_eq!(
            unreadable_input(r#"{"t":"i","v":1,"sid":"x"}"#),
            Some("x".into())
        );
        assert_eq!(unreadable_input(r#"{"t":"q","sid":"abc"}"#), None);
        assert_eq!(unreadable_input(r#"{"t":"i","sid":"a\"b"}"#), None);
        let long = "a".repeat(MAX_SID + 1);
        assert_eq!(
            unreadable_input(&format!(r#"{{"t":"i","sid":"{long}"}}"#)),
            None
        );
        assert_eq!(unreadable_input(r#"{"t":"i","sid":"a b"}"#), None);
        assert_eq!(unreadable_input(r#"{"t":"i","v":2,"sid":"x"}"#), None);
    }

    /// SW-DP-23 review: input queued behind a slow write is not written after
    /// `u`: the worker checks the stream's gate right before each write.
    #[tokio::test(start_paused = true)]
    async fn input_queued_before_u_is_not_written_after_it() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        *host.write_delay.lock().unwrap() = Some(Duration::from_millis(500));
        for k in ["one", "two", "three"] {
            send_input(&relay, "t1", owner_by(), keys(k));
        }
        sleep(Duration::from_millis(100)).await;
        assert_eq!(writes(&host).len(), 1, "the first write is in progress");
        stream_frame(&relay, "u", "t1");
        sleep(secs(2)).await;
        assert_eq!(
            writes(&host).len(),
            1,
            "jobs queued before u never reach the host"
        );
        assert_eq!(refusals_of(&relay, "t1").len(), 2);
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "not_open");
        link.abort();
    }

    /// SW-DP-23 review: the saved block loses `control: owner` while a terminal
    /// is open; after the next pass the owner's input is refused (`control_off`).
    #[tokio::test(start_paused = true)]
    async fn control_removed_from_the_saved_block_refuses_input_after_a_pass() {
        let (data, folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        resave(
            data.path(),
            folders[0].path(),
            "dash-c",
            "live:\n  streams: [terminal, terminals]\n",
        );
        sleep(PASS * 2).await;
        send_input(&relay, "t1", owner_by(), keys("ls\r"));
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"})]
        );
        assert_eq!(writes(&host), []);
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "control_off");
        link.abort();
    }

    /// SW-DP-23 review: a K38 input of control bytes (about 96 KB as JSON) is
    /// within MAX_INBOUND: the link stays up and the bytes are written.
    #[tokio::test(start_paused = true)]
    async fn a_full_k38_input_of_control_bytes_crosses_the_link() {
        use crate::dashboard_publish::agents::tests::Call;
        let (_data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        let data = "\u{1}".repeat(MAX_INPUT_BYTES);
        send_input(&relay, "t1", owner_by(), keys(&data));
        sleep(Duration::from_millis(500)).await;
        assert_eq!(writes(&host), [Call::Write("sess-a1".into(), data)]);
        assert_eq!(refusals_of(&relay, "t1"), Vec::<Value>::new());
        assert_eq!(relay.connections(), 1, "the link stayed up");
        link.abort();
    }

    /// SW-DP-23 review: refusal lines are at most one a second per reason per
    /// link; the next line says how many were left out. Every `ir` still goes.
    #[tokio::test(start_paused = true)]
    async fn refusal_lines_are_throttled_per_reason() {
        let (data, _folders, relay, _host, link) = control_fixture(Some(OWNER)).await;
        for _ in 0..50 {
            send_input(&relay, "ghost", owner_by(), keys("x"));
        }
        sleep(Duration::from_millis(300)).await;
        assert_eq!(refusals_of(&relay, "ghost").len(), 50);
        sleep(secs(2)).await;
        send_input(&relay, "ghost", owner_by(), keys("x"));
        sleep(Duration::from_millis(300)).await;
        let lines = control_log_until(data.path(), |l| l.len() >= 2).await;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].get("suppressed").is_none());
        assert_eq!(lines[1]["suppressed"], 49);
        link.abort();
    }

    /// SW-DP-23 review 3: refusals left out by the throttle are written as
    /// their own line on the minute tick, and when the link ends, even with no
    /// later refusal of that reason.
    #[tokio::test(start_paused = true)]
    async fn suppressed_refusals_are_flushed_on_the_minute_and_at_the_end() {
        let (data, _folders, relay, _host, link) = control_fixture(Some(OWNER)).await;
        for _ in 0..5 {
            send_input(&relay, "ghost", owner_by(), keys("x"));
        }
        sleep(Duration::from_millis(300)).await;
        sleep(CONTROL_LOG_EVERY + secs(1)).await;
        let lines = control_log_until(data.path(), |l| l.len() >= 2).await;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1]["reason"], "not_open");
        // A flushed line counts only the refusals left out: 1 + 4 = the 5 sent.
        assert_eq!(lines[1]["flushed"], true);
        assert_eq!(lines[1]["count"], 4);
        assert!(lines[1].get("suppressed").is_none());
        assert_eq!(lines[1]["account"], OWNER);
        assert_eq!(lines[1]["slug"], Value::Null);

        for _ in 0..3 {
            send_input(&relay, "ghost", owner_by(), keys("x"));
        }
        sleep(Duration::from_millis(300)).await;
        relay.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "".into(),
        })));
        sleep(Duration::from_millis(500)).await;
        let lines = control_log_until(data.path(), |l| l.len() >= 4).await;
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[2].get("suppressed").is_none());
        assert_eq!(lines[3]["flushed"], true, "flushed when the link ended");
        assert_eq!(lines[3]["count"], 2);
        // The lines add up to the refusals: 1 per line written as it happened plus
        // its `suppressed`, and `count` per flushed line.
        let total: u64 = lines
            .iter()
            .map(|l| match l["flushed"].as_bool() {
                Some(true) => l["count"].as_u64().unwrap(),
                _ => 1 + l["suppressed"].as_u64().unwrap_or(0),
            })
            .sum();
        assert_eq!(total, 8);
        link.abort();
    }

    /// SW-DP-23 review 4: the throttle is per (reason, dashboard, session,
    /// account), so refusals of other identities in the same second are each
    /// named; past MAX_REFUSAL_KEYS identities a reason's overflow key (no
    /// identity) counts the rest.
    #[test]
    fn refusals_are_throttled_per_identity_with_a_bounded_overflow() {
        let log = RefusalLog::default();
        let now = Instant::now();
        let refusal = |handle: &str| Refusal {
            slug: Some("dash-c".into()),
            handle: Some(handle.into()),
            account: Some(OWNER.into()),
            reason: "not_in_team",
        };
        let (key_a, a) = log.admit(&refusal("a1"), now);
        let (key_b, b) = log.admit(&refusal("a2"), now);
        assert_eq!((a, b), (Some(0), Some(0)), "each handle gets its own line");
        assert_eq!(key_a.2.as_deref(), Some("a1"));
        assert_eq!(key_b.2.as_deref(), Some("a2"));
        assert_eq!(
            log.admit(&refusal("a1"), now).1,
            None,
            "the same handle is throttled"
        );
        // a1 and a2 hold two keys: these fill the link's keys with an identity.
        for i in 0..MAX_REFUSAL_KEYS - 2 {
            let (key, first) = log.admit(&refusal(&format!("h{i}")), now);
            assert_eq!((key.2.is_some(), first), (true, Some(0)));
        }
        let (overflow, first) = log.admit(&refusal("one-too-many"), now);
        assert_eq!(overflow, ("not_in_team", None, None, None));
        assert_eq!(first, Some(0));
        assert_eq!(log.admit(&refusal("and-another"), now).1, None);
        let pending = log.take_pending();
        assert!(pending.contains(&(key_a, 1)));
        assert!(pending.contains(&(("not_in_team", None, None, None), 1)));
        assert_eq!(pending.len(), 2);
        // A second later, keys with nothing pending make room again.
        let later = now + SECOND * 2;
        let (named, _) = log.admit(&refusal("fresh"), later);
        assert_eq!(named.2.as_deref(), Some("fresh"));
    }

    /// SW-DP-23 review 5: the throttle's keys hold each field clipped, so two
    /// accounts that differ only past the clip share one key, and no key field
    /// is longer than the clip.
    #[test]
    fn refusal_keys_hold_clipped_fields() {
        let log = RefusalLog::default();
        let now = Instant::now();
        let long = |tail: &str| format!("{}{tail}", "a".repeat(128 * 1024));
        let refusal = |account: String| Refusal {
            slug: Some("s".repeat(VIEW_LOG_FIELD_MAX * 3)),
            handle: Some("h".repeat(VIEW_LOG_FIELD_MAX + 1)),
            account: Some(account),
            reason: "not_owner",
        };
        let (key_x, first) = log.admit(&refusal(long("x")), now);
        let (key_y, second) = log.admit(&refusal(long("y")), now);
        assert_eq!(key_x, key_y, "they differ only past the clip");
        assert_eq!((first, second), (Some(0), None));
        for field in [&key_x.1, &key_x.2, &key_x.3] {
            assert_eq!(field.as_ref().unwrap().chars().count(), VIEW_LOG_FIELD_MAX);
        }
    }

    /// SW-DP-23 review 4: on the link, two accounts refused for the same reason
    /// in one second each get their line.
    #[tokio::test(start_paused = true)]
    async fn refusals_of_two_identities_in_one_second_are_each_named() {
        let (data, _folders, relay, _host, link) = control_fixture(Some(OWNER)).await;
        send_input(
            &relay,
            "ghost",
            json!({"role": "owner", "account": "acc_a"}),
            keys("x"),
        );
        send_input(
            &relay,
            "ghost",
            json!({"role": "owner", "account": "acc_b"}),
            keys("x"),
        );
        sleep(Duration::from_millis(300)).await;
        let lines = control_log_until(data.path(), |l| l.len() >= 2).await;
        let mut accounts: Vec<&str> = lines
            .iter()
            .map(|l| l["account"].as_str().unwrap())
            .collect();
        accounts.sort();
        assert_eq!(accounts, ["acc_a", "acc_b"]);
        assert!(lines.iter().all(|l| l["reason"] == "not_owner"));
        link.abort();
    }

    /// SW-DP-23 review 3 (DP-D54): a self-hosted relay's long owner account
    /// (300 characters) is kept, so its owner's input goes; one over the cap is
    /// refused (input refused).
    #[tokio::test(start_paused = true)]
    async fn a_long_owner_account_up_to_the_cap_is_kept() {
        let long = "o".repeat(300);
        let (_data, _folders, relay, host, link) = control_fixture(Some(&long)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(
            &relay,
            "t1",
            json!({"role": "owner", "account": long}),
            keys("ls\r"),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host).len(), 1);
        assert_eq!(refusals_of(&relay, "t1"), Vec::<Value>::new());
        link.abort();

        assert_eq!(
            owner_account_of(Some("o".repeat(MAX_OWNER_ACCOUNT))).map(|a| a.len()),
            Some(MAX_OWNER_ACCOUNT)
        );
        assert_eq!(
            owner_account_of(Some("o".repeat(MAX_OWNER_ACCOUNT + 1))),
            None
        );
        assert_eq!(owner_account_of(Some(String::new())), None);
        assert_eq!(owner_account_of(None), None);
    }

    /// SW-DP-23 review 6: an empty `keys` or `paste` is refused (`bad_input`),
    /// never written and never counted.
    #[tokio::test(start_paused = true)]
    async fn empty_input_is_refused_and_not_written() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(&relay, "t1", owner_by(), keys(""));
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": ""}),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host), []);
        assert_eq!(
            refusals_of(&relay, "t1"),
            [
                json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"}),
                json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"})
            ]
        );
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "bad_input");
        sleep(CONTROL_LOG_EVERY + secs(1)).await;
        let lines = control_log(data.path());
        assert!(
            lines.iter().all(|l| l.get("frames").is_none()),
            "nothing counted: {lines:?}"
        );
        link.abort();
    }

    /// SW-DP-23 review 6: a frame refused because the worker's queue is full
    /// gives its K39 slot back: the second's budget is spent only by frames
    /// that went.
    #[tokio::test]
    async fn a_full_queue_gives_the_k39_slot_back() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<InputJob>(1);
        let job = || InputJob {
            sid: "t1".into(),
            slug: "dash-c".into(),
            handle: "a1".into(),
            account: OWNER.into(),
            target: InputTarget {
                folder: PathBuf::new(),
                scope: None,
                gate: InputGate::new(true),
                handle: Some("a1".into()),
            },
            kind: InputKind::Keys("x".into()),
        };
        let mut rate = InputRate::default();
        let now = Instant::now();
        assert!(rate.take(now, 1));
        assert!(enqueue_input(&mut rate, &tx, job()).is_none());
        for _ in 0..INPUT_FRAMES_PER_SEC * 2 {
            assert!(
                rate.take(now, 1),
                "a refused frame left the budget untouched"
            );
            let (refusal, error) = enqueue_input(&mut rate, &tx, job()).expect("the queue is full");
            assert_eq!((refusal.reason, error), ("rate_limited", "rate_limited"));
        }
        assert_eq!(
            rate.recent.len(),
            1,
            "only the frame that went holds a slot"
        );
        assert_eq!(rate.bytes, 1);
        assert!(rx.recv().await.is_some());
    }

    /// SW-DP-23 review 9 (DP-D45, AC-7): the per-minute line counts keystroke
    /// frames only: a resize reaches the terminal but types nothing, so it is
    /// not a frame.
    #[tokio::test(start_paused = true)]
    async fn a_resize_is_not_counted_as_a_keystroke_frame() {
        use crate::dashboard_publish::agents::tests::Call;
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "resize", "cols": 100, "rows": 30}),
        );
        send_input(&relay, "t1", owner_by(), keys("ls\r"));
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            writes(&host),
            [
                Call::Resize("sess-a1".into(), 100, 30),
                Call::Write("sess-a1".into(), "ls\r".into())
            ]
        );
        sleep(CONTROL_LOG_EVERY + secs(1)).await;
        let lines =
            control_log_until(data.path(), |l| l.iter().any(|x| x.get("frames").is_some())).await;
        let minute = lines
            .iter()
            .find(|l| l.get("frames").is_some())
            .expect("a minute line");
        assert_eq!(minute["frames"], 1);
        assert_eq!(minute["bytes"], 3);
        link.abort();
    }

    /// SW-DP-23 review 16: a paste of nothing but bracketed-paste markers types
    /// nothing: refused (`bad_input`), never written.
    #[tokio::test(start_paused = true)]
    async fn a_marker_only_paste_is_refused_and_not_written() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": "\u{1b}[200~"}),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host), []);
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"})]
        );
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "bad_input");
        link.abort();
    }

    /// SW-DP-23 review 18: K38 is judged before the marker scan, so a paste over
    /// 16 KB made only of bracketed-paste markers is `too_large`, never written.
    #[tokio::test(start_paused = true)]
    async fn an_oversized_marker_only_paste_is_too_large() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        let markers = "\u{1b}[200~".repeat(MAX_INPUT_BYTES / 6 + 1);
        assert!(markers.len() > MAX_INPUT_BYTES);
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": markers}),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host), []);
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "too_large"})]
        );
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "too_large");
        link.abort();
    }

    /// SW-DP-23 review 18: a paste of a marker split around another
    /// (`ESC[20` `ESC[201~` `0~`) strips to nothing: refused (`bad_input`) over
    /// the link, never written.
    #[tokio::test(start_paused = true)]
    async fn a_split_marker_paste_is_refused_and_not_written() {
        let (data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        send_input(
            &relay,
            "t1",
            owner_by(),
            json!({"kind": "paste", "data": "\u{1b}[20\u{1b}[201~0~"}),
        );
        sleep(Duration::from_millis(300)).await;
        assert_eq!(writes(&host), []);
        assert_eq!(
            refusals_of(&relay, "t1"),
            [json!({"t": "ir", "v": 1, "sid": "t1", "error": "refused"})]
        );
        let lines = control_log_until(data.path(), |l| !l.is_empty()).await;
        assert_eq!(lines[0]["reason"], "bad_input");
        link.abort();
    }

    /// SW-DP-23 review 18: there is no write timeout (one could let a later write
    /// overtake a slow one): a write that takes 30 s holds the next job behind it,
    /// no `ir` is sent, and both land in order.
    #[tokio::test(start_paused = true)]
    async fn a_slow_write_holds_the_next_job_and_both_land_in_order() {
        use crate::dashboard_publish::agents::tests::Call;
        let (_data, _folders, relay, host, link) = control_fixture(Some(OWNER)).await;
        open_stream(&relay, "t1", "dash-c", "terminal", json!({"handle": "a1"}));
        sleep(Duration::from_millis(300)).await;
        *host.write_delay.lock().unwrap() = Some(secs(30));
        send_input(&relay, "t1", owner_by(), keys("slow"));
        send_input(&relay, "t1", owner_by(), keys("next"));
        sleep(secs(20)).await;
        assert_eq!(
            writes(&host).len(),
            1,
            "the next job waits behind the slow write"
        );
        *host.write_delay.lock().unwrap() = None;
        sleep(secs(15)).await;
        assert_eq!(
            writes(&host),
            [
                Call::Write("sess-a1".into(), "slow".into()),
                Call::Write("sess-a1".into(), "next".into())
            ]
        );
        assert_eq!(refusals_of(&relay, "t1"), Vec::<Value>::new());
        link.abort();
    }

    /// SW-DP-23: the control log is pruned at 30 days with the view log.
    #[test]
    fn the_control_log_keeps_30_days() {
        let data = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::days(31)).to_rfc3339();
        let new = (now - chrono::Duration::days(1)).to_rfc3339();
        append_log(
            data.path(),
            CONTROL_LOG,
            &json!({"at": old, "outcome": "refused"}),
        );
        append_log(
            data.path(),
            CONTROL_LOG,
            &json!({"at": new, "outcome": "refused"}),
        );
        prune_view_log(data.path(), now);
        let lines = control_log(data.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["at"], new);
    }

    /// SW-DP-23 review 19: tungstenite traces each message it receives (an `i`
    /// frame holds what the owner typed); the app's logger caps it at info.
    #[test]
    fn the_websocket_library_is_never_logged_below_info() {
        let lib = include_str!("../lib.rs");
        assert!(lib.contains(r#".level_for("tungstenite", LevelFilter::Info)"#));
        assert!(lib.contains(r#".level_for("tokio_tungstenite", LevelFilter::Info)"#));
    }

    /// SW-DP-23: no `log::` macro of the link (each read whole, to its `);`)
    /// names input data, a frame's length or a job: the typed bytes are never
    /// logged (only control-log lines with counts).
    #[test]
    fn the_link_never_logs_typed_input() {
        let code = non_test_code();
        assert!(
            code.contains("fn input_worker("),
            "the scan covers the input code"
        );
        let mut macros = 0;
        let mut rest = code;
        while let Some(at) = rest.find("log::") {
            let tail = &rest[at..];
            let end = tail.find(");").map_or(tail.len(), |e| e + 2);
            let mac = &tail[..end];
            macros += 1;
            let text = mac.replace("dashboard_publish", "");
            for word in ["data", "bytes", "job", "input", "kind", "paste", "keys"] {
                assert!(!text.contains(word), "a log macro names {word}: {mac}");
            }
            rest = &tail[end..];
        }
        assert!(macros > 10, "{macros} log macros scanned");
    }

    /// SW-DP-20 follow-up: a query's answer does not wait behind a flooding
    /// stream's frames. Real time: the flood never lets a paused clock advance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn answers_do_not_wait_behind_stream_frames() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(
            data.path(),
            "dash-a",
            "live:\n  queries: all\n  streams: [tick]\n",
        );
        let relay = Relay::new(Mode::Answer);
        let link = start_fixture(data.path(), &relay);
        sleep(secs(1)).await;
        for i in 0..8 {
            open_stream(
                &relay,
                &format!("f{i}"),
                "dash-a",
                "tick",
                json!({"every_ms": 1, "chunk": 60_000}),
            );
        }
        sleep(secs(2)).await;
        let asked = Instant::now();
        ask(&relay, "q1", "dash-a", "fixtureEcho", json!({"n": 1}));
        let answer = |relay: &Relay| {
            relay
                .frames()
                .into_iter()
                .filter_map(|f| serde_json::from_str::<Value>(&f).ok())
                .find(|f| f["t"] == "a" && f["id"] == "q1")
        };
        loop {
            if answer(&relay).is_some() {
                break;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(2),
                "the answer waited behind stream frames"
            );
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(answer(&relay).unwrap()["ok"], true);
        link.abort();
    }

    /// SW-DP-20 follow-up (opens off the loop): a `u` while an open is pending
    /// drops it (no `so`, no frames); a second `s` while one is pending answers
    /// once, from the second; an opener that panics answers `so failed`.
    #[tokio::test(start_paused = true)]
    async fn opens_in_progress_are_cancelled_or_replaced() {
        let data = tempfile::tempdir().unwrap();
        let _a = stream_dashboard(data.path(), "dash-a", "live:\n  streams: [tick]\n");
        let relay = Relay::new(Mode::Answer);
        let link = start(data.path(), &relay, token_store(TOKEN));
        sleep(secs(1)).await;
        open_stream(
            &relay,
            "gone",
            "dash-a",
            "tick",
            json!({"open_delay_ms": 1000}),
        );
        open_stream(
            &relay,
            "twice",
            "dash-a",
            "tick",
            json!({"open_delay_ms": 1000}),
        );
        sleep(Duration::from_millis(100)).await;
        stream_frame(&relay, "u", "gone");
        open_stream(&relay, "twice", "dash-a", "tick", json!({"every_ms": 500}));
        open_stream(
            &relay,
            "boom",
            "dash-a",
            "tick",
            json!({"open_panic": true}),
        );
        sleep(secs(3)).await;
        assert!(
            frames_of(&relay, "gone").is_empty(),
            "{:?}",
            frames_of(&relay, "gone")
        );
        let twice: Vec<Value> = frames_of(&relay, "twice")
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        assert_eq!(
            twice.iter().filter(|f| f["t"] == "so").count(),
            1,
            "{twice:?}"
        );
        assert_eq!(
            twice[0],
            json!({"t": "so", "v": 1, "sid": "twice", "ok": true})
        );
        assert_eq!(twice.iter().filter(|f| f["t"] == "snapshot").count(), 1);
        assert_eq!(
            frames_of(&relay, "boom")[0].1,
            json!({"t": "so", "v": 1, "sid": "boom", "ok": false, "error": "failed"})
        );
        link.abort();
    }
}
