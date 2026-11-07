// SPDX-License-Identifier: MIT
//! The remote's loopback receiver for the agent CLIs' OpenTelemetry export. A CLI run on this
//! machine is pointed here, and each usage record it sends goes into the spool (see `usage_spool`).
//! Plain std HTTP: one route family, no new dependency. The answers mirror the Mac's receiver:
//! 200 once stored, 503 when not stored so the exporter retries, 413 over the size limit, and
//! metrics acknowledged and not stored.
//!
//! Loopback TCP is open to every account on the machine, and a shared machine has several. On Linux
//! a connection is served only when the account on its other end is this one, so another account
//! cannot add usage to this spool. Elsewhere (a Mac's own terminal server) the machine is taken to
//! be single-user, as the Mac's own receiver is.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::usage_spool::Spool;

/// The largest export one request may carry, as on the Mac's receiver.
const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_LINE: u64 = 8 * 1024;
const ENDPOINT_PATH: &str = "/api/v1/telemetry";
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Header lines past the request line; a request with more is refused.
const MAX_HEADERS: usize = 64;
/// Connections served at once. A connection past this is refused with 503, so the exporter retries.
const MAX_CONNECTIONS: usize = 16;

/// The receiver's port and spool, once the binary has advertised them. Set once, at startup.
static ADVERTISED: OnceLock<Receiver> = OnceLock::new();

/// A started receiver: the port launches are pointed at, and the spool it appends to. The desktop's
/// reads take the same spool, so a read's prune and an append never interleave.
pub struct Receiver {
    pub port: u16,
    pub spool: Arc<Mutex<Spool>>,
}

/// Records the receiver for the agent launches this process makes and the desktop's reads.
pub fn advertise(receiver: Receiver) {
    let _ = ADVERTISED.set(receiver);
}

pub fn advertised_port() -> Option<u16> {
    ADVERTISED.get().map(|receiver| receiver.port)
}

pub fn advertised_spool() -> Option<&'static Mutex<Spool>> {
    ADVERTISED.get().map(|receiver| &*receiver.spool)
}

/// The base URL a launch's usage export is pointed at: this receiver, then the launch's token. `None`
/// when the token would not pass the receiver's route, so a launch is never pointed at a URL that
/// drops its usage.
pub fn endpoint(port: u16, token: &str) -> Option<String> {
    valid_token(token).then(|| format!("http://127.0.0.1:{port}{ENDPOINT_PATH}/{token}"))
}

/// Binds the receiver on loopback and serves it on background threads.
pub fn start(spool_dir: &Path) -> io::Result<Receiver> {
    let spool = Arc::new(Mutex::new(Spool::open(spool_dir)?));
    let served = spool.clone();
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let open = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("usage-receiver".into())
        .spawn(move || {
            for mut stream in listener.incoming().flatten() {
                if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::AcqRel);
                    let _ = respond(&mut stream, "503 Service Unavailable", "{}");
                    continue;
                }
                let (spool, slot) = (served.clone(), open.clone());
                let spawned = std::thread::Builder::new().spawn(move || {
                    let _ = handle(stream, &spool);
                    slot.fetch_sub(1, Ordering::AcqRel);
                });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })?;
    Ok(Receiver { port, spool })
}

struct Request {
    method: String,
    path: String,
    content_length: Option<usize>,
    chunked: bool,
}

enum Head {
    /// The connection closed before a request.
    Closed,
    TooManyHeaders,
    Request(Request),
}

fn handle(mut stream: TcpStream, spool: &Mutex<Spool>) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    #[cfg(target_os = "linux")]
    match peer_is_this_account(&stream) {
        Some(true) => {}
        Some(false) => return respond(&mut stream, "403 Forbidden", "{}"),
        // The connection's owner could not be read (it may already be closing): not stored, and
        // the exporter retries.
        None => return respond(&mut stream, "503 Service Unavailable", "{}"),
    }
    let mut reader = BufReader::new(stream.try_clone()?);
    let head = match read_head(&mut reader)? {
        Head::Closed => return Ok(()),
        Head::TooManyHeaders => {
            return respond(&mut stream, "431 Request Header Fields Too Large", "{}")
        }
        Head::Request(head) => head,
    };
    if head.chunked {
        return respond(&mut stream, "411 Length Required", "{}");
    }
    let Some(length) = head.content_length else {
        return respond(&mut stream, "411 Length Required", "{}");
    };
    if length > MAX_EXPORT_BYTES {
        return respond(&mut stream, "413 Payload Too Large", "{}");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;

    let Some((token, signal)) = route(&head.path) else {
        return respond(&mut stream, "404 Not Found", "{}");
    };
    if head.method != "POST" {
        return respond(&mut stream, "405 Method Not Allowed", "{}");
    }
    match signal {
        "metrics" => respond(&mut stream, "200 OK", "{}"),
        _ => {
            let Ok(export) = serde_json::from_slice::<Value>(&body) else {
                return respond(&mut stream, "400 Bad Request", "{}");
            };
            let stored = spool
                .lock()
                .map_err(|_| ())
                .and_then(|mut s| s.append(token, &export).map_err(|_| ()));
            match stored {
                Ok(_) => respond(&mut stream, "200 OK", "{}"),
                Err(()) => respond(&mut stream, "503 Service Unavailable", "{}"),
            }
        }
    }
}

/// Reads the request line and headers.
fn read_head(reader: &mut BufReader<TcpStream>) -> io::Result<Head> {
    let mut line = String::new();
    if (&mut *reader).take(MAX_HEADER_LINE).read_line(&mut line)? == 0 {
        return Ok(Head::Closed);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut head = Request {
        method,
        path,
        content_length: None,
        chunked: false,
    };
    // The blank line that ends the headers, or the end of the stream, must come within the limit.
    let mut head_ended = false;
    for _ in 0..=MAX_HEADERS {
        let mut header = String::new();
        if (&mut *reader)
            .take(MAX_HEADER_LINE)
            .read_line(&mut header)?
            == 0
        {
            head_ended = true;
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            head_ended = true;
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                head.content_length = value.parse().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                head.chunked = value.to_ascii_lowercase().contains("chunked");
            }
        }
    }
    if !head_ended {
        return Ok(Head::TooManyHeaders);
    }
    Ok(Head::Request(head))
}

/// `/api/v1/telemetry/{token}/v1/{logs|metrics}` to its token and signal. The token is limited to
/// characters that cannot form a path: no slash, and not made of dots alone (`..`).
fn route(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(ENDPOINT_PATH)?.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    let [token, "v1", signal] = parts.as_slice() else {
        return None;
    };
    (valid_token(token) && matches!(*signal, "logs" | "metrics")).then_some((*token, *signal))
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 128
        && token.bytes().any(|b| b != b'.')
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Whether the account on the other end of `stream` is this process's. `None` when the kernel's
/// table no longer lists the connection.
#[cfg(target_os = "linux")]
fn peer_is_this_account(stream: &TcpStream) -> Option<bool> {
    let client = stream.peer_addr().ok()?.port();
    let server = stream.local_addr().ok()?.port();
    let table = std::fs::read_to_string("/proc/net/tcp").ok()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    uid_of_client(&table, client, server).map(|uid| uid == me)
}

/// In a `/proc/net/tcp` table, the uid that owns the client end of a loopback connection: the socket
/// whose local port is `client_port` and whose remote port is `server_port`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn uid_of_client(table: &str, client_port: u16, server_port: u16) -> Option<u32> {
    let port = |address: &str| u16::from_str_radix(address.rsplit(':').next()?, 16).ok();
    table.lines().skip(1).find_map(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        let (local, remote, uid) = (columns.get(1)?, columns.get(2)?, columns.get(7)?);
        (port(local)? == client_port && port(remote)? == server_port)
            .then(|| uid.parse().ok())
            .flatten()
    })
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(port: u16, path: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    fn status(reply: &str) -> &str {
        reply.lines().next().unwrap_or_default()
    }

    fn usage_export() -> String {
        serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {"attributes": [
                {"key": "event.name", "value": {"stringValue": "api_request"}},
                {"key": "request_id", "value": {"stringValue": "r1"}},
            ]},
            {"attributes": [
                {"key": "event.name", "value": {"stringValue": "user_prompt"}},
                {"key": "prompt", "value": {"stringValue": "secret text"}},
            ]},
        ]}]}]})
        .to_string()
    }

    fn stored_lines(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("spool.jsonl"))
            .unwrap()
            .lines()
            .count()
    }

    #[test]
    fn a_usage_export_is_stored_and_acknowledged() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        let reply = post(port, "/api/v1/telemetry/tok-1/v1/logs", &usage_export());
        assert_eq!(status(&reply), "HTTP/1.1 200 OK");
        assert_eq!(stored_lines(dir.path()), 1);
    }

    #[test]
    fn metrics_are_acknowledged_and_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        let reply = post(port, "/api/v1/telemetry/tok-1/v1/metrics", &usage_export());
        assert_eq!(status(&reply), "HTTP/1.1 200 OK");
        assert_eq!(stored_lines(dir.path()), 0);
    }

    #[test]
    fn an_unknown_route_or_a_bad_token_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        assert_eq!(
            status(&post(port, "/api/v1/other/tok/v1/logs", "{}")),
            "HTTP/1.1 404 Not Found"
        );
        assert_eq!(
            status(&post(port, "/api/v1/telemetry/../v1/logs", "{}")),
            "HTTP/1.1 404 Not Found"
        );
        assert_eq!(
            status(&post(port, "/api/v1/telemetry//v1/logs", "{}")),
            "HTTP/1.1 404 Not Found"
        );
    }

    #[test]
    fn invalid_json_is_refused_and_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        let reply = post(port, "/api/v1/telemetry/tok-1/v1/logs", "{not json");
        assert_eq!(status(&reply), "HTTP/1.1 400 Bad Request");
        assert_eq!(stored_lines(dir.path()), 0);
    }

    #[test]
    fn an_oversized_export_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST /api/v1/telemetry/tok-1/v1/logs HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_EXPORT_BYTES + 1
        )
        .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert_eq!(status(&reply), "HTTP/1.1 413 Payload Too Large");
    }

    #[test]
    fn the_client_end_of_a_loopback_connection_is_found_by_its_ports() {
        // The server's end (local 8000, remote 41000) belongs to 1000; the client's (local 41000,
        // remote 8000) to 1001. Ports are hex.
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F40 0100007F:A028 01 00000000:00000000 00:00000000 00000000  1000        0 11111 1
   1: 0100007F:A028 0100007F:1F40 01 00000000:00000000 00:00000000 00000000  1001        0 22222 1";
        assert_eq!(uid_of_client(table, 0xA028, 0x1F40), Some(1001));
        assert_eq!(uid_of_client(table, 0x1F40, 0xA028), Some(1000));
        assert_eq!(uid_of_client(table, 1234, 0x1F40), None);
    }

    #[test]
    fn a_request_with_too_many_header_lines_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut request = String::from("POST /api/v1/telemetry/tok-1/v1/logs HTTP/1.1\r\n");
        // One header line past the limit, and nothing after it, so the server reads everything sent
        // and its close is not a reset that could swallow the reply.
        for i in 0..=MAX_HEADERS {
            request.push_str(&format!("X-Filler-{i}: 1\r\n"));
        }
        stream.write_all(request.as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert_eq!(
            status(&reply),
            "HTTP/1.1 431 Request Header Fields Too Large"
        );
    }

    #[test]
    fn a_store_failure_is_503_so_the_exporter_retries() {
        let dir = tempfile::tempdir().unwrap();
        let port = start(dir.path()).unwrap().port;
        // Replacing the spool file with a directory makes the next append fail.
        let file = dir.path().join("spool.jsonl");
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        let reply = post(port, "/api/v1/telemetry/tok-1/v1/logs", &usage_export());
        assert_eq!(status(&reply), "HTTP/1.1 503 Service Unavailable");
    }
}
