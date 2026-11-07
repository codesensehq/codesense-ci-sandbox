// SPDX-License-Identifier: MIT
//! A remote agent's outbound calls queued on its machine while
//! this app was away (`send_message`, `inbox_post`, `signal_progress`) arrive
//! on reconnect, each under an outbox id (`params._meta.codesenseOutboxId`).
//! The machine delivers until it sees an answer, so a link cut between the
//! call and its answer delivers it again: each id runs once here, across app
//! restarts too.
//!
//! The ids delivered are kept on disk (`<home>/state/remote-outbox-
//! delivered.json`) for [`TTL_SECS`], recorded before the call runs: a crash
//! between the two loses that one call rather than running it twice.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The `_meta` key a queued call arrives under.
pub(crate) const OUTBOX_META: &str = "codesenseOutboxId";
/// How long a delivered id is remembered: far longer than any machine holds
/// a queued call before its desktop returns.
const TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// The most ids remembered (the oldest go first).
const REMEMBERED: usize = 20_000;

/// Delivered ids by key, with when (unix seconds), per store file.
type Seen = BTreeMap<String, u64>;

fn stores() -> &'static Mutex<HashMap<PathBuf, Seen>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Seen>>> = OnceLock::new();
    STORES.get_or_init(Mutex::default)
}

/// Where `home`'s delivered ids are kept.
pub(crate) fn store_path(home: &Path) -> PathBuf {
    home.join("state").join("remote-outbox-delivered.json")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn load(path: &Path) -> Seen {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, seen: &Seen) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(seen)?)?;
    std::fs::rename(&tmp, path)
}

/// Whether outbox call `id` of `session` on `machine` is the first delivery
/// (run it): `false` for a redelivery (answer it without running it again).
/// Recorded at `store` (a path from [`store_path`]) before it runs; when it
/// cannot be recorded, an error: the call is not run (the machine delivers
/// it again later).
pub(crate) fn first(store: &Path, machine: &str, session: &str, id: &str) -> Result<bool, String> {
    let key = format!("{machine}\0{session}\0{id}");
    let mut stores = stores().lock().unwrap_or_else(|e| e.into_inner());
    let seen = stores
        .entry(store.to_path_buf())
        .or_insert_with(|| load(store));
    let now = now();
    seen.retain(|_, at| now.saturating_sub(*at) < TTL_SECS);
    if seen.contains_key(&key) {
        return Ok(false);
    }
    seen.insert(key.clone(), now);
    while seen.len() > REMEMBERED {
        let oldest = seen
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(oldest) => seen.remove(&oldest),
            None => break,
        };
    }
    if let Err(error) = save(store, seen) {
        seen.remove(&key);
        return Err(format!(
            "remote outbox ids not saved ({}): {error}",
            store.display()
        ));
    }
    Ok(true)
}

/// The error data type telling the machine to deliver the call again later
/// (it did not run).
pub(crate) const OUTBOX_RETRY: &str = "outbox_retry";

/// The answer when an outbox id could not be recorded: retry later.
pub(crate) fn retry_later(why: &str) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(
        format!("outbox delivery not recorded, so not run: {why}; deliver it again later"),
        Some(serde_json::json!({"type": OUTBOX_RETRY})),
    )
}

/// Forget the in-memory copies (tests: an app restart).
#[cfg(test)]
fn forget_memory() {
    stores().lock().unwrap().clear();
}

/// The outbox id a call carries, in its params' `_meta` or the request's.
pub(crate) fn outbox_id(
    params: Option<&rmcp::model::Meta>,
    request: &rmcp::model::Meta,
) -> Option<String> {
    params
        .and_then(|meta| meta.0.get(OUTBOX_META))
        .or_else(|| request.0.get(OUTBOX_META))
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .map(str::to_string)
}

/// The answer to a redelivered outbox call.
pub(crate) fn duplicate(id: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
        serde_json::json!({"status": "ok", "data": {"duplicate": true, "outboxId": id}})
            .to_string(),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    // A redelivery after a desktop restart still runs once.
    #[test]
    fn each_outbox_id_runs_once_across_an_app_restart() {
        let home = tempfile::tempdir().unwrap();
        let store = store_path(home.path());
        assert!(first(&store, "m1", "s1", "k1").unwrap());
        assert!(
            !first(&store, "m1", "s1", "k1").unwrap(),
            "a redelivery does not run again"
        );
        forget_memory();
        assert!(
            !first(&store, "m1", "s1", "k1").unwrap(),
            "nor after the app restarts"
        );
        assert!(
            first(&store, "m2", "s1", "k1").unwrap(),
            "another machine's id is its own"
        );
        assert!(
            first(&store, "m1", "s2", "k1").unwrap(),
            "so is another session's"
        );
        let mut params = rmcp::model::Meta::new();
        params.0.insert(OUTBOX_META.into(), serde_json::json!("k1"));
        assert_eq!(
            outbox_id(Some(&params), &rmcp::model::Meta::new()).as_deref(),
            Some("k1")
        );
        assert_eq!(outbox_id(None, &params).as_deref(), Some("k1"));
        assert_eq!(outbox_id(None, &rmcp::model::Meta::new()), None);
        let answer = serde_json::to_value(duplicate("k1")).unwrap();
        assert!(answer["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"duplicate\":true"));
    }

    #[test]
    fn expired_ids_are_forgotten() {
        let home = tempfile::tempdir().unwrap();
        let store = store_path(home.path());
        let mut old = Seen::new();
        old.insert("m\0s\0stale".into(), now() - TTL_SECS - 1);
        save(&store, &old).unwrap();
        forget_memory();
        assert!(
            first(&store, "m", "s", "stale").unwrap(),
            "past its TTL: runs"
        );
    }

    // An id that cannot be recorded is a typed, retryable error,
    // and the call is not counted as delivered.
    #[test]
    fn an_unrecordable_id_is_a_retryable_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("state"), b"not a directory").unwrap();
        let store = store_path(home.path());
        assert!(first(&store, "m", "s", "k").is_err());
        assert!(
            first(&store, "m", "s", "k").is_err(),
            "still not recorded: not a duplicate"
        );
        let error = retry_later("disk full");
        assert_eq!(error.data.unwrap()["type"], OUTBOX_RETRY);
    }
}
