// SPDX-License-Identifier: MIT
//! The remote usage spool: the agent CLIs' usage records, kept as the CLI sent them, for the Mac to
//! pull. The remote prices nothing and maps nothing. It keeps only each CLI's usage record, as its
//! adapter names it, one JSON line each, with a sequence number. The epoch names this spool file,
//! so a lost file is seen as a new one and the Mac re-pulls from zero.
//!
//! Pruning: asking for the records after a sequence number says the desktop has stored every record
//! through it. A record the desktop has stored is pruned once it is a week old, which leaves another
//! desktop pulling the same machine a week to catch up. A spool no desktop reads is capped by count,
//! oldest first; the desktop sees the gap and marks the machine incomplete. The newest record is never
//! pruned, so a restarted receiver numbers on from it under the same epoch.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use codesense_pty_core::agent_cli::{descriptor, AgentCli, TelemetryExport};
use codesense_pty_core::protocol::{UsageSpoolRead, UsageSpoolRecord};
use serde_json::{json, Value};

/// The most records one read returns, so one reply stays a bounded size.
pub const MAX_READ_RECORDS: usize = 500;
/// The largest record kept. A usage record is a few hundred bytes; a larger one is not usage, and
/// would make a read's reply unbounded.
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// How long a record the desktop has stored is kept, for another desktop pulling the same machine.
const STORED_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The most records a spool holds; past this the oldest go, stored or not.
const MAX_RECORDS: usize = 100_000;
/// Appends between prunes, and the time between prunes a read may start.
const PRUNE_EVERY_RECORDS: u64 = 1_000;
const PRUNE_EVERY_MS: u64 = 60 * 60 * 1000;

const SPOOL_FILE: &str = "spool.jsonl";
const SPOOL_REWRITE: &str = "spool.jsonl.next";
const EPOCH_FILE: &str = "epoch";

pub struct Spool {
    file: PathBuf,
    epoch: String,
    next_seq: u64,
    /// The highest sequence number a desktop said it stored.
    stored_through: u64,
    appended_since_prune: u64,
    last_prune_ms: u64,
}

impl Spool {
    /// Opens the spool under `dir`, creating it if absent. A spool file that is missing gets a new
    /// epoch, even when an old epoch file is still there.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let file = dir.join(SPOOL_FILE);
        let epoch_path = dir.join(EPOCH_FILE);
        let existing = fs::read_to_string(&epoch_path).ok();
        let epoch = match existing.map(|e| e.trim().to_string()) {
            Some(epoch) if file.exists() && !epoch.is_empty() => epoch,
            _ => {
                let epoch = uuid::Uuid::new_v4().to_string();
                write_private(&epoch_path, epoch.as_bytes())?;
                epoch
            }
        };
        if !file.exists() {
            write_private(&file, b"")?;
        }
        // A line cut off by a crash must not run into the next record.
        if !ends_with_newline(&file)? {
            OpenOptions::new()
                .append(true)
                .open(&file)?
                .write_all(b"\n")?;
        }
        let next_seq = last_seq(&file)?.map_or(1, |seq| seq + 1);
        Ok(Self {
            file,
            epoch,
            next_seq,
            stored_through: 0,
            appended_since_prune: 0,
            last_prune_ms: now_ms(),
        })
    }

    /// The records past `after`, at most `limit`. Asking for them says the desktop stored every record
    /// through `after`; a prune that is due runs first. The desktop asks every minute, so when nothing
    /// is past `after` the answer comes from this spool's next sequence number (sequence numbers start
    /// at 1, and this spool appends every record) without reading the file. Every answer carries this
    /// spool's own epoch, the one its records were numbered under.
    pub fn read_past(&mut self, after: u64, limit: usize) -> io::Result<UsageSpoolRead> {
        self.stored_through = self.stored_through.max(after);
        let now = now_ms();
        if now.saturating_sub(self.last_prune_ms) >= PRUNE_EVERY_MS {
            self.prune(now)?;
        }
        let last = self.next_seq.saturating_sub(1);
        if after >= last {
            // A full read reports the oldest record too; with nothing past `after` it can never show
            // a gap, so none is reported.
            return Ok(UsageSpoolRead {
                epoch: self.epoch.clone(),
                oldest_seq: None,
                last_seq: last,
                records: Vec::new(),
            });
        }
        let mut read = read(self.dir(), after, limit)?;
        read.epoch = self.epoch.clone();
        Ok(read)
    }

    fn dir(&self) -> &Path {
        self.file.parent().unwrap_or(Path::new("."))
    }

    /// Drops what may go as of `now_ms` and returns how many records went. See the module's note.
    pub fn prune(&mut self, now_ms: u64) -> io::Result<usize> {
        self.prune_with(now_ms, STORED_RETENTION_MS, MAX_RECORDS)
    }

    fn prune_with(
        &mut self,
        now_ms: u64,
        retention_ms: u64,
        max_records: usize,
    ) -> io::Result<usize> {
        self.last_prune_ms = now_ms;
        self.appended_since_prune = 0;
        let mut lines: Vec<(u64, u64, String)> = Vec::new();
        for line in BufReader::new(File::open(&self.file)?).lines() {
            let line = line?;
            // A torn line holds no record, and goes with the prune.
            let Ok(row) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(seq) = row.get("seq").and_then(Value::as_u64) else {
                continue;
            };
            let at = row.get("at").and_then(Value::as_u64).unwrap_or(0);
            lines.push((seq, at, line));
        }
        let total = lines.len();
        let newest = lines.iter().map(|(seq, _, _)| *seq).max();
        let mut kept: Vec<(u64, u64, String)> = lines
            .into_iter()
            .filter(|(seq, at, _)| {
                Some(*seq) == newest
                    || *seq > self.stored_through
                    || at.saturating_add(retention_ms) > now_ms
            })
            .collect();
        if kept.len() > max_records {
            kept.drain(..kept.len() - max_records);
        }
        let removed = total - kept.len();
        if removed == 0 {
            return Ok(0);
        }
        // Written beside the spool and renamed over it, so a crash leaves one whole file or the other.
        let next = self.dir().join(SPOOL_REWRITE);
        let mut body = String::new();
        for (_, _, line) in &kept {
            body.push_str(line);
            body.push('\n');
        }
        write_private(&next, body.as_bytes())?;
        File::open(&next)?.sync_all()?;
        fs::rename(&next, &self.file)?;
        if let Ok(dir) = File::open(self.dir()) {
            let _ = dir.sync_all();
        }
        Ok(removed)
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Keeps the usage records in one OTLP/JSON logs export, as `token`'s, and returns how many were
    /// kept. Nothing is kept unless the whole batch reaches disk; a failed write is cut back, so a
    /// retry does not leave a torn line behind.
    pub fn append(&mut self, token: &str, export: &Value) -> io::Result<usize> {
        let records = usage_records(export);
        if records.is_empty() {
            return Ok(0);
        }
        let mut batch = String::new();
        let mut seq = self.next_seq;
        for record in &records {
            let line = json!({"seq": seq, "at": now_ms(), "token": token, "record": record});
            batch.push_str(&line.to_string());
            batch.push('\n');
            seq += 1;
        }
        let mut file = OpenOptions::new().append(true).open(&self.file)?;
        let before = file.metadata()?.len();
        let written = file
            .write_all(batch.as_bytes())
            .and_then(|()| file.sync_data());
        if let Err(error) = written {
            if file.set_len(before).is_err() {
                // Some of the batch may be left on disk. Its numbers are skipped, so none is used
                // twice and a quick answer still counts what landed; a newline ends any torn line, so
                // the next record does not join it. The desktop keeps each model call once, so a
                // retried export cannot count a landed one twice.
                self.next_seq = seq;
                let _ = file.write_all(b"\n");
            }
            return Err(error);
        }
        self.next_seq = seq;
        self.appended_since_prune += records.len() as u64;
        if self.appended_since_prune >= PRUNE_EVERY_RECORDS {
            // The records are already on disk; a prune that fails only leaves the file larger.
            if let Err(error) = self.prune(now_ms()) {
                log::warn!("usage spool prune failed: {error}");
            }
        }
        Ok(records.len())
    }
}

/// The CLI adapter whose usage record this is. Each adapter names its own record, so the host never
/// names a CLI itself.
fn usage_export(record: &Value) -> Option<&'static TelemetryExport> {
    let (name, kind) = (
        attribute(record, "event.name"),
        attribute(record, "event.kind"),
    );
    AgentCli::ALL.into_iter().find_map(|cli| {
        descriptor(cli)
            .telemetry
            .as_ref()
            .filter(|export| export.is_usage_record(name, kind))
    })
}

/// A usage record cut down to the attributes its adapter keeps. The rest, the account's email and
/// ids among them, is never stored or sent to the desktop.
fn trimmed(record: &Value, export: &TelemetryExport) -> Value {
    let attributes: Vec<Value> = record
        .get("attributes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| {
            a.get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| export.usage_attributes.iter().any(|kept| kept == key))
        })
        .cloned()
        .collect();
    json!({ "attributes": attributes })
}

fn usage_records(export: &Value) -> Vec<Value> {
    export
        .get("resourceLogs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|rl| {
            rl.get("scopeLogs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .flat_map(|sl| {
            sl.get("logRecords")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|record| usage_export(record).map(|export| trimmed(record, export)))
        .filter(|record| record.to_string().len() <= MAX_RECORD_BYTES)
        .collect()
}

/// A record's string attribute: its `event.name` and `event.kind`, which say whose usage it is.
fn attribute<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record
        .get("attributes")?
        .as_array()?
        .iter()
        .find(|a| a.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")?
        .get("stringValue")?
        .as_str()
}

/// The records past `after`, at most `limit`, with the spool's epoch and bounds. Reads only: it never
/// creates, repairs or truncates the spool, so it is safe beside the receiver appending to it. A
/// spool that does not exist yet reads as empty.
pub fn read(dir: &Path, after: u64, limit: usize) -> io::Result<UsageSpoolRead> {
    let epoch = fs::read_to_string(dir.join(EPOCH_FILE))
        .map(|epoch| epoch.trim().to_string())
        .unwrap_or_default();
    let mut read = UsageSpoolRead {
        epoch,
        oldest_seq: None,
        last_seq: 0,
        records: Vec::new(),
    };
    let file = match File::open(dir.join(SPOOL_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(read),
        Err(error) => return Err(error),
    };
    for line in BufReader::new(file).lines() {
        let row: Value = match serde_json::from_str(&line?) {
            Ok(row) => row,
            // A torn line from a crash: it holds no record.
            Err(_) => continue,
        };
        let (Some(seq), Some(token)) = (
            row.get("seq").and_then(Value::as_u64),
            row.get("token").and_then(Value::as_str),
        ) else {
            continue;
        };
        read.oldest_seq.get_or_insert(seq);
        read.last_seq = read.last_seq.max(seq);
        if seq > after && read.records.len() < limit {
            read.records.push(UsageSpoolRecord {
                seq,
                token: token.to_string(),
                record: row.get("record").cloned().unwrap_or(Value::Null),
            });
        }
    }
    Ok(read)
}

/// An empty file counts as terminated.
fn ends_with_newline(file: &Path) -> io::Result<bool> {
    let mut file = File::open(file)?;
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

/// The highest sequence number in the file. A torn or foreign line is skipped, not an error.
fn last_seq(file: &Path) -> io::Result<Option<u64>> {
    let reader = BufReader::new(File::open(file)?);
    let mut last = None;
    for line in reader.lines() {
        let line = line?;
        if let Some(seq) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|v| v.get("seq").and_then(Value::as_u64))
        {
            last = Some(last.map_or(seq, |prev: u64| prev.max(seq)));
        }
    }
    Ok(last)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_request(request_id: &str) -> Value {
        json!({"attributes": [
            {"key": "event.name", "value": {"stringValue": "api_request"}},
            {"key": "request_id", "value": {"stringValue": request_id}},
            {"key": "model", "value": {"stringValue": "claude-opus-4-8"}},
        ]})
    }

    fn prompt() -> Value {
        json!({"attributes": [
            {"key": "event.name", "value": {"stringValue": "user_prompt"}},
            {"key": "prompt", "value": {"stringValue": "secret text"}},
        ]})
    }

    fn export(records: Vec<Value>) -> Value {
        json!({"resourceLogs": [{"scopeLogs": [{"logRecords": records}]}]})
    }

    fn lines(dir: &Path) -> Vec<Value> {
        fs::read_to_string(dir.join(SPOOL_FILE))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_read_with_nothing_new_answers_without_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a"), api_request("b")]))
            .unwrap();
        let full = read(dir.path(), 2, 10).unwrap();
        // Nothing past 2, or a cursor ahead of the spool: the same epoch, last sequence number and
        // (no) records as a full read, without the file. A full read also names the oldest record;
        // with nothing past the cursor that can never be a gap, so the quick answer leaves it out.
        std::fs::rename(dir.path().join(SPOOL_FILE), dir.path().join("moved")).unwrap();
        for after in [2, 5] {
            let quick = spool.read_past(after, 10).unwrap();
            assert_eq!(
                (quick.epoch.as_str(), quick.last_seq),
                (full.epoch.as_str(), full.last_seq)
            );
            assert!(quick.records.is_empty() && quick.oldest_seq.is_none());
        }
        std::fs::rename(dir.path().join("moved"), dir.path().join(SPOOL_FILE)).unwrap();
        // A record past the cursor is read from the file as before.
        assert_eq!(spool.read_past(1, 10).unwrap().records.len(), 1);
    }

    #[test]
    fn a_first_record_is_past_a_cursor_of_zero_and_an_empty_spool_has_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let empty = spool.read_past(0, 10).unwrap();
        assert_eq!((empty.last_seq, empty.records.len()), (0, 0));
        spool
            .append("tok", &export(vec![api_request("a")]))
            .unwrap();
        let first = spool.read_past(0, 10).unwrap();
        assert_eq!((first.last_seq, first.records.len()), (1, 1));
        assert_eq!(first.records[0].seq, 1);
    }

    #[test]
    fn every_read_carries_the_spool_s_own_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a"), api_request("b")]))
            .unwrap();
        let epoch = spool.epoch().to_string();
        // The epoch file lost while the spool runs: both a full and a quick read keep its epoch.
        std::fs::remove_file(dir.path().join(EPOCH_FILE)).unwrap();
        assert_eq!(spool.read_past(0, 10).unwrap().epoch, epoch);
        assert_eq!(spool.read_past(2, 10).unwrap().epoch, epoch);
    }

    #[test]
    fn keeps_only_usage_records_with_sequence_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let kept = spool
            .append(
                "tok",
                &export(vec![api_request("a"), prompt(), api_request("b")]),
            )
            .unwrap();
        assert_eq!(kept, 2);
        let rows = lines(dir.path());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["seq"], 1);
        assert_eq!(rows[1]["seq"], 2);
        assert_eq!(rows[0]["token"], "tok");
        assert!(!rows.iter().any(|r| r.to_string().contains("secret text")));
    }

    #[test]
    fn keeps_codex_completed_responses_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let codex = |kind: &str| {
            json!({"attributes": [
                {"key": "event.name", "value": {"stringValue": "codex.sse_event"}},
                {"key": "event.kind", "value": {"stringValue": kind}},
            ]})
        };
        let kept = spool
            .append(
                "tok",
                &export(vec![codex("response.created"), codex("response.completed")]),
            )
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn keeps_opencode_usage_with_its_cost_and_drops_other_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let record = json!({"attributes": [
            {"key": "event.name", "value": {"stringValue": "opencode.usage"}},
            {"key": "message_id", "value": {"stringValue": "msg_1"}},
            {"key": "cost", "value": {"doubleValue": 0.5}},
            {"key": "stray", "value": {"stringValue": "secret text"}},
        ]});
        assert_eq!(spool.append("tok", &export(vec![record])).unwrap(), 1);
        let row = lines(dir.path()).remove(0).to_string();
        assert!(row.contains("msg_1") && row.contains("cost"));
        assert!(!row.contains("secret text"));
    }

    #[test]
    fn reopening_continues_the_sequence_and_keeps_the_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a")]))
            .unwrap();
        let epoch = spool.epoch().to_string();
        let mut reopened = Spool::open(dir.path()).unwrap();
        assert_eq!(reopened.epoch(), epoch);
        reopened
            .append("tok", &export(vec![api_request("b")]))
            .unwrap();
        let seqs: Vec<u64> = lines(dir.path())
            .iter()
            .map(|r| r["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn a_lost_spool_file_gets_a_new_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let first = Spool::open(dir.path()).unwrap().epoch().to_string();
        fs::remove_file(dir.path().join(SPOOL_FILE)).unwrap();
        let second = Spool::open(dir.path()).unwrap();
        assert_ne!(second.epoch(), first);
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_the_sequence_continues() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a")]))
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join(SPOOL_FILE))
            .unwrap();
        file.write_all(b"{\"seq\":2,\"tok").unwrap();
        let mut reopened = Spool::open(dir.path()).unwrap();
        reopened
            .append("tok", &export(vec![api_request("b")]))
            .unwrap();
        // The torn line is skipped by the reader; the new record is seq 2.
        let rows: Vec<Value> = fs::read_to_string(dir.path().join(SPOOL_FILE))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["seq"], 2);
    }

    #[test]
    fn a_read_returns_the_records_past_the_cursor_up_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append(
                "tok",
                &export(vec![api_request("a"), api_request("b"), api_request("c")]),
            )
            .unwrap();
        let read = read(dir.path(), 1, 1).unwrap();
        assert_eq!(read.epoch, spool.epoch());
        assert_eq!(read.oldest_seq, Some(1));
        assert_eq!(read.last_seq, 3);
        let seqs: Vec<u64> = read.records.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, vec![2]);
        assert_eq!(read.records[0].token, "tok");
        assert_eq!(
            read.records[0].record["attributes"][1]["value"]["stringValue"],
            "b"
        );
    }

    #[test]
    fn a_read_of_a_spool_not_yet_written_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let read = read(&dir.path().join("absent"), 0, 10).unwrap();
        assert_eq!(
            read,
            UsageSpoolRead {
                epoch: String::new(),
                oldest_seq: None,
                last_seq: 0,
                records: vec![]
            }
        );
    }

    #[test]
    fn a_read_skips_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a")]))
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(dir.path().join(SPOOL_FILE))
            .unwrap()
            .write_all(b"{\"seq\":2,\"tok\n")
            .unwrap();
        let read = read(dir.path(), 0, 10).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.last_seq, 1);
    }

    #[test]
    fn the_read_request_has_its_wire_shape() {
        let request = serde_json::to_value(codesense_pty_core::protocol::Request::UsageRead {
            after_seq: 5,
            limit: 10,
        })
        .unwrap();
        assert_eq!(
            request,
            json!({"op": "usage_read", "after_seq": 5, "limit": 10})
        );
    }

    #[test]
    fn a_record_keeps_only_its_usage_attributes_and_drops_the_accounts_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let mut record = api_request("a");
        for (key, value) in [
            ("user.email", "someone@example.com"),
            ("user.account_id", "acct-1"),
            ("organization.id", "org-1"),
        ] {
            record["attributes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"key": key, "value": {"stringValue": value}}));
        }
        spool.append("tok", &export(vec![record])).unwrap();
        let stored = fs::read_to_string(dir.path().join(SPOOL_FILE)).unwrap();
        assert!(stored.contains("request_id") && stored.contains("claude-opus-4-8"));
        for dropped in ["someone@example.com", "acct-1", "org-1", "user.email"] {
            assert!(!stored.contains(dropped), "{dropped} was stored");
        }
    }

    fn seqs(dir: &Path) -> Vec<u64> {
        lines(dir)
            .iter()
            .map(|r| r["seq"].as_u64().unwrap())
            .collect()
    }

    const WEEK_AND_A_DAY_MS: u64 = 8 * 24 * 60 * 60 * 1000;

    #[test]
    fn a_prune_drops_only_records_the_desktop_stored_more_than_a_week_ago() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        for id in ["a", "b", "c", "d"] {
            spool.append("tok", &export(vec![api_request(id)])).unwrap();
        }
        // Nothing stored yet: nothing goes, however old.
        assert_eq!(spool.prune(now_ms() + WEEK_AND_A_DAY_MS).unwrap(), 0);
        // The desktop stored through 2: those go once a week old, not before.
        spool.read_past(2, 10).unwrap();
        assert_eq!(spool.prune(now_ms()).unwrap(), 0);
        assert_eq!(spool.prune(now_ms() + WEEK_AND_A_DAY_MS).unwrap(), 2);
        assert_eq!(seqs(dir.path()), vec![3, 4]);
        // The desktop sees where the spool now starts.
        assert_eq!(read(dir.path(), 2, 10).unwrap().oldest_seq, Some(3));
    }

    #[test]
    fn the_newest_record_survives_a_prune_so_numbering_goes_on_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        spool
            .append("tok", &export(vec![api_request("a"), api_request("b")]))
            .unwrap();
        let epoch = spool.epoch().to_string();
        spool.read_past(2, 10).unwrap();
        assert_eq!(spool.prune(now_ms() + WEEK_AND_A_DAY_MS).unwrap(), 1);
        assert_eq!(seqs(dir.path()), vec![2]);
        let mut reopened = Spool::open(dir.path()).unwrap();
        assert_eq!(reopened.epoch(), epoch);
        reopened
            .append("tok", &export(vec![api_request("c")]))
            .unwrap();
        assert_eq!(seqs(dir.path()), vec![2, 3]);
    }

    #[test]
    fn a_spool_no_desktop_reads_is_capped_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        for id in ["a", "b", "c", "d", "e"] {
            spool.append("tok", &export(vec![api_request(id)])).unwrap();
        }
        assert_eq!(
            spool.prune_with(now_ms(), STORED_RETENTION_MS, 3).unwrap(),
            2
        );
        assert_eq!(seqs(dir.path()), vec![3, 4, 5]);
        // A desktop that had read none sees the gap.
        assert_eq!(read(dir.path(), 0, 10).unwrap().oldest_seq, Some(3));
    }

    #[test]
    fn an_oversized_record_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        let mut huge = api_request("big");
        huge["attributes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key": "model", "value": {"stringValue": "x".repeat(MAX_RECORD_BYTES)}}));
        assert_eq!(
            spool
                .append("tok", &export(vec![huge, api_request("small")]))
                .unwrap(),
            1
        );
    }

    #[test]
    fn a_failed_write_is_an_error_and_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(dir.path()).unwrap();
        // A directory where the spool file should be makes the append fail.
        spool.file = dir.path().to_path_buf();
        assert!(spool
            .append("tok", &export(vec![api_request("a")]))
            .is_err());
        assert_eq!(spool.next_seq, 1);
    }
}
