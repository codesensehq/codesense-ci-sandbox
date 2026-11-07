// SPDX-License-Identifier: MIT
//! The durable session record: the facts of a launched session that otherwise live only in memory
//! or in a launch entry that is cleared afterwards. One row per CodeSense session id, in the usage
//! store, so every later question (line → session, story → changes, run → cost) joins here.
//!
//! The table is additive (no schema version bump), like `session_story`. Credentials never enter
//! it: there is no env or argv column, the account is limited to provider, id, label and source,
//! and every free-text value passes through `redact_secrets`.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{db_err, UsageStore};
use crate::shared::secret_scan::redact_secrets;

mod readiness;
#[cfg(test)]
pub(crate) use readiness::wait_for_record_waiter;
pub(crate) use readiness::{begin_record_write, wait_for_record};

/// Persist and publish completion from the same blocking operation.
pub(crate) fn persist_record_in(
    path: &std::path::Path,
    record: &SessionRecord,
) -> Result<(), String> {
    let pending = begin_record_write(path, &record.session_id);
    let result = UsageStore::open(path)?.write_session_record(record);
    pending.finish(result.is_ok());
    result
}

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_record (
    session_id        TEXT PRIMARY KEY,
    native_session_id TEXT,
    org               TEXT,
    team              TEXT,
    handle            TEXT,
    launched_by       TEXT,
    agent             TEXT,
    manifest_hash     TEXT,
    cli               TEXT,
    cli_version       TEXT,
    model             TEXT,
    mcp_allow         TEXT,
    bridge_tools      TEXT,
    account           TEXT,
    story_id          TEXT,
    workspace         TEXT,
    worktree          TEXT,
    branch            TEXT,
    run_id            TEXT,
    config_home       TEXT,
    provided_skills   TEXT,
    unavailable       TEXT NOT NULL DEFAULT '{}',
    recorded_at_ms    INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS session_record_by_native
    ON session_record (native_session_id) WHERE native_session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS session_record_by_run ON session_record (run_id) WHERE run_id IS NOT NULL;
";

/// The account a session ran as. Never a credential: only who it was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordAccount {
    pub provider: Option<String>,
    pub account_id: Option<String>,
    pub label: Option<String>,
    pub source: Option<String>,
}

/// One session's record. A fact the launch could not know is `None` and has a reason in
/// `unavailable`; nothing is guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub session_id: String,
    pub native_session_id: Option<String>,
    pub org: Option<String>,
    pub team: Option<String>,
    pub handle: Option<String>,
    pub launched_by: Option<String>,
    /// `key@revision`.
    pub agent: Option<String>,
    pub manifest_hash: Option<String>,
    pub cli: Option<String>,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub mcp_allow: Option<Vec<String>>,
    pub bridge_tools: Option<Vec<String>>,
    pub account: Option<RecordAccount>,
    pub story_id: Option<String>,
    pub workspace: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub run_id: Option<String>,
    /// The CLI's own home when the launch ran it (an account's `CLAUDE_CONFIG_DIR` or `CODEX_HOME`,
    /// else the default), where its session log lives.
    pub config_home: Option<String>,
    /// The skill names the session's startup manifest provides.
    pub provided_skills: Option<Vec<String>>,
    /// `{field: reason}` for each fact above that is `None`.
    pub unavailable: BTreeMap<String, String>,
    pub recorded_at_ms: i64,
    /// Derived from the session's turns when it is read; never written with the record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<super::timing::SessionTiming>,
}

impl SessionRecord {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            recorded_at_ms: chrono::Utc::now().timestamp_millis(),
            ..Self::default()
        }
    }

    /// Say why `field` is not known.
    pub fn unavailable(&mut self, field: &str, reason: &str) {
        self.unavailable
            .insert(field.to_string(), reason.to_string());
    }

    /// Give every fact that is still `None` and has no reason the reason `reason`.
    pub fn missing_as(&mut self, reason: &str) {
        let missing: Vec<&str> = [
            ("nativeSessionId", self.native_session_id.is_none()),
            ("org", self.org.is_none()),
            ("team", self.team.is_none()),
            ("handle", self.handle.is_none()),
            ("launchedBy", self.launched_by.is_none()),
            ("agent", self.agent.is_none()),
            ("manifestHash", self.manifest_hash.is_none()),
            ("cli", self.cli.is_none()),
            ("cliVersion", self.cli_version.is_none()),
            ("model", self.model.is_none()),
            ("mcpAllow", self.mcp_allow.is_none()),
            ("bridgeTools", self.bridge_tools.is_none()),
            ("account", self.account.is_none()),
            ("storyId", self.story_id.is_none()),
            ("workspace", self.workspace.is_none()),
            ("worktree", self.worktree.is_none()),
            ("branch", self.branch.is_none()),
            ("runId", self.run_id.is_none()),
            ("configHome", self.config_home.is_none()),
            ("providedSkills", self.provided_skills.is_none()),
        ]
        .into_iter()
        .filter_map(|(field, none)| none.then_some(field))
        .collect();
        for field in missing {
            self.unavailable
                .entry(field.to_string())
                .or_insert_with(|| reason.to_string());
        }
    }
}

fn clean(value: &Option<String>) -> Option<String> {
    value.as_deref().map(redact_secrets)
}

fn clean_list(value: &Option<Vec<String>>) -> Option<String> {
    value.as_ref().map(|items| {
        serde_json::to_string(&items.iter().map(|i| redact_secrets(i)).collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".into())
    })
}

fn clean_account(value: &Option<RecordAccount>) -> Option<String> {
    value.as_ref().and_then(|a| {
        let a = RecordAccount {
            provider: clean(&a.provider),
            account_id: clean(&a.account_id),
            label: clean(&a.label),
            source: clean(&a.source),
        };
        serde_json::to_string(&a).ok()
    })
}

fn list_of(text: Option<String>) -> Option<Vec<String>> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

#[cfg(test)]
thread_local! {
    static PAUSE_BEFORE_NATIVE_BACKFILL: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

impl UsageStore {
    /// Write a session's record. A second write for the same session replaces what the launch
    /// knew but keeps a native id already linked.
    pub fn write_session_record(&self, r: &SessionRecord) -> Result<(), String> {
        let unavailable = serde_json::to_string(
            &r.unavailable
                .iter()
                .map(|(k, v)| (k.clone(), redact_secrets(v)))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap_or_else(|_| "{}".into());
        // Publish the launch facts and any native id already exported as one durable record.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx
            .execute(
                "INSERT INTO session_record (session_id, native_session_id, org, team, handle, launched_by, agent,
                    manifest_hash, cli, cli_version, model, mcp_allow, bridge_tools, account, story_id, workspace,
                    worktree, branch, run_id, config_home, provided_skills, unavailable, recorded_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
                 ON CONFLICT (session_id) DO UPDATE SET
                    native_session_id = COALESCE(session_record.native_session_id, excluded.native_session_id),
                    org = excluded.org, team = excluded.team, handle = excluded.handle,
                    launched_by = excluded.launched_by, agent = excluded.agent, manifest_hash = excluded.manifest_hash,
                    cli = excluded.cli, cli_version = excluded.cli_version, model = excluded.model,
                    mcp_allow = excluded.mcp_allow, bridge_tools = excluded.bridge_tools, account = excluded.account,
                    story_id = excluded.story_id, workspace = excluded.workspace, worktree = excluded.worktree,
                    branch = excluded.branch, run_id = excluded.run_id, config_home = excluded.config_home,
                    provided_skills = excluded.provided_skills, unavailable = excluded.unavailable",
                params![
                    r.session_id,
                    clean(&r.native_session_id),
                    clean(&r.org),
                    clean(&r.team),
                    clean(&r.handle),
                    clean(&r.launched_by),
                    clean(&r.agent),
                    clean(&r.manifest_hash),
                    clean(&r.cli),
                    clean(&r.cli_version),
                    clean(&r.model),
                    clean_list(&r.mcp_allow),
                    clean_list(&r.bridge_tools),
                    clean_account(&r.account),
                    clean(&r.story_id),
                    clean(&r.workspace),
                    clean(&r.worktree),
                    clean(&r.branch),
                    clean(&r.run_id),
                    clean(&r.config_home),
                    clean_list(&r.provided_skills),
                    unavailable,
                    r.recorded_at_ms,
                ],
            )
            .map_err(db_err)?;
        #[cfg(test)]
        PAUSE_BEFORE_NATIVE_BACKFILL.with(|pause| {
            if let Some(pause) = pause.borrow().as_ref() {
                pause();
            }
        });
        // The launch's first export can arrive before its record is written, and then it had no
        // record to link to. Take the native id that export already attributed to this launch.
        tx.execute(
            "UPDATE OR IGNORE session_record
                 SET native_session_id = (
                        SELECT a.session_id FROM session_attribution a
                        WHERE a.launch_id = session_record.session_id
                        ORDER BY a.attributed_at_ms LIMIT 1),
                     unavailable = json_remove(unavailable, '$.nativeSessionId')
                 WHERE session_id = ?1 AND native_session_id IS NULL
                   AND EXISTS (SELECT 1 FROM session_attribution a WHERE a.launch_id = ?1)",
            params![r.session_id],
        )
        .map_err(db_err)?;
        rusqlite::Transaction::commit(tx).map_err(db_err)
    }

    /// Write a record only when the session has none (a hand-started CLI's first hook).
    /// Returns whether it wrote.
    pub fn write_session_record_if_absent(&self, r: &SessionRecord) -> Result<bool, String> {
        if self.read_session_record(&r.session_id)?.is_some() {
            return Ok(false);
        }
        self.write_session_record(r)?;
        Ok(true)
    }

    /// Link the CLI's own session id to the record of the CodeSense session `session_id`. The
    /// first link stands, and an id another record already holds is not taken twice.
    pub fn link_native_session(&self, session_id: &str, native: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE OR IGNORE session_record
                 SET native_session_id = ?2, unavailable = json_remove(unavailable, '$.nativeSessionId')
                 WHERE session_id = ?1 AND native_session_id IS NULL",
                params![session_id, redact_secrets(native)],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// The record of `session_id`, by its CodeSense id or its native id.
    pub fn read_session_record(&self, session_id: &str) -> Result<Option<SessionRecord>, String> {
        self.conn
            .query_row(
                "SELECT session_id, native_session_id, org, team, handle, launched_by, agent, manifest_hash, cli,
                    cli_version, model, mcp_allow, bridge_tools, account, story_id, workspace, worktree, branch,
                    run_id, config_home, provided_skills, unavailable, recorded_at_ms
                 FROM session_record WHERE session_id = ?1 OR native_session_id = ?1
                 ORDER BY session_id = ?1 DESC LIMIT 1",
                params![session_id],
                |r| {
                    let account: Option<String> = r.get(13)?;
                    let unavailable: String = r.get(21)?;
                    Ok(SessionRecord {
                        session_id: r.get(0)?,
                        native_session_id: r.get(1)?,
                        org: r.get(2)?,
                        team: r.get(3)?,
                        handle: r.get(4)?,
                        launched_by: r.get(5)?,
                        agent: r.get(6)?,
                        manifest_hash: r.get(7)?,
                        cli: r.get(8)?,
                        cli_version: r.get(9)?,
                        model: r.get(10)?,
                        mcp_allow: list_of(r.get(11)?),
                        bridge_tools: list_of(r.get(12)?),
                        account: account.and_then(|a| serde_json::from_str(&a).ok()),
                        story_id: r.get(14)?,
                        workspace: r.get(15)?,
                        worktree: r.get(16)?,
                        branch: r.get(17)?,
                        run_id: r.get(18)?,
                        config_home: r.get(19)?,
                        provided_skills: list_of(r.get(20)?),
                        unavailable: serde_json::from_str(&unavailable).unwrap_or_default(),
                        recorded_at_ms: r.get(22)?,
                        timing: None,
                    })
                },
            )
            .optional()
            .map_err(db_err)?
            .map(|mut record| {
                record.timing = self.read_session_timing(&record.session_id)?;
                Ok::<_, String>(record)
            })
            .transpose()
    }
}

/// The value after `flag` in `args`, for `--flag value` and `--flag=value`: the one reading of
/// a CLI option every consumer of a launch's args shares.
pub fn argv_option<'a>(args: &'a [String], flags: &[&str]) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some((flag, value)) = arg.split_once('=') {
            if flags.contains(&flag) {
                return Some(value).filter(|v| !v.is_empty());
            }
        }
        if flags.contains(&arg.as_str()) {
            return it.next().map(String::as_str).filter(|v| !v.is_empty());
        }
    }
    None
}

/// The model a launch's args name (`--model`, `--model=` or `-m`); `None` is the CLI's default.
pub fn requested_model(args: &[String]) -> Option<&str> {
    argv_option(args, &["--model", "-m"])
}

/// What trusted preparation knew about a launch before it ran: the one value every launch path
/// (MCP, renderer, headless) fills and hands to the one writer, so the paths cannot record
/// different things. No env values and no raw argv: the args are read for the model and the
/// CLI's session id and dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchFacts {
    pub cli: Option<String>,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub native_session_id: Option<String>,
    pub agent: Option<String>,
    pub manifest_hash: Option<String>,
    pub mcp_allow: Option<Vec<String>>,
    pub bridge_tools: Option<Vec<String>>,
    pub workspace: Option<String>,
    /// What the task record is made from (see [`super::task_capture`]).
    pub task: super::task_capture::TaskInputs,
    pub config_home: Option<String>,
    /// The home the launch's own environment gives the CLI (Codex's per-session scoped home), which
    /// wins over an account's home and the default.
    pub scope_home: Option<String>,
    pub provided_skills: Option<Vec<String>>,
}

impl LaunchFacts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cli: codesense_pty_core::agent_cli::AgentCli,
        cli_version: Option<&str>,
        args: &[String],
        mcp_allow: Option<Vec<String>>,
        bridge_tools: Vec<String>,
        agent: String,
        manifest_hash: String,
        workspace: &str,
    ) -> Self {
        Self {
            cli: Some(cli.as_str().to_string()),
            cli_version: cli_version.map(str::to_owned),
            model: match crate::agents::adapter::desktop(cli).model_endpoint {
                Some(port) => (port.model_arguments)(args).model,
                None => requested_model(args).map(str::to_owned),
            },
            native_session_id: argv_option(args, &["--session-id"]).map(str::to_owned),
            agent: Some(agent),
            manifest_hash: Some(manifest_hash),
            mcp_allow,
            bridge_tools: Some(bridge_tools),
            workspace: Some(workspace.to_string()),
            // Where the CLI keeps its files unless an account relocates them.
            config_home: codesense_pty_core::agent_cli::descriptor(cli)
                .native_home(|key| std::env::var_os(key))
                .map(|home| home.to_string_lossy().into_owned()),
            scope_home: None,
            provided_skills: None,
            task: Default::default(),
        }
    }

    /// Add what the task record is made from.
    #[must_use]
    pub fn with_task(mut self, task: super::task_capture::TaskInputs) -> Self {
        self.task = task;
        self
    }

    /// Note the home the launch's own environment gives the CLI. The agent's saved variables
    /// (`agent_env`) replace the default home; the variables the launch adds (`scope_env`: a scoped
    /// Codex launch runs in a home made for the session) win over both and over an account's home.
    pub fn with_launch_env(
        mut self,
        cli: codesense_pty_core::agent_cli::AgentCli,
        agent_env: &std::collections::BTreeMap<String, String>,
        scope_env: &[(String, String)],
    ) -> Self {
        let descriptor = codesense_pty_core::agent_cli::descriptor(cli);
        let home_env = &descriptor.home_env;
        if let Some(home) = descriptor.native_home(|key| {
            agent_env
                .get(key)
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os(key))
        }) {
            self.config_home = Some(home.to_string_lossy().into_owned());
        }
        self.scope_home = scope_env
            .iter()
            .rev()
            .find(|(key, _)| key == home_env)
            .map(|(_, home)| home.clone());
        self
    }

    /// The skill names the launch's startup manifest provides.
    pub fn with_provided_skills(mut self, skills: Vec<String>) -> Self {
        self.provided_skills = Some(skills);
        self
    }
}

/// The non-empty lines git prints for `args` run in `dir`; `None` when git fails.
pub(super) fn git_lines(dir: &std::path::Path, args: &[&str]) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// The first line git prints for `args` run in `dir`; `None` when git fails or prints nothing.
fn git_line(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    git_lines(dir, args)?.into_iter().next()
}

/// The branch checked out in `dir`, read from git; `None` for a detached HEAD or no repository.
pub fn current_branch(dir: &std::path::Path) -> Option<String> {
    git_line(dir, &["symbolic-ref", "--short", "-q", "HEAD"])
}

/// The commit `dir`'s repository is at; `None` for no repository or no commit.
pub fn current_head(dir: &std::path::Path) -> Option<String> {
    git_line(dir, &["rev-parse", "HEAD"])
}

/// The record of a CLI started by hand in a CodeSense tab, written on its first hook event.
/// Only what the tab and the hook say is filled in; every other fact says why it is unknown.
pub fn hand_started(
    tab_session: &str,
    native: Option<&str>,
    org: Option<&str>,
    team: Option<&str>,
) -> SessionRecord {
    let mut r = SessionRecord::new(tab_session);
    r.native_session_id = native.filter(|n| !n.is_empty()).map(str::to_owned);
    r.cli = Some(codesense_pty_core::agent_cli::project_hook_cli().to_string());
    r.org = org.map(str::to_owned);
    r.team = team.map(str::to_owned);
    if r.org.is_none() {
        r.unavailable("org", "the tab is not in a managed workspace");
    }
    if r.team.is_none() {
        r.unavailable("team", "the tab is not in a managed workspace");
    }
    r.missing_as("started by hand: CodeSense did not launch this CLI");
    r
}

/// The record of a session, for a first hook event: written once per tab session.
pub fn record_hand_started_async(record: SessionRecord) -> impl std::future::Future<Output = ()> {
    let path = super::store_path();
    let pending = path
        .as_ref()
        .ok()
        .map(|p| begin_record_write(p, &record.session_id));
    async move {
        let result = tokio::task::spawn_blocking(move || {
            let path = path?;
            let result = UsageStore::open(&path)?.write_session_record_if_absent(&record);
            if let Some(pending) = pending {
                pending.finish(result.is_ok());
            }
            result
        })
        .await;
        match result {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                log::warn!(target: "session_usage", "hand-started record not written: {e}")
            }
            Err(e) => log::warn!(target: "session_usage", "hand-started record task failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::tests::store;

    #[test]
    fn hermes_launch_facts_use_the_native_model_fact_for_every_alias() {
        use codesense_pty_core::agent_cli::AgentCli;
        for alias in ["--mo=second", "-msecond", "-vQmsecond"] {
            let args = vec!["--model=first".into(), alias.into()];
            let facts = LaunchFacts::new(AgentCli::Hermes, None, &args, None, Vec::new(), "agent".into(), "manifest".into(), ".");
            assert_eq!(facts.model.as_deref(), Some("second"));
            let other = LaunchFacts::new(AgentCli::Claude, None, &args, None, Vec::new(), "agent".into(), "manifest".into(), ".");
            assert_eq!(other.model.as_deref(), Some("first"));
        }
    }

    fn claude() -> SessionRecord {
        let mut r = SessionRecord::new("tab-1");
        r.native_session_id = Some("11111111-2222-3333-4444-555555555555".into());
        r.org = Some("acme".into());
        r.team = Some("riverside".into());
        r.handle = Some("SV-1".into());
        r.launched_by = Some("codesense".into());
        r.agent = Some("acme/riverside/dev@3".into());
        r.manifest_hash = Some("abc123".into());
        r.cli = Some("claude".into());
        r.cli_version = Some("2.5.0".into());
        r.model = Some("claude-opus-5-5".into());
        r.mcp_allow = Some(vec!["codesense".into()]);
        r.bridge_tools = Some(vec!["send_message".into(), "signal_done".into()]);
        r.account = Some(RecordAccount {
            provider: Some("anthropic".into()),
            account_id: Some("acct-1".into()),
            label: Some("work".into()),
            source: Some("explicit".into()),
        });
        r.story_id = Some("SV-1".into());
        r.workspace = Some("/work/ws".into());
        r.worktree = Some("/work/wt".into());
        r.branch = Some("feat/SV-1".into());
        r.run_id = Some("agent-launch:r1".into());
        r
    }

    #[test]
    fn a_claude_record_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let record = claude();
        UsageStore::open(&path)
            .unwrap()
            .write_session_record(&record)
            .unwrap();
        let back = UsageStore::open(&path)
            .unwrap()
            .read_session_record("tab-1")
            .unwrap();
        assert_eq!(back, Some(record));
    }

    #[test]
    fn a_codex_record_gets_its_native_id_when_telemetry_links_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let mut record = SessionRecord::new("tab-2");
        record.cli = Some("codex".into());
        record.agent = Some("acme/riverside/dev@1".into());
        record.unavailable(
            "nativeSessionId",
            "Codex reports its conversation id with its first telemetry",
        );
        UsageStore::open(&path)
            .unwrap()
            .write_session_record(&record)
            .unwrap();
        // The launch registered its owner under its session id; the first export names the CLI's own id.
        let mut store = UsageStore::open(&path).unwrap();
        store
            .register_launch(
                "tab-2",
                "acme",
                Some("riverside"),
                "acme/riverside/dev@1",
                "SV-2",
                None,
            )
            .unwrap();
        crate::session_usage::ingest_telemetry_for_at(
            &path,
            "tab-2",
            &serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{"attributes":[
                {"key":"event.name","value":{"stringValue":"codex.sse_event"}},
                {"key":"event.kind","value":{"stringValue":"response.completed"}},
                {"key":"conversation.id","value":{"stringValue":"codex-conv-9"}},
                {"key":"event.timestamp","value":{"stringValue":"2026-10-22T18:00:00Z"}},
                {"key":"model","value":{"stringValue":"gpt-6"}},
                {"key":"input_token_count","value":{"intValue":"10"}},
                {"key":"output_token_count","value":{"intValue":"5"}}
            ]}]}]}]}),
        )
        .unwrap();
        let _ = &mut store;
        let back = UsageStore::open(&path)
            .unwrap()
            .read_session_record("tab-2")
            .unwrap()
            .unwrap();
        assert_eq!(back.native_session_id.as_deref(), Some("codex-conv-9"));
        // The same record is found by the CLI's own id.
        let by_native = UsageStore::open(&path)
            .unwrap()
            .read_session_record("codex-conv-9")
            .unwrap()
            .unwrap();
        assert_eq!(by_native.session_id, "tab-2");
    }

    #[test]
    fn the_record_table_is_added_without_changing_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let store = UsageStore::open(&path).unwrap();
            store
                .conn
                .execute_batch("DROP TABLE session_record;")
                .unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(
            crate::session_usage::has_column(&store.conn, "session_record", "manifest_hash")
                .unwrap()
        );
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "an older build must still open this store");
    }

    #[test]
    fn a_hand_started_cli_says_why_each_fact_is_unavailable() {
        let (_d, store) = store();
        let r = hand_started("tab-3", Some("native-3"), None, None);
        assert!(store.write_session_record_if_absent(&r).unwrap());
        assert!(
            !store.write_session_record_if_absent(&r).unwrap(),
            "written once"
        );
        let back = store.read_session_record("tab-3").unwrap().unwrap();
        assert_eq!(back.cli.as_deref(), Some("claude"));
        assert_eq!(back.native_session_id.as_deref(), Some("native-3"));
        assert_eq!(back.manifest_hash, None);
        assert!(back.unavailable["manifestHash"].contains("by hand"));
        assert!(back.unavailable["org"].contains("managed workspace"));
        assert!(!back.unavailable.contains_key("nativeSessionId"));
    }

    #[test]
    fn a_credential_never_reaches_the_record() {
        let (_d, store) = store();
        let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let mut r = claude();
        r.model = Some(format!("opus {secret}"));
        r.branch = Some(format!("feat/{secret}"));
        r.account.as_mut().unwrap().label = Some(secret.clone());
        r.unavailable("cliVersion", &format!("failed with token {secret}"));
        store.write_session_record(&r).unwrap();
        let dump: String = store
            .conn
            .query_row(
                "SELECT json_object('a', model, 'b', branch, 'c', account, 'd', unavailable) FROM session_record",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!dump.contains(&secret), "{dump}");
    }

    #[test]
    fn a_rewrite_keeps_the_native_id_already_linked() {
        let (_d, store) = store();
        let mut r = SessionRecord::new("tab-4");
        store.write_session_record(&r).unwrap();
        store.link_native_session("tab-4", "conv-4").unwrap();
        r.handle = Some("SV-4".into());
        store.write_session_record(&r).unwrap();
        let back = store.read_session_record("tab-4").unwrap().unwrap();
        assert_eq!(back.native_session_id.as_deref(), Some("conv-4"));
        assert_eq!(back.handle.as_deref(), Some("SV-4"));
    }

    #[test]
    fn a_launched_runs_session_matches_its_usage_row() {
        let (_d, mut store) = store();
        let mut r = claude();
        r.session_id = "tab-9".into();
        r.native_session_id = Some("native-9".into());
        r.run_id = Some("agent-launch:run-9".into());
        store.write_session_record(&r).unwrap();
        store
            .ingest(&[crate::session_usage::tests::event(
                "e1",
                "native-9",
                "claude-opus-5-5",
                1_700_000_000_000,
                Default::default(),
            )])
            .unwrap();
        let report = store.report(&Default::default()).unwrap();
        let row = report
            .sessions
            .iter()
            .find(|s| s.session_id == "native-9")
            .unwrap();
        assert_eq!(row.run_id.as_deref(), Some("agent-launch:run-9"));
        // The run's own outcome names the CodeSense session that the usage row's id resolves to.
        let record = store.read_session_record(&row.session_id).unwrap().unwrap();
        assert_eq!(record.session_id, "tab-9");
    }

    #[test]
    fn a_session_with_no_record_has_no_run_id() {
        let (_d, mut store) = store();
        store
            .ingest(&[crate::session_usage::tests::event(
                "e2",
                "stray",
                "claude-opus-5-5",
                1_700_000_000_000,
                Default::default(),
            )])
            .unwrap();
        let report = store.report(&Default::default()).unwrap();
        assert_eq!(report.sessions[0].run_id, None);
    }

    fn codex_export(conversation: &str) -> serde_json::Value {
        serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{"attributes":[
            {"key":"event.name","value":{"stringValue":"codex.sse_event"}},
            {"key":"event.kind","value":{"stringValue":"response.completed"}},
            {"key":"conversation.id","value":{"stringValue":conversation}},
            {"key":"event.timestamp","value":{"stringValue":"2026-10-22T18:00:00Z"}},
            {"key":"model","value":{"stringValue":"gpt-6"}},
            {"key":"input_token_count","value":{"intValue":"10"}}
        ]}]}]}]})
    }

    #[test]
    fn pbf210_codex_record_is_published_only_after_early_export_native_backfill() {
        use crate::session_conversation::{note_end, TestWriter, CODEX_ID};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(
            root.join("usage.sqlite"),
            root.join("conv"),
            root.join("home"),
        );
        let session = "pbf210-codex-early-export";
        w.seed_codex(session, CODEX_ID);
        let conn = rusqlite::Connection::open(&w.store).unwrap();
        conn.execute(
            "DELETE FROM session_record WHERE session_id = ?1",
            [session],
        )
        .unwrap();
        UsageStore::open(&w.store)
            .unwrap()
            .register_launch(session, "acme", Some("dev"), "acme/dev/a@1", "SV-1", None)
            .unwrap();
        crate::session_usage::ingest_telemetry_for_at(&w.store, session, &codex_export(CODEX_ID))
            .unwrap();
        let mut record = SessionRecord::new(session);
        record.cli = Some("codex".into());
        record.config_home = Some(w.home.join(".codex").to_string_lossy().into_owned());
        record.unavailable("nativeSessionId", "reported with the first telemetry");
        assert!(
            record.native_session_id.is_none(),
            "Codex has no preassigned native id"
        );
        let (inserted, paused) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let path = w.store.clone();
        let writer = std::thread::spawn(move || {
            PAUSE_BEFORE_NATIVE_BACKFILL.with(|pause| {
                *pause.borrow_mut() = Some(Box::new(move || {
                    inserted.send(()).unwrap();
                    resume.recv().unwrap();
                }));
            });
            persist_record_in(&path, &record).unwrap();
            PAUSE_BEFORE_NATIVE_BACKFILL.with(|pause| *pause.borrow_mut() = None);
        });
        paused.recv().unwrap();
        // This is the lookup a capture can make while the registered writer is between
        // statements. A launch record is ready only once its early-export link is durable.
        let visible: Option<Option<String>> = conn
            .query_row(
                "SELECT native_session_id FROM session_record WHERE session_id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        release.send(()).unwrap();
        writer.join().unwrap();
        note_end(session);
        assert_eq!(
            w.stored(session)
                .expect("final capture reads the backfilled Codex log")
                .1
                .len(),
            3
        );
        let stored_record = UsageStore::open(&w.store)
            .unwrap()
            .read_session_record(session)
            .unwrap()
            .unwrap();
        assert_eq!(stored_record.native_session_id.as_deref(), Some(CODEX_ID));
        assert!(!stored_record.unavailable.contains_key("nativeSessionId"));
        assert_eq!(
            visible, None,
            "capture must not observe the INSERT before its native-id backfill commits"
        );
    }

    /// The first export and the record insert can happen in either order; the native id and the
    /// usage row's run join either way, with no second export.
    #[test]
    fn the_native_link_and_the_run_join_hold_in_either_order() {
        for telemetry_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("usage.sqlite");
            let mut record = SessionRecord::new("tab-o");
            record.run_id = Some("agent-launch:run-o".into());
            record.unavailable("nativeSessionId", "reported with the first telemetry");
            let register = || {
                UsageStore::open(&path)
                    .unwrap()
                    .register_launch("tab-o", "acme", Some("dev"), "acme/dev/a@1", "SV-O", None)
                    .unwrap();
            };
            let ingest = || {
                crate::session_usage::ingest_telemetry_for_at(
                    &path,
                    "tab-o",
                    &codex_export("conv-o"),
                )
                .unwrap();
            };
            let write = || {
                UsageStore::open(&path)
                    .unwrap()
                    .write_session_record(&record)
                    .unwrap()
            };
            register();
            if telemetry_first {
                ingest();
                write();
            } else {
                write();
                ingest();
            }
            let store = UsageStore::open(&path).unwrap();
            let back = store
                .read_session_record("conv-o")
                .unwrap()
                .expect("found by its native id");
            assert_eq!(
                back.session_id, "tab-o",
                "telemetry first: {telemetry_first}"
            );
            assert!(
                !back.unavailable.contains_key("nativeSessionId"),
                "{telemetry_first}"
            );
            let usage = store.report(&Default::default()).unwrap();
            let row = usage
                .sessions
                .iter()
                .find(|s| s.session_id == "conv-o")
                .unwrap();
            assert_eq!(
                row.run_id.as_deref(),
                Some("agent-launch:run-o"),
                "{telemetry_first}"
            );
        }
    }
}
