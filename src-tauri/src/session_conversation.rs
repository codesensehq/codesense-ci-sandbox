// SPDX-License-Identifier: MIT
//! The one session-keyed capture of a conversation: read the CLI's own session log for a session,
//! redact it line by line **before** anything is parsed or written, parse it into the neutral
//! turn format, keep it whole up to the size limit, and index every tool call in the usage store.
//!
//! It is keyed by the CodeSense session id and needs only that session's `session_record` (the
//! native id and the CLI's home), not a run and not `signal_done`. Triggers (a turn ending, a
//! session ending) only name a session; the capture itself runs on one writer thread, off the hook
//! and PTY paths, and rebuilds the whole stored conversation each time, so repeating it, in any
//! order, leaves the same result.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use codesense_pty_core::agent_cli::output::{SessionTranscript, Turn};
use codesense_pty_core::agent_cli::{self, AgentCli};

use crate::commands::sprint_run::run_artifact::valid_run_id;
use crate::commands::sprint_run::run_transcript::parse_transcript_lines;
use crate::session_usage::conversation::{
    ConversationMeta, IndexedCall, State, Truncation, DEFAULT_MAX_BYTES, DEFAULT_RETENTION_DAYS,
};
use crate::session_usage::UsageStore;
use crate::shared::secret_scan::redact_secrets;

mod reader;
mod retention;

pub use reader::{read_conversation_in, Selector};
pub use reader::Availability;
pub(crate) use reader::{indexed_pointer, IndexedPointer};
pub use reader::ConversationPage;
pub use retention::start as start_retention_sweep;
#[cfg(test)]
pub use retention::sweep_in as sweep_expired_in;

/// A checkpoint for a session is run at most this often; the last one is never lost, only delayed.
pub const DEBOUNCE_MS: i64 = 60_000;
/// A field longer than this is cut short, but only when the kept turns still do not fit the limit.
const MAX_FIELD_BYTES: usize = 1024 * 1024;
/// The smallest a field is ever cut to.
const MIN_FIELD_BYTES: usize = 1024;
/// Room for the file's own wrapper when fitting turns into the limit.
const WRAPPER_BYTES: usize = 4096;

/// The size and retention limits. Defaults only: a per-org override is SV-CDE-35.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub retention_days: i64,
    pub max_bytes: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// A stored conversation file: the neutral transcript with where it came from and what was cut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredConversation {
    pub session_id: String,
    pub source_format: String,
    pub captured_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Truncation>,
    pub transcript: SessionTranscript,
}

/// `<codesense_home>/session-conversations`, beside `run-artifacts/`.
pub fn conversations_dir() -> Option<PathBuf> {
    crate::commands::paths::codesense_home_dir().map(|h| h.join("session-conversations"))
}

/// The file of `session_id`'s conversation, or `None` for an id that is not a plain token.
pub fn conversation_file(dir: &Path, session_id: &str) -> Option<PathBuf> {
    valid_run_id(session_id).then(|| dir.join(format!("{session_id}.json")))
}

/// A session's stored conversation file, parsed; `None` when there is none or it cannot be read.
pub fn read_stored(dir: &Path, session_id: &str) -> Option<StoredConversation> {
    let file = conversation_file(dir, session_id)?;
    match std::fs::read(&file) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(stored) => Some(stored),
            Err(e) => {
                log::warn!(target: "session_conversation", "corrupt conversation file for {session_id}: {e}");
                None
            }
        },
        Err(_) => None,
    }
}

/// The sessions with a stored conversation file in `dir`.
pub fn stored_session_ids_in(dir: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .filter(|id| valid_run_id(id))
        .collect()
}

/// The sessions with a stored conversation file.
pub fn stored_session_ids() -> HashSet<String> {
    conversations_dir()
        .map(|dir| stored_session_ids_in(&dir))
        .unwrap_or_default()
}

/// What a capture did.
#[derive(Debug, Clone, PartialEq)]
pub enum Captured {
    /// The conversation and its index were written.
    Stored(ConversationMeta),
    /// The session has no record, so there is no native id to read (a hand-started Codex).
    NoRecord,
    /// Record persistence timed out or failed; a reason is stored without replacing evidence.
    Unavailable,
    /// The record has no native session id yet (Codex links it with its first telemetry).
    NoNativeId,
    /// The CLI has no session log reader; the state is recorded as `unsupported`.
    Unsupported,
    /// The CLI's log is not there (not written yet, or in a home CodeSense does not know).
    NoLog,
    /// Its retention ran out; the text stays removed.
    Expired,
}

/// How `cli`'s session log is found and parsed, when CodeSense can read it.
type LogReader = (
    Option<fn(&Path, &str) -> Option<PathBuf>>,
    Option<agent_cli::TranscriptFormat>,
);

fn reader_of(cli: AgentCli) -> LogReader {
    let output = agent_cli::adapter(cli).output();
    let format = output
        .transcript
        .and_then(|t| t.interactive.as_ref())
        .map(|i| i.format);
    (output.session_log, format)
}

/// Why a CLI's conversation is not read.
fn no_reader_reason(cli: AgentCli) -> String {
    format!("CodeSense has no session-log reader for {cli}")
}

/// `Some(reason)` for a CLI whose conversation CodeSense never reads, whether or not a capture has
/// yet been attempted.
pub(crate) fn unsupported_reason(cli: AgentCli) -> Option<String> {
    let (locate, format) = reader_of(cli);
    (locate.is_none() || format.is_none()).then(|| no_reader_reason(cli))
}

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Capture `session_id`'s conversation: log file → redacted lines → neutral turns → size limit →
/// `dir/<session>.json` plus the index rows. Pure over its paths and environment, so it is
/// tempdir-testable end to end.
pub fn capture_in(
    store_path: &Path,
    dir: &Path,
    env: Env<'_>,
    session_id: &str,
    now_ms: i64,
    policy: &Policy,
) -> Result<Captured, String> {
    // Readiness is independent of publication: a record writer never needs the capture lock.
    let ready = crate::session_usage::record::wait_for_record(store_path, session_id);
    with_session_lock(session_id, || match ready {
        Ok(()) => capture_unlocked(store_path, dir, env, session_id, now_ms, policy),
        Err(reason) => {
            record_unavailable(store_path, session_id, now_ms, &reason)?;
            Ok(Captured::Unavailable)
        }
    })
}

/// A failed final attempt explains the gap, but never destroys already captured evidence/index.
fn record_unavailable(
    store_path: &Path,
    session: &str,
    now_ms: i64,
    reason: &str,
) -> Result<(), String> {
    let mut store = UsageStore::open(store_path)?;
    if store.read_conversation_meta(session)?.is_some_and(|meta| {
        matches!(
            meta.state,
            State::Present | State::Truncated | State::Expired
        )
    }) {
        return Ok(());
    }
    store.write_conversation(
        &ConversationMeta {
            session_id: session.to_owned(),
            state: State::Unavailable,
            source_format: None,
            bytes: 0,
            turn_count: 0,
            truncated: None,
            captured_at_ms: now_ms,
            unavailable: [("conversation".to_owned(), reason.to_owned())].into(),
        },
        &[],
    )
}

static SESSION_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

/// Run `work` holding `session`'s capture lock. Sessions do not wait on each other.
fn with_session_lock<T>(session: &str, work: impl FnOnce() -> T) -> T {
    let lock = SESSION_LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(session.to_owned())
        .or_default()
        .clone();
    let result = {
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        work()
    };
    // Forget the lock once nobody else holds or waits on it.
    let mut locks = SESSION_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    if std::sync::Arc::strong_count(&lock) == 2 {
        locks.remove(session);
    }
    result
}

#[cfg(test)]
thread_local! {
    /// Runs on this thread between a capture's read of the log and its publication.
    static PAUSE_BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn Fn()>>> = std::cell::RefCell::new(None);
}

fn capture_unlocked(
    store_path: &Path,
    dir: &Path,
    env: Env<'_>,
    session_id: &str,
    now_ms: i64,
    policy: &Policy,
) -> Result<Captured, String> {
    let mut store = UsageStore::open(store_path)?;
    if store
        .read_conversation_meta(session_id)?
        .is_some_and(|m| m.state == State::Expired)
    {
        return Ok(Captured::Expired);
    }
    let Some(record) = store.read_session_record(session_id)? else {
        return Ok(Captured::NoRecord);
    };
    let Some(cli) = record
        .cli
        .as_deref()
        .and_then(|c| c.parse::<AgentCli>().ok())
    else {
        return Ok(Captured::NoRecord);
    };
    let (Some(locate), Some(format)) = reader_of(cli) else {
        let meta = ConversationMeta {
            session_id: record.session_id.clone(),
            state: State::Unsupported,
            source_format: None,
            bytes: 0,
            turn_count: 0,
            truncated: None,
            captured_at_ms: now_ms,
            unavailable: [("conversation".to_owned(), no_reader_reason(cli))].into(),
        };
        store.write_conversation(&meta, &[])?;
        return Ok(Captured::Unsupported);
    };
    let Some(native) = record.native_session_id.as_deref() else {
        record_unavailable(
            store_path,
            session_id,
            now_ms,
            "native_session_id_missing: the CLI has not linked its native session id",
        )?;
        return Ok(Captured::NoNativeId);
    };
    let home = record
        .config_home
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| agent_cli::descriptor(cli).native_home(env));
    let Some(path) = home.and_then(|h| locate(&h, native)) else {
        return Ok(Captured::NoLog);
    };
    record_log_usage(store_path, cli, &record.session_id, native, &path);
    let Some(mut transcript) = parse_log(&path, format)? else {
        return Ok(Captured::NoLog);
    };
    transcript
        .session_id
        .get_or_insert_with(|| native.to_owned());
    let calls = index_calls(
        &transcript,
        record.provided_skills.as_deref().unwrap_or(&[]),
    );
    #[cfg(test)]
    PAUSE_BEFORE_PUBLISH.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook();
        }
    });
    let (kept, truncation) = fit(transcript, policy.max_bytes);
    let stored = StoredConversation {
        session_id: record.session_id.clone(),
        source_format: serde_json::to_value(format)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        captured_at_ms: now_ms,
        truncated: truncation.clone(),
        transcript: kept,
    };
    let body = serde_json::to_vec(&stored).map_err(|e| e.to_string())?;
    let file = conversation_file(dir, &record.session_id)
        .ok_or_else(|| "session id is not a plain token".to_string())?;
    crate::shared::fs_atomic::write_bytes_atomic(&file, &body).map_err(|e| e.to_string())?;
    let meta = ConversationMeta {
        session_id: record.session_id.clone(),
        state: if truncation.is_some() {
            State::Truncated
        } else {
            State::Present
        },
        source_format: Some(stored.source_format.clone()),
        bytes: body.len() as u64,
        turn_count: u32::try_from(stored.transcript.turns.len()).unwrap_or(u32::MAX),
        truncated: truncation,
        captured_at_ms: now_ms,
        unavailable: stored.transcript.unavailable.clone(),
    };
    store.write_conversation(&meta, &calls)?;
    Ok(Captured::Stored(meta))
}

/// While managed settings send `cli`'s usage logs past CodeSense, store the usage in `native`'s own
/// session log at `path` as the launch `session` made it (a managed launch's session id is its
/// telemetry token). Best effort: a failure is logged and never stops the conversation capture.
fn record_log_usage(store_path: &Path, cli: AgentCli, session: &str, native: &str, path: &Path) {
    let Some(read) = crate::session_usage::managed_telemetry::session_log_reader(cli) else {
        return;
    };
    // Only a launch CodeSense registered: a CLI started by hand in a tab has a record, but its
    // usage is not counted, as it is not over OTLP.
    match UsageStore::open(store_path).and_then(|store| store.launch_owner(session)) {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(e) => {
            log::warn!(target: "session_conversation", "usage of {session} from its session log not stored: {e}");
            return;
        }
    }
    // Only this session's own calls: a resumed log can carry an earlier session's lines.
    let events: Vec<_> = read(path).into_iter().filter(|e| e.session_id == native).collect();
    match crate::session_usage::store_launch_events(store_path, session, &events) {
        Ok(()) => crate::session_usage::spend_caps::changed(),
        Err(e) => log::warn!(target: "session_conversation", "usage of {session} from its session log not stored: {e}"),
    }
}

/// Parse the log at `path` into neutral turns, the whole of it, one line at a time and each line
/// redacted **before** it is parsed. There is no input ceiling: the index must cover every call,
/// and the size limit applies to what is stored, not to what is read. `None` when `path` is not a
/// regular file or holds no turns. Turn indexes count from the log's start, so they do not move
/// as the log grows.
fn parse_log(
    path: &Path,
    format: agent_cli::TranscriptFormat,
) -> Result<Option<SessionTranscript>, String> {
    use std::io::BufRead;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Ok(None);
    }
    let lines = std::io::BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .filter(|line| !line.is_empty())
        .map(|line| redact_secrets(&String::from_utf8_lossy(&line)));
    Ok(parse_transcript_lines(format, lines))
}

/// The row name for a prompt that invoked a skill (no tool call is involved).
pub const SKILL_PROMPT: &str = "skill-prompt";

/// The provided skill a user's prompt invokes: its first word is `/<name>` (a Claude slash command,
/// also written `<command-name>/<name></command-name>` once expanded) or `$<name>` (a Codex skill),
/// and `<name>` is one of the skills the session's startup manifest provides. A prompt that merely
/// mentions one, or names a skill the session was not started with, invokes nothing.
pub fn skill_named_by_prompt(text: &str, provided: &[String]) -> Option<String> {
    let text = text.trim_start();
    let first = text.split_whitespace().next()?;
    let word = first
        .strip_prefix("<command-name>")
        .map(|rest| rest.split('<').next().unwrap_or(rest))
        .unwrap_or(first);
    let name = word.strip_prefix('/').or_else(|| word.strip_prefix('$'))?;
    provided.iter().find(|p| p.as_str() == name).cloned()
}

/// One index row per tool call of the whole transcript, including turns the size limit will drop,
/// plus one `skill-prompt` row for each user prompt that invokes a provided skill.
fn index_calls(transcript: &SessionTranscript, provided: &[String]) -> Vec<IndexedCall> {
    transcript
        .turns
        .iter()
        .flat_map(|turn| {
            let turn_index = turn.index.unwrap_or(0);
            let prompt_skill = (turn.role == "user")
                .then_some(turn.text.as_deref())
                .flatten()
                .and_then(|text| skill_named_by_prompt(text, provided))
                .map(|skill| IndexedCall {
                    turn_index,
                    seq: 0,
                    native_id: None,
                    name: SKILL_PROMPT.to_owned(),
                    mcp_server: None,
                    is_error: None,
                    ts_ms: turn.ts_ms,
                    skill: Some(skill),
                    aliases: Vec::new(),
                });
            let calls = turn
                .tool_calls
                .iter()
                .enumerate()
                .map(move |(seq, call)| IndexedCall {
                    turn_index,
                    seq: u32::try_from(seq).unwrap_or(u32::MAX),
                    native_id: call.native_id.clone(),
                    name: call.name.clone(),
                    mcp_server: call.mcp_server.clone(),
                    is_error: call.is_error,
                    ts_ms: call.ts_ms,
                    skill: call.skill.clone(),
                    aliases: call.aliases.clone(),
                });
            prompt_skill.into_iter().chain(calls)
        })
        .collect()
}

/// Cut `text` to `limit` bytes (on a character boundary) with a note of what was cut; returns the
/// number of bytes removed (0 when it already fits).
fn cut(text: &mut String, limit: usize) -> usize {
    if text.len() <= limit {
        return 0;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    text.truncate(end);
    text.push_str(&format!("…[{dropped} bytes cut]"));
    dropped
}

/// Fit `transcript` in `max_bytes`. A conversation that fits is kept exactly as it is. One that
/// does not keeps its first request and its newest turns and drops the oldest turns between them;
/// only if the turns kept still do not fit (one turn bigger than the limit) are their longest
/// fields cut short. Whatever was left out is counted.
fn fit(
    mut transcript: SessionTranscript,
    max_bytes: usize,
) -> (SessionTranscript, Option<Truncation>) {
    let size = |turn: &Turn| serde_json::to_vec(turn).map_or(0, |b| b.len() + 1);
    let budget = max_bytes.saturating_sub(WRAPPER_BYTES);
    let total: usize = transcript.turns.iter().map(size).sum();
    if total <= budget {
        return (transcript, None);
    }
    let first_user = transcript
        .turns
        .iter()
        .position(|t| t.role == "user")
        .unwrap_or(0);
    let head_end = first_user + 1;
    let mut used: usize = transcript.turns[..head_end].iter().map(size).sum();
    let mut tail_start = transcript.turns.len();
    while tail_start > head_end {
        let next = size(&transcript.turns[tail_start - 1]);
        if used + next > budget {
            break;
        }
        used += next;
        tail_start -= 1;
    }
    // The newest turn is always kept, trimmed below if it alone is too big.
    if tail_start == transcript.turns.len() && tail_start > head_end {
        tail_start -= 1;
        used += size(&transcript.turns[tail_start]);
    }
    let dropped: Vec<Turn> = transcript.turns.drain(head_end..tail_start).collect();
    let mut dropped_bytes: u64 = dropped.iter().map(|t| size(t) as u64).sum();
    let mut trimmed_fields = 0u32;
    let mut limit = MAX_FIELD_BYTES;
    while used > budget && limit >= MIN_FIELD_BYTES {
        for turn in &mut transcript.turns {
            let fields = turn.text.iter_mut().chain(
                turn.tool_calls
                    .iter_mut()
                    .flat_map(|c| c.input.iter_mut().chain(c.result.iter_mut())),
            );
            for field in fields {
                let removed = cut(field, limit);
                if removed > 0 {
                    trimmed_fields += 1;
                    dropped_bytes += removed as u64;
                }
            }
        }
        used = transcript.turns.iter().map(size).sum();
        limit /= 4;
    }
    let truncation = Truncation {
        dropped_turns: dropped
            .first()
            .zip(dropped.last())
            .map(|(a, b)| (a.index.unwrap_or(0), b.index.unwrap_or(0))),
        dropped_turn_count: u32::try_from(dropped.len()).unwrap_or(u32::MAX),
        dropped_bytes,
        trimmed_fields,
    };
    (transcript, Some(truncation))
}

// ── Triggers ─────────────────────────────────────────────────────────────────────────────────

/// Why a session is being captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A turn ended while the session runs.
    Checkpoint,
    /// The session ended.
    End,
}

/// What the debouncer says about an offered trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Run,
    Deferred,
}

/// Spaces checkpoints apart per session; a session's end always runs and clears what waits.
#[derive(Debug, Default)]
pub struct Debouncer {
    last_run: HashMap<String, i64>,
    due: BTreeMap<String, i64>,
}

impl Debouncer {
    pub fn offer(&mut self, session: &str, trigger: Trigger, now_ms: i64) -> Offer {
        match trigger {
            Trigger::End => {
                self.due.remove(session);
                self.last_run.remove(session);
                Offer::Run
            }
            Trigger::Checkpoint => match self.last_run.get(session).copied() {
                Some(last) if now_ms - last < DEBOUNCE_MS => {
                    self.due
                        .entry(session.to_owned())
                        .or_insert(last + DEBOUNCE_MS);
                    Offer::Deferred
                }
                _ => {
                    self.last_run.insert(session.to_owned(), now_ms);
                    self.due.remove(session);
                    Offer::Run
                }
            },
        }
    }

    /// The sessions whose delayed checkpoint is due at `now_ms`, now run.
    pub fn take_due(&mut self, now_ms: i64) -> Vec<String> {
        let ready: Vec<String> = self
            .due
            .iter()
            .filter(|(_, due)| **due <= now_ms)
            .map(|(s, _)| s.clone())
            .collect();
        for session in &ready {
            self.due.remove(session);
            self.last_run.insert(session.clone(), now_ms);
        }
        ready
    }

    /// When the next delayed checkpoint falls due.
    pub fn next_due(&self) -> Option<i64> {
        self.due.values().min().copied()
    }
}

struct Job {
    session: String,
    trigger: Trigger,
}

static SINK: OnceLock<Sender<Job>> = OnceLock::new();

fn run_job(session: &str, now_ms: i64) -> Option<Captured> {
    #[cfg(test)]
    if let Some(writer) = WRITER.with(|w| w.borrow().clone()) {
        return Some(writer.run(session));
    }
    let (Ok(store), Some(dir)) = (crate::session_usage::store_path(), conversations_dir()) else {
        return None;
    };
    let env = |key: &str| std::env::var_os(key);
    match capture_in(&store, &dir, &env, session, now_ms, &Policy::default()) {
        Ok(captured) => Some(captured),
        Err(e) => {
            log::warn!(target: "session_conversation", "conversation of {session} not stored: {e}");
            None
        }
    }
}

/// Blocking final capture. Callers on an async runtime must use `capture_now`.
pub fn capture_blocking(session_id: &str) -> Option<Captured> {
    run_job(session_id, chrono::Utc::now().timestamp_millis())
}

/// Capture on the blocking pool, including the bounded wait for record persistence.
pub async fn capture_now(session_id: &str) {
    // Only path selection differs in tests. The dispatch to the blocking pool, including its
    // bounded record wait, is shared so runtime-responsiveness tests exercise the shipped seam.
    let paths: Option<(PathBuf, PathBuf, PathBuf, i64)> = {
        #[cfg(test)]
        {
            let writer = WRITER.with(|w| w.borrow().clone());
            let Some(writer) = writer else {
                note_end(session_id);
                return;
            };
            writer
                .debounce
                .borrow_mut()
                .offer(session_id, Trigger::End, writer.now.get());
            writer.runs.set(writer.runs.get() + 1);
            Some((
                writer.store.clone(),
                writer.dir.clone(),
                writer.home.clone(),
                writer.now.get(),
            ))
        }
        #[cfg(not(test))]
        {
            None
        }
    };
    let session = session_id.to_owned();
    let result = crate::commands::await_blocking("session_conversation.capture", move || {
        if let Some((store, dir, home, now)) = paths {
            let env = |key: &str| (key == "HOME").then(|| home.clone().into_os_string());
            capture_in(&store, &dir, &env, &session, now, &Policy::default())?;
        } else {
            capture_blocking(&session);
        }
        Ok::<(), String>(())
    })
    .await;
    #[cfg(test)]
    result.expect("capture succeeds");
    #[cfg(not(test))]
    if let Err(error) = result {
        log::warn!(target: "session_conversation", "conversation capture task failed: {error}");
    }
}

/// Return whether deletion can proceed. A missing/late record keeps the native source for a
/// later end delivery or the existing startup sweep; no new retry or polling is introduced.
pub fn capture_before_removal(session_id: &str, dir: &Path) -> bool {
    if !dir.join("sessions").is_dir() {
        return true;
    }
    matches!(
        capture_blocking(session_id),
        Some(Captured::Stored(_) | Captured::Expired | Captured::Unsupported)
    )
}

fn start_writer() -> Sender<Job> {
    let (tx, rx) = channel::<Job>();
    let spawned = std::thread::Builder::new()
        .name("session-conversation".into())
        .spawn(move || {
            let now = || chrono::Utc::now().timestamp_millis();
            let mut debounce = Debouncer::default();
            loop {
                let wait = debounce
                    .next_due()
                    .map_or(Duration::from_secs(3600), |due| {
                        Duration::from_millis((due - now()).max(0) as u64)
                    });
                match rx.recv_timeout(wait) {
                    Ok(job) => {
                        if debounce.offer(&job.session, job.trigger, now()) == Offer::Run {
                            run_job(&job.session, now());
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        for session in debounce.take_due(now()) {
                            run_job(&session, now());
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!(target: "session_conversation", "conversation writer not started: {e}");
    }
    tx
}

#[cfg(test)]
pub(crate) const CLAUDE_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/codesense-pty-core/tests/fixtures/claude/session-2.1.296.jsonl"
));
#[cfg(test)]
pub(crate) const CODEX_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/codesense-pty-core/tests/fixtures/codex/rollout-0.164.0.jsonl"
));
#[cfg(test)]
pub(crate) const CLAUDE_ID: &str = "c1a0de00-0000-4000-a000-000000000201";
/// The n-th distinct, valid session id, for a test with several sessions.
#[cfg(test)]
pub(crate) fn nth_uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
#[cfg(test)]
pub(crate) const CODEX_ID: &str = "01a17e00-0000-7000-b000-000000000003";

#[cfg(test)]
thread_local! {
    static NOTED: std::cell::RefCell<Vec<(String, Trigger)>> = const { std::cell::RefCell::new(Vec::new()) };
    static WRITER: std::cell::RefCell<Option<std::rc::Rc<TestWriter>>> = const { std::cell::RefCell::new(None) };
}

/// The writer of a test build: the same debouncer and the same capture as production, run on the
/// calling thread against a temporary store and conversation directory with a clock the test sets,
/// so a test drives a real entry point (a hook, a snapshot, a PTY event) and reads what was stored.
#[cfg(test)]
pub(crate) struct TestWriter {
    pub store: PathBuf,
    pub dir: PathBuf,
    /// The `HOME` the capture resolves a CLI's default home against.
    pub home: PathBuf,
    pub now: std::cell::Cell<i64>,
    pub runs: std::cell::Cell<u32>,
    debounce: std::cell::RefCell<Debouncer>,
}

#[cfg(test)]
pub(crate) struct TestWriterGuard(std::rc::Rc<TestWriter>);

#[cfg(test)]
impl std::ops::Deref for TestWriterGuard {
    type Target = TestWriter;
    fn deref(&self) -> &TestWriter {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestWriterGuard {
    fn drop(&mut self) {
        WRITER.with(|w| *w.borrow_mut() = None);
    }
}

#[cfg(test)]
impl TestWriter {
    /// Install a writer on this thread until the guard drops.
    pub(crate) fn install(store: PathBuf, dir: PathBuf, home: PathBuf) -> TestWriterGuard {
        let writer = std::rc::Rc::new(TestWriter {
            store,
            dir,
            home,
            now: std::cell::Cell::new(1_000),
            runs: std::cell::Cell::new(0),
            debounce: std::cell::RefCell::new(Debouncer::default()),
        });
        WRITER.with(|w| *w.borrow_mut() = Some(writer.clone()));
        TestWriterGuard(writer)
    }

    fn run(&self, session: &str) -> Captured {
        self.runs.set(self.runs.get() + 1);
        let home = self.home.clone().into_os_string();
        let env = move |key: &str| (key == "HOME").then(|| home.clone());
        capture_in(
            &self.store,
            &self.dir,
            &env,
            session,
            self.now.get(),
            &Policy::default(),
        )
        .unwrap_or_else(|e| panic!("capture of {session} failed: {e}"))
    }

    fn offer(&self, session: &str, trigger: Trigger) {
        let offer = self
            .debounce
            .borrow_mut()
            .offer(session, trigger, self.now.get());
        if offer == Offer::Run {
            self.run(session);
        }
    }

    /// A Claude session `session` that has a record and a log in the default home.
    pub(crate) fn seed_claude(&self, session: &str, native: &str) {
        let dir = self.home.join(".claude/projects/-work-wt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{native}.jsonl")), CLAUDE_FIXTURE).unwrap();
        self.seed_record(session, "claude", native);
    }

    /// A Codex session `session` that has a record and a rollout in the default home.
    pub(crate) fn seed_codex(&self, session: &str, native: &str) {
        let dir = self.home.join(".codex/sessions/2026/10/27");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("rollout-2026-10-27T10-00-00-{native}.jsonl")),
            CODEX_FIXTURE,
        )
        .unwrap();
        self.seed_record(session, "codex", native);
    }

    fn seed_record(&self, session: &str, cli: &str, native: &str) {
        let mut record = crate::session_usage::record::SessionRecord::new(session);
        record.cli = Some(cli.into());
        record.native_session_id = Some(native.into());
        UsageStore::open(&self.store)
            .unwrap()
            .write_session_record(&record)
            .unwrap();
    }

    /// What was stored for `session`: its conversation and its index rows.
    pub(crate) fn stored(&self, session: &str) -> Option<(StoredConversation, Vec<IndexedCall>)> {
        let stored = read_stored(&self.dir, session)?;
        let calls = UsageStore::open(&self.store)
            .unwrap()
            .read_tool_calls(session)
            .unwrap();
        Some((stored, calls))
    }

    /// Move the clock on and run the checkpoints that fall due.
    pub(crate) fn advance(&self, ms: i64) {
        self.now.set(self.now.get() + ms);
        let due = self.debounce.borrow_mut().take_due(self.now.get());
        for session in due {
            self.run(&session);
        }
    }
}

/// Ask for a session to be captured. Fire and forget: a hook or PTY event never waits on it. A test
/// build runs it on a [`TestWriter`] when one is installed, else records the request for
/// [`take_noted`].
pub fn note(session_id: &str, trigger: Trigger) {
    #[cfg(test)]
    {
        if let Some(writer) = WRITER.with(|w| w.borrow().clone()) {
            writer.offer(session_id, trigger);
        } else {
            NOTED.with(|n| n.borrow_mut().push((session_id.to_owned(), trigger)));
        }
        return;
    }
    #[cfg(not(test))]
    {
        let _ = SINK.get_or_init(start_writer).send(Job {
            session: session_id.to_owned(),
            trigger,
        });
    }
}

/// A turn of `session_id` ended.
pub fn note_checkpoint(session_id: &str) {
    note(session_id, Trigger::Checkpoint);
}

/// An activity event of `session_id`, as a Claude hook or a Codex snapshot change reports it: a turn
/// stopping, failing or being interrupted is a checkpoint; every other event is not.
pub fn note_hook(session_id: &str, event: codesense_pty_core::activity::Event) {
    use codesense_pty_core::activity::Event;
    if matches!(event, Event::Stopped | Event::Failed | Event::Interrupted) {
        note_checkpoint(session_id);
    }
}

/// `session_id` ended (its process exited, its tab was removed, or its headless run finished).
pub fn note_end(session_id: &str) {
    note(session_id, Trigger::End);
}

#[cfg(test)]
pub(crate) fn take_noted() -> Vec<(String, Trigger)> {
    NOTED.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session_usage::record::SessionRecord;

    struct World {
        _dir: tempfile::TempDir,
        store: PathBuf,
        out: PathBuf,
        home: PathBuf,
    }

    impl World {
        fn new() -> World {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            World {
                store: root.join("usage.sqlite"),
                out: root.join("session-conversations"),
                home: root.join("home"),
                _dir: dir,
            }
        }

        fn env(&self) -> impl Fn(&str) -> Option<OsString> + '_ {
            |key| (key == "HOME").then(|| self.home.clone().into_os_string())
        }

        fn claude_log(&self, home: &Path, body: &str) {
            let dir = home.join("projects/-work-wt");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{CLAUDE_ID}.jsonl")), body).unwrap();
        }

        fn codex_log(&self, home: &Path, body: &str) {
            let dir = home.join("sessions/2026/10/27");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("rollout-2026-10-27T10-00-00-{CODEX_ID}.jsonl")),
                body,
            )
            .unwrap();
        }

        fn record(&self, session: &str, cli: &str, native: Option<&str>, home: Option<&Path>) {
            let mut r = SessionRecord::new(session);
            r.cli = Some(cli.into());
            r.native_session_id = native.map(str::to_owned);
            r.config_home = home.map(|h| h.to_string_lossy().into_owned());
            UsageStore::open(&self.store)
                .unwrap()
                .write_session_record(&r)
                .unwrap();
        }

        fn provide(&self, session: &str, skills: &[&str]) {
            let store = UsageStore::open(&self.store).unwrap();
            let mut r = store.read_session_record(session).unwrap().unwrap();
            r.provided_skills = Some(skills.iter().map(|s| s.to_string()).collect());
            store.write_session_record(&r).unwrap();
        }

        fn capture(&self, session: &str) -> Captured {
            self.capture_with(session, &Policy::default())
        }

        fn capture_with(&self, session: &str, policy: &Policy) -> Captured {
            capture_in(&self.store, &self.out, &self.env(), session, 1_000, policy).unwrap()
        }

        fn stored(&self, session: &str) -> StoredConversation {
            let file = conversation_file(&self.out, session).unwrap();
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
        }

        fn calls(&self, session: &str) -> Vec<IndexedCall> {
            UsageStore::open(&self.store)
                .unwrap()
                .read_tool_calls(session)
                .unwrap()
        }
    }

    /// Under a managed-settings redirect, the usage in a launched Claude session's own log (and its
    /// subagents' logs) is stored as that launch's, keyed like its telemetry: priced, attributed to
    /// its agent for spend caps, stored once however often it is read, and one row with the OTLP
    /// event for the same call. Without a redirect nothing is read for usage.
    #[test]
    fn under_a_redirect_a_claude_sessions_log_usage_is_stored_as_its_launch() {
        use crate::session_usage::event::{Tokens, UsageEvent};
        use crate::session_usage::session_log_usage::tests::assistant;
        use crate::session_usage::{Filter, SpendScope, SpendWindow};
        let w = World::new();
        let claude = w.home.join(".claude");
        let now = chrono::Utc::now();
        let line = |request: &str, session: &str, usage: &str| {
            assistant(request, session, "claude-sonnet-5-5", usage)
                .replace("2026-11-06T10:00:00.000Z", &now.to_rfc3339())
        };
        w.claude_log(
            &claude,
            &[
                CLAUDE_FIXTURE.trim_end().to_owned(),
                line("req_1", CLAUDE_ID, r#"{"input_tokens":1000000,"output_tokens":6}"#),
                line("req_1", CLAUDE_ID, r#"{"input_tokens":1000000,"output_tokens":100000}"#),
                // A resumed log's line from an earlier session is not this launch's.
                line("req_old", "0ld00000-0000-4000-a000-000000000000", r#"{"input_tokens":9000000}"#),
            ]
            .join("\n"),
        );
        let subagents = claude.join(format!("projects/-work-wt/{CLAUDE_ID}/subagents"));
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent-a.jsonl"), line("req_2", CLAUDE_ID, r#"{"input_tokens":500000}"#)).unwrap();
        w.record("sess-a", "claude", Some(CLAUDE_ID), Some(&claude));
        let read = || {
            let store = UsageStore::open(&w.store).unwrap();
            let report = store.report(&Filter::default()).unwrap();
            let day = chrono::Local::now().date_naive();
            let spend = store.spend_for(SpendScope::Agent("a"), SpendWindow::Day(day)).unwrap();
            (report.session_count, report.totals.tokens, (spend.cost_usd * 100.0).round() / 100.0)
        };

        assert!(matches!(w.capture("sess-a"), Captured::Stored(_)));
        assert_eq!(read().0, 0, "no redirect: nothing is read for usage");

        let _managed = crate::session_usage::managed_telemetry::Fixture::redirecting();
        // Until its launch is registered it is like a claude started by hand in a tab: it has a
        // record, but CodeSense did not launch it, so its usage is not counted.
        assert!(matches!(w.capture("sess-a"), Captured::Stored(_)));
        assert_eq!(read().0, 0, "a session CodeSense did not launch is not counted");
        UsageStore::open(&w.store).unwrap().register_launch("sess-a", "acme", Some("dev"), "a", "a@one", None).unwrap();
        assert!(matches!(w.capture("sess-a"), Captured::Stored(_)));
        // claude-sonnet-5-5 at $2 in and $10 out per million: req_1 $2 + $1, req_2 $1.
        let tokens = Tokens { input: 1_500_000, output: 100_000, ..Tokens::default() };
        assert_eq!(read(), (1, tokens, 4.0));
        assert!(matches!(w.capture("sess-a"), Captured::Stored(_)));
        assert_eq!(read(), (1, tokens, 4.0), "reading the log again stores nothing new");

        // The same call over OTLP is the same row, and the cost the CLI reported wins.
        let otlp = UsageEvent {
            cli: AgentCli::Claude,
            event_id: "req_1".into(),
            session_id: CLAUDE_ID.into(),
            at_ms: now.timestamp_millis(),
            cwd: None,
            model: "claude-sonnet-5-5".into(),
            tokens: Tokens { input: 1_000_000, output: 100_000, ..Tokens::default() },
            reported_cost_usd: Some(5.0),
        };
        crate::session_usage::store_launch_events(&w.store, "sess-a", &[otlp]).unwrap();
        assert_eq!(read(), (1, tokens, 6.0));
        assert!(matches!(w.capture("sess-a"), Captured::Stored(_)));
        assert_eq!(read(), (1, tokens, 6.0), "a later log read keeps the reported cost");

        // The agent's $5 daily cap counts that spend, so it is reached and refuses the next launch.
        let cap = crate::session_usage::spend_caps::Budget { per_day: Some(5.0), per_week: None };
        let unrecorded = crate::session_usage::managed_telemetry::unrecorded(AgentCli::Claude);
        let day = chrono::Local::now().date_naive();
        let status = UsageStore::open(&w.store).unwrap().cap_status("agent", "a", cap, day, unrecorded).unwrap();
        assert_eq!(status.spend.state, "reached");
        assert!(status.refusal.is_some(), "its next launch is refused");
    }

    #[test]
    fn pbf210_missing_record_records_unavailable_surviving_reopen() {
        let w = World::new();
        w.capture("pbf210-timeout");
        let meta = UsageStore::open(&w.store)
            .unwrap()
            .read_conversation_meta("pbf210-timeout")
            .unwrap()
            .expect("a final capture must explain its missing record");
        assert_eq!(meta.state.as_str(), "unavailable");
        assert!(meta.unavailable["conversation"].contains("session_record_timeout"));
    }

    #[test]
    fn pbf210_record_write_failure_wakes_capture_and_survives_reopen() {
        let w = World::new();
        UsageStore::open(&w.store).unwrap();
        rusqlite::Connection::open(&w.store).unwrap().execute_batch(
            "CREATE TRIGGER fail_record BEFORE INSERT ON session_record BEGIN SELECT RAISE(FAIL, 'injected write failure'); END;"
        ).unwrap();
        let path = w.store.clone();
        let session = "pbf210-write-failure";
        let writer = std::thread::spawn(move || {
            crate::session_usage::record::wait_for_record_waiter(&path, session);
            let record = SessionRecord::new(session);
            assert!(crate::session_usage::record::persist_record_in(&path, &record).is_err());
        });
        assert_eq!(w.capture(session), Captured::Unavailable);
        writer.join().unwrap();
        let page = read_conversation_in(
            &w.store,
            &w.out,
            session,
            &Selector::Page {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(page.state, Availability::Unavailable);
        assert!(page.reason.unwrap().contains("session_record_write_failed"));
    }

    #[test]
    fn pbf210_failed_late_capture_keeps_present_truncated_and_expired_evidence() {
        for state in [State::Present, State::Truncated, State::Expired] {
            let w = World::new();
            let session = format!("pbf210-preserve-{}", state.as_str());
            w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
            w.record(&session, "claude", Some(CLAUDE_ID), None);
            w.capture(&session);
            let mut store = UsageStore::open(&w.store).unwrap();
            let mut meta = store.read_conversation_meta(&session).unwrap().unwrap();
            let calls = store.read_tool_calls(&session).unwrap();
            meta.state = state;
            store.write_conversation(&meta, &calls).unwrap();
            let before = std::fs::read(conversation_file(&w.out, &session).unwrap()).unwrap();
            rusqlite::Connection::open(&w.store)
                .unwrap()
                .execute(
                    "DELETE FROM session_record WHERE session_id = ?1",
                    [&session],
                )
                .unwrap();
            let path = w.store.clone();
            let writer_session = session.clone();
            let writer = std::thread::spawn(move || {
                crate::session_usage::record::wait_for_record_waiter(&path, &writer_session);
                let pending =
                    crate::session_usage::record::begin_record_write(&path, &writer_session);
                pending.finish(false);
            });
            w.capture(&session);
            writer.join().unwrap();
            let reopened = UsageStore::open(&w.store).unwrap();
            assert_eq!(
                reopened.read_conversation_meta(&session).unwrap().unwrap(),
                meta
            );
            assert_eq!(reopened.read_tool_calls(&session).unwrap(), calls);
            assert_eq!(
                std::fs::read(conversation_file(&w.out, &session).unwrap()).unwrap(),
                before
            );
        }
    }

    #[tokio::test]
    async fn pbf210_hand_started_record_and_end_in_either_order_keep_hook_checkpoints() {
        let _home = crate::routines::test_support::TempHome::new();
        let path = crate::session_usage::store_path().unwrap();
        let dir = conversations_dir().unwrap();
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let writer = TestWriter::install(path.clone(), dir, home.clone());
        let logs = home.join(".claude/projects/-work");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join(format!("{CLAUDE_ID}.jsonl")), CLAUDE_FIXTURE).unwrap();
        for (session, early) in [("pbf210-hand-early", true), ("pbf210-hand-late", false)] {
            let record =
                crate::session_usage::record::hand_started(session, Some(CLAUDE_ID), None, None);
            // Distinct native ids keep the normal one-record-per-native invariant.
            let native = if early { nth_uuid(90) } else { nth_uuid(91) };
            std::fs::write(logs.join(format!("{native}.jsonl")), CLAUDE_FIXTURE).unwrap();
            let mut record = record;
            record.native_session_id = Some(native);
            if early {
                crate::session_usage::record::record_hand_started_async(record).await;
                capture_now(session).await;
            } else {
                let path = path.clone();
                let (release, ready) = tokio::sync::oneshot::channel();
                let waiter = std::thread::spawn(move || {
                    crate::session_usage::record::wait_for_record_waiter(&path, session);
                    release.send(()).unwrap();
                });
                // Future construction registers the real hook writer before it is polled.
                let write = crate::session_usage::record::record_hand_started_async(record);
                tokio::join!(capture_now(session), async {
                    ready.await.unwrap();
                    write.await;
                });
                waiter.join().unwrap();
            }
            assert_eq!(writer.stored(session).unwrap().1.len(), 3);
            // A later hook checkpoint follows the same capture path without replacing evidence.
            note_hook(session, codesense_pty_core::activity::Event::Stopped);
            assert_eq!(writer.stored(session).unwrap().1.len(), 3);
        }
    }

    #[test]
    fn a_claude_session_is_stored_whole_with_one_index_row_per_call() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-1", "claude", Some(CLAUDE_ID), None);
        let Captured::Stored(meta) = w.capture("tab-1") else {
            panic!("stored");
        };
        assert_eq!(meta.state, State::Present);
        assert_eq!(meta.turn_count, 5);
        let stored = w.stored("tab-1");
        assert_eq!(stored.transcript.turns[0].role, "user");
        let calls = w.calls("tab-1");
        assert_eq!(
            calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Read", "Skill", "mcp__codesense__read_messages"]
        );
        assert_eq!(calls[0].native_id.as_deref(), Some("toolu_01AAA"));
        assert_eq!(calls[2].mcp_server.as_deref(), Some("codesense"));
        assert_eq!(calls[2].is_error, Some(true));
        let again = UsageStore::open(&w.store).unwrap();
        assert_eq!(
            again
                .read_conversation_meta("tab-1")
                .unwrap()
                .unwrap()
                .turn_count,
            5
        );
    }

    #[test]
    fn a_codex_session_is_stored_with_its_calls_and_no_invented_error_flag() {
        let w = World::new();
        w.codex_log(&w.home.join(".codex"), CODEX_FIXTURE);
        w.record("cx-1", "codex", Some(CODEX_ID), None);
        assert!(matches!(w.capture("cx-1"), Captured::Stored(_)));
        let calls = w.calls("cx-1");
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c.is_error.is_none()));
        assert_eq!(calls[0].native_id.as_deref(), Some("exec-0001"));
        let stored = w.stored("cx-1");
        assert!(stored
            .transcript
            .unavailable
            .contains_key("toolCall.isError"));
    }

    #[test]
    fn an_account_home_is_read_instead_of_the_default_home() {
        let w = World::new();
        let account = w.home.join("accounts/work");
        w.claude_log(&account, CLAUDE_FIXTURE);
        // The default home holds a different log; the record's home wins.
        w.claude_log(
            &w.home.join(".claude"),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"wrong home\"}}\n",
        );
        w.record("tab-acct", "claude", Some(CLAUDE_ID), Some(&account));
        assert!(matches!(w.capture("tab-acct"), Captured::Stored(_)));
        assert_eq!(
            w.stored("tab-acct").transcript.turns[0].text.as_deref(),
            Some("Please read auth.py and fix the bug")
        );

        let cx_account = w.home.join("accounts/codex-work");
        w.codex_log(&cx_account, CODEX_FIXTURE);
        w.record("cx-acct", "codex", Some(CODEX_ID), Some(&cx_account));
        assert!(matches!(w.capture("cx-acct"), Captured::Stored(_)));
    }

    #[test]
    fn a_hand_started_claude_tab_is_captured_from_the_default_home() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        let record =
            crate::session_usage::record::hand_started("tab-hand", Some(CLAUDE_ID), None, None);
        UsageStore::open(&w.store)
            .unwrap()
            .write_session_record(&record)
            .unwrap();
        assert!(matches!(w.capture("tab-hand"), Captured::Stored(_)));
        assert_eq!(w.stored("tab-hand").transcript.turns.len(), 5);
    }

    #[test]
    fn a_session_with_no_record_or_no_native_id_or_no_log_stores_nothing() {
        let w = World::new();
        assert_eq!(w.capture("ghost"), Captured::Unavailable);
        w.record("cx-wait", "codex", None, None);
        assert_eq!(w.capture("cx-wait"), Captured::NoNativeId);
        w.record("tab-nolog", "claude", Some(CLAUDE_ID), None);
        assert_eq!(w.capture("tab-nolog"), Captured::NoLog);
        for s in ["ghost", "cx-wait", "tab-nolog"] {
            assert!(!conversation_file(&w.out, s).unwrap().exists());
            assert!(w.calls(s).is_empty());
        }
    }

    #[test]
    fn interactive_opencode_reads_unsupported_and_writes_no_file() {
        let w = World::new();
        w.record("oc-1", "opencode", Some("ses_x"), None);
        assert_eq!(w.capture("oc-1"), Captured::Unsupported);
        let meta = UsageStore::open(&w.store)
            .unwrap()
            .read_conversation_meta("oc-1")
            .unwrap()
            .unwrap();
        assert_eq!(meta.state, State::Unsupported);
        assert!(meta.unavailable["conversation"].contains("opencode"));
        assert!(!conversation_file(&w.out, "oc-1").unwrap().exists());
    }

    #[test]
    fn secrets_are_gone_from_the_file_the_index_and_the_store_before_anything_is_written() {
        let w = World::new();
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let body = "RkFLRUtFWV9OT1RfQV9SRUFMX0tFWV9SRUFEX01FX0hFUkU9PT0K";
        let pem_raw = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----");
        let line = |role: &str, content: serde_json::Value| {
            serde_json::json!({"type": role, "message": {"role": role, "content": content}})
                .to_string()
        };
        let log = [
            line("user", serde_json::json!(format!("my token is {token}"))),
            line(
                "assistant",
                serde_json::json!([
                    {"type": "text", "text": format!("key follows\n{pem_raw}\nthe end, ordinary base64 {body}")},
                    {"type": "tool_use", "id": "toolu_x", "name": "Bash", "input": {"cmd": format!("echo {token}")}}
                ]),
            ),
        ]
        .join("\n");
        w.claude_log(&w.home.join(".claude"), &log);
        w.record("tab-sec", "claude", Some(CLAUDE_ID), None);
        assert!(matches!(w.capture("tab-sec"), Captured::Stored(_)));
        let file = std::fs::read_to_string(conversation_file(&w.out, "tab-sec").unwrap()).unwrap();
        assert!(!file.contains(&token), "token must be gone: {file}");
        assert!(!file.contains("END PRIVATE KEY"), "the key block is gone");
        let dump: String = rusqlite::Connection::open(&w.store)
            .unwrap()
            .query_row(
                "SELECT json_object('c', (SELECT json_group_array(json_array(name, native_id)) FROM session_tool_call),
                                    'm', (SELECT group_concat(unavailable) FROM session_conversation))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!dump.contains(&token) && !dump.contains(body));
        // Only the key block is lost to the scrub: the same base64 outside it survives in prose.
        assert!(file.contains("the end, ordinary base64"));
        assert!(
            file.contains(body),
            "base64 outside a key block is not a secret"
        );
    }

    pub(crate) fn long_claude_log(turns: usize) -> String {
        let mut out = Vec::new();
        for i in 0..turns {
            out.push(
                serde_json::json!({"type": "user", "origin": {"kind": "human"}, "promptId": format!("p{i}"),
                    "timestamp": "2026-10-27T10:00:00.000Z",
                    "message": {"role": "user", "content": format!("request number {i} {}", "x".repeat(400))}})
                .to_string(),
            );
            out.push(
                serde_json::json!({"type": "assistant", "timestamp": "2026-10-27T10:00:01.000Z",
                    "message": {"role": "assistant", "model": "m", "content": [
                        {"type": "text", "text": format!("answer {i} {}", "y".repeat(400))},
                        {"type": "tool_use", "id": format!("toolu_{i}"), "name": "Bash", "input": {"n": i}}
                    ]}})
                .to_string(),
            );
        }
        out.join("\n")
    }

    #[test]
    fn a_long_session_keeps_every_turn_past_one_mebibyte() {
        let w = World::new();
        let log = long_claude_log(1500);
        assert!(
            log.len() > 1024 * 1024,
            "the fixture outgrows the old 1 MiB tail"
        );
        w.claude_log(&w.home.join(".claude"), &log);
        w.record("tab-long", "claude", Some(CLAUDE_ID), None);
        let Captured::Stored(meta) = w.capture("tab-long") else {
            panic!("stored");
        };
        assert_eq!(meta.state, State::Present, "{meta:?}");
        assert_eq!(meta.turn_count, 3000);
        assert_eq!(w.stored("tab-long").transcript.turns[0].index, Some(0));
        assert_eq!(w.calls("tab-long").len(), 1500);
    }

    #[test]
    fn over_the_limit_keeps_the_first_request_and_the_newest_turns_and_says_what_was_dropped() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), &long_claude_log(200));
        w.record("tab-cut", "claude", Some(CLAUDE_ID), None);
        let policy = Policy {
            max_bytes: 60 * 1024,
            ..Policy::default()
        };
        let Captured::Stored(meta) = w.capture_with("tab-cut", &policy) else {
            panic!("stored");
        };
        assert_eq!(meta.state, State::Truncated);
        let t = meta.truncated.clone().unwrap();
        let stored = w.stored("tab-cut");
        let turns = &stored.transcript.turns;
        assert_eq!(turns[0].index, Some(0), "the first user turn stays");
        assert_eq!(
            turns.last().unwrap().index,
            Some(399),
            "the newest turn stays"
        );
        let (from, to) = t.dropped_turns.unwrap();
        assert_eq!(from, 1);
        assert_eq!(
            to + 1,
            turns[1].index.unwrap(),
            "the dropped range is contiguous"
        );
        assert_eq!(t.dropped_turn_count as usize + turns.len(), 400);
        assert!(t.dropped_bytes > 0);
        assert!(meta.bytes <= 60 * 1024, "{}", meta.bytes);
        // The index still covers every call, dropped turns included.
        assert_eq!(w.calls("tab-cut").len(), 200);
    }

    #[test]
    fn repeating_a_capture_in_any_order_leaves_the_same_rows_and_survives_a_reopen() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-rep", "claude", Some(CLAUDE_ID), None);
        // checkpoint, end, a late checkpoint after the end, the end twice.
        for _ in 0..4 {
            assert!(matches!(w.capture("tab-rep"), Captured::Stored(_)));
        }
        let first = (w.stored("tab-rep").transcript, w.calls("tab-rep"));
        // A grown log is picked up and the index follows it exactly.
        let grown = format!(
            "{CLAUDE_FIXTURE}{}\n",
            serde_json::json!({"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"toolu_new","name":"Edit","input":{}}]}})
        );
        w.claude_log(&w.home.join(".claude"), &grown);
        assert!(matches!(w.capture("tab-rep"), Captured::Stored(_)));
        assert_eq!(w.calls("tab-rep").len(), first.1.len() + 1);
        drop(UsageStore::open(&w.store).unwrap());
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        assert!(matches!(w.capture("tab-rep"), Captured::Stored(_)));
        assert_eq!((w.stored("tab-rep").transcript, w.calls("tab-rep")), first);
    }

    #[test]
    fn an_expired_conversation_is_not_brought_back_by_a_late_checkpoint() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-exp", "claude", Some(CLAUDE_ID), None);
        assert!(matches!(w.capture("tab-exp"), Captured::Stored(_)));
        let mut store = UsageStore::open(&w.store).unwrap();
        let mut meta = store.read_conversation_meta("tab-exp").unwrap().unwrap();
        meta.state = State::Expired;
        let calls = store.read_tool_calls("tab-exp").unwrap();
        store.write_conversation(&meta, &calls).unwrap();
        std::fs::remove_file(conversation_file(&w.out, "tab-exp").unwrap()).unwrap();
        assert_eq!(w.capture("tab-exp"), Captured::Expired);
        assert!(!conversation_file(&w.out, "tab-exp").unwrap().exists());
        assert_eq!(w.calls("tab-exp").len(), 3, "the index stays");
    }

    #[test]
    fn a_symlinked_log_is_not_read() {
        let w = World::new();
        let real = w.home.join("elsewhere.jsonl");
        std::fs::create_dir_all(&w.home).unwrap();
        std::fs::write(&real, CLAUDE_FIXTURE).unwrap();
        let dir = w.home.join(".claude/projects/-work-wt");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(format!("{CLAUDE_ID}.jsonl"))).unwrap();
        w.record("tab-link", "claude", Some(CLAUDE_ID), None);
        assert_eq!(w.capture("tab-link"), Captured::NoLog);
    }

    #[test]
    fn checkpoints_are_spaced_by_the_debouncer_and_the_end_always_runs() {
        let mut d = Debouncer::default();
        assert_eq!(d.offer("a", Trigger::Checkpoint, 0), Offer::Run);
        assert_eq!(d.offer("a", Trigger::Checkpoint, 10_000), Offer::Deferred);
        assert_eq!(d.offer("a", Trigger::Checkpoint, 20_000), Offer::Deferred);
        assert_eq!(
            d.next_due(),
            Some(DEBOUNCE_MS),
            "one delayed run for the burst"
        );
        assert!(d.take_due(DEBOUNCE_MS - 1).is_empty());
        assert_eq!(d.take_due(DEBOUNCE_MS), ["a"]);
        assert_eq!(
            d.offer("a", Trigger::Checkpoint, DEBOUNCE_MS + 1),
            Offer::Deferred
        );
        // The end runs now and drops the delayed checkpoint it makes redundant.
        assert_eq!(d.offer("a", Trigger::End, DEBOUNCE_MS + 2), Offer::Run);
        assert_eq!(d.next_due(), None);
        assert_eq!(
            d.offer("b", Trigger::Checkpoint, 5),
            Offer::Run,
            "another session is independent"
        );
        // Ending twice runs twice (idempotent capture), and a checkpoint after an end runs.
        assert_eq!(d.offer("a", Trigger::End, 100_000), Offer::Run);
        assert_eq!(d.offer("a", Trigger::Checkpoint, 100_001), Offer::Run);
    }

    fn skill_rows(w: &World, session: &str) -> Vec<(u32, String, String, Option<String>)> {
        w.calls(session)
            .into_iter()
            .filter_map(|c| Some((c.turn_index, c.name, c.skill?, c.native_id)))
            .collect()
    }

    #[test]
    fn claudes_skill_tool_and_a_slash_prompt_for_a_provided_skill_are_each_one_row() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-skill", "claude", Some(CLAUDE_ID), None);
        w.provide("tab-skill", &["bmad-help", "review"]);
        assert!(matches!(w.capture("tab-skill"), Captured::Stored(_)));
        assert_eq!(
            skill_rows(&w, "tab-skill"),
            [
                (
                    2,
                    "Skill".to_owned(),
                    "bmad-help".to_owned(),
                    Some("toolu_01BBB".to_owned())
                ),
                (4, SKILL_PROMPT.to_owned(), "bmad-help".to_owned(), None),
            ]
        );
    }

    #[test]
    fn a_dollar_prompt_for_a_provided_skill_in_a_codex_session_is_one_row() {
        let w = World::new();
        w.codex_log(&w.home.join(".codex"), CODEX_FIXTURE);
        w.record("cx-skill", "codex", Some(CODEX_ID), None);
        w.provide("cx-skill", &["bmad-help"]);
        assert!(matches!(w.capture("cx-skill"), Captured::Stored(_)));
        assert_eq!(
            skill_rows(&w, "cx-skill"),
            [(3, SKILL_PROMPT.to_owned(), "bmad-help".to_owned(), None)]
        );
    }

    #[test]
    fn a_prompt_that_names_no_provided_skill_makes_no_skill_row() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-none", "claude", Some(CLAUDE_ID), None);
        // The session was started without the skill: the slash prompt invokes nothing.
        w.provide("tab-none", &["review"]);
        assert!(matches!(w.capture("tab-none"), Captured::Stored(_)));
        let rows = skill_rows(&w, "tab-none");
        assert!(rows.iter().all(|r| r.1 != SKILL_PROMPT), "{rows:?}");
        // Claude's Skill tool call is still the model's own record of a call.
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn only_a_first_word_slash_or_dollar_that_names_a_provided_skill_invokes_it() {
        let provided = vec!["bmad-help".to_owned(), "review".to_owned()];
        let hit = |text: &str| skill_named_by_prompt(text, &provided);
        assert_eq!(hit("/bmad-help what next?").as_deref(), Some("bmad-help"));
        assert_eq!(hit("  $review this diff").as_deref(), Some("review"));
        assert_eq!(hit("/review").as_deref(), Some("review"));
        assert_eq!(
            hit("<command-name>/bmad-help</command-name>\n<command-args></command-args>")
                .as_deref(),
            Some("bmad-help")
        );
        for miss in [
            "please run /bmad-help",
            "/other-skill do it",
            "$unknown",
            "/Users/me/review",
            "bmad-help",
            "//bmad-help",
            "/bmad-helpful",
            "",
        ] {
            assert_eq!(hit(miss), None, "{miss:?}");
        }
        assert_eq!(skill_named_by_prompt("/bmad-help", &[]), None);
    }

    #[test]
    fn only_a_turn_ending_is_a_checkpoint() {
        use codesense_pty_core::activity::Event;
        take_noted();
        for event in [
            Event::Submitted,
            Event::ToolStarted,
            Event::ToolFinished,
            Event::Waiting,
        ] {
            note_hook("s", event);
        }
        assert!(take_noted().is_empty());
        note_hook("s", Event::Stopped);
        note_hook("s", Event::Failed);
        note_hook("s", Event::Interrupted);
        assert_eq!(take_noted().len(), 3);
    }

    #[test]
    fn noting_a_session_records_the_request_without_capturing_in_a_test_build() {
        take_noted();
        note_checkpoint("s1");
        note_end("s1");
        assert_eq!(
            take_noted(),
            [
                ("s1".to_owned(), Trigger::Checkpoint),
                ("s1".to_owned(), Trigger::End)
            ]
        );
    }

    #[test]
    fn checkpoint_end_end_and_a_late_checkpoint_in_either_order_leave_the_same_conversation() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(
            root.join("usage.sqlite"),
            root.join("conv"),
            root.join("home"),
        );
        w.seed_claude("tab-order", CLAUDE_ID);
        note_checkpoint("tab-order");
        let first = w.stored("tab-order").unwrap();
        // The end, the end again, then a checkpoint that was queued behind them.
        note_end("tab-order");
        note_end("tab-order");
        note_checkpoint("tab-order");
        assert_eq!(w.stored("tab-order").unwrap(), first);
        // An end that arrives first and a checkpoint racing it afterwards agree too.
        let other = nth_uuid(7);
        w.seed_claude("tab-race", &other);
        note_end("tab-race");
        let ended = w.stored("tab-race").unwrap();
        note_checkpoint("tab-race");
        w.advance(DEBOUNCE_MS);
        assert_eq!(w.stored("tab-race").unwrap(), ended);
        // Reopen the store from the file: the same rows, and a re-capture adds none.
        let calls = rusqlite::Connection::open(&w.store)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM session_tool_call WHERE session_id = 'tab-order'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        note_end("tab-order");
        let after = rusqlite::Connection::open(&w.store)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM session_tool_call WHERE session_id = 'tab-order'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!((calls, after), (3, 3));
    }

    #[test]
    fn a_log_that_grows_past_the_old_input_tail_keeps_every_earlier_call_and_turn_number() {
        let w = World::new();
        w.record("tab-big", "claude", Some(CLAUDE_ID), None);
        // Before: a small log. Then it grows by far more than one policy cap of ignorable noise.
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        assert!(matches!(w.capture("tab-big"), Captured::Stored(_)));
        let before = w.calls("tab-big");
        let noise = serde_json::json!({"type": "attachment", "attachment": {"type": "environment", "pad": "x".repeat(4096)}}).to_string();
        let big = format!("{CLAUDE_FIXTURE}{}", format!("{noise}\n").repeat(600));
        assert!(
            big.len() > 5 * 64 * 1024,
            "the log is far larger than the old input tail"
        );
        w.claude_log(&w.home.join(".claude"), &big);
        let small = Policy {
            max_bytes: 2 * 1024 * 1024,
            ..Policy::default()
        };
        assert!(matches!(
            w.capture_with("tab-big", &small),
            Captured::Stored(_)
        ));
        assert_eq!(
            w.calls("tab-big"),
            before,
            "earlier calls and turn numbers are unchanged"
        );
        let stored = w.stored("tab-big");
        assert_eq!(stored.transcript.turns[0].index, Some(0));
        assert_eq!(stored.transcript.turns.len(), 5);
        assert!(
            stored.truncated.is_none(),
            "noise the conversation does not hold is not a cut"
        );
    }

    #[test]
    fn a_field_over_a_mebibyte_is_kept_whole_while_the_conversation_fits_and_cut_only_when_it_does_not(
    ) {
        let w = World::new();
        let huge = "y".repeat(3 * 1024 * 1024);
        let line = |role: &str, content: serde_json::Value| {
            serde_json::json!({"type": role, "origin": {"kind": "human"}, "message": {"role": role, "content": content}}).to_string()
        };
        let log = [
            line("user", serde_json::json!("paste this")),
            line("assistant", serde_json::json!([{"type": "tool_use", "id": "toolu_h", "name": "Bash", "input": {"cmd": huge.clone()}}])),
        ]
        .join("\n");
        w.claude_log(&w.home.join(".claude"), &log);
        w.record("tab-huge", "claude", Some(CLAUDE_ID), None);
        // Under the cap: whole, nothing cut, nothing counted.
        let Captured::Stored(meta) = w.capture("tab-huge") else {
            panic!("stored")
        };
        assert_eq!((meta.state, meta.truncated.clone()), (State::Present, None));
        assert!(
            w.stored("tab-huge").transcript.turns[1].tool_calls[0]
                .input
                .as_deref()
                .unwrap()
                .len()
                >= huge.len()
        );
        // Over the cap: the field is cut to fit and the bytes cut are counted.
        let policy = Policy {
            max_bytes: 1024 * 1024,
            ..Policy::default()
        };
        let Captured::Stored(cut) = w.capture_with("tab-huge", &policy) else {
            panic!("stored")
        };
        let t = cut.truncated.unwrap();
        assert!(
            t.trimmed_fields >= 1 && t.dropped_bytes > 1024 * 1024,
            "{t:?}"
        );
        assert!(cut.bytes as usize <= 1024 * 1024 + 4096, "{}", cut.bytes);
    }

    #[test]
    fn thinking_that_claude_wrote_as_its_own_message_joins_the_message_after_it() {
        let w = World::new();
        let row = |ts: &str, content: serde_json::Value| {
            serde_json::json!({"type": "assistant", "timestamp": ts, "message": {"role": "assistant", "model": "m", "content": content}}).to_string()
        };
        let log = [
            serde_json::json!({"type": "user", "origin": {"kind": "human"}, "message": {"role": "user", "content": "go"}}).to_string(),
            row("2026-10-27T10:00:01.000Z", serde_json::json!([{"type": "thinking", "thinking": "weigh the options", "signature": "s"}])),
            row("2026-10-27T10:00:02.000Z", serde_json::json!([{"type": "tool_use", "id": "toolu_t", "name": "Bash", "input": {}}])),
            row("2026-10-27T10:00:03.000Z", serde_json::json!([{"type": "thinking", "thinking": "", "signature": "s"}])),
        ]
        .join("\n");
        w.claude_log(&w.home.join(".claude"), &log);
        w.record("tab-think", "claude", Some(CLAUDE_ID), None);
        assert!(matches!(w.capture("tab-think"), Captured::Stored(_)));
        let turns = w.stored("tab-think").transcript.turns;
        assert_eq!(
            turns.len(),
            3,
            "the thinking-only message is not a turn of its own"
        );
        let reasoning = turns[1].reasoning.as_ref().unwrap();
        assert_eq!(reasoning.text.as_deref(), Some("weigh the options"));
        assert_eq!(turns[1].tool_calls[0].native_id.as_deref(), Some("toolu_t"));
        // A trailing thinking-only message with nothing after it still keeps its step.
        assert_eq!(
            turns[2].reasoning.as_ref().map(|r| r.kind),
            Some(codesense_pty_core::agent_cli::output::ReasoningKind::None)
        );
    }

    #[test]
    fn a_capture_that_read_an_older_log_cannot_publish_over_the_final_one() {
        use std::sync::mpsc::channel;
        let w = std::sync::Arc::new(World::new());
        w.claude_log(&w.home.join(".claude"), CLAUDE_FIXTURE);
        w.record("tab-race", "claude", Some(CLAUDE_ID), None);
        let (read_done, read_done_rx) = channel::<()>();
        let (go, go_rx) = channel::<()>();
        // The older capture (a turn-end checkpoint) has read the log and is held before it
        // publishes.
        let older = {
            let w = w.clone();
            std::thread::spawn(move || {
                PAUSE_BEFORE_PUBLISH.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        read_done.send(()).unwrap();
                        go_rx.recv().unwrap();
                    }));
                });
                w.capture("tab-race")
            })
        };
        read_done_rx.recv().unwrap();
        // The session finishes: the log grows by a call, and the final capture is asked for.
        let grown = format!(
            "{CLAUDE_FIXTURE}{}\n",
            serde_json::json!({"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"toolu_final","name":"Edit","input":{}}]}})
        );
        w.claude_log(&w.home.join(".claude"), &grown);
        let final_capture = {
            let w = w.clone();
            std::thread::spawn(move || w.capture("tab-race"))
        };
        // The final capture waits for the one in flight; release it, and both finish.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            w.calls("tab-race").is_empty(),
            "nothing is published while the older capture is held"
        );
        go.send(()).unwrap();
        assert!(matches!(older.join().unwrap(), Captured::Stored(_)));
        assert!(matches!(final_capture.join().unwrap(), Captured::Stored(_)));
        let names: Vec<String> = w.calls("tab-race").into_iter().map(|c| c.name).collect();
        assert_eq!(
            names.last().map(String::as_str),
            Some("Edit"),
            "the final capture's index stands: {names:?}"
        );
        assert_eq!(
            w.stored("tab-race").transcript.turns.len(),
            6,
            "and so does its conversation"
        );
    }

    #[test]
    fn sessions_do_not_wait_on_each_others_captures() {
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let a = {
            let started = started.clone();
            std::thread::spawn(move || {
                with_session_lock("lock-a", || {
                    started.wait();
                    started.wait();
                })
            })
        };
        started.wait();
        // While lock-a is held, lock-b runs freely.
        assert_eq!(with_session_lock("lock-b", || 7), 7);
        started.wait();
        a.join().unwrap();
        assert!(
            SESSION_LOCKS.lock().unwrap().get("lock-a").is_none(),
            "an idle lock is forgotten"
        );
    }

    #[test]
    fn a_call_after_a_long_stretch_of_other_records_is_indexed_whatever_the_size() {
        let w = World::new();
        let noise = serde_json::json!({"type": "attachment", "attachment": {"type": "environment", "pad": "x".repeat(8192)}}).to_string();
        let call = |id: &str| {
            serde_json::json!({"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":id,"name":"Bash","input":{}}]}}).to_string()
        };
        // Megabytes of records the conversation does not hold, then the call that matters: the
        // read has no ceiling, so the last call is indexed however far into the log it is.
        let filler = format!("{noise}\n").repeat(300);
        let log = format!("{}\n{filler}{}\n", call("toolu_early"), call("toolu_late"));
        w.claude_log(&w.home.join(".claude"), &log);
        w.record("tab-late", "claude", Some(CLAUDE_ID), None);
        assert!(matches!(w.capture("tab-late"), Captured::Stored(_)));
        let ids: Vec<Option<String>> = w
            .calls("tab-late")
            .into_iter()
            .map(|c| c.native_id)
            .collect();
        assert_eq!(ids, [Some("toolu_early".into()), Some("toolu_late".into())]);
    }

    // ── Reading ──────────────────────────────────────────────────────────────────────────────

    fn read(w: &World, session: &str, selector: Selector) -> ConversationPage {
        read_conversation_in(&w.store, &w.out, session, &selector).unwrap()
    }

    fn stored_world(session: &str, log: &str) -> World {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), log);
        w.record(session, "claude", Some(CLAUDE_ID), None);
        assert!(matches!(w.capture(session), Captured::Stored(_)));
        w
    }

    #[test]
    fn indexed_pointers_share_native_jumps_and_keep_expired_and_truncated_locations() {
        let w = stored_world("pointer", CLAUDE_FIXTURE);
        let store = UsageStore::open(&w.store).unwrap();
        let pointer = indexed_pointer(&store, CLAUDE_ID, "toolu_01CCC").unwrap();
        assert_eq!(pointer.session_id, "pointer"); assert_eq!(pointer.turn_index, Some(2));
        assert_eq!(pointer.state, Availability::Present); assert!(!pointer.text_truncated);
        assert_eq!(indexed_pointer(&store,"pointer","missing").unwrap().turn_index,None);
        store.mark_conversation_expired("pointer").unwrap();
        let expired = indexed_pointer(&store,"pointer","toolu_01CCC").unwrap();
        assert_eq!(expired.turn_index,Some(2)); assert_eq!(expired.state,Availability::Expired);
        let w = World::new();
        w.codex_log(&w.home.join(".codex"), CODEX_FIXTURE);
        w.record("codex-pointer","codex",Some(CODEX_ID),None);
        assert!(matches!(w.capture("codex-pointer"),Captured::Stored(_)));
        let store = UsageStore::open(&w.store).unwrap();
        assert!(indexed_pointer(&store,CODEX_ID,"exec-0001").unwrap().turn_index.is_some());
        let w = World::new(); w.claude_log(&w.home.join(".claude"),&long_claude_log(200));
        w.record("dropped-pointer","claude",Some(CLAUDE_ID),None);
        assert!(matches!(w.capture_with("dropped-pointer",&Policy {max_bytes:60*1024,..Policy::default()}),Captured::Stored(_)));
        let store = UsageStore::open(&w.store).unwrap();
        let dropped = indexed_pointer(&store,"dropped-pointer","toolu_2").unwrap();
        assert!(dropped.turn_index.is_some() && dropped.text_truncated);
        let retained = indexed_pointer(&store,"dropped-pointer","toolu_199").unwrap();
        assert!(retained.turn_index.is_some() && !retained.text_truncated);
    }

    #[test]
    fn pages_follow_the_cursor_to_the_end_and_cover_every_turn_once() {
        let w = stored_world("tab-page", &long_claude_log(10));
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = read(
                &w,
                "tab-page",
                Selector::Page {
                    cursor,
                    limit: Some(7),
                },
            );
            assert_eq!(page.state, Availability::Present);
            seen.extend(page.turns.iter().map(|t| t.index.unwrap()));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, (0..20).collect::<Vec<u32>>());
        let first = read(
            &w,
            "tab-page",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(first.turns.len(), 20.min(DEFAULT_PAGE));
        assert_eq!(first.tool_call_count, 10);
        assert_eq!(first.turn_count, 20);
    }

    const DEFAULT_PAGE: usize = 20;

    #[test]
    fn a_turn_and_a_tool_use_id_can_be_jumped_to_by_the_native_id_or_the_session_id() {
        let w = stored_world("tab-jump", CLAUDE_FIXTURE);
        let turn = read(
            &w,
            "tab-jump",
            Selector::Turn {
                index: 2,
                limit: Some(2),
            },
        );
        assert_eq!(
            turn.turns
                .iter()
                .map(|t| t.index.unwrap())
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(turn.lookup.as_ref().map(|l| l.found), Some(true));
        // By the CLI's tool-use id, and asking by the CLI's own session id.
        let by_tool = read(
            &w,
            CLAUDE_ID,
            Selector::ToolUse {
                id: "toolu_01CCC".into(),
                limit: None,
            },
        );
        assert_eq!(by_tool.session_id, "tab-jump");
        assert_eq!(by_tool.turns.len(), 1);
        assert_eq!(by_tool.turns[0].index, Some(2));
        assert_eq!(by_tool.lookup.unwrap().turn_index, Some(2));
        let missing = read(
            &w,
            "tab-jump",
            Selector::ToolUse {
                id: "toolu_nope".into(),
                limit: None,
            },
        );
        assert!(missing.turns.is_empty() && missing.lookup.as_ref().is_some_and(|l| !l.found));
        assert!(missing.reason.unwrap().contains("toolu_nope"));
        let past = read(
            &w,
            "tab-jump",
            Selector::Turn {
                index: 99,
                limit: None,
            },
        );
        assert!(past.turns.is_empty() && past.reason.is_some());
    }

    #[test]
    fn a_dropped_turn_says_it_was_dropped_and_the_index_still_finds_its_tool_call() {
        let w = World::new();
        w.claude_log(&w.home.join(".claude"), &long_claude_log(200));
        w.record("tab-drop", "claude", Some(CLAUDE_ID), None);
        let policy = Policy {
            max_bytes: 60 * 1024,
            ..Policy::default()
        };
        assert!(matches!(
            w.capture_with("tab-drop", &policy),
            Captured::Stored(_)
        ));
        let page = read(
            &w,
            "tab-drop",
            Selector::Turn {
                index: 5,
                limit: None,
            },
        );
        assert_eq!(page.state, Availability::Truncated);
        assert!(page.turns.is_empty());
        assert!(
            page.reason
                .as_deref()
                .unwrap()
                .contains("dropped by the size limit"),
            "{:?}",
            page.reason
        );
        assert!(page.truncated.is_some());
        let by_tool = read(
            &w,
            "tab-drop",
            Selector::ToolUse {
                id: "toolu_2".into(),
                limit: None,
            },
        );
        assert!(by_tool.reason.unwrap().contains("dropped"));
        assert_eq!(by_tool.tool_call_count, 200);
    }

    #[test]
    fn a_live_opencode_session_reads_unsupported_before_any_capture_is_tried() {
        let w = World::new();
        w.record("oc-live", "opencode", Some("ses_live"), None);
        let page = read(
            &w,
            "oc-live",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(page.state, Availability::Unsupported);
        assert!(page.reason.unwrap().contains("opencode"));
        // A CLI that is read but has produced nothing yet is absent, and an unknown session too.
        w.record("tab-new", "claude", Some(CLAUDE_ID), None);
        assert_eq!(
            read(
                &w,
                "tab-new",
                Selector::Page {
                    cursor: None,
                    limit: None
                }
            )
            .state,
            Availability::Absent
        );
        assert_eq!(
            read(
                &w,
                "nobody",
                Selector::Page {
                    cursor: None,
                    limit: None
                }
            )
            .state,
            Availability::Absent
        );
    }

    #[test]
    fn a_read_always_names_its_state_when_there_are_no_turns() {
        let w = World::new();
        let page = read(
            &w,
            "ghost",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!((page.state, page.turns.len()), (Availability::Absent, 0));
        assert!(page.reason.unwrap().contains("no such session"));

        w.record("tab-pending", "claude", Some(CLAUDE_ID), None);
        let pending = read(
            &w,
            "tab-pending",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(pending.state, Availability::Absent);
        assert!(pending.reason.unwrap().contains("yet"));

        w.record("oc-1", "opencode", Some("ses_x"), None);
        assert_eq!(w.capture("oc-1"), Captured::Unsupported);
        let oc = read(
            &w,
            "oc-1",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(oc.state, Availability::Unsupported);
        assert!(oc.reason.unwrap().contains("opencode"));

        let stored = stored_world("tab-gone", CLAUDE_FIXTURE);
        std::fs::remove_file(conversation_file(&stored.out, "tab-gone").unwrap()).unwrap();
        let gone = read(
            &stored,
            "tab-gone",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(gone.state, Availability::Absent);
        assert!(gone.reason.is_some());
    }

    #[test]
    fn a_secret_that_reached_a_file_is_still_masked_when_it_is_read() {
        let w = stored_world("tab-render", CLAUDE_FIXTURE);
        let file = conversation_file(&w.out, "tab-render").unwrap();
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let planted = std::fs::read_to_string(&file)
            .unwrap()
            .replace("Fixed it.", &format!("Fixed it with {token}"));
        std::fs::write(&file, planted).unwrap();
        let page = read(
            &w,
            "tab-render",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        let text = serde_json::to_string(&page).unwrap();
        assert!(!text.contains(&token) && text.contains("Fixed it with"));
    }

    // ── Retention ────────────────────────────────────────────────────────────────────────────

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn retention_removes_the_text_after_ninety_days_and_keeps_the_index() {
        let w = stored_world("tab-old", CLAUDE_FIXTURE);
        w.record(
            "tab-young",
            "claude",
            Some("11111111-2222-4333-8444-555555555555"),
            None,
        );
        let young_log = w
            .home
            .join(".claude/projects/-work-wt/11111111-2222-4333-8444-555555555555.jsonl");
        std::fs::write(&young_log, CLAUDE_FIXTURE).unwrap();
        capture_in(
            &w.store,
            &w.out,
            &w.env(),
            "tab-young",
            89 * DAY,
            &Policy::default(),
        )
        .unwrap();
        // tab-old was captured at t=1_000 and ended at t=2_000.
        rusqlite::Connection::open(&w.store)
            .unwrap()
            .execute(
                "INSERT INTO session_end (session_id, ended_at_ms) VALUES ('tab-old', 2000)",
                [],
            )
            .unwrap();
        let policy = Policy::default();
        assert_eq!(
            sweep_expired_in(&w.store, &w.out, 89 * DAY, &policy).unwrap(),
            0,
            "not yet"
        );
        assert_eq!(
            sweep_expired_in(&w.store, &w.out, 91 * DAY, &policy).unwrap(),
            1
        );
        assert!(!conversation_file(&w.out, "tab-old").unwrap().exists());
        assert!(
            conversation_file(&w.out, "tab-young").unwrap().exists(),
            "ended 2 days ago"
        );
        let page = read(
            &w,
            "tab-old",
            Selector::Page {
                cursor: None,
                limit: None,
            },
        );
        assert_eq!(page.state, Availability::Expired);
        assert!(page.turns.is_empty() && page.reason.is_some());
        assert_eq!(page.tool_call_count, 3, "names and counts stay");
        assert_eq!(w.calls("tab-old").len(), 3);
        // A second sweep and a late checkpoint change nothing.
        assert_eq!(
            sweep_expired_in(&w.store, &w.out, 91 * DAY, &policy).unwrap(),
            0
        );
        assert_eq!(w.capture("tab-old"), Captured::Expired);
    }

    // ── Reaping a scoped home ────────────────────────────────────────────────────────────────

    fn scoped_codex(session: &str) -> (crate::routines::test_support::TempHome, PathBuf) {
        let home = crate::routines::test_support::TempHome::new();
        let scope = crate::commands::mcp_scope::mcp_scope_session_dir(session);
        let sessions = scope.join("sessions/2026/10/27");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("rollout-2026-10-27T10-00-00-{CODEX_ID}.jsonl")),
            CODEX_FIXTURE,
        )
        .unwrap();
        let store = crate::session_usage::store_path().unwrap();
        let mut r = SessionRecord::new(session);
        r.cli = Some("codex".into());
        r.native_session_id = Some(CODEX_ID.into());
        r.config_home = Some(scope.to_string_lossy().into_owned());
        UsageStore::open(&store)
            .unwrap()
            .write_session_record(&r)
            .unwrap();
        (home, scope)
    }

    #[test]
    fn reaping_a_scoped_codex_home_stores_its_conversation_first() {
        let (_home, scope) = scoped_codex("cx-reap-first");
        crate::commands::mcp_scope::remove_scope_config("cx-reap-first");
        assert!(!scope.exists(), "the scoped home is removed");
        let store = crate::session_usage::store_path().unwrap();
        let dir = conversations_dir().unwrap();
        let page = read_conversation_in(
            &store,
            &dir,
            "cx-reap-first",
            &Selector::Page {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(page.state, Availability::Present, "{:?}", page.reason);
        assert_eq!(page.tool_call_count, 3);
        // The end trigger arriving after the reap finds no log and leaves what is stored.
        let env = |_: &str| None;
        assert_eq!(
            capture_in(
                &store,
                &dir,
                &env,
                "cx-reap-first",
                5_000,
                &Policy::default()
            )
            .unwrap(),
            Captured::NoLog
        );
        let again = read_conversation_in(
            &store,
            &dir,
            "cx-reap-first",
            &Selector::Page {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(
            (again.state, again.turn_count, again.tool_call_count),
            (Availability::Present, page.turn_count, 3)
        );
    }

    #[test]
    fn a_capture_that_already_ran_before_the_reap_is_unchanged_by_it() {
        let (_home, scope) = scoped_codex("cx-reap-after");
        let store = crate::session_usage::store_path().unwrap();
        let dir = conversations_dir().unwrap();
        let env = |_: &str| None;
        assert!(matches!(
            capture_in(
                &store,
                &dir,
                &env,
                "cx-reap-after",
                1_000,
                &Policy::default()
            )
            .unwrap(),
            Captured::Stored(_)
        ));
        let before = std::fs::read(conversation_file(&dir, "cx-reap-after").unwrap()).unwrap();
        crate::commands::mcp_scope::remove_scope_config("cx-reap-after");
        assert!(!scope.exists());
        let after = std::fs::read(conversation_file(&dir, "cx-reap-after").unwrap()).unwrap();
        let (b, a): (StoredConversation, StoredConversation) = (
            serde_json::from_slice(&before).unwrap(),
            serde_json::from_slice(&after).unwrap(),
        );
        assert_eq!(
            a.transcript, b.transcript,
            "the reap's own capture rebuilds the same conversation"
        );
    }

    #[test]
    fn a_home_without_session_logs_is_not_captured_at_removal() {
        let _home = crate::routines::test_support::TempHome::new();
        let dir = crate::commands::mcp_scope::mcp_scope_session_dir("claude-scope");
        std::fs::create_dir_all(&dir).unwrap();
        capture_before_removal("claude-scope", &dir);
        assert!(!conversations_dir()
            .unwrap()
            .join("claude-scope.json")
            .exists());
    }
}
