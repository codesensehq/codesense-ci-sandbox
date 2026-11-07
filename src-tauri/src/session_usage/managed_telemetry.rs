// SPDX-License-Identifier: MIT
//! Whether this Mac's managed Claude settings send Claude's usage logs somewhere CodeSense can't
//! read them. Claude Code applies a managed-settings `env` block over the launch environment, so
//! the endpoint [`super::telemetry::configure_export`] sets loses to it. Only key names are kept:
//! a value (an endpoint, a header, a token) is never stored, shown or logged.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use codesense_pty_core::agent_cli::AgentCli;
use serde_json::Value;

/// Why a Claude session's usage is missing when its logs go elsewhere.
pub(crate) const USAGE_REDIRECTED: &str = "usage not recorded: this Mac's managed Claude settings send session telemetry to your organization's collector";

/// The keys that decide where Claude's logs go, in what format, or whether they are sent, each
/// with the one value that keeps them reaching CodeSense (`None`: any value redirects them).
/// Only keys that override what CodeSense sets count: the generic `OTEL_EXPORTER_OTLP_ENDPOINT`
/// and `OTEL_EXPORTER_OTLP_PROTOCOL` lose to the logs-specific ones CodeSense also sets, so they
/// redirect nothing. Metrics-only and traces-only keys change nothing CodeSense reads.
const LOG_KEYS: [(&str, Option<&str>); 5] = [
    ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", None),
    ("OTEL_LOGS_EXPORTER", Some("otlp")),
    // The local ingest reads JSON only.
    ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", Some("http/json")),
    ("OTEL_SDK_DISABLED", Some("false")),
    ("CLAUDE_CODE_ENABLE_TELEMETRY", Some("1")),
];

/// A managed-settings file is a few KB; anything this large is not one.
const MAX_BYTES: u64 = 1024 * 1024;

/// A managed-settings file that redirects Claude's usage logs: where it is and which keys do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    pub path: PathBuf,
    pub keys: Vec<&'static str>,
}

/// What a file said when it had this mtime and length.
struct Seen {
    modified: Option<SystemTime>,
    len: u64,
    keys: Vec<&'static str>,
}

static SEEN: LazyLock<Mutex<HashMap<PathBuf, Seen>>> = LazyLock::new(Default::default);

#[cfg(test)]
thread_local! {
    /// The paths a test points the detector at. Unset, tests read no file at all.
    static PATHS: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
    /// Set by a test of a CLI whose usage CodeSense cannot read from its session log.
    static NO_SESSION_LOG_USAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn paths() -> Vec<PathBuf> {
    #[cfg(test)]
    let paths = PATHS.with(|paths| paths.borrow().clone()).unwrap_or_default();
    #[cfg(not(test))]
    let paths = AgentCli::ALL
        .into_iter()
        .flat_map(|cli| crate::agents::adapter::desktop(cli).managed_settings)
        .map(PathBuf::from)
        .collect();
    paths
}

/// The first managed-settings file that redirects Claude's usage logs, or `None`. A file is read
/// again only when its mtime or length changes; an absent or malformed one redirects nothing.
pub(crate) fn redirect() -> Option<Redirect> {
    paths().into_iter().find_map(redirect_at)
}

/// Whether managed settings can send `runtime`'s usage past CodeSense: its adapter names where it
/// reads them, and CodeSense cannot read its usage from its session log instead.
pub(crate) fn redirectable(runtime: AgentCli) -> bool {
    managed(runtime) && session_log_usage(runtime).is_none()
}

/// Whether `runtime`'s adapter names where it reads managed settings.
fn managed(runtime: AgentCli) -> bool {
    !crate::agents::adapter::desktop(runtime).managed_settings.is_empty()
}

/// Whether a session of `runtime` on this Mac has its usage sent elsewhere.
pub(crate) fn unrecorded(runtime: AgentCli) -> bool {
    redirectable(runtime) && redirect().is_some()
}

/// [`USAGE_REDIRECTED`] while a managed-settings file sends some CLI's usage past CodeSense.
pub(crate) fn usage_reason() -> Option<&'static str> {
    (AgentCli::ALL.into_iter().any(redirectable) && redirect().is_some()).then_some(USAGE_REDIRECTED)
}

/// How to read a `runtime` session's usage from its own session log, while a managed-settings file
/// sends that runtime's usage logs elsewhere; `None` otherwise, when the logs reach CodeSense.
pub(crate) fn session_log_reader(runtime: AgentCli) -> Option<fn(&Path) -> Vec<super::event::UsageEvent>> {
    let reader = session_log_usage(runtime)?;
    (managed(runtime) && redirect().is_some()).then_some(reader)
}

fn session_log_usage(runtime: AgentCli) -> Option<fn(&Path) -> Vec<super::event::UsageEvent>> {
    #[cfg(test)]
    if NO_SESSION_LOG_USAGE.with(std::cell::Cell::get) {
        return None;
    }
    crate::agents::adapter::desktop(runtime).session_log_usage
}

fn seen() -> MutexGuard<'static, HashMap<PathBuf, Seen>> {
    SEEN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn redirect_at(path: PathBuf) -> Option<Redirect> {
    let Some(meta) = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file) else {
        seen().remove(&path);
        return None;
    };
    let (modified, len) = (meta.modified().ok(), meta.len());
    let known = seen()
        .get(&path)
        .filter(|known| known.modified == modified && known.len == len)
        .map(|known| known.keys.clone());
    if let Some(keys) = known {
        return (!keys.is_empty()).then_some(Redirect { path, keys });
    }
    // Read with the cache unlocked: another caller may read the same file meanwhile, and the
    // later insert of the same keys wins harmlessly.
    let keys = read_keys(&path);
    seen().insert(path.clone(), Seen { modified, len, keys: keys.clone() });
    let redirect = (!keys.is_empty()).then_some(Redirect { path, keys })?;
    // Key names only: their values can hold credentials.
    log::info!(target: "session_usage", "{} sets {}: Claude session usage is not recorded on this Mac", redirect.path.display(), redirect.keys.join(", "));
    Some(redirect)
}

/// The file is read to at most one byte past [`MAX_BYTES`], so one that grew since its metadata
/// was read is still refused rather than read whole.
fn read_contents(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
}

fn read_keys(path: &Path) -> Vec<&'static str> {
    let parsed = read_contents(path).and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(settings) = parsed else {
        // The file's content is never logged: it can hold credentials.
        log::warn!(target: "session_usage", "{} is not readable JSON; its telemetry settings are ignored", path.display());
        return Vec::new();
    };
    let Some(env) = settings.get("env").and_then(Value::as_object) else {
        return Vec::new();
    };
    LOG_KEYS
        .iter()
        .filter(|(key, keeps)| {
            let value = match env.get(*key) {
                None | Some(Value::Null) => return false,
                Some(Value::String(text)) => text.trim().to_owned(),
                Some(other) => other.to_string(),
            };
            !keeps.is_some_and(|keeps| value.eq_ignore_ascii_case(keeps))
        })
        .map(|(key, _)| *key)
        .collect()
}

/// A managed-settings file for one test, which the detector reads on this thread until it drops.
#[cfg(test)]
pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
    previous: Option<Vec<PathBuf>>,
}

#[cfg(test)]
impl Fixture {
    pub(crate) fn new(contents: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed-settings.json");
        std::fs::write(&path, contents).unwrap();
        let previous = PATHS.with(|paths| paths.borrow_mut().replace(vec![path.clone()]));
        Self { _dir: dir, path, previous }
    }

    /// A file that sends Claude's logs to an organization's collector.
    pub(crate) fn redirecting() -> Self {
        Self::new(r#"{"env":{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://collector.example.com/v1/logs"}}"#)
    }

    /// [`Self::redirecting`], for a CLI whose usage CodeSense cannot read from its session log, so
    /// its usage goes unrecorded.
    pub(crate) fn redirecting_unread() -> Self {
        NO_SESSION_LOG_USAGE.with(|off| off.set(true));
        Self::redirecting()
    }
}

#[cfg(test)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let previous = self.previous.take();
        PATHS.with(|paths| *paths.borrow_mut() = previous);
        NO_SESSION_LOG_USAGE.with(|off| off.set(false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(contents: &str) -> Option<Vec<&'static str>> {
        let _fixture = Fixture::new(contents);
        redirect().map(|r| r.keys)
    }

    #[test]
    fn each_log_key_redirects_and_is_named() {
        for (env, key) in [
            (r#"{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://collector.example.com/v1/logs"}"#, "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
            (r#"{"OTEL_LOGS_EXPORTER":"none"}"#, "OTEL_LOGS_EXPORTER"),
            (r#"{"OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"grpc"}"#, "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL"),
            (r#"{"OTEL_SDK_DISABLED":"true"}"#, "OTEL_SDK_DISABLED"),
            (r#"{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}"#, "CLAUDE_CODE_ENABLE_TELEMETRY"),
            (r#"{"CLAUDE_CODE_ENABLE_TELEMETRY":0}"#, "CLAUDE_CODE_ENABLE_TELEMETRY"),
        ] {
            assert_eq!(keys(&format!(r#"{{"env":{env}}}"#)), Some(vec![key]), "{env}");
        }
    }

    #[test]
    fn settings_that_leave_the_logs_alone_redirect_nothing() {
        for contents in [
            r#"{}"#,
            r#"{"permissions":{"deny":["WebFetch"]}}"#,
            r#"{"env":"not an object"}"#,
            r#"{"env":{"OTEL_METRICS_EXPORTER":"otlp","OTEL_EXPORTER_OTLP_METRICS_ENDPOINT":"https://collector.example.com","OTEL_EXPORTER_OTLP_METRICS_PROTOCOL":"http/protobuf","OTEL_TRACES_EXPORTER":"otlp","OTEL_EXPORTER_OTLP_TRACES_ENDPOINT":"https://collector.example.com"}}"#,
            r#"{"env":{"OTEL_LOGS_EXPORTER":"otlp","CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"http/json","OTEL_SDK_DISABLED":"false"}}"#,
            // The generic endpoint and protocol lose to the logs-specific ones CodeSense sets.
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://collector.example.com","OTEL_EXPORTER_OTLP_PROTOCOL":"grpc"}}"#,
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":1,"OTEL_SDK_DISABLED":false,"OTEL_LOGS_EXPORTER":null}}"#,
            "{ not json",
        ] {
            assert_eq!(keys(contents), None, "{contents}");
        }
    }

    #[test]
    fn no_file_and_no_paths_redirect_nothing() {
        assert_eq!(redirect(), None, "a test with no fixture reads no file");
        let fixture = Fixture::redirecting();
        std::fs::remove_file(&fixture.path).unwrap();
        assert_eq!(redirect(), None);
    }

    #[test]
    fn a_directory_or_an_oversized_file_redirects_nothing() {
        let fixture = Fixture::redirecting();
        std::fs::remove_file(&fixture.path).unwrap();
        std::fs::create_dir(&fixture.path).unwrap();
        assert_eq!(redirect(), None, "a directory at the path");
        std::fs::remove_dir(&fixture.path).unwrap();
        let redirecting = r#"{"env":{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://collector.example.com/v1/logs"}}"#;
        let padding = " ".repeat(MAX_BYTES as usize + 1 - redirecting.len());
        std::fs::write(&fixture.path, format!("{redirecting}{padding}")).unwrap();
        assert_eq!(redirect(), None, "one byte over the limit");
        std::fs::write(&fixture.path, format!("{redirecting}{}", &padding[1..])).unwrap();
        assert!(redirect().is_some(), "at the limit");
    }

    #[test]
    fn a_change_or_removal_is_seen_on_the_next_read() {
        let fixture = Fixture::new(r#"{"env":{"OTEL_LOGS_EXPORTER":"otlp"}}"#);
        assert_eq!(redirect(), None);
        std::fs::write(&fixture.path, r#"{"env":{"OTEL_LOGS_EXPORTER":"otlp","OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://collector.example.com/v1/logs"}}"#).unwrap();
        assert_eq!(
            redirect(),
            Some(Redirect { path: fixture.path.clone(), keys: vec!["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"] })
        );
        std::fs::remove_file(&fixture.path).unwrap();
        assert_eq!(redirect(), None);
    }

    #[test]
    fn only_claude_sessions_are_unrecorded() {
        let _fixture = Fixture::redirecting_unread();
        assert!(unrecorded(AgentCli::Claude));
        assert!(!unrecorded(AgentCli::Codex));
        assert_eq!(usage_reason(), Some(USAGE_REDIRECTED));
    }

    #[test]
    fn only_the_claude_adapter_names_managed_settings() {
        let _fixture = Fixture::redirecting_unread();
        assert!(redirectable(AgentCli::Claude));
        assert_eq!(
            crate::agents::adapter::desktop(AgentCli::Claude).managed_settings,
            [
                "/Library/Application Support/ClaudeCode/managed-settings.json",
                "/etc/claude-code/managed-settings.json",
            ]
        );
        for cli in AgentCli::ALL.into_iter().filter(|cli| *cli != AgentCli::Claude) {
            assert!(crate::agents::adapter::desktop(cli).managed_settings.is_empty(), "{cli:?}");
            assert!(!redirectable(cli), "{cli:?}");
            assert!(!unrecorded(cli), "{cli:?}");
        }
    }

    #[test]
    fn claude_usage_is_read_from_its_session_log_only_under_a_redirect() {
        assert!(session_log_reader(AgentCli::Claude).is_none(), "no managed settings");
        let fixture = Fixture::redirecting();
        assert!(session_log_reader(AgentCli::Claude).is_some());
        for cli in AgentCli::ALL.into_iter().filter(|cli| *cli != AgentCli::Claude) {
            assert!(session_log_reader(cli).is_none(), "{cli:?}");
        }
        // Read from its log, Claude's usage is recorded: nothing says otherwise.
        assert!(!unrecorded(AgentCli::Claude));
        assert_eq!(usage_reason(), None);
        drop(fixture);
        let _unread = Fixture::redirecting_unread();
        assert!(session_log_reader(AgentCli::Claude).is_none());
        assert!(unrecorded(AgentCli::Claude));
    }
}
