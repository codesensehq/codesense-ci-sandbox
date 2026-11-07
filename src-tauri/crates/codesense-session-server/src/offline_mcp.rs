// SPDX-License-Identifier: MIT
//! What keeps a remote agent's CodeSense MCP useful while no
//! desktop relays it, and whole when one comes back.
//!
//! A remote agent's CodeSense MCP is reverse-proxied through the desktop. When
//! the desktop goes away its bridge stays up (`reverse_mcp`): the machine
//! answers the stream itself, and hands it back to the desktop when one
//! returns. Beyond the `routine_done` / `signal_done` it answers
//! (`routine_arm::local_mcp`), the machine:
//!
//! - answers `list_wakeups` and `cancel_wakeup` for the agent's own session
//!   from the timers it holds;
//! - queues `send_message`, `inbox_post` and `signal_progress` in a
//!   per-session outbox, delivered to the desktop when it reconnects, in
//!   order, each under an outbox id the desktop answers once
//!   (`params._meta.codesenseOutboxId`);
//! - answers a call that was in flight when the desktop went away with a
//!   typed `desktop_offline` that says it may or may not have run: it is never
//!   replayed, so a reattach duplicates nothing.
//!
//! Everything else (`hydrate_*`, `reverse_sync_worktree`, `query_context`...)
//! gets a typed `desktop_offline`.
//!
//! The helpers here are pure: `reverse_mcp` wires them to its streams.

use std::collections::VecDeque;

use codesense_pty_core::session_wake::{TimerInfo, TimerSchedule};
use serde_json::{json, Value};

use crate::routine_arm::local_mcp::{DESKTOP_OFFLINE, DESKTOP_OFFLINE_CODE};

/// The tools the machine queues while no desktop relays the session.
pub const OUTBOX_TOOLS: &[&str] = &["send_message", "inbox_post", "signal_progress"];
/// The `params._meta` key a queued call is delivered under; the desktop
/// answers each id once.
pub const OUTBOX_META: &str = "codesenseOutboxId";
/// The most calls one session's outbox holds.
pub const OUTBOX_CAP: usize = 200;
/// The request id of the `initialize` replayed to a returning desktop; its
/// answer is the machine's, never the agent's.
pub const REATTACH_ID: &str = "codesense-reattach";
/// The request id of the outbox delivery's own `initialize`.
pub const FLUSH_INIT_ID: &str = "codesense-outbox-init";
/// The request id prefix of a delivered outbox call.
pub const FLUSH_ID_PREFIX: &str = "codesense-outbox:";
/// Tells an agent to list its tools again: the desktop's are back, or gone.
pub const LIST_CHANGED: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

/// One JSON-RPC request line (an `id` that is not null, and a `method`).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: Value,
    pub method: String,
    /// `params.name` of a `tools/call`.
    pub tool: Option<String>,
    pub args: Value,
}

impl Call {
    /// What the call is, for a message: its tool, or its method.
    pub fn what(&self) -> String {
        self.tool.clone().unwrap_or_else(|| self.method.clone())
    }
}

/// `line` as a request, or `None` for a notification, a response or a line
/// that is not JSON-RPC.
pub fn parse(line: &str) -> Option<Call> {
    let message: Value = serde_json::from_str(line.trim()).ok()?;
    let id = message.get("id").filter(|id| !id.is_null())?.clone();
    let method = message.get("method")?.as_str()?.to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let tool = (method == "tools/call")
        .then(|| {
            params
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    Some(Call {
        id,
        method,
        tool,
        args,
    })
}

/// The id of a response line (an `id` and no `method`).
pub fn response_id(line: &str) -> Option<Value> {
    let message: Value = serde_json::from_str(line.trim()).ok()?;
    if message.get("method").is_some() {
        return None;
    }
    message.get("id").filter(|id| !id.is_null()).cloned()
}

/// Whether a response line is an error.
pub fn is_error(line: &str) -> bool {
    serde_json::from_str::<Value>(line.trim())
        .ok()
        .is_some_and(|message| message.get("error").is_some())
}

/// Whether a response line is the machine's `desktop_offline` error.
pub fn is_desktop_offline(line: &str) -> bool {
    serde_json::from_str::<Value>(line.trim())
        .ok()
        .is_some_and(|message| message["error"]["data"]["type"] == DESKTOP_OFFLINE)
}

/// A key a request id is remembered by.
pub fn id_key(id: &Value) -> String {
    id.to_string()
}

/// `line` with its request `id` replaced by `id`.
pub fn with_id(line: &str, id: Value) -> Option<String> {
    let mut message: Value = serde_json::from_str(line.trim()).ok()?;
    message.as_object_mut()?.insert("id".into(), id);
    Some(message.to_string())
}

/// The queued call `line` as delivered: id `codesense-outbox:<key>` and
/// `params._meta.codesenseOutboxId = key`, so the desktop answers it once.
pub fn delivery_line(line: &str, key: &str) -> Option<String> {
    let mut message: Value = serde_json::from_str(line.trim()).ok()?;
    let object = message.as_object_mut()?;
    object.insert("id".into(), json!(format!("{FLUSH_ID_PREFIX}{key}")));
    let params = object
        .entry("params")
        .or_insert_with(|| json!({}))
        .as_object_mut()?;
    let meta = params
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()?;
    meta.insert(OUTBOX_META.into(), json!(key));
    Some(message.to_string())
}

fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error(id: &Value, code: i64, message: String, kind: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message, "data": {"type": kind}}
    })
    .to_string()
}

fn text(body: Value) -> Value {
    json!({"content": [{"type": "text", "text": body.to_string()}], "isError": false})
}

/// The answer to a call that was on its way to the desktop when it went
/// away: it may or may not have run, and it is not retried.
pub fn in_flight_error(id: &Value, what: &str) -> String {
    error(
        id,
        DESKTOP_OFFLINE_CODE,
        format!(
            "desktop_offline: your desktop disconnected while {what} was in flight, so it may or may not have run. CodeSense does not retry it (that could run it twice): check its effect before calling it again, once your desktop is back."
        ),
        DESKTOP_OFFLINE,
    )
}

/// The answer to an outbox call the machine queued as `key`, `position` in
/// line.
pub fn queued_answer(id: &Value, tool: &str, key: &str, position: usize) -> String {
    result(
        id,
        text(json!({
            "status": "ok",
            "data": {
                "queued": true,
                "outboxId": key,
                "position": position,
                "tool": tool,
                "deliveredWhen": "your desktop reconnects to this machine, in the order you sent them",
                "ifThisSessionEndsFirst": "still delivered for a tab your desktop launched or a routine's agent; otherwise recorded on this machine as not delivered"
            }
        })),
    )
}

/// The answer to a line of the outbox delivery's own stream when the
/// desktop is gone: it stops, and keeps what it has not delivered.
pub fn delivery_refused(id: &Value) -> String {
    error(
        id,
        DESKTOP_OFFLINE_CODE,
        "desktop_offline: the outbox delivery's desktop is gone".into(),
        DESKTOP_OFFLINE,
    )
}

/// The refusal of a timer tool naming another session.
pub fn other_session(id: &Value, tool: &str) -> String {
    error(
        id,
        -32602,
        format!("{tool}: only your own session's timers (your $CODESENSE_TAB_SESSION_ID)"),
        "refused",
    )
}

fn rfc3339(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(i64::MAX))
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn duration(ms: u64) -> String {
    let mut seconds = ms / 1_000;
    let mut out = String::new();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if seconds >= size {
            out.push_str(&format!("{}{unit}", seconds / size));
            seconds %= size;
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// `list_wakeups` answered by the machine: `timers`, soonest first, in the
/// desktop's row shape (without `machineId`, which only the desktop knows).
pub fn timers_answer(id: &Value, mut timers: Vec<TimerInfo>) -> String {
    timers.sort_by_key(|timer| timer.next_at_ms);
    let rows: Vec<Value> = timers
        .into_iter()
        .map(|info| {
            let every = match &info.schedule {
                TimerSchedule::After { every_ms, .. } => every_ms.map(duration),
                TimerSchedule::Cron { .. } => None,
            };
            json!({
                "timerId": info.timer_id,
                "name": info.name,
                "prompt": info.text,
                "nextAt": rfc3339(info.next_at_ms),
                "every": every,
                "until": info.until_at_ms.map(rfc3339),
                "count": info.count,
                "remaining": info.count.map(|count| count.saturating_sub(info.fires)),
                "fires": info.fires,
                "createdBy": info.created_by,
                "sessionId": info.session.as_str(),
            })
        })
        .collect();
    result(
        id,
        text(json!({"timers": rows, "servedBy": "machine (desktop offline)"})),
    )
}

/// `cancel_wakeup` answered by the machine.
pub fn cancel_answer(id: &Value, cancelled: bool) -> String {
    result(
        id,
        text(json!({"cancelled": cancelled, "servedBy": "machine (desktop offline)"})),
    )
}

/// Whether a timer tool's `sessionId` names `session` (or none).
pub fn own_session(call: &Call, session: &str) -> bool {
    call.args
        .get("sessionId")
        .and_then(Value::as_str)
        .is_none_or(|named| named == session)
}

/// The tools the machine answers beyond `routine_done` and `signal_done`,
/// for a bridge that starts while no desktop relays it.
pub fn extra_tools() -> Vec<Value> {
    let session = json!({"type": "string", "description": "your own $CODESENSE_TAB_SESSION_ID"});
    vec![
        json!({
            "name": "list_wakeups",
            "description": "List your own session's active timers, answered by this machine while your desktop is offline.",
            "inputSchema": {"type": "object", "properties": {"sessionId": session}}
        }),
        json!({
            "name": "cancel_wakeup",
            "description": "Cancel one of your own session's timers, answered by this machine while your desktop is offline.",
            "inputSchema": {"type": "object", "properties": {"sessionId": session, "timerId": {"type": "string"}}, "required": ["timerId"]}
        }),
        json!({
            "name": "send_message",
            "description": "Send a message. While your desktop is offline it is queued on this machine and delivered when it reconnects, in order.",
            "inputSchema": {"type": "object"}
        }),
        json!({
            "name": "inbox_post",
            "description": "Post to an inbox. While your desktop is offline it is queued on this machine and delivered when it reconnects, in order.",
            "inputSchema": {"type": "object"}
        }),
        json!({
            "name": "signal_progress",
            "description": "Report progress. While your desktop is offline it is queued on this machine and delivered when it reconnects, in order.",
            "inputSchema": {"type": "object"}
        }),
    ]
}

/// A `tools/list` answer with [`extra_tools`] added.
pub fn with_extra_tools(answer: &str) -> String {
    let Ok(mut message) = serde_json::from_str::<Value>(answer.trim()) else {
        return answer.to_string();
    };
    if let Some(tools) = message
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)
    {
        tools.extend(extra_tools());
    }
    message.to_string()
}

/// The most bytes one session's outbox holds.
pub const OUTBOX_SESSION_BYTES: usize = 4 * 1024 * 1024;
/// The most bytes every outbox on this host holds together.
pub const OUTBOX_HOST_BYTES: usize = 64 * 1024 * 1024;

/// What a queued call is missing, if anything: the desktop would refuse it,
/// so the agent hears now, not after the desktop is back.
pub fn missing(call: &Call) -> Option<&'static str> {
    let filled = |name: &str| {
        call.args
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    match call.tool.as_deref()? {
        "send_message" if !filled("to") => Some("to"),
        "send_message" if !filled("text") => Some("text"),
        "inbox_post" if !filled("title") => Some("title"),
        "signal_progress" if !filled("phase") => Some("phase"),
        _ => None,
    }
}

/// The refusal of a queued call missing `field`.
pub fn invalid(id: &Value, tool: &str, field: &str) -> String {
    error(
        id,
        -32602,
        format!("{tool} needs `{field}` (not queued)"),
        "invalid_params",
    )
}

/// `line` without an outbox id the agent put in it: only the machine names
/// a delivery (a forged one could make the desktop skip a real call).
pub fn strip_outbox_meta(line: &str) -> Option<String> {
    if !line.contains(OUTBOX_META) {
        return None;
    }
    let mut message: Value = serde_json::from_str(line.trim()).ok()?;
    let meta = message.pointer_mut("/params/_meta")?.as_object_mut()?;
    meta.remove(OUTBOX_META)?;
    Some(message.to_string())
}

/// The answer to an outbox call when the outbox is full (`why`).
pub fn outbox_full_because(id: &Value, tool: &str, why: &str) -> String {
    error(
        id,
        DESKTOP_OFFLINE_CODE,
        format!(
            "desktop_offline: {tool} needs CodeSense on your desktop, which is not connected to this machine, and {why}. Retry once it is back."
        ),
        DESKTOP_OFFLINE,
    )
}

/// The inbox note telling the person a queued call was refused on delivery
/// (`answer`, the desktop's), as an `inbox_post` delivered under `key`.
pub fn refusal_note(queued: &Queued, answer: &str, key: &str) -> Option<String> {
    let call = parse(&queued.line)?;
    let tool = call.what();
    let why = serde_json::from_str::<Value>(answer.trim())
        .ok()
        .and_then(|message| {
            message["error"]["message"]
                .as_str()
                .or_else(|| message["result"]["content"][0]["text"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let line = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "tools/call",
        "params": {
            "name": "inbox_post",
            "arguments": {
                "title": format!("A {tool} an agent queued while your desktop was away was refused"),
                "context": format!("Outbox id {}: {}\n\nThe call: {}", queued.key, why.chars().take(2000).collect::<String>(), call.args.to_string().chars().take(2000).collect::<String>()),
                "need": "Check what the agent meant to send; it was not delivered."
            }
        }
    });
    delivery_line(&line.to_string(), key)
}

/// Whether a delivered answer is the desktop's refusal of the call.
pub fn refused(answer: &str) -> bool {
    is_error(answer)
        || serde_json::from_str::<Value>(answer.trim())
            .ok()
            .is_some_and(|message| message["result"]["isError"] == true)
}

/// A queued call: its outbox id and its line as the agent sent it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Queued {
    pub key: String,
    pub line: String,
}

/// One session's outbox. It outlives the session: a routine's agent that
/// finished while the desktop was away still has its calls delivered.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Outbox {
    pub session: String,
    entries: VecDeque<Queued>,
    /// The desktop identity it is delivered to.
    #[serde(default)]
    pub desktop: Option<String>,
    /// The session ended; only its queued calls remain.
    #[serde(default)]
    pub ended: bool,
    /// When it ended (ms since the epoch): kept [`ENDED_KEEP_MS`], then lost.
    #[serde(default)]
    pub ended_at_ms: Option<u64>,
    /// A delivery is under way.
    #[serde(skip)]
    pub flushing: bool,
}

impl Outbox {
    pub fn new(session: &str) -> Self {
        Self {
            session: session.to_string(),
            ..Self::default()
        }
    }

    /// Queue `line` as `key`, within `host_left` bytes left on this host: its
    /// position, or why not.
    pub fn push(&mut self, key: String, line: &str, host_left: usize) -> Result<usize, String> {
        let line = line.trim_end().to_string();
        if self.entries.len() >= OUTBOX_CAP {
            return Err(format!(
                "this session already has {OUTBOX_CAP} calls queued"
            ));
        }
        if self.bytes() + line.len() > OUTBOX_SESSION_BYTES {
            return Err("this session's queued calls are at their size limit".into());
        }
        if line.len() > host_left {
            return Err("this machine's queued calls are at their size limit".into());
        }
        self.entries.push_back(Queued { key, line });
        Ok(self.entries.len())
    }

    /// The next call to deliver.
    pub fn front(&self) -> Option<Queued> {
        self.entries.front().cloned()
    }

    /// `key` was delivered (the desktop answered it).
    pub fn delivered(&mut self, key: &str) {
        if self.entries.front().is_some_and(|entry| entry.key == key) {
            self.entries.pop_front();
        }
    }

    /// The call queued last, taken back (it could not be kept).
    pub fn unpush(&mut self, key: &str) {
        if self.entries.back().is_some_and(|entry| entry.key == key) {
            self.entries.pop_back();
        }
    }

    /// Mark it ended at `now_ms` (once).
    pub fn end(&mut self, now_ms: u64) {
        self.ended = true;
        self.ended_at_ms.get_or_insert(now_ms);
    }

    /// Ended longer than [`ENDED_KEEP_MS`] ago.
    pub fn expired(&self, now_ms: u64) -> bool {
        self.ended
            && self
                .ended_at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) > ENDED_KEEP_MS)
    }

    /// Everything still queued, taken out (a loss being recorded).
    pub fn drain(&mut self) -> Vec<Queued> {
        self.entries.drain(..).collect()
    }

    pub fn bytes(&self) -> usize {
        self.entries.iter().map(|entry| entry.line.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// How long an ended session's queued calls wait for its desktop: then they
/// are recorded as not delivered.
pub const ENDED_KEEP_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// The size the lost-call log rotates at (one older generation is kept).
pub const LOST_LOG_BYTES: u64 = 1024 * 1024;
/// The most lost calls kept for one desktop's next note.
const NOTE_PENDING: usize = 100;
/// The outbox key prefix of a note the machine adds itself (summarizing lost
/// calls): refused or not, it is never recorded as lost in turn.
pub const NOTE_KEY_PREFIX: &str = "note-";

/// Whether a record's `routine_host` (the desktop install that
/// armed it) is the install `host_part` of a named desktop identity
/// (`<host>.<person>`, the host kept to letters, digits and `-`).
pub fn same_install(routine_host: Option<&str>, host_part: &str) -> bool {
    routine_host.is_some_and(|host| {
        let clean: String = host
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(64)
            .collect();
        !clean.is_empty() && clean == host_part
    })
}

/// Milliseconds since the epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// The error data type a desktop answers when it could not record an outbox
/// id: the call did not run; deliver it again later.
pub const OUTBOX_RETRY: &str = "outbox_retry";

/// Whether a delivered answer asks for the call again later.
pub fn retry_later(answer: &str) -> bool {
    serde_json::from_str::<Value>(answer.trim())
        .ok()
        .is_some_and(|message| message["error"]["data"]["type"] == OUTBOX_RETRY)
}

/// The one inbox note telling the person which queued calls were not
/// delivered (`lost`, as recorded), as an `inbox_post` line.
pub fn lost_note(lost: &[Value]) -> String {
    let lines: Vec<String> = lost
        .iter()
        .take(20)
        .map(|entry| {
            let call = parse(entry["call"].as_str().unwrap_or_default());
            format!(
                "- {} from session {}: {}",
                call.map(|call| call.what())
                    .unwrap_or_else(|| "a call".into()),
                entry["session"].as_str().unwrap_or_default(),
                entry["why"].as_str().unwrap_or_default()
            )
        })
        .collect();
    let more = lost.len().saturating_sub(lines.len());
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "tools/call",
        "params": {
            "name": "inbox_post",
            "arguments": {
                "title": format!("{} call(s) agents queued on a machine were not delivered", lost.len()),
                "context": format!("{}{}", lines.join("\n"), if more > 0 { format!("\n…and {more} more (see outbox/lost.jsonl on the machine)") } else { String::new() }),
                "need": "Check what those agents meant to send."
            }
        }
    })
    .to_string()
}

/// Where outboxes are kept on disk, so a terminal-server restart (or the
/// session ending) loses no queued call: one file per session, replaced
/// whole, and a log of the calls that could not be delivered.
#[derive(Debug, Clone)]
pub struct OutboxStore {
    dir: Option<std::path::PathBuf>,
}

impl OutboxStore {
    /// Keep them under `dir` (`None`: in memory only).
    pub fn at(dir: Option<std::path::PathBuf>) -> Self {
        if let Some(dir) = &dir {
            let _ = std::fs::create_dir_all(dir);
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Self { dir }
    }

    fn file(&self, session: &str) -> Option<std::path::PathBuf> {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(session.as_bytes());
        let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        Some(self.dir.as_ref()?.join(format!("{name}.json")))
    }

    /// Save `outbox` (or remove its file once it is empty).
    pub fn save(&self, outbox: &Outbox) -> Result<(), String> {
        let Some(path) = self.file(&outbox.session) else {
            return Ok(());
        };
        if outbox.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error.to_string())
                }
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec(outbox).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// [`save`](Self::save), logging a failure (the calls are still held in
    /// memory).
    pub fn keep(&self, outbox: &Outbox) {
        if let Err(error) = self.save(outbox) {
            log::warn!(
                "session {}: queued calls not saved: {error}",
                outbox.session
            );
        }
    }

    fn pending_notes(&self, desktop: &str) -> Option<std::path::PathBuf> {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(desktop.as_bytes());
        let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        Some(self.dir.as_ref()?.join(format!("notes-{name}.jsonl")))
    }

    /// The lost calls `desktop` has not been told of yet, taken (it is told
    /// once).
    pub fn take_notes(&self, desktop: &str) -> Vec<Value> {
        let Some(path) = self.pending_notes(desktop) else {
            return Vec::new();
        };
        let notes = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let _ = std::fs::remove_file(&path);
        notes
    }

    /// Every outbox saved, as left by the last run.
    pub fn load(&self) -> Vec<Outbox> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok())
            .filter(|outbox: &Outbox| !outbox.is_empty())
            .collect()
    }

    /// Move `from`'s (a viewer's) pending loss notes for the sessions `ours`
    /// keeps to `to` (the desktop identity that named itself).
    pub fn rename_notes(&self, from: &str, to: &str, ours: impl Fn(&str) -> bool) {
        let notes = self.take_notes(from);
        let (moved, kept): (Vec<Value>, Vec<Value>) = notes
            .into_iter()
            .partition(|entry| ours(entry["session"].as_str().unwrap_or_default()));
        use std::io::Write;
        for (desktop, entries) in [(to, moved), (from, kept)] {
            let Some(path) = self.pending_notes(desktop) else {
                continue;
            };
            if entries.is_empty() {
                continue;
            }
            if let Ok(mut log) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                for entry in entries {
                    let _ = writeln!(log, "{entry}");
                }
            }
        }
    }

    /// Record `lost` calls of `session` that will never be delivered, and
    /// why; `desktop` (the one that relayed it) is told once it connects.
    pub fn record_lost(&self, session: &str, lost: &[Queued], why: &str, desktop: Option<&str>) {
        let lost: Vec<&Queued> = lost
            .iter()
            .filter(|queued| !queued.key.starts_with(NOTE_KEY_PREFIX))
            .collect();
        if lost.is_empty() {
            return;
        }
        log::warn!(
            "session {session}: {} queued call(s) not delivered: {why}",
            lost.len()
        );
        let Some(dir) = &self.dir else {
            return;
        };
        use std::io::Write;
        let path = dir.join("lost.jsonl");
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOST_LOG_BYTES) {
            let _ = std::fs::rename(&path, dir.join("lost.1.jsonl"));
        }
        let entries: Vec<Value> = lost
            .iter()
            .map(|queued| {
                json!({"session": session, "outboxId": queued.key, "call": queued.line, "why": why, "desktop": desktop})
            })
            .collect();
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            for entry in &entries {
                let _ = writeln!(log, "{entry}");
            }
        }
        if let Some(notes) = desktop.and_then(|desktop| self.pending_notes(desktop)) {
            let pending = std::fs::read_to_string(&notes)
                .map(|text| text.lines().count())
                .unwrap_or(0);
            if let Ok(mut log) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&notes)
            {
                for entry in entries.iter().take(NOTE_PENDING.saturating_sub(pending)) {
                    let _ = writeln!(log, "{entry}");
                }
            }
        }
    }

    /// The calls recorded as lost (tests, diagnostics).
    pub fn lost(&self) -> Vec<Value> {
        self.dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("lost.jsonl")).ok())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::domain::PtySessionId;
    use codesense_pty_core::session_wake::Originator;

    fn call(id: Value, tool: &str, args: Value) -> String {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}})
            .to_string()
    }

    #[test]
    fn requests_and_responses_are_told_apart() {
        let request = parse(&call(json!(7), "send_message", json!({"to": "x"}))).unwrap();
        assert_eq!(request.id, json!(7));
        assert_eq!(request.tool.as_deref(), Some("send_message"));
        assert_eq!(request.what(), "send_message");
        assert!(parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(parse(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).is_none());
        assert!(parse(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#).is_none());
        assert_eq!(
            response_id(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#),
            Some(json!("a"))
        );
        assert_eq!(response_id(&call(json!(1), "x", json!({}))), None);
        assert!(is_error(r#"{"id":1,"error":{"code":1}}"#));
        assert!(!is_error(r#"{"id":1,"result":{}}"#));
    }

    // AC (in flight at the disconnect): typed, says it may have run, never
    // claims a retry.
    #[test]
    fn a_call_in_flight_at_a_disconnect_is_typed_and_never_retried() {
        let answer: Value =
            serde_json::from_str(&in_flight_error(&json!(9), "hydrate_worktree")).unwrap();
        assert_eq!(answer["id"], 9);
        assert_eq!(answer["error"]["code"], DESKTOP_OFFLINE_CODE);
        assert_eq!(answer["error"]["data"]["type"], DESKTOP_OFFLINE);
        let message = answer["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("desktop_offline:"), "{message}");
        assert!(message.contains("may or may not have run"), "{message}");
        assert!(message.contains("does not retry"), "{message}");
        assert!(is_desktop_offline(&in_flight_error(&json!(1), "x")));
    }

    // Outbox: in order, capped, and delivered under its id (idempotent).
    #[test]
    fn the_outbox_keeps_order_and_delivers_each_call_under_its_id() {
        let mut outbox = Outbox::new("s1");
        let first = call(json!(1), "send_message", json!({"to": "lead", "text": "a"}));
        let second = call(json!(2), "inbox_post", json!({"title": "b"}));
        assert_eq!(outbox.push("k1".into(), &first, usize::MAX), Ok(1));
        assert_eq!(outbox.push("k2".into(), &second, usize::MAX), Ok(2));
        let next = outbox.front().unwrap();
        assert_eq!(next.key, "k1");
        let delivered: Value =
            serde_json::from_str(&delivery_line(&next.line, &next.key).unwrap()).unwrap();
        assert_eq!(delivered["id"], "codesense-outbox:k1");
        assert_eq!(delivered["params"]["_meta"][OUTBOX_META], "k1");
        assert_eq!(delivered["params"]["arguments"]["text"], "a");
        // Only the front is ever taken off, so a late answer cannot skip one.
        outbox.delivered("k2");
        assert_eq!(outbox.len(), 2);
        outbox.delivered("k1");
        assert_eq!(outbox.front().unwrap().key, "k2");
        outbox.delivered("k2");
        assert!(outbox.is_empty());
        for n in 0..OUTBOX_CAP {
            assert!(outbox.push(format!("k{n}"), &first, usize::MAX).is_ok());
        }
        assert!(outbox.push("over".into(), &first, usize::MAX).is_err());
        let full: Value =
            serde_json::from_str(&outbox_full_because(&json!(5), "send_message", "full")).unwrap();
        assert_eq!(full["error"]["data"]["type"], DESKTOP_OFFLINE);
        let queued: Value =
            serde_json::from_str(&queued_answer(&json!(4), "send_message", "k9", 3)).unwrap();
        let body: Value =
            serde_json::from_str(queued["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["data"]["queued"], true);
        assert_eq!(body["data"]["outboxId"], "k9");
        assert!(body["data"]["ifThisSessionEndsFirst"].is_string());
    }

    // SHOULD: per-session and host-wide byte caps.
    #[test]
    fn the_outbox_is_capped_in_bytes_per_session_and_per_host() {
        let mut outbox = Outbox::new("s1");
        let big = call(
            json!(1),
            "send_message",
            json!({"to": "x", "text": "y".repeat(1024 * 1024)}),
        );
        for n in 0..3 {
            assert!(outbox.push(format!("k{n}"), &big, usize::MAX).is_ok());
        }
        let refused = outbox.push("k4".into(), &big, usize::MAX).unwrap_err();
        assert!(refused.contains("size limit"), "{refused}");
        let mut other = Outbox::new("s2");
        let host = other.push("h".into(), &big, 1024).unwrap_err();
        assert!(host.contains("machine"), "{host}");
    }

    // SHOULD: required fields are checked when queued, not at delivery.
    #[test]
    fn a_call_missing_a_required_field_is_refused_when_queued() {
        let check = |tool: &str, args: Value| missing(&parse(&call(json!(1), tool, args)).unwrap());
        assert_eq!(check("send_message", json!({"text": "x"})), Some("to"));
        assert_eq!(
            check("send_message", json!({"to": "x", "text": " "})),
            Some("text")
        );
        assert_eq!(check("send_message", json!({"to": "x", "text": "y"})), None);
        assert_eq!(check("inbox_post", json!({})), Some("title"));
        assert_eq!(check("signal_progress", json!({"phase": "dev"})), None);
        assert_eq!(check("signal_progress", json!({})), Some("phase"));
        let answer: Value =
            serde_json::from_str(&invalid(&json!(1), "inbox_post", "title")).unwrap();
        assert_eq!(answer["error"]["data"]["type"], "invalid_params");
    }

    // SHOULD (forged outbox id): an agent's own outbox id is taken out.
    #[test]
    fn an_agents_own_outbox_id_is_stripped() {
        let forged = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{},"_meta":{OUTBOX_META:"stolen","progressToken":1}}}).to_string();
        let clean: Value = serde_json::from_str(&strip_outbox_meta(&forged).unwrap()).unwrap();
        assert!(clean["params"]["_meta"].get(OUTBOX_META).is_none());
        assert_eq!(clean["params"]["_meta"]["progressToken"], 1);
        assert_eq!(strip_outbox_meta(&call(json!(1), "x", json!({}))), None);
    }

    // Outboxes survive a restart; a loss is recorded.
    #[test]
    fn outboxes_are_kept_on_disk_and_losses_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let store = OutboxStore::at(Some(dir.path().join("outbox")));
        let mut outbox = Outbox::new("routine-1");
        outbox.desktop = Some("mac-1".into());
        outbox.ended = true;
        outbox
            .push(
                "k1".into(),
                &call(json!(1), "send_message", json!({"to": "a", "text": "b"})),
                usize::MAX,
            )
            .unwrap();
        store.save(&outbox).unwrap();
        let loaded = OutboxStore::at(Some(dir.path().join("outbox"))).load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session, "routine-1");
        assert_eq!(loaded[0].desktop.as_deref(), Some("mac-1"));
        assert!(loaded[0].ended);
        assert_eq!(loaded[0].front().unwrap().key, "k1");
        let lost = outbox.drain();
        store.record_lost("routine-1", &lost, "no authority", Some("mac-1"));
        store.save(&outbox).unwrap();
        assert!(store.load().is_empty());
        assert_eq!(store.lost()[0]["outboxId"], "k1");
        assert_eq!(store.lost()[0]["why"], "no authority");
    }

    // An ended session's calls expire after ~30 days; the lost
    // log rotates; the desktop is told of its lost calls once.
    #[test]
    fn ended_outboxes_expire_and_lost_calls_are_noted_once() {
        let mut outbox = Outbox::new("s");
        assert!(!outbox.expired(u64::MAX));
        outbox.end(1_000);
        outbox.end(9_000);
        assert_eq!(outbox.ended_at_ms, Some(1_000), "ended once");
        assert!(!outbox.expired(1_000 + ENDED_KEEP_MS));
        assert!(outbox.expired(1_001 + ENDED_KEEP_MS));
        outbox
            .push(
                "k1".into(),
                &call(json!(1), "inbox_post", json!({"title": "t"})),
                usize::MAX,
            )
            .unwrap();
        outbox
            .push(
                "k2".into(),
                &call(json!(2), "inbox_post", json!({"title": "u"})),
                usize::MAX,
            )
            .unwrap();
        outbox.unpush("k1");
        assert_eq!(outbox.len(), 2, "only the last one is taken back");
        outbox.unpush("k2");
        assert_eq!(outbox.len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let store = OutboxStore::at(Some(dir.path().to_path_buf()));
        let queued = |key: &str| Queued {
            key: key.into(),
            line: call(json!(1), "send_message", json!({"to": "a", "text": "b"})),
        };
        store.record_lost(
            "s",
            &[queued("k1"), queued("note-x")],
            "expired",
            Some("mac-1"),
        );
        assert_eq!(
            store.lost().len(),
            1,
            "a machine note is never recorded lost"
        );
        assert!(store.take_notes("mac-2").is_empty());
        let notes = store.take_notes("mac-1");
        assert_eq!(notes.len(), 1);
        assert!(store.take_notes("mac-1").is_empty(), "told once");
        let note: Value = serde_json::from_str(&lost_note(&notes)).unwrap();
        assert_eq!(note["params"]["name"], "inbox_post");
        assert!(note["params"]["arguments"]["context"]
            .as_str()
            .unwrap()
            .contains("send_message from session s: expired"));
        std::fs::write(
            dir.path().join("lost.jsonl"),
            vec![b'x'; LOST_LOG_BYTES as usize + 1],
        )
        .unwrap();
        store.record_lost("s", &[queued("k3")], "expired", None);
        assert!(dir.path().join("lost.1.jsonl").exists(), "rotated");
        assert_eq!(store.lost().len(), 1);
        assert!(retry_later(
            r#"{"id":1,"error":{"code":-32603,"message":"x","data":{"type":"outbox_retry"}}}"#
        ));
        assert!(!retry_later(
            r#"{"id":1,"error":{"code":-32603,"message":"x"}}"#
        ));
    }

    // SHOULD: a refused delivery reaches the person as an inbox note.
    #[test]
    fn a_refused_delivery_becomes_an_inbox_note() {
        let queued = Queued {
            key: "k1".into(),
            line: call(
                json!(1),
                "send_message",
                json!({"to": "nobody", "text": "hi"}),
            ),
        };
        assert!(refused(
            r#"{"id":1,"error":{"code":-32602,"message":"unknown recipient"}}"#
        ));
        assert!(refused(
            r#"{"id":1,"result":{"isError":true,"content":[{"type":"text","text":"no"}]}}"#
        ));
        assert!(!refused(r#"{"id":1,"result":{"content":[]}}"#));
        let note: Value = serde_json::from_str(
            &refusal_note(
                &queued,
                r#"{"id":1,"error":{"message":"unknown recipient"}}"#,
                "k1-refused",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(note["params"]["name"], "inbox_post");
        assert_eq!(note["params"]["_meta"][OUTBOX_META], "k1-refused");
        assert!(note["params"]["arguments"]["context"]
            .as_str()
            .unwrap()
            .contains("unknown recipient"));
    }

    // list_wakeups answered machine-side, the agent's own session only.
    #[test]
    fn the_machine_lists_and_cancels_the_sessions_own_timers() {
        let timer = |id: &str, next: u64| TimerInfo {
            timer_id: id.into(),
            session: PtySessionId::new("s1"),
            name: format!("timer {id}"),
            created_by: "agent:Barb".into(),
            originator: Originator::default(),
            schedule: TimerSchedule::After {
                after_ms: 60_000,
                every_ms: Some(90 * 60_000),
            },
            text: "check CI".into(),
            next_at_ms: next,
            fires: 1,
            count: Some(3),
            until_at_ms: None,
            created_at_ms: 0,
            min_gap_ms: None,
        };
        let answer: Value = serde_json::from_str(&timers_answer(
            &json!(2),
            vec![
                timer("late", 1_800_000_000_000),
                timer("soon", 1_700_000_000_000),
            ],
        ))
        .unwrap();
        let body: Value =
            serde_json::from_str(answer["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let timers = body["timers"].as_array().unwrap();
        assert_eq!(timers[0]["timerId"], "soon");
        assert_eq!(timers[0]["nextAt"], "2023-11-14T22:13:20Z");
        assert_eq!(timers[0]["every"], "1h30m");
        assert_eq!(timers[0]["remaining"], 2);
        assert_eq!(timers[0]["sessionId"], "s1");
        let own = parse(&call(json!(1), "list_wakeups", json!({"sessionId": "s1"}))).unwrap();
        assert!(own_session(&own, "s1"));
        let unnamed = parse(&call(json!(1), "list_wakeups", json!({}))).unwrap();
        assert!(own_session(&unnamed, "s1"));
        let other = parse(&call(json!(1), "cancel_wakeup", json!({"sessionId": "s2"}))).unwrap();
        assert!(!own_session(&other, "s1"));
        let cancel: Value = serde_json::from_str(&cancel_answer(&json!(3), true)).unwrap();
        assert!(cancel["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"cancelled\":true"));
    }

    #[test]
    fn a_tools_list_gains_the_machines_extra_tools() {
        let answer = with_extra_tools(
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"routine_done"}]}}"#,
        );
        let value: Value = serde_json::from_str(&answer).unwrap();
        let names: Vec<&str> = value["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "routine_done",
                "list_wakeups",
                "cancel_wakeup",
                "send_message",
                "inbox_post",
                "signal_progress"
            ]
        );
        let replay: Value = serde_json::from_str(
            &with_id(
                r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#,
                json!(REATTACH_ID),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(replay["id"], REATTACH_ID);
        assert_eq!(replay["method"], "initialize");
    }
}
