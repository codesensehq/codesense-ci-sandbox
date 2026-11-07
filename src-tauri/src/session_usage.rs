// SPDX-License-Identifier: MIT
//! Token use and spend per agent session, from the usage each agent CLI reports.
//!
//! Claude Code and Codex export per-request usage over OpenTelemetry to CodeSense's local receiver
//! (see `telemetry`); OpenCode's CodeSense plugin posts one record per finished assistant message
//! to the same receiver, carrying OpenCode's own notional cost. Every
//! source becomes a [`UsageEvent`], and the store keeps one row per event, keyed by CLI and event
//! id, so a repeated export never counts twice.
//!
//! Each event is priced when it is stored: the CLI's own reported cost when it sends one,
//! otherwise the rate table. A rate applies from its effective day, so a price change adds a row and
//! leaves earlier usage at the price it was billed at.

pub(crate) mod contribution;
pub mod conversation;
pub(crate) mod cost_rollup;
pub(crate) mod managed_telemetry;
pub(crate) mod trace_link;
pub mod event;
pub mod record;
pub mod session_read;
pub mod story;
pub mod story_changes;
pub mod story_totals;
pub mod task;
pub mod task_capture;
pub mod task_outcome;
pub mod timing;
pub mod telemetry;
pub(crate) mod spend_caps;
pub(crate) mod session_log_usage;
#[cfg(test)]
mod spend_scan_tests;
#[cfg(test)]
mod session_report_tests;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use rmcp::schemars::JsonSchema;
use codesense_pty_core::protocol::UsageSpoolRead;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::shared::sqlite::open_wal_db;
use codesense_pty_core::agent_cli::AgentCli;
use event::{Tokens, UsageEvent};
pub use story::{StoryLink, StoryResolver, UsageStory};

const PER_MTOK: f64 = 1_000_000.0;
/// The largest CLI-reported cost kept, in dollars. A single message or turn above this is not a real
/// cost, so the figure is treated as unknown and priced from the rate table instead.
pub(crate) const MAX_REPORTED_COST_USD: f64 = 100_000.0;
const SEED_EFFECTIVE_FROM: &str = "1970-01-01";
const SEED_SOURCE: &str = "seed: standard list price from the vendor's pricing page, looked up 2026-10-17";
const ZEN_SOURCE: &str = "seed: OpenCode Zen list price (opencode.ai/docs/zen), looked up 2026-10-24";

/// A model's list price before any rate row: USD per million tokens, the cache-read price where it is
/// not the usual 0.1x input, and the higher prices for a long prompt where the vendor has them.
struct Seed {
    prefix: &'static str,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    long: Option<LongRate>,
    /// Where the price came from and when it was looked up, as `model_rates` shows it.
    source: &'static str,
}

const fn seed(prefix: &'static str, input: f64, output: f64) -> Seed {
    Seed { prefix, input, output, cache_read: None, long: None, source: SEED_SOURCE }
}

/// An OpenCode Zen list price: input, output and cache read, USD per million tokens.
const fn zen(prefix: &'static str, input: f64, output: f64, cache_read: f64) -> Seed {
    Seed { cache_read: Some(cache_read), source: ZEN_SOURCE, ..seed(prefix, input, output) }
}

/// The standard list prices on Anthropic's pricing page (platform.claude.com/docs/en/about-claude/pricing)
/// and OpenAI's (developers.openai.com/api/docs/pricing), looked up 2026-10-17. Claude reports its own
/// cost, so a Claude seed is used only when a call has none; Codex reports none, so its usage is priced
/// from here. Models with only promotional prices are not seeded.
const SEED_RATES: &[Seed] = &[
    seed("claude-opus-4-6", 5.0, 25.0),
    seed("claude-opus-4-7", 5.0, 25.0),
    seed("claude-opus-4-8", 5.0, 25.0),
    seed("claude-opus-5", 5.0, 25.0),
    // Opus 5.5 and Sonnet 5.5 cache reads are 0.05x input.
    Seed { cache_read: Some(0.20), ..seed("claude-opus-5-5", 4.0, 20.0) },
    seed("claude-sonnet-4-6", 3.0, 15.0),
    seed("claude-sonnet-5", 2.0, 10.0),
    Seed { cache_read: Some(0.10), ..seed("claude-sonnet-5-5", 2.0, 10.0) },
    seed("claude-haiku-4-5", 1.0, 5.0),
    // Haiku 5.5 bills a prompt over 100,000 tokens at its higher prices.
    Seed {
        long: Some(LongRate { from_tokens: 100_000, input: 0.50, output: 2.50, cache_read: None }),
        ..seed("claude-haiku-5-5", 0.10, 0.50)
    },
    seed("gpt-5-3-codex", 1.75, 14.0),
    // The GPT-6 models bill a prompt over 272,000 input tokens at their long-context prices.
    Seed {
        long: Some(LongRate { from_tokens: 272_000, input: 20.0, output: 75.0, cache_read: None }),
        ..seed("gpt-6-astra", 10.0, 50.0)
    },
    Seed {
        cache_read: Some(0.10),
        long: Some(LongRate { from_tokens: 272_000, input: 4.0, output: 15.0, cache_read: Some(0.20) }),
        ..seed("gpt-6-1-sol", 2.0, 10.0)
    },
    Seed {
        long: Some(LongRate { from_tokens: 272_000, input: 4.0, output: 15.0, cache_read: None }),
        ..seed("gpt-6-sol", 2.0, 10.0)
    },
    Seed {
        long: Some(LongRate { from_tokens: 272_000, input: 0.20, output: 0.75, cache_read: None }),
        ..seed("gpt-6-luna", 0.10, 0.50)
    },
    // OpenCode Zen's list prices (opencode.ai/docs/zen). OpenCode reports its own cost, so these
    // price only a call it left unpriced. A Go subscription's cost is this notional price too.
    zen("deepseek-v4-1-flash", 0.30, 1.20, 0.006),
    zen("deepseek-v4-pro", 1.74, 3.48, 0.145),
    // Qwen3.8 Flash (its own cache-write price) and Qwen3.7 Plus (a long-prompt tier) are not
    // seeded: a seed here cannot express those prices, so they use OpenCode's reported cost.
    zen("minimax-m3", 0.30, 1.20, 0.06),
    zen("kimi-k2-7-code", 0.95, 4.00, 0.19),
    zen("glm-5-3", 1.40, 4.40, 0.26),
];

/// The usage store's schema version, recorded in the database so a later change can migrate it.
const SCHEMA_VERSION: i64 = 5;
#[cfg(test)]
thread_local! {
    /// Runs at the start of `spend_for`, so a test can park the spend query.
    pub(crate) static SPEND_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS model_rate (
    model_prefix    TEXT NOT NULL,
    effective_from  TEXT NOT NULL,
    input_per_mtok  REAL NOT NULL,
    output_per_mtok REAL NOT NULL,
    source          TEXT NOT NULL,
    -- The cache-read price, where it is not 0.1x input. NULL: 0.1x input.
    cache_read_per_mtok REAL,
    -- A long prompt's prices: a call whose prompt is over long_from_tokens is priced at these.
    -- NULL: one price at every length.
    long_from_tokens         INTEGER,
    long_input_per_mtok      REAL,
    long_output_per_mtok     REAL,
    long_cache_read_per_mtok REAL,
    PRIMARY KEY (model_prefix, effective_from)
);
CREATE TABLE IF NOT EXISTS usage_event (
    machine_id  TEXT NOT NULL DEFAULT '',
    cli         TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    day         TEXT NOT NULL,
    at_ms       INTEGER NOT NULL,
    cwd         TEXT,
    model       TEXT NOT NULL,
    input       INTEGER NOT NULL,
    output      INTEGER NOT NULL,
    cache_read  INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    cost_usd    REAL,
    cost_from   TEXT,
    -- The part of cache_write written to the one-hour cache.
    cache_write_1h INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (machine_id, cli, event_id)
);
CREATE TABLE IF NOT EXISTS usage_pull_cursor (
    machine_id TEXT PRIMARY KEY,
    epoch      TEXT NOT NULL,
    last_seq   INTEGER NOT NULL,
    incomplete INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_event_by_day ON usage_event (day);
CREATE INDEX IF NOT EXISTS usage_event_by_session ON usage_event (session_id);
CREATE INDEX IF NOT EXISTS usage_event_by_session_day_cost ON usage_event (session_id, day, cost_usd);
CREATE INDEX IF NOT EXISTS usage_event_unpriced_by_session_day_model ON usage_event (session_id, day, model) WHERE cost_usd IS NULL;
CREATE TABLE IF NOT EXISTS launch_owner (
    token            TEXT PRIMARY KEY,
    org              TEXT NOT NULL,
    team             TEXT,
    agent            TEXT NOT NULL,
    handle           TEXT NOT NULL,
    cwd              TEXT,
    registered_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_attribution (
    session_id       TEXT PRIMARY KEY,
    org              TEXT NOT NULL,
    team             TEXT,
    agent            TEXT NOT NULL,
    handle           TEXT NOT NULL,
    cwd              TEXT,
    attributed_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS session_attribution_by_agent ON session_attribution (agent);
CREATE INDEX IF NOT EXISTS session_attribution_by_handle ON session_attribution (handle);
";

/// USD per million tokens for one model on one day. `cache_read` is the cache-read price where the
/// model's is not 0.1x input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    long: Option<LongRate>,
}

/// The prices of a call whose prompt (its input, cache reads and cache writes) is over `from_tokens`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LongRate {
    from_tokens: u64,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
}

/// A long prompt's prices as the page's `setModelRate` and the MCP `set_model_rate` pass them and
/// `modelRates` returns them (USD per million tokens).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LongRateArgs {
    /// A call whose prompt (input, cache reads and cache writes) is over this many tokens.
    pub from_tokens: u64,
    /// USD per million input tokens on a long prompt.
    pub input_per_mtok: f64,
    /// USD per million output tokens on a long prompt.
    pub output_per_mtok: f64,
    /// Omitted: 0.1x the long input price.
    pub cache_read_per_mtok: Option<f64>,
}

/// `setModelRate`'s arguments, as the page bridge sends them and the MCP `set_model_rate` takes them
/// (USD per million tokens). A key it does not know is refused, so a misspelled price is never dropped.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetModelRateParams {
    /// The model id, or a prefix of it, the rate prices (e.g. `claude-opus-5-5`): at least six
    /// lowercase letters, digits, dots and dashes, not starting or ending in a dot or dash. The
    /// longest matching prefix prices a model.
    pub model_prefix: String,
    /// The local day it applies from, `YYYY-MM-DD`. Usage before that day keeps its price.
    pub effective_from: String,
    /// USD per million input tokens.
    pub input_per_mtok: f64,
    /// USD per million output tokens.
    pub output_per_mtok: f64,
    /// Omitted or null: 0.1x input.
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    /// Omitted or null: one price at every prompt length.
    #[serde(default)]
    pub long: Option<LongRateArgs>,
}

impl SetModelRateParams {
    fn rate(&self) -> Rate {
        Rate::with(self.input_per_mtok, self.output_per_mtok, self.cache_read_per_mtok, self.long)
    }
}

/// One stored rate, as the page's `modelRates` returns it (USD per million tokens). The seeded list
/// prices are stored rates too.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRate {
    pub model_prefix: String,
    /// The local day it applies from, `YYYY-MM-DD`. A seeded list price applies from `1970-01-01`.
    pub effective_from: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// `null`: 0.1x input.
    pub cache_read_per_mtok: Option<f64>,
    /// `null`: one price at every prompt length. Its `cacheReadPerMtok` `null`: 0.1x the long input
    /// price.
    pub long: Option<LongRateArgs>,
    /// `set by the user` (native acceptance), `set by an agent` (MCP/page request), or the
    /// seeded list price's note.
    pub source: String,
}

impl Rate {
    /// One input and one output price at every prompt length, with cache reads at 0.1x input.
    pub const fn flat(input: f64, output: f64) -> Self {
        Self { input, output, cache_read: None, long: None }
    }

    /// The rate a user sets: the flat prices, a cache-read price where it is not 0.1x input, and the
    /// prices of a long prompt where the model has them.
    pub fn with(input: f64, output: f64, cache_read: Option<f64>, long: Option<LongRateArgs>) -> Self {
        Self {
            input,
            output,
            cache_read,
            long: long.map(|l| LongRate {
                from_tokens: l.from_tokens,
                input: l.input_per_mtok,
                output: l.output_per_mtok,
                cache_read: l.cache_read_per_mtok,
            }),
        }
    }

    fn price(&self, t: &Tokens) -> f64 {
        let prompt = t.input.saturating_add(t.cache_read).saturating_add(t.cache_write);
        let (input, output, cache_read) = match self.long {
            Some(long) if prompt > long.from_tokens => (long.input, long.output, long.cache_read),
            _ => (self.input, self.output, self.cache_read),
        };
        // A five-minute cache write bills at 1.25x input, a one-hour one at 2x.
        let one_hour = t.cache_write_1h.min(t.cache_write);
        let dollars = t.input as f64 * input
            + t.output as f64 * output
            + t.cache_read as f64 * cache_read.unwrap_or(input * 0.1)
            + (t.cache_write - one_hour) as f64 * input * 1.25
            + one_hour as f64 * input * 2.0;
        dollars / PER_MTOK
    }
}

/// Which usage a read counts. Usage counts by whole local day: `sinceMs`'s day is included, and
/// `untilMs`'s day is excluded. Optionally limited to one session or one working directory. The MCP
/// `session_usage` tool takes this as its input, and the page bridge's `sessionUsage` takes the same
/// fields.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    /// Count from the local day containing this time (epoch milliseconds). Omitted: no lower bound.
    pub since_ms: Option<i64>,
    /// Count up to, not including, the local day containing this time (epoch milliseconds). Omitted:
    /// no upper bound.
    pub until_ms: Option<i64>,
    /// Only this session's usage (a session id from list_sessions or sessions()).
    pub session_id: Option<String>,
    /// Only usage recorded in this working directory (a workspace path, exact match).
    pub cwd: Option<String>,
    /// Only the sessions spent on this story (a story id, e.g. `DEMO-5`; see each session's
    /// `story`).
    pub story: Option<String>,
    /// The most sessions to return, newest first. The totals still cover every session. Omitted: all.
    pub limit: Option<usize>,
    /// The `nextCursor` of the previous page: continue after the last session it returned. Pass the
    /// same other fields. A cursor from a different window or filter is refused.
    pub cursor: Option<String>,
}

/// What a session cursor is bound to: the fields that choose which sessions match (not `limit`
/// or the cursor), so a cursor from another window or filter is refused.
fn cursor_key(filter: &Filter) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        filter.since_ms,
        filter.until_ms,
        &filter.session_id,
        &filter.cwd,
        filter.story.as_deref().map(|s| s.trim().to_lowercase()),
    )
        .hash(&mut h);
    h.finish()
}

/// The cursor that continues after `session`: a keyset over the report's sort
/// `(lastAtMs desc, sessionId asc)`, so sessions arriving or leaving between pages move no row.
fn cursor_after(key: u64, session: &SessionUsage) -> String {
    format!("u:{key:x}:{}:{}", session.last_at_ms, session.session_id)
}

/// The `(lastAtMs, sessionId)` a cursor continues after, or why it is refused.
fn parse_cursor(filter: &Filter) -> Result<Option<(i64, String)>, String> {
    let Some(cursor) = filter.cursor.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let parts: Vec<&str> = cursor.splitn(4, ':').collect();
    let parsed = match parts.as_slice() {
        ["u", key, at, id] if !id.is_empty() => u64::from_str_radix(key, 16)
            .ok()
            .zip(at.parse::<i64>().ok())
            .map(|(key, at)| (key, at, id.to_string())),
        _ => None,
    };
    let Some((key, at, id)) = parsed else {
        return Err(format!("invalid: cursor {cursor} is not one this tool returned"));
    };
    if key != cursor_key(filter) {
        return Err("invalid: this cursor belongs to a different window or filter; page with the arguments that returned it".to_string());
    }
    Ok(Some((at, id)))
}

/// The saved agent or fresh-session routine whose spend is counted, across all machines.
#[derive(Debug, Clone, Copy)]
pub enum SpendScope<'a> {
    Agent(&'a str),
    Routine(&'a str),
}

/// The spend query for `scope`, and its bounds for `?3` (and `?4` for a routine).
fn spend_sql(scope: SpendScope<'_>) -> (String, String, String) {
    let (predicate, lower, upper) = match scope {
        SpendScope::Agent(id) => ("a.agent = ?3", id.to_string(), String::new()),
        // '@' ends the exact routine key; 'A' is its next ASCII character, giving an
        // indexed prefix range without treating '_' or '%' in a key as wildcards.
        SpendScope::Routine(key) => (
            "a.handle >= ?3 AND a.handle < ?4",
            format!("routine:{key}@"),
            format!("routine:{key}A"),
        ),
    };
    let sessions = format!("SELECT a.session_id FROM session_attribution a WHERE {predicate}");
    let sql = format!(
        "SELECT
          (SELECT SUM(e.cost_usd)
           FROM usage_event e
           WHERE e.day >= ?1 AND e.day < ?2 AND e.session_id IN ({sessions})),
          (SELECT json_group_array(DISTINCT e.model)
           FROM usage_event e
           WHERE e.day >= ?1 AND e.day < ?2 AND e.session_id IN ({sessions}) AND e.cost_usd IS NULL)"
    );
    (sql, lower, upper)
}

/// A local calendar day, or the Monday-starting week containing the given local date.
#[derive(Debug, Clone, Copy)]
pub enum SpendWindow {
    Day(NaiveDate),
    Week(NaiveDate),
}

impl SpendWindow {
    fn days(self) -> Result<(String, String), String> {
        let (start, length) = match self {
            Self::Day(day) => (Some(day), 1),
            Self::Week(day) => (
                day.checked_sub_days(Days::new(day.weekday().num_days_from_monday().into())),
                7,
            ),
        };
        let start = start.ok_or("invalid: spend window is outside the supported range")?;
        let until = start
            .checked_add_days(Days::new(length))
            .ok_or("invalid: spend window is outside the supported range")?;
        if start.year() < 0 || until.year() > 9999 {
            return Err("invalid: spend window is outside the supported range".into());
        }
        Ok((
            start.format("%Y-%m-%d").to_string(),
            until.format("%Y-%m-%d").to_string(),
        ))
    }
}

/// Priced event costs plus the models whose rows still have no price.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub cost_usd: f64,
    pub unpriced_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    /// The agent CLI the session ran in: `claude`, `codex` or `opencode`.
    pub cli: AgentCli,
    pub session_id: String,
    /// Who ran the session, recorded at launch. `null` for sessions CodeSense did not launch, or
    /// that launched before attribution was recorded.
    pub org: Option<String>,
    pub team: Option<String>,
    /// The saved agent it ran, as `agent_get` names it (e.g. `builtin/claude`).
    pub agent: Option<String>,
    /// The machine the session ran on, as `list_machines` names it: `local` for this Mac. A session
    /// whose usage came from more than one machine names the machine of its latest call (at a tie, the
    /// other machine over this Mac, else the greater id). A removed machine keeps its id here.
    pub machine_id: String,
    pub cwd: Option<String>,
    /// The story the session was spent on, or `null` when it is not tied to one: the story
    /// worktree its working folder is in, else the story its launch was opened for.
    pub story: Option<UsageStory>,
    pub models: Vec<String>,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
    pub messages: u32,
    pub tokens: Tokens,
    /// `null` when any of the session's usage has no price.
    pub cost_usd: Option<f64>,
    pub unpriced_models: Vec<String>,
    /// The run the session worked, from its durable session record; `null` for a session with no
    /// record (older sessions, or CLIs CodeSense did not launch) or one launched outside a run.
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub tokens: Tokens,
    /// The sum over sessions whose cost is known. `unpricedSessions` counts the rest.
    pub cost_usd: f64,
    pub priced_sessions: u32,
    pub unpriced_sessions: u32,
}

/// One local day's usage across every session, CLI and model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    /// Local calendar day, `YYYY-MM-DD`.
    pub day: String,
    pub tokens: Tokens,
    /// The priced usage of the day. `unpricedModels` names what it leaves out.
    pub cost_usd: f64,
    pub unpriced_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    /// Newest activity first.
    pub sessions: Vec<SessionUsage>,
    /// One entry per local day with usage, newest day first.
    pub days: Vec<DayUsage>,
    pub totals: UsageTotals,
    /// How many sessions matched before `limit` cut `sessions`. Served as `total` on MCP, Tauri
    /// and hosted responses so callers can tell when a walk is complete.
    #[serde(rename = "total")]
    pub session_count: usize,
    /// Pass as `cursor` for the next page. Present only when sessions follow this page's last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Why sessions are missing from this report: present while managed settings send a CLI's
    /// session usage past CodeSense and it cannot read that usage from the session's own log,
    /// even when older sessions are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    /// What this report's cursors are bound to (see [`cursor_key`]).
    #[serde(skip)]
    pub cursor_key: u64,
}

impl UsageReport {
    /// The cursor that continues after the first `kept` sessions of this page, for a caller that
    /// sent fewer than the page held (a byte cut). `None` when no sessions follow them.
    pub fn cursor_after_kept(&self, kept: usize) -> Option<String> {
        let Self { sessions, .. } = self;
        match kept {
            0 => None,
            k if k < sessions.len() => Some(cursor_after(self.cursor_key, &sessions[k - 1])),
            _ => self.next_cursor.clone(),
        }
    }
}

/// [`local_day`], or an error for a time outside the calendar.
fn checked_day(ms: i64) -> Result<String, String> {
    let day = local_day(ms);
    // A year past 9999 prints with a sign and no fixed width, and would not compare as a day.
    if day.len() != 10 {
        return Err(format!("invalid: time {ms} is outside the supported range"));
    }
    Ok(day)
}

/// Whether `table` has a column named `column`.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(count > 0)
}

/// Validate a rate and store it, returning the prefix and day as stored. A prefix is a model id's
/// start, letters, digits, dots and dashes, at least six characters, and not starting or ending in a
/// separator. A family prefix such as `claude` prices only the models that have no longer prefix of
/// their own: a seeded model such as `claude-sonnet-4-6` keeps its seeded price. Every price is
/// positive and bounded, so a cost stays finite, and a long prompt starts past zero tokens. The day is
/// stored padded, since days compare as text.
fn insert_rate(
    conn: &Connection,
    model_prefix: &str,
    effective_from: &str,
    rate: &Rate,
    source: &str,
) -> Result<(String, String), String> {
    // Model ids are lowercase, and a prefix ending in a separator matches nothing, so both are refused
    // rather than stored as a rate that never prices anything.
    if model_prefix.len() < 6
        || !model_prefix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        || model_prefix.starts_with(['-', '.'])
        || model_prefix.ends_with(['-', '.'])
    {
        return Err("invalid: model prefix must be at least 6 lowercase letters, digits, dots and dashes, and not start or end in a separator".to_string());
    }
    const MAX_RATE: f64 = 100_000.0;
    let valid = |price: f64| price > 0.0 && price <= MAX_RATE;
    let long = rate.long.as_ref();
    let prices = [Some(rate.input), Some(rate.output), rate.cache_read]
        .into_iter()
        .chain(long.map(|l| [Some(l.input), Some(l.output), l.cache_read]).into_iter().flatten());
    if !prices.flatten().all(valid) {
        return Err(format!("invalid: rates must be positive numbers no larger than {MAX_RATE} USD per million tokens"));
    }
    // A threshold past i64 could not be stored, and one of zero would make every call long.
    if long.is_some_and(|l| l.from_tokens == 0 || l.from_tokens > i64::MAX as u64) {
        return Err(format!("invalid: long.fromTokens must be a whole number of tokens from 1 to {}", i64::MAX));
    }
    // A signed or five-digit year (`+10000-01-01`) parses, but sorts as text before every day and would
    // price all usage. A day before the seeded list prices' would never price anything.
    let invalid_day =
        || format!("invalid: effectiveFrom must be a day from {SEED_EFFECTIVE_FROM} to 9999-12-31 as YYYY-MM-DD, got {effective_from:?}");
    let day = NaiveDate::parse_from_str(effective_from, "%Y-%m-%d").map_err(|_| invalid_day())?;
    let effective_from = day.format("%Y-%m-%d").to_string();
    if day.year() > 9999 || effective_from.as_str() < SEED_EFFECTIVE_FROM {
        return Err(invalid_day());
    }
    // Model ids are matched with dots read as dashes (a CLI may write claude-sonnet-4.6), so the
    // stored prefix is normalised the same way, or it would never match.
    let model_prefix = model_prefix.replace('.', "-");
    conn.execute(
        "INSERT OR REPLACE INTO model_rate (model_prefix, effective_from, input_per_mtok, output_per_mtok, cache_read_per_mtok,
            long_from_tokens, long_input_per_mtok, long_output_per_mtok, long_cache_read_per_mtok, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            model_prefix,
            effective_from,
            rate.input,
            rate.output,
            rate.cache_read,
            long.map(|l| l.from_tokens as i64),
            long.map(|l| l.input),
            long.map(|l| l.output),
            long.and_then(|l| l.cache_read),
            source,
        ],
    )
    .map_err(db_err)?;
    Ok((model_prefix, effective_from))
}

/// The local calendar day (`YYYY-MM-DD`) of an epoch-ms time. Usage is grouped by this day.
fn local_day(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn db_err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// The durable usage store: the rate table, the usage events, and the launch attribution.
pub struct UsageStore {
    conn: Connection,
}

impl UsageStore {
    /// Open (creating if needed) the store at `path`, and seed the rate table. Seeding never
    /// overwrites a rate that is already there.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = open_wal_db(path).map_err(|e| e.to_string())?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "the usage store is from a newer CodeSense (schema {version}); this build reads schema {SCHEMA_VERSION}"
            ));
        }
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        // Each later schema change is applied here, in one transaction with the version stamp, so a
        // crash cannot leave a half-upgraded store. Version 2 records where each cost came from: a
        // table without that column gets it, whatever version it claims. A store's old costs have no
        // recorded source, so they are never repriced.
        // Immediate, so two processes upgrading the same store queue on the lock rather than one
        // failing with a busy snapshot.
        let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        if !has_column(&tx, "usage_event", "cost_from")? {
            tx.execute_batch("ALTER TABLE usage_event ADD COLUMN cost_from TEXT;").map_err(db_err)?;
        }
        // Version 3 links a session to the launch whose telemetry token it was exported under, so a
        // CodeSense session id finds the CLI's own session id's usage.
        if !has_column(&tx, "session_attribution", "launch_id")? {
            tx.execute_batch("ALTER TABLE session_attribution ADD COLUMN launch_id TEXT;").map_err(db_err)?;
        }
        // A session's launch finds its other id-spaces by this column, once per queued source.
        tx.execute_batch("CREATE INDEX IF NOT EXISTS session_attribution_by_launch ON session_attribution (launch_id);")
            .map_err(db_err)?;
        // Version 4 also lets a rate name its own cache-read price, for models whose cache reads are
        // not 0.1x input.
        if !has_column(&tx, "model_rate", "cache_read_per_mtok")? {
            tx.execute_batch("ALTER TABLE model_rate ADD COLUMN cache_read_per_mtok REAL;").map_err(db_err)?;
        }
        // Version 4 keys each usage row by the machine it came from, so two machines' events never
        // overwrite each other. A store's rows are this Mac's, so they take the empty machine id.
        // SQLite cannot change a primary key in place, so the table is rebuilt.
        if !has_column(&tx, "usage_event", "machine_id")? {
            tx.execute_batch(
                "ALTER TABLE usage_event RENAME TO usage_event_v3;
                 CREATE TABLE usage_event (
                    machine_id  TEXT NOT NULL DEFAULT '',
                    cli         TEXT NOT NULL,
                    event_id    TEXT NOT NULL,
                    session_id  TEXT NOT NULL,
                    day         TEXT NOT NULL,
                    at_ms       INTEGER NOT NULL,
                    cwd         TEXT,
                    model       TEXT NOT NULL,
                    input       INTEGER NOT NULL,
                    output      INTEGER NOT NULL,
                    cache_read  INTEGER NOT NULL,
                    cache_write INTEGER NOT NULL,
                    cost_usd    REAL,
                    cost_from   TEXT,
                    PRIMARY KEY (machine_id, cli, event_id)
                 );
                 INSERT INTO usage_event (machine_id, cli, event_id, session_id, day, at_ms, cwd, model,
                    input, output, cache_read, cache_write, cost_usd, cost_from)
                    SELECT '', cli, event_id, session_id, day, at_ms, cwd, model,
                    input, output, cache_read, cache_write, cost_usd, cost_from FROM usage_event_v3;
                 DROP TABLE usage_event_v3;
                 CREATE INDEX IF NOT EXISTS usage_event_by_day ON usage_event (day);
                 CREATE INDEX IF NOT EXISTS usage_event_by_session ON usage_event (session_id);
                 CREATE INDEX IF NOT EXISTS usage_event_by_session_day_cost ON usage_event (session_id, day, cost_usd);
                 CREATE INDEX IF NOT EXISTS usage_event_unpriced_by_session_day_model ON usage_event (session_id, day, model) WHERE cost_usd IS NULL;",
            )
            .map_err(db_err)?;
        }
        // Version 5 prices a long prompt at its own rate where the vendor has one, and keeps the
        // one-hour part of a cache write, which bills higher.
        for (column, kind) in [
            ("long_from_tokens", "INTEGER"),
            ("long_input_per_mtok", "REAL"),
            ("long_output_per_mtok", "REAL"),
            ("long_cache_read_per_mtok", "REAL"),
        ] {
            if !has_column(&tx, "model_rate", column)? {
                tx.execute_batch(&format!("ALTER TABLE model_rate ADD COLUMN {column} {kind};")).map_err(db_err)?;
            }
        }
        if !has_column(&tx, "usage_event", "cache_write_1h")? {
            tx.execute_batch("ALTER TABLE usage_event ADD COLUMN cache_write_1h INTEGER NOT NULL DEFAULT 0;")
                .map_err(db_err)?;
        }
        // The story a session was spent on: the story worktree its working folder was in when its
        // usage was recorded, and that story's file, so the session keeps its story after the
        // worktree is removed. An extra table, not a schema version: an older CodeSense reading
        // the same store ignores it, where a newer version would make it refuse the store.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_story (
                session_id      TEXT PRIMARY KEY,
                story_id        TEXT NOT NULL,
                story_path      TEXT,
                org             TEXT,
                linked_at_ms    INTEGER NOT NULL
             );",
        )
        .map_err(db_err)?;
        // The durable session record: another additive table, for the same reason.
        tx.execute_batch(record::SCHEMA).map_err(db_err)?;
        tx.execute_batch(timing::SCHEMA).map_err(db_err)?;
        task::init(&tx)?;
        conversation::init(&tx)?;
        trace_link::init(&tx)?;
        if !has_column(&tx, "session_turn", "source")? {
            tx.execute_batch("ALTER TABLE session_turn ADD COLUMN source TEXT;").map_err(db_err)?;
        }
        // The story totals index, and the triggers by which the tables above mark a story changed.
        story_totals::init(&tx)?;
        // Existing stores may carry old session/day indexes (the two-column prefix or the
        // wide covering index from earlier SV-PBF-169 attempts). The new narrow indexes are
        // created once by SCHEMA above; drop any obsolete names idempotently.
        tx.execute_batch(
            "DROP INDEX IF EXISTS usage_event_by_session_day;
             DROP INDEX IF EXISTS usage_event_by_session_day_covering;"
        )
        .map_err(db_err)?;
        if version < SCHEMA_VERSION {
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};")).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        // A seed new to this store (one a later CodeSense added) prices the usage it covers that had
        // no price, so a newly seeded model's earlier usage is priced too.
        let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate).map_err(db_err)?;
        // A seed row CodeSense wrote is corrected when its seed changes, and the usage it priced is
        // priced again. A rate the user set is never touched.
        for seed in SEED_RATES {
            let long = seed.long.as_ref();
            let values = params![
                seed.prefix,
                SEED_EFFECTIVE_FROM,
                seed.input,
                seed.output,
                seed.cache_read,
                long.map(|l| l.from_tokens as i64),
                long.map(|l| l.input),
                long.map(|l| l.output),
                long.and_then(|l| l.cache_read),
                seed.source,
            ];
            let added = tx
                .execute(
                    "INSERT OR IGNORE INTO model_rate (model_prefix, effective_from, input_per_mtok, output_per_mtok, cache_read_per_mtok,
                        long_from_tokens, long_input_per_mtok, long_output_per_mtok, long_cache_read_per_mtok, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    values,
                )
                .map_err(db_err)?;
            let corrected = tx
                .execute(
                    "UPDATE model_rate SET input_per_mtok = ?3, output_per_mtok = ?4, cache_read_per_mtok = ?5,
                        long_from_tokens = ?6, long_input_per_mtok = ?7, long_output_per_mtok = ?8,
                        long_cache_read_per_mtok = ?9, source = ?10
                     WHERE model_prefix = ?1 AND effective_from = ?2 AND source LIKE 'seed:%'
                       AND (input_per_mtok IS NOT ?3 OR output_per_mtok IS NOT ?4 OR cache_read_per_mtok IS NOT ?5
                         OR long_from_tokens IS NOT ?6 OR long_input_per_mtok IS NOT ?7
                         OR long_output_per_mtok IS NOT ?8 OR long_cache_read_per_mtok IS NOT ?9)",
                    values,
                )
                .map_err(db_err)?;
            if added > 0 || corrected > 0 {
                reprice_covered(&tx, seed.prefix, SEED_EFFECTIVE_FROM)?;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(Self { conn })
    }

    /// Set a model's rate from `effective_from` (a local `YYYY-MM-DD`). A later day adds a rate, so
    /// earlier usage keeps its price. The same model and day replaces the rate there. Tests only: the
    /// app sets rates through `set_rate_and_reprice`.
    #[cfg(test)]
    pub fn set_rate(
        &self,
        model_prefix: &str,
        effective_from: &str,
        input_per_mtok: f64,
        output_per_mtok: f64,
        source: &str,
    ) -> Result<(), String> {
        insert_rate(&self.conn, model_prefix, effective_from, &Rate::flat(input_per_mtok, output_per_mtok), source)
            .map(|_| ())
    }

    /// Set a rate, then price the usage it covers: unpriced usage, and usage priced from the rate
    /// table (so a mistyped rate can be corrected). A cost the CLI reported is never changed. Returns
    /// how many costs it changed.
    pub fn set_rate_and_reprice(
        &mut self,
        model_prefix: &str,
        effective_from: &str,
        rate: Rate,
        source: &str,
    ) -> Result<usize, String> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        // The rate and its repricing commit together, so a failed repricing leaves no new rate behind.
        let (prefix, from) = insert_rate(&tx, model_prefix, effective_from, &rate, source)?;
        let priced = reprice_covered(&tx, &prefix, &from)?;
        tx.commit().map_err(db_err)?;
        Ok(priced)
    }

    /// Every stored rate, by model prefix and then by day. A model's rate on a day is the latest
    /// one from on or before that day, under its longest matching prefix.
    pub fn rates(&self) -> Result<Vec<StoredRate>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT model_prefix, effective_from, input_per_mtok, output_per_mtok, cache_read_per_mtok,
                        long_from_tokens, long_input_per_mtok, long_output_per_mtok, long_cache_read_per_mtok, source
                 FROM model_rate ORDER BY model_prefix, effective_from",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                let long_from: Option<i64> = r.get(5)?;
                let long_input: Option<f64> = r.get(6)?;
                let long_output: Option<f64> = r.get(7)?;
                Ok(StoredRate {
                    model_prefix: r.get(0)?,
                    effective_from: r.get(1)?,
                    input_per_mtok: r.get(2)?,
                    output_per_mtok: r.get(3)?,
                    cache_read_per_mtok: r.get(4)?,
                    long: match (long_from, long_input, long_output) {
                        (Some(from), Some(input), Some(output)) => Some(LongRateArgs {
                            from_tokens: from.max(0) as u64,
                            input_per_mtok: input,
                            output_per_mtok: output,
                            cache_read_per_mtok: r.get(8)?,
                        }),
                        _ => None,
                    },
                    source: r.get(9)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Record who ran a session. Written at launch, once per session: a second write replaces the
    /// first.
    pub fn attribute(
        &self,
        session_id: &str,
        org: &str,
        team: Option<&str>,
        agent: &str,
        handle: &str,
        cwd: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                // Replaces the owner, but keeps the launch id an export may already have filled in.
                "INSERT INTO session_attribution (session_id, org, team, agent, handle, cwd, attributed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (session_id) DO UPDATE SET org = excluded.org, team = excluded.team, agent = excluded.agent,
                    handle = excluded.handle, cwd = excluded.cwd, attributed_at_ms = excluded.attributed_at_ms",
                params![session_id, org, team, agent, handle, cwd, chrono::Utc::now().timestamp_millis()],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Keep a session's known worktree or launch story. The first link stands.
    pub fn link_story(&self, session_id: &str, link: &StoryLink) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO session_story (session_id, story_id, story_path, org, linked_at_ms) VALUES (?1, ?2, NULLIF(?3, ''), ?4, ?5)",
                params![
                    session_id,
                    link.story_id,
                    link.source.to_string_lossy(),
                    link.org,
                    chrono::Utc::now().timestamp_millis()
                ],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Link each session in `cwds` (session id, working folder) that has no story yet to the
    /// story worktree its folder is in. The worktree records are read only when a session needs
    /// them.
    pub fn link_stories(
        &self,
        cwds: &[(String, String)],
        stories: impl FnOnce() -> StoryResolver,
    ) -> Result<(), String> {
        let mut unlinked = Vec::new();
        for (session, cwd) in cwds {
            let linked: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM session_story WHERE session_id = ?1)",
                    params![session],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            if !linked {
                unlinked.push((session, cwd));
            }
        }
        if unlinked.is_empty() {
            return Ok(());
        }
        let stories = stories();
        for (session, cwd) in unlinked {
            if let Some(link) = stories.worktree_link(cwd) {
                self.link_story(session, &link)?;
            }
        }
        Ok(())
    }

    /// The stored story per session id.
    fn stored_stories(&self) -> Result<StoredStories, String> {
        count_stored_read(self);
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, story_id, story_path FROM session_story")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Record who a launch belongs to, under the token its telemetry address carries.
    pub fn register_launch(
        &self,
        token: &str,
        org: &str,
        team: Option<&str>,
        agent: &str,
        handle: &str,
        cwd: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO launch_owner (token, org, team, agent, handle, cwd, registered_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![token, org, team, agent, handle, cwd, chrono::Utc::now().timestamp_millis()],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// The launch a telemetry token belongs to, if it was registered.
    pub(crate) fn launch_owner(&self, token: &str) -> Result<Option<LaunchOwner>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT org, team, agent, handle, cwd FROM launch_owner WHERE token = ?1")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![token], |r| {
                Ok(LaunchOwner {
                    org: r.get(0)?,
                    team: r.get(1)?,
                    agent: r.get(2)?,
                    handle: r.get(3)?,
                    cwd: r.get(4)?,
                })
            })
            .map_err(db_err)?;
        rows.next().transpose().map_err(db_err)
    }

    /// Attribute a session to the launch that reported it, unless it is already attributed. A session
    /// that was attributed at launch keeps that attribution.
    /// Attribute a session to the launch whose telemetry `token` exported it. The first attribution
    /// stands; a session attributed at launch first gets the launch id filled in on its first export.
    fn attribute_if_absent(&self, session_id: &str, owner: &LaunchOwner, token: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO session_attribution (session_id, org, team, agent, handle, cwd, attributed_at_ms, launch_id) VALUES (?1, ?2, ?3, ?4, COALESCE((SELECT handle FROM launch_owner WHERE token = ?8), ?5), ?6, ?7, ?8)",
                params![session_id, owner.org, owner.team, owner.agent, owner.handle, owner.cwd, chrono::Utc::now().timestamp_millis(), token],
            )
            .map_err(db_err)?;
        self.conn
            .execute(
                // Only the owner this launch wrote gets the link: a session already owned by another
                // launch keeps its owner and gets no link from this token.
                "UPDATE session_attribution SET launch_id = ?2
                 WHERE session_id = ?1 AND launch_id IS NULL AND org = ?3 AND agent = ?4 AND handle = ?5",
                params![session_id, token, owner.org, owner.agent, owner.handle],
            )
            .map_err(db_err)?;
        // A managed launch's own session id is its token: the CLI's id joins its record.
        self.link_native_session(token, session_id)
    }

    /// The pull cursor for a remote machine, or `None` before its first pull stores anything.
    pub fn pull_cursor(&self, machine: &str) -> Result<Option<PullCursor>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT epoch, last_seq, incomplete FROM usage_pull_cursor WHERE machine_id = ?1")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![machine], |r| {
                Ok(PullCursor {
                    epoch: r.get(0)?,
                    last_seq: r.get::<_, i64>(1)? as u64,
                    incomplete: r.get::<_, i64>(2)? != 0,
                })
            })
            .map_err(db_err)?;
        rows.next().transpose().map_err(db_err)
    }

    /// Starts `machine`'s pull over under `epoch`, with its cursor back at zero. Its rows stay.
    pub fn reset_pull_cursor(&mut self, machine: &str, epoch: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO usage_pull_cursor (machine_id, epoch, last_seq, incomplete) VALUES (?1, ?2, 0, 0)
                 ON CONFLICT (machine_id) DO UPDATE SET epoch = excluded.epoch, last_seq = 0, incomplete = 0",
                params![machine, epoch],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Stores one page of `machine`'s spool: its usage rows and the cursor past them, in one
    /// transaction. `gap` marks the machine incomplete. Attribution is written first; it is
    /// idempotent, so a crash between the two is repaired by the next pull.
    pub fn apply_pull_page(&mut self, machine: &str, page: &UsageSpoolRead, gap: bool) -> Result<usize, String> {
        let mut events = Vec::new();
        for record in &page.records {
            let export = serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [record.record]}]}]});
            let mapped = telemetry::events_from_logs(&export);
            if let Some(owner) = self.launch_owner(&record.token)? {
                for session in mapped.iter().map(|e| e.session_id.as_str()).collect::<BTreeSet<_>>() {
                    self.attribute_if_absent(session, &owner, &record.token)?;
                }
            }
            events.extend(mapped);
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        ingest_rows(&tx, machine, &events)?;
        if let Some(last) = page.records.last() {
            tx.execute(
                "INSERT INTO usage_pull_cursor (machine_id, epoch, last_seq, incomplete) VALUES (?1, ?2, ?3, 0)
                 ON CONFLICT (machine_id) DO UPDATE SET epoch = excluded.epoch, last_seq = excluded.last_seq",
                params![machine, page.epoch, last.seq as i64],
            )
            .map_err(db_err)?;
        }
        if gap {
            tx.execute(
                "INSERT INTO usage_pull_cursor (machine_id, epoch, last_seq, incomplete) VALUES (?1, ?2, 0, 1)
                 ON CONFLICT (machine_id) DO UPDATE SET incomplete = 1",
                params![machine, page.epoch],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        spend_caps::changed();
        Ok(events.len())
    }

    /// Store usage events. Each is priced at its day's rate unless the CLI reported a cost. A
    /// repeated (CLI, event id) replaces the earlier row, so re-reading a source never counts twice.
    pub fn ingest(&mut self, events: &[UsageEvent]) -> Result<(), String> {
        self.ingest_for("", events)
    }

    /// Store usage events from `machine` (`""` for this Mac). Rows are keyed by machine, so the same
    /// event id from two machines is two events.
    pub fn ingest_for(&mut self, machine: &str, events: &[UsageEvent]) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        ingest_rows(&tx, machine, events)?;
        tx.commit().map_err(db_err)
    }

    /// Spend from the same priced event rows and local day keys used by the usage report.
    /// A partial priced sum is retained even when another row cannot yet be priced.
    pub fn spend_for(&self, scope: SpendScope<'_>, window: SpendWindow) -> Result<Spend, String> {
        #[cfg(test)]
        SPEND_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
        let (since, until) = window.days()?;
        let (sql, lower, upper) = spend_sql(scope);
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let mut rows = if matches!(scope, SpendScope::Agent(_)) {
            stmt.query(params![since, until, lower]).map_err(db_err)?
        } else {
            stmt.query(params![since, until, lower, upper])
                .map_err(db_err)?
        };
        let row = match rows.next().map_err(db_err)? {
            Some(row) => row,
            None => return Err(db_err(rusqlite::Error::QueryReturnedNoRows)),
        };
        let mut spend = Spend::default();
        spend.cost_usd = row.get::<_, Option<f64>>(0).map_err(db_err)?.unwrap_or(0.0);
        let models_json: String = row
            .get::<_, Option<String>>(1)
            .map_err(db_err)?
            .unwrap_or_else(|| "[]".to_string());
        let mut models: Vec<String> = serde_json::from_str(&models_json).map_err(|e| e.to_string())?;
        models.sort();
        spend.unpriced_models = models;
        Ok(spend)
    }

    /// Replace the provisional frontend handle, including usage that arrived before bind.
    pub(crate) fn bind_launch(&mut self, token: &str, handle: &str) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE launch_owner SET handle = ?2 WHERE token = ?1",
            params![token, handle],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE session_attribution SET handle = ?2 WHERE launch_id = ?1",
            params![token, handle],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// The stored usage that `filter` allows, grouped into sessions, priced and totalled. No
    /// session is tied to a story; [`Self::report_with`] ties them.
    pub fn report(&self, filter: &Filter) -> Result<UsageReport, String> {
        self.report_with(filter, &mut StoryResolver::default())
    }

    /// [`Self::report`], with each session's story from `stories`.
    pub fn report_with(&self, filter: &Filter, stories: &mut StoryResolver) -> Result<UsageReport, String> {
        self.report_with_stored(filter, stories, &mut None)
    }

    /// [`Self::report_with`] over the stored story links in `stored`, read now when it is `None`
    /// and kept there, so a caller making many reports (a sweep of sources) reads them once.
    pub(crate) fn report_with_stored(
        &self,
        filter: &Filter,
        stories: &mut StoryResolver,
        stored: &mut Option<StoredStories>,
    ) -> Result<UsageReport, String> {
        count_full_report(self, filter);
        let since_day = filter.since_ms.map(checked_day).transpose()?;
        let until_day = filter.until_ms.map(checked_day).transpose()?;
        if let (Some(since), Some(until)) = (&since_day, &until_day) {
            if since >= until {
                return Err("invalid: sinceMs and untilMs must fall on different local days (the window covers whole days)".to_string());
            }
        }
        if filter.limit == Some(0) {
            return Err("invalid: limit must be at least 1".to_string());
        }
        let after = parse_cursor(filter)?;
        let event_ids = filter.session_id.as_deref().map(|id| self.event_session_ids(id)).transpose()?;
        let event_ids_json = event_ids.map(|ids| serde_json::to_string(&ids)).transpose().map_err(|e| e.to_string())?;
        // A launch can emit several native CLI sessions (for example after /clear).
        // Resolve their trusted attribution once for both stored links and launch records.
        // Older launch records copied reserved non-story handles into story_id. Exclude
        // only that handle-derived value; real stories on those launches still stand.
        let mut stmt = rusqlite::Connection::prepare(&self.conn, &usage_report_sql(Option::is_some(&event_ids_json)))
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![since_day, until_day, event_ids_json, filter.cwd], |r| {
                let cli: String = r.get(0)?;
                // A row of a CLI this build does not know (one a newer build recorded) is
                // left out, not counted as another CLI's.
                let Ok(cli) = cli.parse() else {
                    return Ok(None);
                };
                Ok(Some(StoredRow {
                    cli,
                    session_id: r.get(1)?,
                    day: r.get(2)?,
                    cwd: r.get(3)?,
                    at_ms: r.get(4)?,
                    model: r.get(5)?,
                    tokens: Tokens {
                        input: r.get(6)?,
                        output: r.get(7)?,
                        cache_read: r.get(8)?,
                        cache_write: r.get(9)?,
                        cache_write_1h: r.get(14)?,
                    },
                    cost_usd: r.get(10)?,
                    org: r.get(11)?,
                    team: r.get(12)?,
                    agent: r.get(13)?,
                    handle: r.get(15)?,
                    machine_id: r.get(16)?,
                    run_id: r.get(17)?,
                    story_id: r.get(18)?,
                    launch_session_id: r.get(19)?,
                }))
            })
            .map_err(db_err)?;

        let mut by_session: HashMap<(AgentCli, String), Acc> = HashMap::new();
        let mut unknown = 0usize;
        for row in rows {
            let Some(row) = row.map_err(db_err)? else {
                unknown += 1;
                continue;
            };
            let acc = by_session
                .entry((row.cli, row.session_id.clone()))
                .or_insert_with(|| Acc::new(row.at_ms));
            // Days are kept per session, so a story filter leaves out the days of the
            // sessions it drops.
            let day = acc.days.entry(row.day.clone()).or_default();
            day.tokens.add(&row.tokens);
            match row.cost_usd {
                Some(c) => day.cost += c,
                None => {
                    day.unpriced.insert(row.model.clone());
                }
            }
            acc.cwd = acc.cwd.take().or(row.cwd);
            acc.org = acc.org.take().or(row.org);
            acc.team = acc.team.take().or(row.team);
            acc.agent = acc.agent.take().or(row.agent);
            acc.handle = acc.handle.take().or(row.handle);
            acc.run_id = acc.run_id.take().or(row.run_id);
            acc.launch_session_id = acc.launch_session_id.take().or(Some(row.launch_session_id));
            if acc.story_id.is_none() {
                acc.story_id = row.story_id;
            }
            acc.remote |= !row.machine_id.is_empty();
            if acc.machine.as_ref().is_none_or(|(at, id)| (row.at_ms, &row.machine_id) > (*at, id)) {
                acc.machine = Some((row.at_ms, row.machine_id));
            }
            acc.first_at_ms = acc.first_at_ms.min(row.at_ms);
            acc.last_at_ms = acc.last_at_ms.max(row.at_ms);
            acc.messages += 1;
            acc.tokens.add(&row.tokens);
            acc.models.insert(row.model.clone());
            cost_rollup::CostRollup::add(&mut acc.rollup, &row.model, row.cost_usd);
        }
        if unknown > 0 {
            log::warn!(
                target: "session_usage",
                "left {unknown} usage rows of CLIs this build does not know out of the report"
            );
        }

        let want_story = filter.story.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let fresh = if Option::is_none(stored) { Some(self.stored_stories()?) } else { None };
        let stored = Option::get_or_insert_with(stored, || Option::unwrap_or_default(fresh));
        let mut by_day: HashMap<String, DayAcc> = HashMap::new();
        let mut sessions: Vec<SessionUsage> = Vec::with_capacity(by_session.len());
        for ((cli, session_id), acc) in by_session {
            // The stored link first (the worktree the session's usage was recorded in), then a
            // story worktree holding its folder now (kept, so it outlives the worktree), then
            // the story its durable launch record names, or the live launch's story.
            let story = if let Some((id, path)) = stored
                .get(acc.launch_session_id.as_deref().unwrap_or(&session_id))
                .or_else(|| stored.get(&session_id))
            {
                Some(stories.story(id, path.as_deref().map(Path::new)))
            } else if let Some(link) = acc.cwd.as_deref().and_then(|cwd| stories.worktree_link(cwd)) {
                // Kept only for usage recorded on this Mac: another machine's folders are not
                // this Mac's worktrees, so its sessions stay on the live lookup.
                if !acc.remote {
                    if let Err(e) = self.link_story(&session_id, &link) {
                        log::warn!(target: "session_usage", "story link not kept: {e}");
                    }
                }
                Some(stories.story(&link.story_id, Some(&link.source)))
            } else if let Some(id) = &acc.story_id {
                let source = stories.source_in_org(id, acc.org.as_deref());
                Some(stories.story(id, source.as_deref()))
            } else if let Some((id, source)) = acc
                .handle
                .as_deref()
                .and_then(|h| stories.launch_story(h, acc.org.as_deref()))
            {
                Some(stories.story(&id, source.as_deref()))
            } else {
                // SV-CDE-85: a context worktree named for the story ties the session to it, but
                // only by association, and the link is not kept.
                acc.cwd.as_deref().and_then(|cwd| stories.named_story(cwd)).map(|(id, source)| {
                    UsageStory { basis: Some("associated"), ..stories.story(&id, Some(&source)) }
                })
            };
            if let Some(want) = want_story {
                if !story.as_ref().is_some_and(|s| s.id.eq_ignore_ascii_case(want)) {
                    continue;
                }
            }
            for (day, d) in acc.days {
                let into = by_day.entry(day).or_default();
                into.tokens.add(&d.tokens);
                into.cost += d.cost;
                into.unpriced.extend(d.unpriced);
            }
            sessions.push(SessionUsage {
                cli,
                session_id,
                org: acc.org,
                team: acc.team,
                agent: acc.agent,
                machine_id: match acc.machine {
                    Some((_, id)) if !id.is_empty() => id,
                    _ => crate::commands::machines::LOCAL_MACHINE_ID.to_string(),
                },
                cwd: acc.cwd,
                story,
                models: acc.models.into_iter().collect(),
                first_at_ms: acc.first_at_ms,
                last_at_ms: acc.last_at_ms,
                messages: acc.messages,
                tokens: acc.tokens,
                cost_usd: cost_rollup::CostRollup::total(&acc.rollup),
                unpriced_models: acc.rollup.unpriced(),
                run_id: acc.run_id,
            });
        }
        sessions.sort_by(|a, b| {
            b.last_at_ms
                .cmp(&a.last_at_ms)
                .then(a.session_id.cmp(&b.session_id))
        });
        let mut days: Vec<DayUsage> = by_day
            .into_iter()
            .map(|(day, acc)| DayUsage {
                day,
                tokens: acc.tokens,
                cost_usd: acc.cost,
                unpriced_models: acc.unpriced.into_iter().collect(),
            })
            .collect();
        days.sort_by(|a, b| b.day.cmp(&a.day));

        let mut totals = UsageTotals {
            tokens: Tokens::default(),
            cost_usd: 0.0,
            priced_sessions: 0,
            unpriced_sessions: 0,
        };
        for session in &sessions {
            totals.tokens.add(&session.tokens);
            match session.cost_usd {
                Some(c) => {
                    totals.cost_usd += c;
                    totals.priced_sessions += 1;
                }
                None => totals.unpriced_sessions += 1,
            }
        }
        let session_count = sessions.len();
        if let Some((at, id)) = &after {
            // The sort is (lastAtMs desc, sessionId asc): keep what sorts after the cursor's row.
            // A session whose lastAtMs rose may have moved above the cursor and be skipped by this
            // walk, but none is repeated; the reader recovers by walking from page 1 through
            // continuation and merging by sessionId.
            sessions.retain(|s| {
                s.last_at_ms < *at || (s.last_at_ms == *at && s.session_id.as_str() > id.as_str())
            });
        }
        let mut next_cursor = None;
        if let Some(limit) = filter.limit {
            if sessions.len() > limit {
                sessions.truncate(limit);
                next_cursor = sessions.last().map(|last| cursor_after(cursor_key(filter), last));
            }
        }
        Ok(UsageReport {
            sessions,
            days,
            totals,
            session_count,
            next_cursor,
            unavailable_reason: None,
            cursor_key: cursor_key(filter),
        })
    }
}

/// Counts of what a store has done, by store file (tests only).
#[cfg(test)]
struct Tally(std::sync::Mutex<std::collections::BTreeMap<String, usize>>);

#[cfg(test)]
impl Tally {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(std::collections::BTreeMap::new()))
    }

    fn bump(&self, store: &UsageStore) {
        let db = store.conn.path().unwrap_or_default().to_owned();
        *self.0.lock().unwrap().entry(db).or_default() += 1;
    }

    /// Matched by the store's folder and file name, as SQLite may report the path through a
    /// resolved link.
    fn of(&self, db: &Path) -> usize {
        let tail = db.iter().rev().take(2).collect::<Vec<_>>();
        let counts = self.0.lock().unwrap();
        counts
            .iter()
            .filter(|(path, _)| Path::new(path).iter().rev().take(2).collect::<Vec<_>>() == tail)
            .map(|(_, n)| n)
            .sum()
    }
}

/// Reports over every usage event (no session filter).
#[cfg(test)]
static FULL_REPORTS: Tally = Tally::new();
/// Reports over one session's usage events.
#[cfg(test)]
static SESSION_REPORTS: Tally = Tally::new();
/// Reads of every stored story link.
#[cfg(test)]
static STORED_READS: Tally = Tally::new();

/// How many reports over every usage event the store at `db` has made (tests only).
#[cfg(test)]
pub(crate) fn full_reports(db: &Path) -> usize {
    FULL_REPORTS.of(db)
}

/// How many reports over one session's usage events the store at `db` has made (tests only).
#[cfg(test)]
pub(crate) fn session_reports(db: &Path) -> usize {
    SESSION_REPORTS.of(db)
}

/// How many times the store at `db` has read every stored story link (tests only).
#[cfg(test)]
pub(crate) fn stored_reads(db: &Path) -> usize {
    STORED_READS.of(db)
}

#[cfg(test)]
fn count_full_report(store: &UsageStore, filter: &Filter) {
    let tally = if filter.session_id.is_none() { &FULL_REPORTS } else { &SESSION_REPORTS };
    tally.bump(store);
}
#[cfg(test)]
fn count_stored_read(store: &UsageStore) {
    STORED_READS.bump(store);
}
#[cfg(not(test))]
fn count_stored_read(_store: &UsageStore) {}
#[cfg(not(test))]
fn count_full_report(_store: &UsageStore, _filter: &Filter) {}

/// The statement behind [`UsageStore::report_with`]. `by_session` binds `?3` to the session ids to
/// read and filters them inside the CTE, a plain `IN` so `usage_event_by_session` serves it and
/// no other session's events are touched; otherwise `?3` is unused and every event is read.
fn usage_report_sql(by_session: bool) -> String {
    let (inner, outer) = if by_session {
        ("WHERE e.session_id IN (SELECT value FROM json_each(?3))", "1")
    } else {
        ("", "?3 IS NULL")
    };
    format!(
        "WITH attributed_usage AS (
            SELECT e.*, a.org, a.team, a.agent, a.handle, COALESCE(e.cwd, a.cwd) AS launch_cwd,
                   COALESCE(a.launch_id,
                            (SELECT r.session_id FROM session_record r WHERE r.native_session_id = e.session_id),
                            e.session_id) AS launch_session_id
            FROM usage_event e
            LEFT JOIN session_attribution a ON a.session_id = e.session_id
            {inner}
         )
         SELECT e.cli, e.session_id, e.day, e.launch_cwd, e.at_ms, e.model, e.input, e.output, e.cache_read, e.cache_write, e.cost_usd,
                e.org, e.team, e.agent, e.cache_write_1h, e.handle, e.machine_id, r.run_id,
                CASE WHEN r.story_id = r.handle AND (r.handle GLOB 'routine:*@*' OR r.handle GLOB '_worker/*') THEN NULL ELSE r.story_id END,
                e.launch_session_id
         FROM attributed_usage e
         LEFT JOIN session_record r ON r.session_id = e.launch_session_id
         WHERE (?1 IS NULL OR e.day >= ?1)
           AND (?2 IS NULL OR e.day < ?2)
           AND ({outer})
           AND (?4 IS NULL OR e.launch_cwd = ?4)"
    )
}

/// Each session's stored story link: the story id and its file.
pub(crate) type StoredStories = HashMap<String, (String, Option<String>)>;

/// The name a model's rate is looked up and corrected under: the model without a `provider/`
/// namespace (a multi-provider CLI may report one), with dots read as dashes.
pub(crate) fn model_key(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).replace('.', "-")
}

/// [`model_key`] as an SQL expression over `column`, for the queries that select the usage a rate covers.
pub(crate) fn model_key_sql(column: &str) -> String {
    format!("replace(substr({column}, length(rtrim({column}, replace({column}, '/', ''))) + 1), '.', '-')")
}

/// A usage row a rate may price: machine, CLI, event id, model, day, and its four token counts.
type UnpricedRow = (String, String, String, String, String, u64, u64, u64, u64, u64);

/// Prices the usage a rate for `prefix` from day `from` covers: unpriced usage, and usage priced
/// from the rate table. A cost the CLI reported is never changed. Each row takes the rate in force on
/// its own day, so a longer prefix still wins. Returns how many costs changed.
fn reprice_covered(tx: &rusqlite::Transaction<'_>, prefix: &str, from: &str) -> Result<usize, String> {
    let unpriced: Vec<UnpricedRow> = {
        let key = model_key_sql("model");
        let mut stmt = tx
            .prepare(&format!(
                "SELECT machine_id, cli, event_id, model, day, input, output, cache_read, cache_write, cache_write_1h FROM usage_event
                 WHERE (cost_usd IS NULL OR cost_from = 'rate')
                   AND ({key} = ?1 OR {key} LIKE ?1 || '-%')
                   AND day >= ?2"
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![prefix, from], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)?
    };
    let mut priced = 0;
    for (machine, cli, event_id, model, day, input, output, cache_read, cache_write, cache_write_1h) in unpriced {
        let tokens = Tokens { input, output, cache_read, cache_write, cache_write_1h };
        if let Some(rate) = rate_on(tx, &model, &day)? {
            // Counts only rows whose cost changed, so a rate equal to the one already in
            // force reports nothing.
            priced += tx
                .execute(
                    "UPDATE usage_event SET cost_usd = ?1, cost_from = 'rate'
                     WHERE machine_id = ?4 AND cli = ?2 AND event_id = ?3 AND (cost_usd IS NOT ?1 OR cost_from IS NOT 'rate')",
                    params![rate.price(&tokens), cli, event_id, machine],
                )
                .map_err(db_err)?;
        }
    }
    Ok(priced)
}

/// How many records one pull reads at a time.
const PULL_PAGE: usize = 500;
/// How many times a pull starts over because the remote spool's epoch changed, before it gives up.
const MAX_RESTARTS: u32 = 2;

/// Where a pull of one remote machine's spool has got to: the spool epoch it reads, the last
/// sequence number stored, and whether records were lost before a pull reached them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullCursor {
    pub epoch: String,
    pub last_seq: u64,
    pub incomplete: bool,
}

/// Pulls one machine's usage spool into the store, a page at a time, from where its cursor stands.
/// `read(after, limit)` returns the spool's records past `after`. A new epoch means the remote lost
/// its spool, so the pull starts over from zero; rows already stored are replaced, not counted twice.
/// Returns how many events were stored.
pub fn pull_machine(
    store: &mut UsageStore,
    machine: &str,
    mut read: impl FnMut(u64, usize) -> Result<UsageSpoolRead, String>,
) -> Result<usize, String> {
    let mut cursor = store.pull_cursor(machine)?;
    let mut after = cursor.as_ref().map_or(0, |c| c.last_seq);
    let mut restarts = 0;
    let mut stored = 0;
    loop {
        let page = read(after, PULL_PAGE)?;
        if cursor.as_ref().is_some_and(|c| c.epoch != page.epoch) {
            restarts += 1;
            if restarts > MAX_RESTARTS {
                return Err("the remote usage spool keeps changing; the pull was stopped".into());
            }
            store.reset_pull_cursor(machine, &page.epoch)?;
            cursor = store.pull_cursor(machine)?;
            after = 0;
            continue;
        }
        // The spool's oldest record is past the next one this pull needs: records in between were
        // pruned before the pull reached them.
        let gap = page.oldest_seq.is_some_and(|oldest| oldest > after + 1);
        if page.records.is_empty() {
            if gap {
                store.apply_pull_page(machine, &page, true)?;
            }
            return Ok(stored);
        }
        stored += store.apply_pull_page(machine, &page, gap)?;
        after = page.records.last().map_or(after, |r| r.seq);
        cursor = store.pull_cursor(machine)?;
    }
}

/// Writes usage events into `tx`, from `machine` (`""` for this Mac). A later read's cost replaces an
/// earlier one, and a rate-table cost is repriced by a later rate, as the store's upsert rules say.
fn ingest_rows(tx: &rusqlite::Transaction<'_>, machine: &str, events: &[UsageEvent]) -> Result<(), String> {
    for e in events {
        let day = match checked_day(e.at_ms) {
            Ok(day) => day,
            Err(_) => {
                log::warn!(target: "session_usage", "usage event {} has a time outside the calendar; skipped", e.event_id);
                continue;
            }
        };
        let (cost, cost_from) = match e.reported_cost_usd {
            Some(c) => (Some(c), Some("reported")),
            // A call with no tokens costs nothing, whatever its model is priced at.
            None if e.tokens.input == 0 && e.tokens.output == 0 && e.tokens.cache_read == 0 && e.tokens.cache_write == 0 => {
                (Some(0.0), Some("rate"))
            }
            None => match rate_on(tx, &e.model, &day)? {
                Some(rate) => (Some(rate.price(&e.tokens)), Some("rate")),
                None => (None, None),
            },
        };
        tx.execute(
            "INSERT INTO usage_event (machine_id, cli, event_id, session_id, day, at_ms, cwd, model, input, output, cache_read, cache_write, cost_usd, cost_from, cache_write_1h) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT (machine_id, cli, event_id) DO UPDATE SET
                session_id = excluded.session_id, day = excluded.day, at_ms = excluded.at_ms,
                cwd = COALESCE(excluded.cwd, usage_event.cwd), model = excluded.model, input = excluded.input,
                output = excluded.output, cache_read = excluded.cache_read,
                cache_write = excluded.cache_write, cache_write_1h = excluded.cache_write_1h,
                -- A later read's cost replaces an earlier one, since an earlier read can be a
                -- partial turn. A positive cost the CLI reported is never replaced by a rate-table
                -- cost. A stored zero is unknown: a later read with no price leaves it unknown, not
                -- free. A schema 1 row has no recorded source, so a later read prices it again.
                cost_from = CASE
                    WHEN usage_event.cost_from = 'reported' AND usage_event.cost_usd > 0
                        AND excluded.cost_from IS NOT 'reported' THEN usage_event.cost_from
                    WHEN excluded.cost_from IS NULL AND usage_event.cost_usd > 0 THEN usage_event.cost_from
                    ELSE excluded.cost_from END,
                cost_usd = CASE
                    WHEN usage_event.cost_from = 'reported' AND usage_event.cost_usd > 0
                        AND excluded.cost_from IS NOT 'reported' THEN usage_event.cost_usd
                    WHEN excluded.cost_from IS NULL AND usage_event.cost_usd > 0 THEN usage_event.cost_usd
                    ELSE excluded.cost_usd END",
            params![
                machine,
                e.cli.as_str(),
                e.event_id,
                e.session_id,
                day,
                e.at_ms,
                e.cwd,
                e.model,
                e.tokens.input,
                e.tokens.output,
                e.tokens.cache_read,
                e.tokens.cache_write,
                cost,
                cost_from,
                e.tokens.cache_write_1h,
            ],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

/// The rate in effect for `model` on local `day`: the longest matching prefix, latest effective day.
/// A `-free` variant is never priced at its paid model's rate, only at a rate of its own.
fn rate_on(conn: &Connection, model: &str, day: &str) -> Result<Option<Rate>, String> {
    let model = model_key(model);
    let mut stmt = conn
        .prepare(
            "SELECT input_per_mtok, output_per_mtok, cache_read_per_mtok,
                    long_from_tokens, long_input_per_mtok, long_output_per_mtok, long_cache_read_per_mtok
             FROM model_rate
             WHERE (?1 = model_prefix OR ?1 LIKE model_prefix || '-%') AND effective_from <= ?2
               AND (?1 NOT LIKE '%-free' OR model_prefix LIKE '%-free')
             ORDER BY length(model_prefix) DESC, effective_from DESC LIMIT 1",
        )
        .map_err(db_err)?;
    let mut rows = stmt
        .query_map(params![model, day], |r| {
            let long = match (r.get::<_, Option<i64>>(3)?, r.get::<_, Option<f64>>(4)?, r.get::<_, Option<f64>>(5)?) {
                (Some(from), Some(input), Some(output)) => Some(LongRate {
                    from_tokens: from.max(0) as u64,
                    input,
                    output,
                    cache_read: r.get(6)?,
                }),
                _ => None,
            };
            Ok(Rate {
                input: r.get(0)?,
                output: r.get(1)?,
                cache_read: r.get(2)?,
                long,
            })
        })
        .map_err(db_err)?;
    rows.next().transpose().map_err(db_err)
}

/// Who a launch belongs to, including a personal launch with no org.
pub(crate) struct LaunchOwner {
    org: String,
    team: Option<String>,
    agent: String,
    handle: String,
    cwd: Option<String>,
}

impl LaunchOwner {
    pub(crate) fn new(
        org: Option<String>,
        team: Option<String>,
        agent: String,
        handle: String,
        cwd: Option<String>,
    ) -> Self {
        Self {
            org: org.unwrap_or_default(),
            team,
            agent,
            handle,
            cwd,
        }
    }

    pub(crate) fn register_at(&self, path: &Path, token: &str) -> Result<(), String> {
        UsageStore::open(path)?.register_launch(
            token,
            &self.org,
            self.team.as_deref(),
            &self.agent,
            &self.handle,
            self.cwd.as_deref(),
        )
    }

    pub(crate) async fn register(self, token: String) {
        self.register_from(store_path(), token).await;
    }

    async fn register_from(self, path: Result<std::path::PathBuf, String>, token: String) {
        let result = tokio::task::spawn_blocking(move || self.register_at(&path?, &token)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!(target: "session_usage", "launch owner not recorded: {e}"),
            Err(e) => log::warn!(target: "session_usage", "launch owner task failed: {e}"),
        }
    }

    /// The pre-spawn boundary: register the same token the CLI will export under.
    pub(crate) async fn prepare_export(
        self,
        path: Result<std::path::PathBuf, String>,
        token: &str,
        export: Option<&codesense_pty_core::agent_cli::TelemetryExport>,
        api_port: Option<u16>,
        env: &mut Vec<(String, String)>,
        args: &mut Vec<String>,
    ) {
        self.register_from(path, token.to_string()).await;
        telemetry::configure_export(export, api_port, token, env, args);
    }
}

struct StoredRow {
    cli: AgentCli,
    session_id: String,
    day: String,
    cwd: Option<String>,
    at_ms: i64,
    model: String,
    tokens: Tokens,
    cost_usd: Option<f64>,
    org: Option<String>,
    team: Option<String>,
    agent: Option<String>,
    /// The launch handle the session is attributed to.
    handle: Option<String>,
    /// The machine the row came from; empty for this Mac.
    machine_id: String,
    run_id: Option<String>,
    story_id: Option<String>,
    launch_session_id: String,
}

/// Per-session running totals while the stored rows are folded in.
struct Acc {
    org: Option<String>,
    team: Option<String>,
    agent: Option<String>,
    handle: Option<String>,
    run_id: Option<String>,
    story_id: Option<String>,
    launch_session_id: Option<String>,
    /// Any of its usage came from another machine.
    remote: bool,
    /// The time and machine (`""` for this Mac) of its latest call.
    machine: Option<(i64, String)>,
    cwd: Option<String>,
    days: HashMap<String, DayAcc>,
    first_at_ms: i64,
    last_at_ms: i64,
    messages: u32,
    tokens: Tokens,
    models: BTreeSet<String>,
    /// The session's cost, by the one calculation `session_read` and the story totals share.
    rollup: cost_rollup::CostRollup,
}

impl Acc {
    fn new(at_ms: i64) -> Self {
        Self {
            org: None,
            team: None,
            agent: None,
            handle: None,
            run_id: None,
            story_id: None,
            launch_session_id: None,
            remote: false,
            machine: None,
            cwd: None,
            days: HashMap::new(),
            first_at_ms: at_ms,
            last_at_ms: at_ms,
            messages: 0,
            tokens: Tokens::default(),
            models: BTreeSet::new(),
            rollup: cost_rollup::CostRollup::new(),
        }
    }
}

#[derive(Default)]
struct DayAcc {
    tokens: Tokens,
    cost: f64,
    unpriced: BTreeSet<String>,
}

/// Record who ran a launched session (org, team and saved agent), in the same store the reader uses.
/// The launch path calls this once per fresh session that has an org.
pub fn record_attribution(
    session_id: &str,
    org: &str,
    team: Option<&str>,
    agent: &str,
    handle: &str,
    cwd: Option<&str>,
) -> Result<(), String> {
    UsageStore::open(&store_path()?)?.attribute(session_id, org, team, agent, handle, cwd)
}

/// [`record_attribution`] for a launch, off the async runtime. A failure is logged and the session
/// runs on, unattributed.
pub async fn attribute_launch(
    session_id: String,
    org: String,
    team: Option<String>,
    agent: String,
    handle: String,
    cwd: Option<String>,
) {
    let result = tokio::task::spawn_blocking(move || {
        record_attribution(&session_id, &org, team.as_deref(), &agent, &handle, cwd.as_deref())
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!(target: "session_usage", "launch attribution not recorded: {e}"),
        Err(e) => log::warn!(target: "session_usage", "launch attribution task failed: {e}"),
    }
}

/// The per-launch export, stored into the store at `path`. The receiver calls [`ingest_telemetry_for`].
pub fn ingest_telemetry_for_at(path: &Path, token: &str, export: &serde_json::Value) -> Result<usize, String> {
    for (at_ms, signal) in telemetry::codex_timing_signals(export) {
        timing::note(token, at_ms, signal);
    }
    let events = telemetry::events_from_logs(export);
    store_launch_events(path, token, &events)?;
    Ok(events.len())
}

/// Store `events` that the launch whose telemetry token is `token` made: attribute their sessions
/// to that launch unless they already are, store the rows, and link each new session to the story
/// worktree it ran in. A launch's OTLP export and its session log both come through here.
pub(crate) fn store_launch_events(path: &Path, token: &str, events: &[UsageEvent]) -> Result<(), String> {
    if !events.is_empty() {
        let mut store = UsageStore::open(path)?;
        if let Some(owner) = store.launch_owner(token)? {
            for session in events.iter().map(|e| e.session_id.as_str()).collect::<BTreeSet<_>>() {
                store.attribute_if_absent(session, &owner, token)?;
            }
        }
        store.ingest(events)?;
        // Keep the story worktree each new session ran in, so it keeps its story after the
        // worktree is removed. The store sits in the CodeSense home.
        let mut cwds: Vec<(String, String)> = events
            .iter()
            .filter_map(|e| Some((e.session_id.clone(), e.cwd.clone()?)))
            .collect();
        cwds.sort();
        cwds.dedup_by(|a, b| a.0 == b.0);
        if let (false, Some(home)) = (cwds.is_empty(), path.parent()) {
            if let Err(e) = store.link_stories(&cwds, || StoryResolver::for_home(home, HashMap::new())) {
                log::warn!(target: "session_usage", "story links not kept: {e}");
            }
        }
    }
    Ok(())
}

/// A rate accepted in the native app, as `modelRates` and MCP `model_rates` show in `source`.
pub const SET_BY_USER: &str = "set by the user";
/// An effective rate requested through MCP or the unprivileged page bridge.
pub const SET_BY_AGENT: &str = "set by an agent";

/// Request a model rate from a page or MCP. Spend reductions wait for native acceptance.
pub fn set_model_rate(params: SetModelRateParams) -> Result<usize, String> {
    set_model_rate_at(&store_path()?, params)
}

fn set_model_rate_at(path: &Path, params: SetModelRateParams) -> Result<usize, String> {
    UsageStore::open(path)?.request_rate(params, SET_BY_AGENT)
}

/// Every stored rate: the seeded list prices and the ones the user set.
pub fn model_rates() -> Result<Vec<StoredRate>, String> {
    UsageStore::open(&store_path()?)?.rates()
}

/// Store one OTLP logs export that arrived at a launch's own address. The sessions it reports are
/// attributed to that launch, unless they already are. An unknown token stores the usage unattributed.
pub fn ingest_telemetry_for(token: &str, export: &serde_json::Value) -> Result<usize, String> {
    let count = ingest_telemetry_for_at(&store_path()?, token, export)?;
    spend_caps::changed();
    Ok(count)
}

/// Record who a launch belongs to, under its telemetry token. Off the async runtime, and best effort:
/// a failure is logged and the launch goes on, with its usage unattributed.
pub async fn register_launch_async(
    token: String,
    org: String,
    team: Option<String>,
    agent: String,
    handle: String,
    cwd: Option<String>,
) {
    LaunchOwner::new(Some(org), team, agent, handle, cwd).register(token).await;
}

/// Commit the renderer's handle after a prepared PTY launch has bound.
pub(crate) async fn bind_launch_async(token: String, handle: String) {
    let result = tokio::task::spawn_blocking(move || {
        UsageStore::open(&store_path()?)?.bind_launch(&token, &handle)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!(target: "session_usage", "launch handle not recorded: {e}"),
        Err(e) => log::warn!(target: "session_usage", "launch handle task failed: {e}"),
    }
}

/// The store's file under the CodeSense home.
pub(crate) fn store_path() -> Result<std::path::PathBuf, String> {
    let home = crate::commands::paths::codesense_home_dir().ok_or("no CodeSense home directory")?;
    Ok(home.join("usage.sqlite"))
}

/// The page bridge's and the MCP tool's read: the store, reported through `filter`, each session
/// tied to its story. Claude and Codex usage arrives by telemetry, so it is already stored.
/// `launch_stories` is the sessions registry's story per launch handle ([`launch_stories`]).
pub(crate) fn read_usage_for(filter: &Filter, launch_stories: HashMap<String, String>, scope: &crate::context_repo::caller_scope::CallerScope) -> Result<UsageReport, String> {
    let home = crate::commands::paths::codesense_home_dir().ok_or("no CodeSense home directory")?;
    let mut stories = StoryResolver::for_home(&home, launch_stories).with_scope(scope.clone());
    UsageStore::open(&home.join("usage.sqlite"))?.report_with(filter, &mut stories).map(explained)
}

/// `report`, saying why its sessions are missing when this Mac's managed Claude settings send
/// session usage elsewhere.
fn explained(mut report: UsageReport) -> UsageReport {
    report.unavailable_reason = managed_telemetry::usage_reason().map(str::to_owned);
    report
}

/// The story each launch in the sessions registry was opened for, by launch handle. Empty when
/// the registry is not running.
pub fn launch_stories<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> HashMap<String, String> {
    use tauri::Manager;
    app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .map(|launch| {
            crate::commands::dashboard_launch::DashboardLaunchState::snapshot(&launch)
                .into_iter()
                .filter_map(|s| Some((s.handle, s.meta.story?)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use codesense_pty_core::protocol::UsageSpoolRecord;
    use super::*;

    pub fn store() -> (tempfile::TempDir, UsageStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
        (dir, store)
    }

    /// Noon in the machine's own zone on a local calendar day, so the day it lands on is that day
    /// in every zone.
    pub fn local_noon(y: i32, m: u32, d: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, m, d, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    pub fn event(id: &str, session: &str, model: &str, at_ms: i64, tokens: Tokens) -> UsageEvent {
        UsageEvent {
            cli: AgentCli::Claude,
            event_id: id.into(),
            session_id: session.into(),
            at_ms,
            cwd: Some("/work/a".into()),
            model: model.into(),
            tokens,
            reported_cost_usd: None,
        }
    }

    fn input(n: u64) -> Tokens {
        Tokens {
            input: n,
            ..Tokens::default()
        }
    }

    /// Ingest an export into the store the launch preparation already registered.
    pub(crate) fn assert_launch_attribution(
        path: &Path,
        token: &str,
        cli: AgentCli,
        agent: &str,
        handle: &str,
        org: &str,
        team: Option<&str>,
    ) {
        let record = match cli {
            AgentCli::Claude => Some(remote_record("request", "cli-session")),
            AgentCli::OpenCode => Some(serde_json::json!({"attributes": [
                {"key":"event.name", "value":{"stringValue":"opencode.usage"}},
                {"key":"message_id", "value":{"stringValue":"msg-1"}},
                {"key":"session.id", "value":{"stringValue":"cli-session"}},
                {"key":"event.timestamp", "value":{"stringValue":"2026-10-22T18:00:00Z"}},
                {"key":"model", "value":{"stringValue":"deepseek-v4-1-flash"}},
                {"key":"input", "value":{"intValue":"100"}}
            ]})),
            AgentCli::Codex => Some(serde_json::json!({"attributes": [
                {"key":"event.name", "value":{"stringValue":"codex.sse_event"}},
                {"key":"event.kind", "value":{"stringValue":"response.completed"}},
                {"key":"conversation.id", "value":{"stringValue":"cli-session"}},
                {"key":"event.timestamp", "value":{"stringValue":"2026-10-22T18:00:00Z"}},
                {"key":"model", "value":{"stringValue":"gpt-5-3-codex"}},
                {"key":"input_token_count", "value":{"intValue":"100"}}
            ]})),
            AgentCli::Hermes => None,
        };
        // A usage record exactly for the CLIs whose descriptor exports usage telemetry.
        assert_eq!(
            record.is_some(),
            codesense_pty_core::agent_cli::descriptor(cli)
                .telemetry
                .is_some(),
            "{cli}"
        );
        let Some(record) = record else {
            // Nothing exports usage for it, so its launch attributes no session.
            let report = UsageStore::open(&path)
                .unwrap()
                .report(&Filter {
                    session_id: Some(token.to_string()),
                    ..Default::default()
                })
                .unwrap();
            assert!(report.sessions.is_empty(), "{cli}");
            return;
        };
        let export = serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[record]}]}]});
        assert_eq!(ingest_telemetry_for_at(&path, token, &export).unwrap(), 1);
        let store = UsageStore::open(&path).unwrap();
        let report = store
            .report(&Filter {
                session_id: Some(token.to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.sessions.len(), 1);
        let session = &report.sessions[0];
        assert_eq!(session.agent.as_deref(), Some(agent));
        assert_eq!(session.org.as_deref(), Some(org));
        assert_eq!(session.team.as_deref(), team);
        let recorded_handle: String = store
            .conn
            .query_row(
                "SELECT handle FROM session_attribution WHERE session_id = 'cli-session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded_handle, handle);
        let day = Local
            .timestamp_millis_opt(session.first_at_ms)
            .single()
            .unwrap()
            .date_naive();
        assert_eq!(
            store
                .spend_for(SpendScope::Agent(agent), SpendWindow::Day(day))
                .unwrap()
                .cost_usd,
            session.cost_usd.unwrap()
        );
    }

    #[test]
    fn a_pty_bind_updates_exports_received_before_and_after_the_bind() {
        let (dir, mut store) = store();
        let path = dir.path().join("usage.sqlite");
        store
            .register_launch("token", "", None, "agent", "token", None)
            .unwrap();
        let export = |id, session| serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[remote_record(id, session)]}]}]});
        ingest_telemetry_for_at(&path, "token", &export("before", "s1")).unwrap();
        let before_bind = store.launch_owner("token").unwrap().unwrap();
        store.bind_launch("token", "agent-handle").unwrap();
        store
            .attribute_if_absent("racing", &before_bind, "token")
            .unwrap();
        ingest_telemetry_for_at(&path, "token", &export("after", "s2")).unwrap();
        let handles: Vec<String> = store
            .conn
            .prepare("SELECT handle FROM session_attribution ORDER BY session_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(handles, ["agent-handle", "agent-handle", "agent-handle"]);
    }

    #[test]
    fn spend_counts_both_clis_and_machines_and_keeps_partial_unpriced_costs() {
        let (_dir, mut store) = store();
        let at = local_noon(2026, 10, 22);
        store
            .register_launch(
                "launch",
                "",
                None,
                "personal/alice/worker",
                "routine:daily_check@1",
                None,
            )
            .unwrap();
        let export = serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [remote_record("claude", "claude-session")]}]}]});
        let owner = store.launch_owner("launch").unwrap().unwrap();
        store
            .attribute_if_absent("claude-session", &owner, "launch")
            .unwrap();
        let mut claude = telemetry::events_from_logs(&export).remove(0);
        claude.at_ms = at;
        claude.reported_cost_usd = Some(0.42);
        store.ingest(&[claude]).unwrap();
        store
            .attribute(
                "codex-session",
                "",
                None,
                "personal/alice/worker",
                "routine:daily_check@2",
                None,
            )
            .unwrap();
        store
            .register_launch(
                "tok",
                "",
                None,
                "personal/alice/worker",
                "routine:daily_check@2",
                None,
            )
            .unwrap();
        let codex = serde_json::json!({"attributes": [
            {"key":"event.name", "value":{"stringValue":"codex.sse_event"}},
            {"key":"event.kind", "value":{"stringValue":"response.completed"}},
            {"key":"conversation.id", "value":{"stringValue":"codex-session"}},
            {"key":"event.timestamp", "value":{"stringValue": Local.timestamp_millis_opt(at).single().unwrap().to_rfc3339()}},
            {"key":"model", "value":{"stringValue":"gpt-5-3-codex"}},
            {"key":"input_token_count", "value":{"intValue":"1000000"}}
        ]});
        pull_machine(
            &mut store,
            "remote-box",
            fake_spool("epoch", vec![(1, codex)]),
        )
        .unwrap();
        store
            .ingest(&[event(
                "unknown",
                "claude-session",
                "unpriced-model",
                at,
                input(1),
            )])
            .unwrap();
        store
            .attribute(
                "other",
                "acme",
                None,
                "other-agent",
                "routine:dailyXcheck@3",
                None,
            )
            .unwrap();
        store
            .ingest(&[event(
                "other",
                "other",
                "claude-opus-4-8",
                at,
                input(1_000_000),
            )])
            .unwrap();
        let window = SpendWindow::Day(chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        let spend = store
            .spend_for(SpendScope::Agent("personal/alice/worker"), window)
            .unwrap();
        assert!((spend.cost_usd - 2.17).abs() < 1e-9);
        assert_eq!(spend.unpriced_models, ["unpriced-model"]);
        assert_eq!(
            store
                .spend_for(SpendScope::Routine("daily_check"), window)
                .unwrap(),
            spend
        );
        let json = serde_json::to_value(&spend).unwrap();
        assert_eq!(json["unpricedModels"][0], "unpriced-model");
        assert!(json["costUsd"].is_number());
    }

    #[test]
    fn spend_windows_use_local_days_and_monday_weeks_like_the_report() {
        let (_dir, mut store) = store();
        store
            .attribute("s", "org", None, "agent", "routine:r@1", None)
            .unwrap();
        let midnight = |y, m, d| {
            Local
                .with_ymd_and_hms(y, m, d, 0, 0, 0)
                .single()
                .unwrap()
                .timestamp_millis()
        };
        let monday = midnight(2026, 10, 19);
        let tuesday = midnight(2026, 10, 20);
        let next_monday = midnight(2026, 10, 26);
        for (i, at) in [
            monday - 1,
            monday,
            tuesday - 1,
            tuesday,
            next_monday - 1,
            next_monday,
        ]
        .into_iter()
        .enumerate()
        {
            let mut e = event(&i.to_string(), "s", "claude-opus-4-8", at, input(1));
            e.reported_cost_usd = Some(1.0);
            store.ingest(&[e]).unwrap();
        }
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        assert_eq!(
            store
                .spend_for(SpendScope::Agent("agent"), SpendWindow::Day(day))
                .unwrap()
                .cost_usd,
            2.0
        );
        assert_eq!(
            store
                .spend_for(
                    SpendScope::Agent("agent"),
                    SpendWindow::Week(day.succ_opt().unwrap())
                )
                .unwrap()
                .cost_usd,
            4.0
        );
        let report = store
            .report(&Filter {
                since_ms: Some(monday),
                until_ms: Some(next_monday),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.totals.cost_usd, 4.0);
        assert_eq!(
            store
                .spend_for(SpendScope::Agent("absent"), SpendWindow::Day(day))
                .unwrap()
                .cost_usd,
            0.0
        );
    }

    #[test]
    fn a_repeated_event_counts_once() {
        let (_d, mut store) = store();
        let e = event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(7));
        store.ingest(&[e.clone(), e]).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(r.sessions[0].messages, 1);
        assert_eq!(r.sessions[0].tokens.input, 7);
    }

    #[test]
    fn a_cli_reported_cost_is_used_over_the_rate_table() {
        let (_d, mut store) = store();
        let mut e = event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000));
        e.reported_cost_usd = Some(0.5);
        store.ingest(&[e]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, Some(0.5));
    }

    #[test]
    fn a_seeded_model_is_priced_from_the_rate_table() {
        let (_d, mut store) = store();
        let e = event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000));
        store.ingest(&[e]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn the_seeded_list_prices_price_each_model_family() {
        let (_d, mut store) = store();
        let day = local_noon(2026, 10, 15);
        let million = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 0, cache_write: 0, ..Default::default() };
        // Each model prices at its own list price, not its family's: opus 5.5 is $4/$20, opus 5 $5/$25.
        store.ingest(&[
            event("a", "s-opus-55", "claude-opus-5-5", day, million),
            event("b", "s-opus-5", "claude-opus-5", day, million),
            event("c", "s-sonnet-55", "claude-sonnet-5-5", day, million),
            event("d", "s-codex", "gpt-5.3-codex", day, million),
        ])
        .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let cost = |id: &str| r.sessions.iter().find(|s| s.session_id == id).unwrap().cost_usd.unwrap();
        assert!((cost("s-opus-55") - 24.0).abs() < 1e-9);
        assert!((cost("s-opus-5") - 30.0).abs() < 1e-9);
        assert!((cost("s-sonnet-55") - 12.0).abs() < 1e-9);
        assert!((cost("s-codex") - 15.75).abs() < 1e-9);
    }

    #[test]
    fn a_model_with_its_own_cache_read_price_is_priced_at_it() {
        let (_d, mut store) = store();
        // Under gpt-6.1-sol's 272,000-token long-context threshold.
        let cached = Tokens { input: 0, output: 0, cache_read: 100_000, cache_write: 0, ..Default::default() };
        let day = local_noon(2026, 10, 15);
        store
            .ingest(&[
                // gpt-6.1-sol reads cache at $0.10 (0.05x its $2 input); gpt-6-sol at the usual 0.1x.
                event("a", "s-61", "gpt-6.1-sol", day, cached),
                event("b", "s-6", "gpt-6-sol", day, cached),
            ])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let cost = |id: &str| r.sessions.iter().find(|s| s.session_id == id).unwrap().cost_usd.unwrap();
        assert!((cost("s-61") - 0.01).abs() < 1e-9, "{}", cost("s-61"));
        assert!((cost("s-6") - 0.02).abs() < 1e-9, "{}", cost("s-6"));
    }

    #[test]
    fn a_seed_new_to_a_store_prices_the_usage_it_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            // A store from before the Codex seed: its Codex usage has no price.
            let mut store = UsageStore::open(&path).unwrap();
            store.conn.execute("DELETE FROM model_rate WHERE model_prefix = 'gpt-5-3-codex'", []).unwrap();
            let million = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 0, cache_write: 0, ..Default::default() };
            store.ingest(&[event("c1", "s-codex", "gpt-5.3-codex", local_noon(2026, 10, 15), million)]).unwrap();
            assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, None);
        }
        // Opening it again seeds the rate, and the usage it covers is priced.
        let store = UsageStore::open(&path).unwrap();
        let cost = store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap();
        assert!((cost - 15.75).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn a_one_hour_cache_write_is_priced_at_twice_input() {
        let (_d, mut store) = store();
        let day = local_noon(2026, 10, 15);
        // Opus 4.8 at $5 input: 600k one-hour writes at $10, 400k five-minute ones at $6.25.
        let writes = Tokens { cache_write: 1_000_000, cache_write_1h: 600_000, ..Default::default() };
        store.ingest(&[event("a", "s1", "claude-opus-4-8", day, writes)]).unwrap();
        let cost = store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap();
        assert!((cost - 8.5).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn a_long_prompt_is_priced_at_its_long_context_rate() {
        let (_d, mut store) = store();
        let day = local_noon(2026, 10, 15);
        let input = |n: u64| Tokens { input: n, ..Default::default() };
        store
            .ingest(&[
                // gpt-6.1-sol: $2 up to 272,000 prompt tokens, $4 over.
                event("a", "s-short", "gpt-6.1-sol", day, input(200_000)),
                event("b", "s-long", "gpt-6.1-sol", day, input(300_000)),
                // Haiku 5.5: $0.10 up to 100,000, $0.50 over.
                event("c", "s-haiku-short", "claude-haiku-5-5", day, input(50_000)),
                event("d", "s-haiku-long", "claude-haiku-5-5", day, input(150_000)),
            ])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let cost = |id: &str| r.sessions.iter().find(|s| s.session_id == id).unwrap().cost_usd.unwrap();
        assert!((cost("s-short") - 0.4).abs() < 1e-9, "{}", cost("s-short"));
        assert!((cost("s-long") - 1.2).abs() < 1e-9, "{}", cost("s-long"));
        assert!((cost("s-haiku-short") - 0.005).abs() < 1e-9, "{}", cost("s-haiku-short"));
        assert!((cost("s-haiku-long") - 0.075).abs() < 1e-9, "{}", cost("s-haiku-long"));
    }

    #[test]
    fn a_changed_seed_corrects_its_row_and_reprices_and_a_user_rate_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let mut store = UsageStore::open(&path).unwrap();
            // An earlier CodeSense's seed for gpt-6-sol, priced wrong, and a rate the user set.
            store
                .conn
                .execute(
                    "UPDATE model_rate SET input_per_mtok = 99.0, source = 'seed: an older price' WHERE model_prefix = 'gpt-6-sol'",
                    [],
                )
                .unwrap();
            store.set_rate_and_reprice("gpt-6-luna", "2026-01-01", Rate::flat(7.0, 7.0), "set by the user").unwrap();
            let million = Tokens { input: 1_000_000, ..Default::default() };
            let day = local_noon(2026, 10, 15);
            store
                .ingest(&[event("a", "s-sol", "gpt-6-sol", day, million), event("b", "s-luna", "gpt-6-luna", day, million)])
                .unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let cost = |id: &str| r.sessions.iter().find(|s| s.session_id == id).unwrap().cost_usd.unwrap();
        // The 1M-token prompt is over 272,000, so the corrected seed's long-context $4 applies.
        assert!((cost("s-sol") - 4.0).abs() < 1e-9, "{}", cost("s-sol"));
        // The user's rate from 2026-01-01 still prices its model.
        assert!((cost("s-luna") - 7.0).abs() < 1e-9, "{}", cost("s-luna"));
    }

    #[test]
    fn opencode_usage_counts_once_toward_the_launching_agents_cap() {
        use spend_caps::Budget;
        let (dir, mut usage) = store();
        let path = dir.path().join("usage.sqlite");
        LaunchOwner::new(Some("org".into()), None, "a".into(), "h".into(), None)
            .register_at(&path, "tok")
            .unwrap();
        let text = |key: &str, v: &str| serde_json::json!({"key": key, "value": {"stringValue": v}});
        let record = serde_json::json!({"attributes": [
            text("event.name", "opencode.usage"),
            text("message_id", "msg_1"),
            text("session.id", "ses_1"),
            text("event.timestamp", &chrono::Utc::now().to_rfc3339()),
            text("model", "deepseek-v4-1-flash"),
            {"key": "input", "value": {"intValue": 1000}},
            {"key": "cost", "value": {"doubleValue": 7.0}},
        ]});
        let export = serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[record]}]}]});
        for _ in 0..2 {
            ingest_telemetry_for_at(&path, "tok", &export).unwrap();
        }
        let today = chrono::Local::now().date_naive();
        let spent = usage.spend_for(SpendScope::Agent("a"), SpendWindow::Day(today)).unwrap();
        assert_eq!(spent.cost_usd, 7.0);
        let budget = Budget { per_day: Some(5.0), per_week: None };
        let status = usage.cap_status("agent", "a", budget, today, false).unwrap();
        assert!(status.refusal.is_some());
    }

    #[test]
    fn a_qwen_model_has_no_fallback_rate_the_rate_model_cannot_express() {
        let (_d, mut store) = store();
        let t = Tokens { input: 1_000, ..Default::default() };
        store
            .ingest(&[
                event("q1", "s1", "qwen3-7-plus", local_noon(2026, 10, 15), t),
                event("q2", "s2", "qwen3-8-flash", local_noon(2026, 10, 15), t),
            ])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert!(r.sessions.iter().all(|s| s.cost_usd.is_none()), "{:?}", r.sessions);
    }

    #[test]
    fn opencode_seed_rates_cite_their_source_and_date() {
        let (_d, store) = store();
        let source: String = store
            .conn
            .query_row("SELECT source FROM model_rate WHERE model_prefix = 'deepseek-v4-1-flash'", [], |r| r.get(0))
            .unwrap();
        assert!(source.contains("opencode.ai") && source.contains("2026-10-24"), "{source}");
    }

    #[test]
    fn a_free_opencode_variant_is_never_priced_at_its_paid_models_rate() {
        let (_d, mut store) = store();
        let t = Tokens { input: 1_000_000, ..Default::default() };
        let day = local_noon(2026, 10, 15);
        let mut reported = event("f2", "s-reported", "deepseek-v4.1-flash-free", day, t);
        reported.reported_cost_usd = Some(0.0);
        store
            .ingest(&[event("f1", "s-fallback", "deepseek-v4.1-flash-free", day, t), reported])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let session = |id: &str| r.sessions.iter().find(|s| s.session_id == id).unwrap();
        assert_eq!(session("s-fallback").cost_usd, None);
        assert_eq!(session("s-reported").cost_usd, Some(0.0));
    }

    #[test]
    fn a_rate_correction_reprices_a_namespaced_model() {
        let (_d, mut store) = store();
        let t = Tokens { input: 1_000_000, ..Default::default() };
        store
            .ingest(&[event("n1", "s1", "deepseek/deepseek-v4.1-flash", local_noon(2026, 10, 15), t)])
            .unwrap();
        store
            .set_rate_and_reprice("deepseek-v4-1-flash", "2026-01-01", Rate::flat(5.0, 5.0), "set by the user")
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(r.sessions[0].cost_usd, Some(5.0));
    }

    #[test]
    fn the_sql_model_key_matches_the_rust_one() {
        let (_d, store) = store();
        let sql = format!("SELECT {}", model_key_sql("?1"));
        for model in ["deepseek-v4.1-flash", "deepseek/deepseek-v4.1-flash", "openrouter/deepseek/v4.1", "gpt-6-sol", ""] {
            let key: String = store.conn.query_row(&sql, [model], |r| r.get(0)).unwrap();
            assert_eq!(key, model_key(model), "{model}");
        }
    }

    #[test]
    fn a_provider_prefixed_opencode_model_is_priced_from_its_seeded_rate() {
        let (_d, mut store) = store();
        let t = Tokens { input: 1_000_000, output: 1_000_000, ..Default::default() };
        store
            .ingest(&[event("m1", "s1", "opencode-go/deepseek-v4.1-flash", local_noon(2026, 10, 15), t)])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let cost = r.sessions[0].cost_usd.unwrap();
        assert!((cost - 1.5).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn cache_writes_and_reads_are_priced_at_their_rates() {
        let (_d, mut store) = store();
        let t = Tokens {
            input: 0,
            output: 0,
            cache_read: 1_000_000,
            cache_write: 1_000_000, ..Default::default() };
        store.ingest(&[event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), t)]).unwrap();
        // Opus at $5 input: a read is 0.1x = 0.5, a write is 1.25x = 6.25.
        let cost = store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap();
        assert!((cost - 6.75).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn an_unseeded_model_keeps_its_tokens_and_leaves_the_cost_null() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(10))]).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(r.sessions[0].tokens.input, 10);
        assert_eq!(r.sessions[0].cost_usd, None);
        assert_eq!(r.sessions[0].unpriced_models, vec!["claude-test-9-9".to_string()]);
        assert_eq!(r.totals.unpriced_sessions, 1);
    }

    #[test]
    fn a_rate_applies_from_its_day_and_earlier_usage_keeps_its_price() {
        let (_d, mut store) = store();
        store.set_rate("claude-opus-4-8", "2026-10-16", 10.0, 50.0, "test").unwrap();
        store
            .ingest(&[
                event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000)),
                event("req_2", "s1", "claude-opus-4-8", local_noon(2026, 10, 17), input(1_000_000)),
            ])
            .unwrap();
        // 1M input on the 15th at $5, plus 1M input on the 17th at $10.
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn the_window_counts_whole_local_days() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("req_2", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)),
            ])
            .unwrap();
        let since = Local.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).single().unwrap().timestamp_millis();
        let r = store.report(&Filter { since_ms: Some(since), ..Filter::default() }).unwrap();
        assert_eq!(r.sessions[0].messages, 1);
    }

    #[test]
    fn until_excludes_the_day_it_falls_on() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("req_2", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)),
            ])
            .unwrap();
        let until = Local.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).single().unwrap().timestamp_millis();
        let r = store.report(&Filter { until_ms: Some(until), ..Filter::default() }).unwrap();
        assert_eq!(r.sessions[0].messages, 1, "the 16th is excluded, the 15th is counted");
    }

    #[test]
    fn session_and_cwd_filters_select_their_own_usage() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("req_2", "s2", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
            ])
            .unwrap();
        let by_session = store.report(&Filter { session_id: Some("s2".into()), ..Filter::default() }).unwrap();
        assert_eq!(by_session.sessions.len(), 1);
        assert_eq!(by_session.sessions[0].session_id, "s2");
        let by_cwd = store.report(&Filter { cwd: Some("/work/a".into()), ..Filter::default() }).unwrap();
        assert_eq!(by_cwd.sessions.len(), 2);
        let elsewhere = store.report(&Filter { cwd: Some("/elsewhere".into()), ..Filter::default() }).unwrap();
        assert!(elsewhere.sessions.is_empty());
    }

    #[test]
    fn the_same_session_id_in_two_clis_is_two_sessions() {
        let (_d, mut store) = store();
        let claude = event("req_1", "shared", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1));
        let mut codex = event("resp_1", "shared", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1));
        codex.cli = AgentCli::Codex;
        store.ingest(&[claude, codex]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions.len(), 2);
    }

    #[test]
    fn attribution_joins_onto_the_session_it_names() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1))]).unwrap();
        store.attribute("s1", "acme/team-context", Some("riverside"), "builtin/claude", "h1", None).unwrap();
        let s = &store.report(&Filter::default()).unwrap().sessions[0];
        assert_eq!(s.org.as_deref(), Some("acme/team-context"));
        assert_eq!(s.team.as_deref(), Some("riverside"));
        assert_eq!(s.agent.as_deref(), Some("builtin/claude"));
    }

    fn codex_export(conversation: &str, input: &str) -> serde_json::Value {
        serde_json::json!({
            "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{ "attributes": [
                { "key": "event.name", "value": { "stringValue": "codex.sse_event" } },
                { "key": "event.kind", "value": { "stringValue": "response.completed" } },
                { "key": "conversation.id", "value": { "stringValue": conversation } },
                { "key": "event.timestamp", "value": { "stringValue": "2026-10-16T18:53:59.775Z" } },
                { "key": "model", "value": { "stringValue": "gpt-6-astra" } },
                { "key": "input_token_count", "value": { "stringValue": input } },
                { "key": "output_token_count", "value": { "stringValue": "5" } }
            ] }] }] }]
        })
    }

    #[test]
    fn an_export_at_a_launchs_token_attributes_its_sessions_to_that_launch() {
        let (dir, store) = store();
        let path = dir.path().join("usage.sqlite");
        store
            .register_launch("launch-1", "acme/team-context", Some("riverside"), "builtin/codex", "launch-1", Some("/work/a"))
            .unwrap();
        drop(store);
        // Codex mints its own conversation id, so the launch's token is the only link to the launch.
        assert_eq!(ingest_telemetry_for_at(&path, "launch-1", &codex_export("conv-9", "100")).unwrap(), 1);
        let s = &UsageStore::open(&path).unwrap().report(&Filter::default()).unwrap().sessions[0];
        assert_eq!(s.cli, AgentCli::Codex);
        assert_eq!(s.org.as_deref(), Some("acme/team-context"));
        assert_eq!(s.team.as_deref(), Some("riverside"));
        assert_eq!(s.agent.as_deref(), Some("builtin/codex"));
        // The launch's working directory matches the cwd filter, though Codex reports none.
        let by_cwd = UsageStore::open(&path)
            .unwrap()
            .report(&Filter { cwd: Some("/work/a".into()), ..Filter::default() })
            .unwrap();
        assert_eq!(by_cwd.sessions.len(), 1);
        // The launch's own session id finds the CLI's session, though Codex's conversation id differs.
        let by_launch = UsageStore::open(&path)
            .unwrap()
            .report(&Filter { session_id: Some("launch-1".into()), ..Filter::default() })
            .unwrap();
        assert_eq!(by_launch.sessions.len(), 1);
    }

    #[test]
    fn an_export_at_an_unknown_token_is_stored_unattributed() {
        let (dir, _store) = store();
        let path = dir.path().join("usage.sqlite");
        ingest_telemetry_for_at(&path, "no-such-launch", &codex_export("conv-9", "100")).unwrap();
        let r = UsageStore::open(&path).unwrap().report(&Filter::default()).unwrap();
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].org, None);
    }

    #[test]
    fn an_attribution_made_at_launch_is_not_overwritten_by_a_later_export() {
        let (dir, store) = store();
        let path = dir.path().join("usage.sqlite");
        store.attribute("conv-9", "org-at-launch", None, "builtin/codex", "h", None).unwrap();
        store.register_launch("launch-1", "other-org", None, "builtin/codex", "h", None).unwrap();
        drop(store);
        ingest_telemetry_for_at(&path, "launch-1", &codex_export("conv-9", "100")).unwrap();
        let r = UsageStore::open(&path).unwrap().report(&Filter::default()).unwrap();
        assert_eq!(r.sessions[0].org.as_deref(), Some("org-at-launch"));
    }

    #[test]
    fn a_new_rate_prices_the_unpriced_usage_it_covers_and_nothing_else() {
        let (_d, mut store) = store();
        // Haiku 5.5 has no seed, so its usage is unpriced until a rate is set.
        store
            .ingest(&[
                event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000)),
                event("req_2", "s1", "claude-test-9-9", local_noon(2026, 10, 17), input(1_000_000)),
                event("req_3", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000)),
            ])
            .unwrap();
        // Usage on or after the 16th is covered; the 15th is not.
        let priced = store.set_rate_and_reprice("claude-test-9-9", "2026-10-16", Rate::flat(2.0, 10.0), "test").unwrap();
        assert_eq!(priced, 1);
        // A row that is priced already keeps its cost, even when the new rate's prefix matches it.
        let mut reported = event("req_4", "s1", "claude-test-9-9", local_noon(2026, 10, 17), input(1_000_000));
        reported.reported_cost_usd = Some(0.42);
        store.ingest(&[reported]).unwrap();
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-16", Rate::flat(9.0, 9.0), "test").unwrap();
        let again = store.report(&Filter::default()).unwrap();
        assert!(again.days.iter().find(|d| d.day == "2026-10-17").unwrap().cost_usd >= 0.42);
        let r = store.report(&Filter::default()).unwrap();
        let haiku = r.sessions[0].cost_usd;
        // The 17th is priced at $2 on 1M input; the 15th stays unpriced, so the session stays unpriced.
        assert_eq!(haiku, None);
        // The 17th holds the rate-table event, corrected to the newer $9 rate, and the CLI-reported $0.42.
        assert_eq!(r.days.iter().find(|d| d.day == "2026-10-17").unwrap().cost_usd, 9.42);
        // Priced usage is untouched.
        assert!((r.days.iter().find(|d| d.day == "2026-10-15").unwrap().cost_usd - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_user_rate_prices_cache_reads_and_long_prompts_at_its_own_prices() {
        let (_d, mut store) = store();
        // A 2M-token prompt sits at the threshold; one token more is over it.
        let at = Tokens { cache_read: 1_000_000, output: 1_000_000, ..input(1_000_000) };
        let over = Tokens { cache_read: 1_000_000, output: 1_000_000, ..input(1_000_001) };
        store
            .ingest(&[
                event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), at),
                event("req_2", "s1", "claude-test-9-9", local_noon(2026, 10, 16), over),
                event("req_3", "s1", "claude-test-9-9", local_noon(2026, 10, 17), over),
            ])
            .unwrap();
        let long = |cache_read_per_mtok| LongRateArgs {
            from_tokens: 2_000_000,
            input_per_mtok: 4.0,
            output_per_mtok: 20.0,
            cache_read_per_mtok,
        };
        // From the 1st, long cache reads at 0.1x the long input price; from the 17th, at their own $0.30.
        let rate = Rate::with(2.0, 10.0, Some(0.05), Some(long(None)));
        assert_eq!(store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", rate, "test").unwrap(), 3);
        let own = Rate::with(2.0, 10.0, Some(0.05), Some(long(Some(0.30))));
        assert_eq!(store.set_rate_and_reprice("claude-test-9-9", "2026-10-17", own, "test").unwrap(), 1);
        let r = store.report(&Filter::default()).unwrap();
        let day = |d: &str| r.days.iter().find(|x| x.day == d).unwrap().cost_usd;
        // At the threshold: $2 input, the rate's own $0.05 cache reads and $10 output.
        assert!((day("2026-10-15") - 12.05).abs() < 1e-9, "{}", day("2026-10-15"));
        // Over it: $4 input, $0.40 cache reads (0.1x long input) and $20 output.
        assert!((day("2026-10-16") - 24.400_004).abs() < 1e-9, "{}", day("2026-10-16"));
        // Over it with the long cache-read price set: $0.30 cache reads.
        assert!((day("2026-10-17") - 24.300_004).abs() < 1e-9, "{}", day("2026-10-17"));
    }

    #[test]
    fn a_later_flat_rate_drops_the_long_prompt_prices_from_its_day_on() {
        let (_d, mut store) = store();
        let big = input(3_000_000);
        store
            .ingest(&[
                event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), big),
                event("req_2", "s1", "claude-test-9-9", local_noon(2026, 10, 17), big),
            ])
            .unwrap();
        let long = LongRateArgs { from_tokens: 1_000, input_per_mtok: 4.0, output_per_mtok: 20.0, cache_read_per_mtok: None };
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", Rate::with(2.0, 10.0, None, Some(long)), "test").unwrap();
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-16", Rate::flat(2.0, 10.0), "test").unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let day = |d: &str| r.days.iter().find(|x| x.day == d).unwrap().cost_usd;
        assert!((day("2026-10-15") - 12.0).abs() < 1e-9, "the long price before the flat rate");
        assert!((day("2026-10-17") - 6.0).abs() < 1e-9, "one price at every length after it");
        // Replacing the rate on the same day drops its long prices too.
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", Rate::flat(1.0, 10.0), "test").unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert!((r.days.iter().find(|x| x.day == "2026-10-15").unwrap().cost_usd - 3.0).abs() < 1e-9);
    }

    #[test]
    fn rates_lists_the_seeded_list_prices_and_the_ones_the_user_set() {
        let (_dir, mut store) = store();
        let rates = store.rates().unwrap();
        assert_eq!(rates.len(), SEED_RATES.len());
        let haiku = rates.iter().find(|r| r.model_prefix == "claude-haiku-5-5").unwrap();
        assert_eq!(haiku.effective_from, SEED_EFFECTIVE_FROM);
        assert_eq!((haiku.input_per_mtok, haiku.output_per_mtok, haiku.cache_read_per_mtok), (0.10, 0.50, None));
        assert_eq!(
            haiku.long,
            Some(LongRateArgs { from_tokens: 100_000, input_per_mtok: 0.50, output_per_mtok: 2.50, cache_read_per_mtok: None })
        );
        assert_eq!(haiku.source, SEED_SOURCE);
        // A price left out is sent as null, not dropped.
        assert_eq!(serde_json::to_value(haiku).unwrap()["long"]["cacheReadPerMtok"], serde_json::Value::Null);
        assert_eq!(serde_json::to_value(haiku).unwrap()["cacheReadPerMtok"], serde_json::Value::Null);

        let long = LongRateArgs { from_tokens: 200_000, input_per_mtok: 6.0, output_per_mtok: 30.0, cache_read_per_mtok: Some(0.6) };
        store
            .set_rate_and_reprice("claude-haiku-5-5", "2026-11-01", Rate::with(0.2, 1.0, Some(0.03), Some(long)), "set by the user")
            .unwrap();
        let rates = store.rates().unwrap();
        let mine: Vec<_> = rates.iter().filter(|r| r.model_prefix == "claude-haiku-5-5").collect();
        // Oldest first: the seed, then the user's rate from its day.
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[1].effective_from, "2026-11-01");
        assert_eq!(mine[1].cache_read_per_mtok, Some(0.03));
        assert_eq!(mine[1].long, Some(long));
        assert_eq!(mine[1].source, "set by the user");
        let json = serde_json::to_value(mine[1]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "modelPrefix": "claude-haiku-5-5",
                "effectiveFrom": "2026-11-01",
                "inputPerMtok": 0.2,
                "outputPerMtok": 1.0,
                "cacheReadPerMtok": 0.03,
                "long": {"fromTokens": 200000, "inputPerMtok": 6.0, "outputPerMtok": 30.0, "cacheReadPerMtok": 0.6},
                "source": "set by the user",
            })
        );
    }

    #[test]
    fn set_model_rate_params_read_exactly_what_the_app_sends() {
        // The payload commands.session-usage.test.ts pins for sessionSetModelRate.
        let params: SetModelRateParams = serde_json::from_value(serde_json::json!({
            "modelPrefix": "gpt-6-sol", "effectiveFrom": "2026-10-17", "inputPerMtok": 2, "outputPerMtok": 10,
            "cacheReadPerMtok": 0.1,
            "long": {"fromTokens": 272000, "inputPerMtok": 4, "outputPerMtok": 15, "cacheReadPerMtok": 0.2}
        }))
        .unwrap();
        assert_eq!(params.model_prefix, "gpt-6-sol");
        assert_eq!(params.cache_read_per_mtok, Some(0.1));
        assert_eq!(params.long.map(|l| (l.from_tokens, l.cache_read_per_mtok)), Some((272_000, Some(0.2))));
        let flat: SetModelRateParams = serde_json::from_value(serde_json::json!({
            "modelPrefix": "gpt-6-sol", "effectiveFrom": "2026-10-17", "inputPerMtok": 2, "outputPerMtok": 10,
            "cacheReadPerMtok": null, "long": null
        }))
        .unwrap();
        assert!(flat.cache_read_per_mtok.is_none() && flat.long.is_none());
        // A misspelled or snake_case key is refused, never dropped.
        for key in ["cacheReadPerMTok", "cache_read_per_mtok"] {
            let mut payload = serde_json::json!({
                "modelPrefix": "gpt-6-sol", "effectiveFrom": "2026-10-17", "inputPerMtok": 2, "outputPerMtok": 10
            });
            payload[key] = serde_json::json!(0.1);
            assert!(serde_json::from_value::<SetModelRateParams>(payload).is_err(), "{key}");
        }
    }

    #[test]
    fn a_later_rate_without_a_cache_read_price_drops_the_earlier_one_and_long_cache_reads_keep_their_own_default() {
        let (_d, mut store) = store();
        let reads = Tokens { cache_read: 1_000_000, ..input(0) };
        let long_reads = Tokens { cache_read: 3_000_000, ..input(0) };
        store
            .ingest(&[
                event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), reads),
                event("req_2", "s1", "claude-test-9-9", local_noon(2026, 10, 17), reads),
                event("req_3", "s1", "claude-test-9-9", local_noon(2026, 10, 15), long_reads),
            ])
            .unwrap();
        // A $0.05 cache-read price, and a long price from 2M tokens with no cache-read price of its own.
        let long = LongRateArgs { from_tokens: 2_000_000, input_per_mtok: 4.0, output_per_mtok: 20.0, cache_read_per_mtok: None };
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", Rate::with(2.0, 10.0, Some(0.05), Some(long)), "test").unwrap();
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-16", Rate::flat(2.0, 10.0), "test").unwrap();
        let r = store.report(&Filter::default()).unwrap();
        let day = |d: &str| r.days.iter().find(|x| x.day == d).unwrap().cost_usd;
        // The 15th: 1M short cache reads at $0.05, and 3M long ones at 0.1x the long input ($0.40),
        // not at the flat rate's own cache-read price.
        assert!((day("2026-10-15") - (0.05 + 1.2)).abs() < 1e-9, "{}", day("2026-10-15"));
        // The 17th: the later rate names no cache-read price, so reads are back at 0.1x input.
        assert!((day("2026-10-17") - 0.2).abs() < 1e-9, "{}", day("2026-10-17"));
    }

    #[test]
    fn long_rate_args_read_the_page_bridge_s_camel_case_and_refuse_other_keys() {
        let args: LongRateArgs = serde_json::from_value(serde_json::json!({
            "fromTokens": 272000, "inputPerMtok": 4.0, "outputPerMtok": 15.0, "cacheReadPerMtok": 0.2
        }))
        .unwrap();
        assert_eq!(args.from_tokens, 272_000);
        assert_eq!(args.cache_read_per_mtok, Some(0.2));
        let without: LongRateArgs =
            serde_json::from_value(serde_json::json!({"fromTokens": 1, "inputPerMtok": 1.0, "outputPerMtok": 1.0})).unwrap();
        assert_eq!(without.cache_read_per_mtok, None);
        assert!(serde_json::from_value::<LongRateArgs>(serde_json::json!({
            "fromTokens": 1, "inputPerMtok": 1.0, "outputPerMtok": 1.0, "cacheReadPerMTok": 0.2
        }))
        .is_err());
        assert!(serde_json::from_value::<LongRateArgs>(serde_json::json!({"from_tokens": 1, "inputPerMtok": 1.0, "outputPerMtok": 1.0})).is_err());
    }

    #[test]
    fn a_user_rate_with_a_bad_cache_read_or_long_price_is_refused_and_stores_nothing() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        let long = |from_tokens, input| LongRateArgs { from_tokens, input_per_mtok: input, output_per_mtok: 5.0, cache_read_per_mtok: None };
        for rate in [
            Rate::with(1.0, 5.0, Some(0.0), None),
            Rate::with(1.0, 5.0, Some(f64::NAN), None),
            Rate::with(1.0, 5.0, None, Some(long(0, 2.0))),
            Rate::with(1.0, 5.0, None, Some(long(u64::MAX, 2.0))),
            Rate::with(1.0, 5.0, None, Some(long(100, -2.0))),
            Rate::with(1.0, 5.0, None, Some(LongRateArgs { cache_read_per_mtok: Some(1e9), ..long(100, 2.0) })),
            Rate::with(1.0, 5.0, None, Some(LongRateArgs { output_per_mtok: 0.0, ..long(100, 2.0) })),
            Rate::with(1.0, 5.0, None, Some(long(100, f64::INFINITY))),
            Rate::with(1.0, 5.0, None, Some(long(100, f64::NAN))),
            Rate::with(1.0, 5.0, Some(-0.1), None),
            Rate::with(1.0, 5.0, Some(100_001.0), None),
            Rate::with(100_001.0, 5.0, None, None),
            Rate::with(1.0, f64::INFINITY, None, None),
        ] {
            let err = store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", rate, "test").unwrap_err();
            assert!(err.starts_with("invalid: "), "{rate:?}: {err}");
        }
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, None, "no rate was stored");
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM model_rate WHERE model_prefix = 'claude-test-9-9'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "no rate row was written");
    }

    #[test]
    fn guarded_rate_requests_and_native_acceptance_record_their_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let params = |model: &str| -> SetModelRateParams {
            serde_json::from_value(serde_json::json!({
                "modelPrefix": model, "effectiveFrom": "2026-10-17", "inputPerMtok": 2, "outputPerMtok": 10
            }))
            .unwrap()
        };
        let mut lower = params("gpt-6-sol");
        lower.input_per_mtok = 0.1;
        lower.output_per_mtok = 0.1;
        set_model_rate_at(&path, lower.clone()).unwrap();
        UsageStore::open(&path).unwrap().accept_rate(&lower.model_prefix, &lower.effective_from, &lower).unwrap();
        set_model_rate_at(&path, params("gpt-6-luna")).unwrap();
        let rates = UsageStore::open(&path).unwrap().rates().unwrap();
        // A seeded model also has its list price from 1970-01-01, so the rate is found by its day.
        let source = |model: &str| {
            rates.iter().find(|r| r.model_prefix == model && r.effective_from == "2026-10-17").map(|r| r.source.as_str())
        };
        assert_eq!(source("gpt-6-sol"), Some("set by the user"));
        assert_eq!(source("gpt-6-luna"), Some("set by an agent"));
    }

    #[test]
    fn a_day_outside_1970_to_9999_is_refused_and_prices_nothing() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        // Each parses as a date: the signed ones would sort as text before every real day, and the
        // early ones come before the seeded list prices.
        for day in ["+10000-01-01", "-0001-01-01", "-2026-10-01", "0099-01-01", "1969-12-31"] {
            let err = store.set_rate_and_reprice("claude-test-9-9", day, Rate::flat(2.0, 10.0), "test").unwrap_err();
            assert!(err.starts_with("invalid: effectiveFrom"), "{day}: {err}");
        }
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, None, "nothing was priced");
        assert_eq!(store.set_rate_and_reprice("claude-test-9-9", "2026-10-01", Rate::flat(2.0, 10.0), "test").unwrap(), 1);
    }

    #[test]
    fn a_dotted_model_prefix_is_stored_as_the_dashed_form_it_matches() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        // A dashboard may name the model with dots; the rate must still apply.
        // The seeded price was taken from the rate table, so a corrected rate reprices it.
        let priced = store.set_rate_and_reprice("claude-opus-4.8", "2026-10-15", Rate::flat(10.0, 50.0), "test").unwrap();
        assert_eq!(priced, 1, "the seed-priced usage is corrected by the new rate");
        store.ingest(&[event("req_2", "s2", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        let priced = store.set_rate_and_reprice("claude-test.9.9", "2026-10-15", Rate::flat(2.0, 10.0), "test").unwrap();
        assert_eq!(priced, 1, "the dotted prefix now matches the dashed model id");
    }

    #[test]
    fn a_limit_caps_the_sessions_and_the_totals_still_cover_them_all() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("req_2", "s2", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)),
                event("req_3", "s3", "claude-sonnet-4-6", local_noon(2026, 10, 17), input(1)),
            ])
            .unwrap();
        let r = store.report(&Filter { limit: Some(2), ..Filter::default() }).unwrap();
        assert_eq!(r.sessions.len(), 2);
        assert_eq!(r.sessions[0].session_id, "s3", "newest first");
        assert_eq!(r.totals.priced_sessions, 3);
        assert_eq!(r.session_count, 3, "the count is taken before the limit");
    }

    /// SV-ENG-63 review round 3: the shared match count is serialized as `total` in MCP, Tauri and
    /// hosted responses, not as `session_count`.
    #[test]
    fn a_serialized_report_exposes_total_not_session_count() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("req_2", "s2", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)),
                event("req_3", "s3", "claude-sonnet-4-6", local_noon(2026, 10, 17), input(1)),
            ])
            .unwrap();
        let report = store
            .report(&Filter { limit: Some(1), ..Filter::default() })
            .unwrap();
        let payload = serde_json::to_value(&report).unwrap();
        assert_eq!(payload["total"], 3);
        assert_eq!(payload["sessions"].as_array().unwrap().len(), 1);
        assert!(
            payload.get("session_count").is_none(),
            "the internal field name is not served"
        );
    }

    /// SV-ENG-63: pages follow `nextCursor` over the whole window, with ties on `lastAtMs`,
    /// identical totals on every page, a new session arriving mid-walk, and no cursor on the last page.
    #[test]
    fn cursor_pages_walk_every_session_once() {
        let (_d, mut store) = store();
        let noon = local_noon(2026, 10, 15);
        // s1..s4 tie on lastAtMs; s5 is newer, s0 older.
        let mut events = vec![event("r0", "s0", "claude-sonnet-4-6", local_noon(2026, 10, 14), input(1))];
        for i in 1..=4 {
            events.push(event(&format!("r{i}"), &format!("s{i}"), "claude-sonnet-4-6", noon, input(1)));
        }
        events.push(event("r5", "s5", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)));
        store.ingest(&events).unwrap();

        let walk = |store: &UsageStore| {
            let mut seen = Vec::new();
            let mut cursor = None;
            let mut totals = Vec::new();
            loop {
                let r = store
                    .report(&Filter { limit: Some(2), cursor: cursor.clone(), ..Filter::default() })
                    .unwrap();
                seen.extend(r.sessions.iter().map(|s| s.session_id.clone()));
                totals.push(r.totals.priced_sessions);
                assert_eq!(r.session_count, 6, "the count covers the whole window on every page");
                cursor = r.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            (seen, totals)
        };
        let (seen, totals) = walk(&store);
        assert_eq!(seen, ["s5", "s1", "s2", "s3", "s4", "s0"]);
        assert_eq!(totals, [6, 6, 6], "totals are the same on every page");

        // A session arriving at the top between pages duplicates nothing and skips nothing.
        let r = store.report(&Filter { limit: Some(2), ..Filter::default() }).unwrap();
        store
            .ingest(&[event("r6", "s6", "claude-sonnet-4-6", local_noon(2026, 10, 17), input(1))])
            .unwrap();
        let next = store
            .report(&Filter { limit: Some(2), cursor: r.next_cursor, ..Filter::default() })
            .unwrap();
        let ids: Vec<_> = next.sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["s2", "s3"]);
    }

    /// SV-ENG-63 review round 3: a walk is plain keyset paging on the current state. Sessions that
    /// become active move to the first page, so a walk can skip them but never repeats one. A
    /// same-limit first-page reread only merges the current first page; full recovery means walking
    /// from page 1 through continuation, merging by sessionId, up to 2 rewalks. After that report
    /// the truthful latest N of M. `total`, `totals` and `days` count the current sessions on every
    /// page.
    #[test]
    fn a_walk_may_skip_sessions_that_move_up_but_a_settled_full_rewalk_recovers_them() {
        let base = local_noon(2026, 10, 15);
        let call = |n: u32, session: &str, at: i64| {
            event(&format!("r{n}"), session, "claude-sonnet-4-6", at, input(1))
        };
        let all_ids = |store: &UsageStore| -> BTreeSet<String> {
            store
                .report(&Filter::default())
                .unwrap()
                .sessions
                .iter()
                .map(|s| s.session_id.clone())
                .collect()
        };
        let page = |store: &UsageStore, limit, cursor: Option<String>| {
            store
                .report(&Filter {
                    limit: Some(limit),
                    cursor,
                    ..Filter::default()
                })
                .unwrap()
        };
        let walk = |store: &UsageStore, limit| -> Vec<String> {
            let mut ids = Vec::new();
            let mut cursor = None;
            loop {
                let r = page(store, limit, cursor);
                ids.extend(r.sessions.iter().map(|s| s.session_id.clone()));
                cursor = r.next_cursor;
                if cursor.is_none() {
                    break ids;
                }
            }
        };

        // Deterministic case at limit 1: A30 B20 C10. After A and B are read, C takes a late call
        // at 25, moving it above the cursor but below page 1.
        let (_d, mut store) = store();
        store
            .ingest(&[call(1, "A", base + 30_000), call(2, "B", base + 20_000), call(3, "C", base + 10_000)])
            .unwrap();
        let p1 = page(&store, 1, None);
        let p2 = page(&store, 1, p1.next_cursor.clone());
        assert_eq!((p1.sessions[0].session_id.as_str(), p2.sessions[0].session_id.as_str()), ("A", "B"));
        store.ingest(&[call(4, "C", base + 25_000)]).unwrap();
        let p3 = page(&store, 1, p2.next_cursor.clone());
        assert!(p3.sessions.is_empty(), "C moved above the cursor: the walk skips it");
        assert_eq!(p1.session_count, 3);
        assert_eq!(p3.session_count, 3, "total counts current matches on every page");
        assert_eq!(p3.totals.priced_sessions, 3, "so do totals");

        // A same-limit first-page reread still leaves C unseen (only A and B).
        let first_page_reread = page(&store, 1, None);
        let merged: BTreeSet<String> = [&p1, &p2, &p3, &first_page_reread]
            .iter()
            .flat_map(|p| p.sessions.iter().map(|s| s.session_id.clone()))
            .collect();
        assert_eq!(merged.len(), 2, "same-limit first-page reread leaves C unseen");

        // A settled full rewalk at the same limit recovers every current session.
        let rewalk = walk(&store, 1);
        let rewalk_distinct: BTreeSet<String> = rewalk.iter().cloned().collect();
        assert_eq!(rewalk.len(), rewalk_distinct.len(), "a walk never repeats a session");
        assert_eq!(rewalk_distinct, all_ids(&store), "settled full rewalk recovers all current sessions");
        assert_eq!(rewalk_distinct.len(), 3);

        // Multi movers with limit 2: A80 B70 C60 D50 E40 F30 G20 H10. After A/B and C/D are read,
        // E/F/G/H move to between page 1 and page 2; a same-limit full rewalk recovers all 8.
        let (_d2, mut store) = self::store();
        let sessions = [("A", 80), ("B", 70), ("C", 60), ("D", 50), ("E", 40), ("F", 30), ("G", 20), ("H", 10)];
        let seed_events: Vec<_> = sessions
            .iter()
            .enumerate()
            .map(|(i, (s, t))| call(i as u32 + 1, s, base + (*t as i64) * 1_000))
            .collect();
        store.ingest(&seed_events).unwrap();
        let ab = page(&store, 2, None);
        let cd = page(&store, 2, ab.next_cursor.clone());
        assert_eq!(ab.sessions.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(cd.sessions.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(), vec!["C", "D"]);
        store
            .ingest(&[
                call(9, "E", base + 65_000),
                call(10, "F", base + 64_000),
                call(11, "G", base + 63_000),
                call(12, "H", base + 62_000),
            ])
            .unwrap();
        let rewalk = walk(&store, 2);
        let rewalk_distinct: BTreeSet<String> = rewalk.iter().cloned().collect();
        assert_eq!(rewalk.len(), rewalk_distinct.len(), "a walk never repeats a session");
        assert_eq!(rewalk_distinct, all_ids(&store), "full rewalk recovers 8/8 after multi-page moves");
        assert_eq!(rewalk_distinct.len(), 8);

        // Bounded incomplete guard: continuous new sessions arrive above the cursor after each
        // recovery's first page, so 2 rewalks do not claim completeness. Start with A30 B20 C10.
        let (_d3, mut store) = self::store();
        store
            .ingest(&[call(1, "A", base + 30_000), call(2, "B", base + 20_000), call(3, "C", base + 10_000)])
            .unwrap();
        let limit = 1;
        let mut collected: BTreeSet<String> = BTreeSet::new();
        let injections = [(4, "D", 35_000), (5, "E", 40_000)];

        // Up to 2 recovery rewalks; a new session arrives above the cursor after each first page.
        for (req, session, offset_ms) in injections {
            let mut cursor = None;
            let mut page_num = 0;
            loop {
                let r = page(&store, limit, cursor.clone());
                page_num += 1;
                collected.extend(r.sessions.iter().map(|s| s.session_id.clone()));
                if page_num == 1 {
                    store.ingest(&[call(req, session, base + offset_ms)]).unwrap();
                }
                cursor = r.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
        }

        let current_total = all_ids(&store).len();
        assert_eq!((collected.len(), current_total), (4, 5), "merged ends 4/5");
        assert!(
            collected.len() < current_total,
            "after 2 rewalks with continuous arrivals we remain incomplete: {}/{}",
            collected.len(),
            current_total
        );
    }

    #[test]
    fn a_malformed_or_foreign_cursor_is_refused() {
        let (_d, mut store) = store();
        store
            .ingest(&[
                event("r1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1)),
                event("r2", "s2", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1)),
            ])
            .unwrap();
        let first = store.report(&Filter { limit: Some(1), ..Filter::default() }).unwrap();
        let cursor = first.next_cursor.unwrap();
        for bad in ["x", "u:zz:1:s1", "u:0:abc:s1", "u:0:1:", "o:1:2:3"] {
            let got = store.report(&Filter { cursor: Some(bad.into()), ..Filter::default() });
            assert!(got.unwrap_err().starts_with("invalid:"), "{bad}");
        }
        let foreign = store.report(&Filter {
            cwd: Some("/elsewhere".into()),
            cursor: Some(cursor),
            ..Filter::default()
        });
        assert!(foreign.unwrap_err().starts_with("invalid:"));
    }

    #[test]
    fn a_report_that_fits_one_page_has_no_next_cursor() {
        let (_d, mut store) = store();
        store.ingest(&[event("r1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1))]).unwrap();
        let r = store.report(&Filter { limit: Some(1), ..Filter::default() }).unwrap();
        assert!(r.next_cursor.is_none());
    }

    #[test]
    fn a_store_records_its_schema_version_and_refuses_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        drop(UsageStore::open(&path).unwrap());
        let version: i64 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        let err = UsageStore::open(&path).err().expect("a newer store is refused");
        assert!(err.contains("newer CodeSense"), "{err}");
    }

    #[test]
    fn a_mistyped_rate_is_corrected_and_a_reported_cost_is_not_changed() {
        let (_d, mut store) = store();
        let mut reported = event("req_r", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000));
        reported.reported_cost_usd = Some(0.42);
        store
            .ingest(&[
                event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000)),
                reported,
            ])
            .unwrap();
        // The first rate is wrong: it prices the rate-table event at $2 on the 15th.
        store.set_rate_and_reprice("claude-test-9-9", "2026-10-15", Rate::flat(2.0, 10.0), "test").unwrap();
        // Correcting it reprices that event; the reported $0.42 is kept as the CLI sent it.
        let repriced = store.set_rate_and_reprice("claude-test-9-9", "2026-10-15", Rate::flat(9.0, 9.0), "test").unwrap();
        assert_eq!(repriced, 1);
        let day = store.report(&Filter::default()).unwrap().days.into_iter().find(|d| d.day == "2026-10-15").unwrap();
        assert!((day.cost_usd - 9.42).abs() < 1e-9, "{}", day.cost_usd);
    }

    /// One Claude API request as the remote spool keeps it: a LogRecord with its attributes.
    fn remote_record(request_id: &str, session: &str) -> serde_json::Value {
        let attr = |key: &str, value: &str| serde_json::json!({"key": key, "value": {"stringValue": value}});
        serde_json::json!({"attributes": [
            attr("event.name", "api_request"),
            attr("event.timestamp", "2026-10-15T12:00:00.000Z"),
            attr("request_id", request_id),
            attr("session.id", session),
            attr("model", "claude-opus-4-8"),
            attr("input_tokens", "1000"),
            attr("output_tokens", "100"),
            attr("cache_read_tokens", "0"),
            attr("cache_creation_tokens", "0"),
        ]})
    }

    /// A fake remote spool holding `records` (with their seq), read the way the remote reads it.
    fn fake_spool(epoch: &'static str, records: Vec<(u64, serde_json::Value)>) -> impl FnMut(u64, usize) -> Result<UsageSpoolRead, String> {
        move |after, limit| {
            let kept: Vec<UsageSpoolRecord> = records
                .iter()
                .filter(|(seq, _)| *seq > after)
                .take(limit)
                .map(|(seq, record)| UsageSpoolRecord { seq: *seq, token: "tok".into(), record: record.clone() })
                .collect();
            Ok(UsageSpoolRead {
                epoch: epoch.into(),
                oldest_seq: records.first().map(|(seq, _)| *seq),
                last_seq: records.last().map_or(0, |(seq, _)| *seq),
                records: kept,
            })
        }
    }

    fn rows_for(store: &UsageStore, machine: &str) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_event WHERE machine_id = ?1", params![machine], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_pull_stores_each_machines_usage_once_and_advances_its_cursor() {
        let (_dir, mut store) = store();
        let records = vec![(1, remote_record("r1", "s1")), (2, remote_record("r2", "s1")), (3, remote_record("r3", "s2"))];
        assert_eq!(pull_machine(&mut store, "m1", fake_spool("e1", records.clone())).unwrap(), 3);
        assert_eq!(rows_for(&store, "m1"), 3);
        assert_eq!(store.pull_cursor("m1").unwrap(), Some(PullCursor { epoch: "e1".into(), last_seq: 3, incomplete: false }));
        // A second pull reads nothing new and stores nothing twice.
        assert_eq!(pull_machine(&mut store, "m1", fake_spool("e1", records)).unwrap(), 0);
        assert_eq!(rows_for(&store, "m1"), 3);
    }

    #[test]
    fn each_session_names_the_machine_it_ran_on() {
        let (d, mut store) = store();
        let wt = d.path().join("wt");
        store.ingest(&[event_in("a", "local", &wt, 1)]).unwrap();
        pull_machine(&mut store, "m1", fake_spool("e1", vec![(1, remote_record("r1", "remote"))])).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        // This Mac is `local`, as list_machines names it.
        assert_eq!(by_id(&r, "local").machine_id, "local");
        assert_eq!(by_id(&r, "remote").machine_id, "m1");
        let json = serde_json::to_value(by_id(&r, "local")).unwrap();
        assert_eq!(json["machineId"], "local");
    }

    #[test]
    fn a_session_with_usage_from_two_machines_names_the_machine_of_its_latest_call() {
        let (d, mut first) = store();
        let wt = d.path().join("wt");
        let mut late = event_in("late", "both", &wt, 1);
        late.at_ms += 60_000;
        // The latest call is this Mac's, whichever order the rows are stored in.
        first.ingest_for("m1", &[event_in("early", "both", &wt, 1)]).unwrap();
        first.ingest(&[late.clone()]).unwrap();
        assert_eq!(by_id(&first.report(&Filter::default()).unwrap(), "both").machine_id, "local");
        let (_d2, mut second) = store();
        second.ingest(&[event_in("early", "both", &wt, 1)]).unwrap();
        second.ingest_for("m1", &[late]).unwrap();
        assert_eq!(by_id(&second.report(&Filter::default()).unwrap(), "both").machine_id, "m1");

        // Calls at the same millisecond: the other machine, whichever is stored first.
        for remote_first in [true, false] {
            let (_d, mut tied) = store();
            let local = event_in("l", "tie", &wt, 1);
            let remote = event_in("r", "tie", &wt, 1);
            assert_eq!(local.at_ms, remote.at_ms);
            if remote_first {
                tied.ingest_for("m1", &[remote]).unwrap();
                tied.ingest(&[local]).unwrap();
            } else {
                tied.ingest(&[local]).unwrap();
                tied.ingest_for("m1", &[remote]).unwrap();
            }
            assert_eq!(by_id(&tied.report(&Filter::default()).unwrap(), "tie").machine_id, "m1");
        }
    }

    #[test]
    fn the_same_event_id_from_two_machines_is_two_events() {
        let (_dir, mut store) = store();
        pull_machine(&mut store, "m1", fake_spool("e1", vec![(1, remote_record("same", "s1"))])).unwrap();
        pull_machine(&mut store, "m2", fake_spool("e9", vec![(1, remote_record("same", "s1"))])).unwrap();
        assert_eq!(rows_for(&store, "m1"), 1);
        assert_eq!(rows_for(&store, "m2"), 1);
    }

    #[test]
    fn a_remote_launch_registered_under_its_session_id_attributes_the_usage_pulled_for_it() {
        let (_dir, mut store) = store();
        // The Mac registers a remote tab's launch under the session id it minted, which the machine
        // uses as the launch's telemetry token.
        store
            .register_launch("remote-shell-1", "acme", Some("api"), "acme/api/triage", "story-7", Some("/home/u/wt"))
            .unwrap();
        let mut spool = fake_spool("e1", vec![(1, remote_record("r1", "cli-session"))]);
        pull_machine(&mut store, "m1", |after, limit| {
            spool(after, limit).map(|mut page| {
                page.records.iter_mut().for_each(|r| r.token = "remote-shell-1".into());
                page
            })
        })
        .unwrap();
        let report = store.report(&Filter::default()).unwrap();
        let session = &report.sessions[0];
        assert_eq!(session.org.as_deref(), Some("acme"));
        assert_eq!(session.team.as_deref(), Some("api"));
        assert_eq!(session.agent.as_deref(), Some("acme/api/triage"));
        // The remote tab's own session id finds the usage too.
        let by_tab = store
            .report(&Filter { session_id: Some("remote-shell-1".into()), ..Filter::default() })
            .unwrap();
        assert_eq!(by_tab.sessions.len(), 1);
    }

    #[test]
    fn a_new_spool_epoch_starts_the_pull_over_and_replaces_its_rows() {
        let (_dir, mut store) = store();
        let records = vec![(1, remote_record("r1", "s1")), (2, remote_record("r2", "s1"))];
        pull_machine(&mut store, "m1", fake_spool("e1", records.clone())).unwrap();
        let mut reads = Vec::new();
        let mut respooled = fake_spool("e2", records);
        pull_machine(&mut store, "m1", |after, limit| {
            reads.push(after);
            respooled(after, limit)
        })
        .unwrap();
        // The first read used the old cursor and was thrown away; the restart read from zero.
        assert_eq!(reads, vec![2, 0, 2]);
        assert_eq!(store.pull_cursor("m1").unwrap().unwrap().epoch, "e2");
        assert_eq!(rows_for(&store, "m1"), 2);
    }

    #[test]
    fn records_pruned_before_the_pull_reached_them_mark_the_machine_incomplete() {
        let (_dir, mut store) = store();
        // Records 1 to 4 were pruned: the spool starts at 5.
        pull_machine(&mut store, "m1", fake_spool("e1", vec![(5, remote_record("r5", "s1"))])).unwrap();
        let cursor = store.pull_cursor("m1").unwrap().unwrap();
        assert!(cursor.incomplete);
        assert_eq!(cursor.last_seq, 5);
    }

    #[test]
    fn a_rate_reprices_each_machines_row_with_the_same_event_id() {
        let (_dir, mut store) = store();
        let unseeded = |request: &str| {
            let mut record = remote_record(request, "s1");
            record["attributes"][4]["value"]["stringValue"] = serde_json::json!("claude-test-9-9");
            record
        };
        pull_machine(&mut store, "m1", fake_spool("e1", vec![(1, unseeded("same"))])).unwrap();
        pull_machine(&mut store, "m2", fake_spool("e9", vec![(1, unseeded("same"))])).unwrap();
        assert_eq!(store.set_rate_and_reprice("claude-test-9", "1970-01-01", Rate::flat(2.0, 2.0), "test").unwrap(), 2);
    }

    #[test]
    fn a_store_from_schema_one_upgrades_and_keeps_its_costs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            // A schema 1 store: the table as version 1 wrote it, with one priced event.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE usage_event (cli TEXT NOT NULL, event_id TEXT NOT NULL, session_id TEXT NOT NULL, day TEXT NOT NULL, at_ms INTEGER NOT NULL, cwd TEXT, model TEXT NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, cost_usd REAL, PRIMARY KEY (cli, event_id));
                 INSERT INTO usage_event VALUES ('claude', 'old_1', 'sold', '2026-10-15', 0, NULL, 'claude-opus-4-8', 1000000, 0, 0, 0, 7.5);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let mut store = UsageStore::open(&path).unwrap();
        let version: i64 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        // A rate change does not reprice the old event, since its cost has no recorded source.
        store.set_rate_and_reprice("claude-opus-4-8", "1970-01-01", Rate::flat(50.0, 50.0), "test").unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert!((r.sessions[0].cost_usd.unwrap() - 7.5).abs() < 1e-9);
    }

    #[test]
    fn a_rate_does_not_price_a_longer_model_id_that_only_starts_the_same_way() {
        let (_d, mut store) = store();
        store
            .ingest(&[event("req_1", "s1", "claude-opus-4-10", local_noon(2026, 10, 15), input(1_000_000))])
            .unwrap();
        // A rate for opus 4.1 must not price opus 4.10: they differ at a digit, not a dash.
        let priced = store.set_rate_and_reprice("claude-opus-4-1", "2026-10-01", Rate::flat(1.0, 1.0), "test").unwrap();
        assert_eq!(priced, 0);
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, None);
    }

    #[test]
    fn a_rate_day_given_without_padding_still_reprices_the_days_it_covers() {
        let (_d, mut store) = store();
        store
            .ingest(&[event("req_1", "s1", "claude-test-9-9", local_noon(2026, 1, 7), input(1_000_000))])
            .unwrap();
        // The day is padded before it is stored, so the January usage is inside the window.
        let priced = store.set_rate_and_reprice("claude-test-9-9", "2026-1-6", Rate::flat(2.0, 10.0), "test").unwrap();
        assert_eq!(priced, 1);
    }

    #[test]
    fn a_short_prefix_and_an_unbounded_rate_are_refused() {
        let (_d, store) = store();
        assert!(store.set_rate("claud", "2026-10-16", 1.0, 1.0, "x").is_err(), "five characters");
        assert!(store.set_rate("claude", "2026-10-16", 1.0, 1.0, "x").is_ok(), "six characters");
        assert!(store.set_rate("claude-x", "2026-10-16", 1e9, 1.0, "x").is_err(), "a rate that would overflow is refused");
    }

    #[test]
    fn a_later_read_of_a_partial_turn_replaces_its_cost() {
        let (_d, mut store) = store();
        // The first read caught the turn before it had tokens: it is priced at $0.
        store.ingest(&[event("msg_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(0))]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, Some(0.0));
        // The completed turn has the tokens, and the cost follows them.
        store.ingest(&[event("msg_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_rate_equal_to_the_one_in_force_reprices_nothing() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        // The seeded rate is $5 per million input; setting it again changes no cost.
        let changed = store.set_rate_and_reprice("claude-opus-4-8", "2026-10-15", Rate::flat(5.0, 25.0), "test").unwrap();
        assert_eq!(changed, 0);
    }

    #[test]
    fn a_prefix_with_capitals_or_a_trailing_separator_is_refused() {
        let (_d, mut store) = store();
        assert!(store.set_rate_and_reprice("Claude-Opus", "2026-10-16", Rate::flat(1.0, 1.0), "test").is_err());
        assert!(store.set_rate_and_reprice("claude-", "2026-10-16", Rate::flat(1.0, 1.0), "test").is_err());
        assert!(store.set_rate_and_reprice("claude-opus-", "2026-10-16", Rate::flat(1.0, 1.0), "test").is_err());
    }

    #[test]
    fn a_dashboard_launch_session_id_finds_the_usage_its_export_sent() {
        let (dir, store) = store();
        let path = dir.path().join("usage.sqlite");
        drop(store);
        // The launch's session is attributed at launch under the CLI's own id, with no link yet.
        UsageStore::open(&path)
            .unwrap()
            .attribute("conv-launch", "acme/team-context", Some("riverside"), "builtin/codex", "launch-7", Some("/work/a"))
            .unwrap();
        UsageStore::open(&path)
            .unwrap()
            .register_launch("launch-7", "acme/team-context", Some("riverside"), "builtin/codex", "launch-7", Some("/work/a"))
            .unwrap();
        // Its export, under the launch's token, links the session to the launch id.
        assert_eq!(ingest_telemetry_for_at(&path, "launch-7", &codex_export("conv-launch", "100")).unwrap(), 1);
        let by_launch = UsageStore::open(&path)
            .unwrap()
            .report(&Filter { session_id: Some("launch-7".into()), ..Filter::default() })
            .unwrap();
        assert_eq!(by_launch.sessions.len(), 1);
        assert_eq!(by_launch.sessions[0].session_id, "conv-launch");
    }

    #[test]
    fn a_launch_is_linked_by_its_token_even_when_the_token_and_handle_differ() {
        let (dir, store) = store();
        let path = dir.path().join("usage.sqlite");
        drop(store);
        // A launch's token and its handle are different strings; the link needs the handle the
        // attribution was written with, not the token.
        UsageStore::open(&path)
            .unwrap()
            .register_launch("tok-launch", "org/x", Some("team-y"), "agent-z", "handle-w", Some("/work/c"))
            .unwrap();
        UsageStore::open(&path)
            .unwrap()
            .attribute("conv-cli", "org/x", Some("team-y"), "agent-z", "handle-w", Some("/work/c"))
            .unwrap();
        assert_eq!(ingest_telemetry_for_at(&path, "tok-launch", &codex_export("conv-cli", "1")).unwrap(), 1);
        let r = UsageStore::open(&path)
            .unwrap()
            .report(&Filter { session_id: Some("tok-launch".into()), ..Filter::default() })
            .unwrap();
        assert_eq!(r.sessions.len(), 1);
    }

    #[test]
    fn a_window_that_starts_mid_day_includes_that_whole_day() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 16), input(1))]).unwrap();
        // sinceMs is 18:00 on the 16th, after the event at noon. Whole days count, so the 16th is included.
        let since = Local.with_ymd_and_hms(2026, 10, 16, 18, 0, 0).single().unwrap().timestamp_millis();
        let until = local_noon(2026, 10, 17);
        let r = store.report(&Filter { since_ms: Some(since), until_ms: Some(until), ..Filter::default() }).unwrap();
        assert_eq!(r.sessions.len(), 1);
    }

    #[test]
    fn tokens_add_up_across_a_session_its_days_and_the_totals() {
        let (_d, mut store) = store();
        let mut second = event("req_2", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(0));
        second.tokens = Tokens { input: 250, output: 7, ..Tokens::default() };
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(100)), second]).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(r.sessions[0].tokens.input, 350);
        assert_eq!(r.sessions[0].tokens.output, 7);
        assert_eq!(r.days[0].tokens.input, 350);
        assert_eq!(r.totals.tokens.input, 350);
        assert_eq!(r.totals.tokens.output, 7);
    }

    #[test]
    fn a_dotted_model_id_is_priced_from_the_dashed_rate() {
        let (_d, mut store) = store();
        // A CLI may write a dotted id; the seeded rate is written with dashes.
        store.ingest(&[event("msg_1", "s1", "claude-sonnet-4.6", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_wildcard_character_in_a_prefix_is_refused_even_at_full_length() {
        let (_d, mut store) = store();
        assert!(store.set_rate_and_reprice("claude%", "2026-10-16", Rate::flat(1.0, 1.0), "test").is_err());
        assert!(store.set_rate_and_reprice("claude_opus", "2026-10-16", Rate::flat(1.0, 1.0), "test").is_err());
    }

    #[test]
    fn a_later_read_with_no_cwd_keeps_the_stored_one() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1))]).unwrap();
        let mut again = event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1));
        again.cwd = None;
        store.ingest(&[again]).unwrap();
        let by_cwd = store.report(&Filter { cwd: Some("/work/a".into()), ..Filter::default() }).unwrap();
        assert_eq!(by_cwd.sessions.len(), 1);
    }

    #[test]
    fn a_zero_token_call_on_an_unpriced_model_costs_nothing_and_leaves_the_session_priced() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_0", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(0))]).unwrap();
        let s = &store.report(&Filter::default()).unwrap().sessions[0];
        assert_eq!(s.cost_usd, Some(0.0));
        assert!(s.unpriced_models.is_empty());
    }

    #[test]
    fn a_refusal_names_itself_as_invalid_so_the_caller_can_tell_it_from_a_failure() {
        let (_d, mut store) = store();
        let err = store.set_rate_and_reprice("Claude-Opus", "2026-10-16", Rate::flat(1.0, 1.0), "test").unwrap_err();
        assert!(err.starts_with("invalid:"), "{err}");
        let err = store.set_rate_and_reprice("claude-opus-4-8", "not-a-day", Rate::flat(1.0, 1.0), "test").unwrap_err();
        assert!(err.starts_with("invalid:"), "{err}");
    }

    #[test]
    fn a_reported_cost_is_kept_over_a_later_rate_priced_read() {
        let (_d, mut store) = store();
        let mut reported = event("req_r", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000));
        reported.reported_cost_usd = Some(0.42);
        store.ingest(&[reported]).unwrap();
        // A later read without the CLI's cost must not replace it with the rate table's figure.
        store.ingest(&[event("req_r", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, Some(0.42));
    }

    #[test]
    fn a_later_record_with_tokens_replaces_a_zero_reported_by_a_tokenless_one() {
        let (_d, mut store) = store();
        let mut tokenless = event("req_x", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(0));
        tokenless.reported_cost_usd = Some(0.0);
        store.ingest(&[tokenless]).unwrap();
        store.ingest(&[event("req_x", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_schema_one_cost_with_no_source_is_priced_again_by_a_later_read() {
        let (_d, mut store) = store();
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        // Simulate a schema 1 row: a cost with no recorded source, here a stale figure.
        store.conn.execute("UPDATE usage_event SET cost_usd = 0.02, cost_from = NULL WHERE event_id = 'req_1'", []).unwrap();
        store.ingest(&[event("req_1", "s1", "claude-sonnet-4-6", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_stored_zero_stays_unknown_when_a_later_read_has_no_price() {
        let (_d, mut store) = store();
        let mut tokenless = event("req_x", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(0));
        tokenless.reported_cost_usd = Some(0.0);
        store.ingest(&[tokenless]).unwrap();
        // The later read has tokens and no price (no rate, no CLI cost): the cost is unknown, not $0.
        store.ingest(&[event("req_x", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, None);
    }

    #[test]
    fn a_reported_cost_replaces_a_cost_first_priced_from_the_rate_table() {
        let (_d, mut store) = store();
        // Priced from the rate table first (no reported cost).
        store.ingest(&[event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000))]).unwrap();
        assert!((store.report(&Filter::default()).unwrap().sessions[0].cost_usd.unwrap() - 5.0).abs() < 1e-9);
        // A later read brings the CLI's own cost for the same event: the CLI's figure is the one kept.
        let mut later = event("req_1", "s1", "claude-opus-4-8", local_noon(2026, 10, 15), input(1_000_000));
        later.reported_cost_usd = Some(0.31);
        store.ingest(&[later]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].cost_usd, Some(0.31));
    }

    #[test]
    fn a_store_with_no_schema_version_that_lacks_the_column_is_upgraded_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE usage_event (cli TEXT NOT NULL, event_id TEXT NOT NULL, session_id TEXT NOT NULL, day TEXT NOT NULL, at_ms INTEGER NOT NULL, cwd TEXT, model TEXT NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, cost_usd REAL, PRIMARY KEY (cli, event_id));",
            )
            .unwrap();
        }
        // Opening upgrades it, so a write after that works.
        let mut store = UsageStore::open(&path).unwrap();
        store.ingest(&[event("req_1", "s1", "claude-test-9-9", local_noon(2026, 10, 15), input(1))]).unwrap();
        assert_eq!(store.report(&Filter::default()).unwrap().sessions[0].messages, 1);
    }

    #[test]
    fn set_rate_rejects_prices_that_are_not_positive() {
        let (_d, store) = store();
        assert!(store.set_rate("claude-opus-4-8", "2026-10-16", 0.0, 1.0, "x").is_err());
        assert!(store.set_rate("claude-opus-4-8", "2026-10-16", -1.0, 1.0, "x").is_err());
        assert!(store.set_rate("claude-opus-4-8", "2026-10-16", f64::NAN, 1.0, "x").is_err());
    }

    #[test]
    fn a_claude_export_is_stored_as_usage_once_however_many_times_it_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let export = serde_json::json!({
            "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{ "attributes": [
                { "key": "event.name", "value": { "stringValue": "api_request" } },
                { "key": "request_id", "value": { "stringValue": "req_1" } },
                { "key": "session.id", "value": { "stringValue": "sess-a" } },
                { "key": "event.timestamp", "value": { "stringValue": "2026-10-16T18:52:24.455Z" } },
                { "key": "model", "value": { "stringValue": "claude-test-9-9" } },
                { "key": "input_tokens", "value": { "intValue": 2 } },
                { "key": "output_tokens", "value": { "intValue": 4 } },
                { "key": "cost_usd", "value": { "doubleValue": 0.00325937 } }
            ] }] }] }]
        });
        assert_eq!(ingest_telemetry_for_at(&path, "no-launch", &export).unwrap(), 1);
        assert_eq!(ingest_telemetry_for_at(&path, "no-launch", &export).unwrap(), 1);
        let r = UsageStore::open(&path).unwrap().report(&Filter::default()).unwrap();
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].messages, 1);
        assert_eq!(r.sessions[0].cli, AgentCli::Claude);
        // Claude's own cost is kept, even though this model has no rate in the table.
        assert_eq!(r.sessions[0].cost_usd, Some(0.00325937));
    }

    #[test]
    fn a_row_of_a_cli_this_build_does_not_know_is_left_out() {
        let (_d, mut store) = store();
        let at = local_noon(2026, 10, 16);
        store
            .ingest(&[
                event("e1", "known", "claude-opus-4-6", at, input(10)),
                event("e2", "newer", "claude-opus-4-6", at, input(5)),
            ])
            .unwrap();
        store
            .conn
            .execute("UPDATE usage_event SET cli = 'gemini' WHERE event_id = 'e2'", [])
            .unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].session_id, "known");
    }

    #[test]
    fn set_rate_rejects_a_day_that_is_not_a_date() {
        let (_d, store) = store();
        assert!(store.set_rate("claude-opus-4-8", "16/10/2026", 1.0, 1.0, "x").is_err());
        assert!(store.set_rate("%", "2026-10-16", 1.0, 1.0, "x").is_err(), "a wildcard prefix is refused");
    }
    /// A story worktree at `<dir>/wt` working `id`, whose story file says `frontmatter`.
    fn story_worktree(dir: &Path, id: &str, frontmatter: &str) -> crate::install::worktree_index::WorktreeStoryRecord {
        let wt = dir.join(format!("wt-{id}"));
        std::fs::create_dir_all(wt.join("src/deep")).unwrap();
        let story = dir.join(format!("{id}.md"));
        std::fs::write(&story, format!("---\n{frontmatter}---\n\n# {id} heading title\n")).unwrap();
        crate::install::worktree_index::WorktreeStoryRecord::new(
            wt,
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            id,
            story,
        )
    }

    fn event_in(id: &str, session: &str, cwd: &Path, tokens: u64) -> UsageEvent {
        let mut e = event(id, session, "claude-opus-4-8", local_noon(2026, 10, 15), input(tokens));
        e.cwd = Some(cwd.to_string_lossy().into_owned());
        e
    }

    fn by_id<'a>(r: &'a UsageReport, session: &str) -> &'a SessionUsage {
        r.sessions.iter().find(|s| s.session_id == session).unwrap()
    }

    #[test]
    fn a_session_in_a_story_worktree_or_a_folder_inside_it_gets_that_story_and_others_get_none() {
        let (d, mut store) = store();
        let rec = story_worktree(
            d.path(),
            "SV-5",
            "type: story\nstory_id: SV-5\ntitle: \"Redesign the site\"\nstatus: in-progress\nparent_epic: SV\n",
        );
        let wt = rec.code_repo_path.clone();
        let elsewhere = d.path().join("content");
        std::fs::create_dir_all(&elsewhere).unwrap();
        store
            .ingest(&[
                event_in("a", "root", &wt, 1),
                event_in("b", "sub", &wt.join("src/deep"), 1),
                event_in("c", "other", &elsewhere, 1),
                // A sibling whose name only starts like the worktree's is not inside it.
                event_in("d", "sibling", &d.path().join("wt-SV-5-other"), 1),
            ])
            .unwrap();
        let mut stories = StoryResolver::new(vec![rec], HashMap::new());
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        let want = Some(UsageStory {
            id: "SV-5".into(),
            title: Some("Redesign the site".into()),
            status: Some("in-progress".into()),
            epic: Some("SV".into()),
            basis: None,
        });
        assert_eq!(by_id(&r, "root").story, want);
        assert_eq!(by_id(&r, "sub").story, want);
        assert_eq!(by_id(&r, "other").story, None);
        assert_eq!(by_id(&r, "sibling").story, None);
        let row = serde_json::to_value(by_id(&r, "root")).unwrap();
        assert_eq!(
            row["story"],
            serde_json::json!({"id": "SV-5", "title": "Redesign the site", "status": "in-progress", "epic": "SV"})
        );
        assert!(serde_json::to_value(by_id(&r, "other")).unwrap()["story"].is_null());
    }

    #[test]
    fn a_story_file_that_does_not_say_leaves_title_status_and_epic_null_or_from_its_heading() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-3", "type: story\nepic: SV-NAV\n");
        let wt = rec.code_repo_path.clone();
        let mut gone = story_worktree(d.path(), "SV-9", "type: story\n");
        std::fs::remove_file(&gone.story_source_path).unwrap();
        gone.story_source_path = d.path().join("missing.md");
        let wt_gone = gone.code_repo_path.clone();
        store.ingest(&[event_in("a", "s3", &wt, 1), event_in("b", "s9", &wt_gone, 1)]).unwrap();
        let mut stories = StoryResolver::new(vec![rec, gone], HashMap::new());
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        let s3 = by_id(&r, "s3").story.clone().unwrap();
        assert_eq!(s3.title.as_deref(), Some("SV-3 heading title"));
        assert_eq!(s3.status, None);
        assert_eq!(s3.epic.as_deref(), Some("SV-NAV"));
        assert_eq!(
            by_id(&r, "s9").story,
            Some(UsageStory { id: "SV-9".into(), title: None, status: None, epic: None, basis: None })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_is_canonicalised_on_both_sides() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-7", "status: review\n");
        let wt = rec.code_repo_path.clone();
        let link = d.path().join("link-to-wt");
        std::os::unix::fs::symlink(&wt, &link).unwrap();
        // The session ran in the link; a second record was written through the link.
        let rec_via_link = {
            let mut r = story_worktree(d.path(), "SV-8", "status: done\n");
            let real = r.code_repo_path.clone();
            let link8 = d.path().join("link-to-wt8");
            std::os::unix::fs::symlink(&real, &link8).unwrap();
            r.code_repo_path = link8;
            r
        };
        let real8 = d.path().join("wt-SV-8");
        store
            .ingest(&[event_in("a", "via-link", &link.join("src"), 1), event_in("b", "real8", &real8.join("src/deep"), 1)])
            .unwrap();
        let mut stories = StoryResolver::new(vec![rec, rec_via_link], HashMap::new());
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        assert_eq!(by_id(&r, "via-link").story.as_ref().map(|s| s.id.as_str()), Some("SV-7"));
        assert_eq!(by_id(&r, "via-link").story.as_ref().and_then(|s| s.status.as_deref()), Some("review"));
        assert_eq!(by_id(&r, "real8").story.as_ref().map(|s| s.id.as_str()), Some("SV-8"));
    }

    #[test]
    fn the_story_filter_keeps_one_storys_sessions_and_its_days_and_totals() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-5", "status: todo\n");
        let wt = rec.code_repo_path.clone();
        let mut late = event_in("c", "other", &d.path().join("content"), 100);
        late.at_ms = local_noon(2026, 10, 16);
        store
            .ingest(&[event_in("a", "s1", &wt, 3), event_in("b", "s2", &wt.join("src"), 4), late])
            .unwrap();
        let mut stories = StoryResolver::new(vec![rec], HashMap::new());
        // The id matches whatever its case.
        let filter = Filter { story: Some("sv-5".into()), ..Filter::default() };
        let r = store.report_with(&filter, &mut stories).unwrap();
        let mut ids: Vec<_> = r.sessions.iter().map(|s| s.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["s1", "s2"]);
        assert_eq!(r.totals.tokens.input, 7);
        assert_eq!(r.days.len(), 1, "the other session's day is left out");
        assert_eq!(r.days[0].tokens.input, 7);
        let none = Filter { story: Some("SV-404".into()), ..Filter::default() };
        let r = store.report_with(&none, &mut stories).unwrap();
        assert!(r.sessions.is_empty() && r.days.is_empty());
        assert_eq!(r.totals.tokens.input, 0);
        // A store with no links and no resolver ties no session to a story.
        let (_d2, mut plain) = super::tests::store();
        plain.ingest(&[event_in("a", "s1", &wt, 3)]).unwrap();
        assert!(plain.report(&Filter::default()).unwrap().sessions.iter().all(|s| s.story.is_none()));
    }

    fn assert_record_only_story_is_reported(native: bool) {
        let (dir, mut store) = store();
        let session = if native { "00000000-0000-4000-8000-000000000036" } else { "record-only-tab" };
        let mut record = record::SessionRecord::new("record-only-tab");
        record.story_id = Some("SV-CDE-36".into());
        record.org = Some("acme".into());
        record.native_session_id = native.then(|| session.to_string());
        store.write_session_record(&record).unwrap();
        store.ingest(&[event_in("record-only-request", session, &dir.path().join("content-worktree"), 3)]).unwrap();
        assert!(store.stored_stories().unwrap().is_empty());
        drop(store);
        for _ in 0..2 {
            let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
            let filtered = store.report_with(&Filter { story: Some("SV-CDE-36".into()), ..Default::default() }, &mut StoryResolver::default()).unwrap();
            assert_eq!(filtered.sessions.len(), 1, "record-only story, native id: {native}");
            assert_eq!(filtered.sessions[0].session_id, session);
            assert_eq!(filtered.totals.tokens.input, 3);
            let direct = store.report_with(&Filter::default(), &mut StoryResolver::default()).unwrap();
            assert_eq!(direct.sessions[0].story.as_ref().map(|s| s.id.as_str()), Some("SV-CDE-36"));
            assert!(store.stored_stories().unwrap().is_empty(), "a read never backfills a link");
        }
    }

    #[test]
    fn cde36_record_only_tab_story_is_filterable_after_reopen() {
        assert_record_only_story_is_reported(false);
    }

    #[test]
    fn cde36_record_only_native_story_is_filterable_after_reopen() {
        assert_record_only_story_is_reported(true);
    }

    #[test]
    fn cde36_record_story_is_only_a_launch_fallback_after_stored_and_worktree_links() {
        let (dir, mut store) = store();
        let worktree = story_worktree(dir.path(), "SV-WORKTREE", "title: Worktree\n");
        let stored = story_worktree(dir.path(), "SV-STORED", "title: Stored\n");
        for (session, cwd) in [
            ("record-only", dir.path().join("content-worktree")),
            ("worktree", worktree.code_repo_path.clone()),
            ("stored", worktree.code_repo_path.clone()),
        ] {
            let mut record = record::SessionRecord::new(session);
            record.story_id = Some("SV-RECORD".into());
            store.write_session_record(&record).unwrap();
            store.ingest(&[event_in(session, session, &cwd, 1)]).unwrap();
        }
        store.link_story("stored", &StoryLink {
            story_id: "SV-STORED".into(), source: stored.story_source_path, org: "acme".into(),
        }).unwrap();
        let report = store.report_with(&Filter::default(), &mut StoryResolver::new(vec![worktree], HashMap::new())).unwrap();
        for (session, expected) in [("record-only", "SV-RECORD"), ("worktree", "SV-WORKTREE"), ("stored", "SV-STORED")] {
            assert_eq!(by_id(&report, session).story.as_ref().map(|s| s.id.as_str()), Some(expected));
        }
    }

    #[test]
    fn cde36_record_story_fallback_excludes_old_reserved_handles_but_keeps_real_stories() {
        let routine = crate::routines::dispatch::handle("acme/context/dev/daily", "historical-run");
        let workspace = crate::commands::workspace_open::team_workspace_handle("acme/context", "dev", "draft");
        let story = crate::commands::workspace_open::story_session_handle("SV-CDE-36", None).unwrap();
        for native in [false, true] {
            let (dir, mut store) = store();
            let cases = [
                ("old-routine", routine.as_str(), routine.as_str(), None),
                ("old-workspace", workspace.as_str(), workspace.as_str(), None),
                ("story-target", story.as_str(), "SV-CDE-36", Some("SV-CDE-36")),
                ("explicit-workspace", workspace.as_str(), "SV-CDE-36", Some("SV-CDE-36")),
            ];
            let mut sessions = Vec::new();
            for (i, (tab, handle, recorded_story, expected)) in cases.into_iter().enumerate() {
                let session = if native { format!("00000000-0000-4000-8000-{:012x}", i + 1) } else { tab.into() };
                let mut record = record::SessionRecord::new(tab);
                record.handle = Some(handle.into());
                record.story_id = Some(recorded_story.into());
                record.org = Some("acme/context".into());
                record.team = Some("dev".into());
                record.run_id = Some("historical-run".into());
                record.native_session_id = native.then(|| session.clone());
                store.write_session_record(&record).unwrap();
                store.ingest(&[event_in(tab, &session, &dir.path().join("content-worktree"), 1)]).unwrap();
                sessions.push((session, expected));
            }
            drop(store);
            let store = UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
            let report = store.report_with(&Filter::default(), &mut StoryResolver::default()).unwrap();
            for (session, expected) in sessions {
                assert_eq!(by_id(&report, &session).story.as_ref().map(|s| s.id.as_str()), expected);
            }
            let real = store.report_with(&Filter { story: Some("SV-CDE-36".into()), ..Default::default() }, &mut StoryResolver::default()).unwrap();
            assert_eq!(real.sessions.len(), 2);
            for handle in [&routine, &workspace] {
                let synthetic = store.report_with(&Filter { story: Some(handle.clone()), ..Default::default() }, &mut StoryResolver::default()).unwrap();
                assert!(synthetic.sessions.is_empty());
            }
            assert!(store.stored_stories().unwrap().is_empty());
            assert_eq!(store.read_session_record("old-routine").unwrap().unwrap().story_id.as_deref(), Some(routine.as_str()));
            assert_eq!(store.read_session_record("old-workspace").unwrap().unwrap().story_id.as_deref(), Some(workspace.as_str()));
        }
    }

    #[test]
    fn a_launchs_registered_story_is_used_when_no_worktree_holds_the_session_and_the_worktree_wins() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-5", "title: Five\n");
        let wt = rec.code_repo_path.clone();
        let elsewhere = d.path().join("content");
        store
            .ingest(&[event_in("a", "launched", &elsewhere, 1), event_in("b", "both", &wt, 1)])
            .unwrap();
        store.attribute("launched", "acme", Some("t"), "builtin/claude", "h-launched", None).unwrap();
        store.attribute("both", "acme", Some("t"), "builtin/claude", "h-both", None).unwrap();
        let hints = HashMap::from([
            ("h-launched".to_string(), "SV-5".to_string()),
            ("h-both".to_string(), "SV-1".to_string()),
        ]);
        let mut stories = StoryResolver::new(vec![rec], hints);
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        // The registry's story id is read from that story's worktree record when there is one.
        let launched = by_id(&r, "launched").story.clone().unwrap();
        assert_eq!((launched.id.as_str(), launched.title.as_deref()), ("SV-5", Some("Five")));
        assert_eq!(by_id(&r, "both").story.as_ref().map(|s| s.id.as_str()), Some("SV-5"));
    }

    #[test]
    fn a_launch_story_takes_its_file_only_from_a_record_of_the_sessions_org_whatever_the_ids_case() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-5", "title: Five\n");
        let elsewhere = d.path().join("content");
        store
            .ingest(&[event_in("a", "same-org", &elsewhere, 1), event_in("b", "other-org", &elsewhere, 1)])
            .unwrap();
        store.attribute("same-org", "acme", None, "builtin/claude", "h1", None).unwrap();
        store.attribute("other-org", "globex", None, "builtin/claude", "h2", None).unwrap();
        let hints = HashMap::from([("h1".to_string(), "sv-5".to_string()), ("h2".to_string(), "SV-5".to_string())]);
        let mut stories = StoryResolver::new(vec![rec], hints);
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        assert_eq!(by_id(&r, "same-org").story.as_ref().and_then(|s| s.title.as_deref()), Some("Five"));
        assert_eq!(
            by_id(&r, "other-org").story,
            Some(UsageStory { id: "SV-5".into(), title: None, status: None, epic: None, basis: None }),
            "another org's story file is not read"
        );
    }

    /// An org context repo under `home` holding SV-CDE-63 in team `riverside`.
    fn context_org_with_story(home: &Path) -> std::path::PathBuf {
        let root = home.join("users/1/orgs/acme");
        std::fs::create_dir_all(root.join("riverside/content")).unwrap();
        std::fs::write(root.join("org.yml"), "").unwrap();
        std::fs::write(
            root.join("riverside/content/SV-CDE-63.md"),
            "---\ntype: story\nstatus: done\nstory_id: SV-CDE-63\nsource_repo: acme/app\n---\n\n# Turn labels\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn sv_cde_85_a_launch_story_with_no_worktree_record_reads_the_story_file_from_the_org() {
        let (d, mut store) = store();
        context_org_with_story(d.path());
        let cwd = d.path().join("users/1/context-worktrees/acme/riverside/review");
        store.ingest(&[event_in("a", "ctx", &cwd, 1)]).unwrap();
        store.attribute("ctx", "acme", None, "builtin/claude", "h1", None).unwrap();
        let hints = HashMap::from([("h1".to_string(), "SV-CDE-63".to_string())]);
        let mut stories = StoryResolver::for_home(d.path(), hints);
        let r = store.report_with(&Filter::default(), &mut stories).unwrap();
        let story = by_id(&r, "ctx").story.clone().unwrap();
        assert_eq!((story.id.as_str(), story.title.as_deref(), story.status.as_deref()), ("SV-CDE-63", Some("Turn labels"), Some("done")));
        assert_eq!(story.basis, None, "a launch story id is exact");
    }

    #[test]
    fn sv_cde_85_a_name_stands_for_the_story_whose_id_ends_with_it() {
        use super::story::{convention_names, convention_tail};
        assert!(convention_names(&convention_tail("cde-63"), "SV-CDE-63"));
        assert!(convention_names(&convention_tail("cde-63-2"), "sv-cde-63"));
        assert!(!convention_names(&convention_tail("cde-6"), "SV-CDE-63"));
        assert!(!convention_names(&convention_tail("63"), "SV-CDE-63"), "a bare number names nothing");
    }

    #[test]
    fn sv_cde_85_a_context_worktree_named_for_the_story_is_associated_and_not_kept() {
        let (d, mut store) = store();
        context_org_with_story(d.path());
        let wt = d.path().join("users/1/context-worktrees/acme/riverside");
        store
            .ingest(&[
                event_in("a", "named", &wt.join("cde-63/stories"), 1),
                event_in("b", "suffixed", &wt.join("cde-63-2"), 1),
                event_in("c", "other-team", &d.path().join("users/1/context-worktrees/acme/globex/cde-63"), 1),
                event_in("d", "other-name", &wt.join("cde-64"), 1),
            ])
            .unwrap();
        let mut stories = StoryResolver::for_home(d.path(), HashMap::new());
        let r = store.report_with(&Filter { story: Some("SV-CDE-63".into()), ..Default::default() }, &mut stories).unwrap();
        let mut ids: Vec<_> = r.sessions.iter().map(|s| s.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["named", "suffixed"]);
        let story = by_id(&r, "named").story.clone().unwrap();
        assert_eq!((story.basis, story.title.as_deref()), (Some("associated"), Some("Turn labels")));
        // The association is not stored: without the org roots the session has no story.
        let r = store.report_with(&Filter::default(), &mut StoryResolver::default()).unwrap();
        assert_eq!(by_id(&r, "named").story, None);
    }

    /// The ids `named_story` gives these worktree names, one resolver per call.
    fn named_ids(
        home: &Path,
        index: Option<(std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>, u64)>,
        names: &[String],
    ) -> Vec<Option<String>> {
        let wt = home.join("users/1/context-worktrees/acme/riverside");
        let mut stories = StoryResolver::for_home(home, HashMap::new());
        if let Some((index, generation)) = index {
            stories = stories.with_index(index, generation);
        }
        names.iter().map(|n| stories.named_story(&wt.join(n).to_string_lossy()).map(|(id, _)| id)).collect()
    }

    /// SV-CDE-100: story computes never walk the org per name. However many names match no
    /// story and however many computes, an org costs one walk, with the index or without.
    #[test]
    fn sv_cde_100_unmatched_worktree_names_cost_one_org_walk() {
        use crate::install::frontmatter_cache::FrontmatterCache;
        let names: Vec<String> = std::iter::once("cde-63".to_string()).chain((0..36).map(|n| format!("loose-{n}"))).collect();
        let mut expected = vec![None; names.len()];
        expected[0] = Some("SV-CDE-63".to_string());

        let d = tempfile::tempdir().unwrap();
        let root = context_org_with_story(d.path());
        let index = std::sync::Arc::new(FrontmatterCache::build_from_scan(&root));
        for _ in 0..5 {
            assert_eq!(named_ids(d.path(), Some((index.clone(), 0)), &names), expected);
        }
        assert_eq!(super::story::org_scans(&root), 1);

        let d = tempfile::tempdir().unwrap();
        let root = context_org_with_story(d.path());
        for _ in 0..5 {
            assert_eq!(named_ids(d.path(), None, &names), expected);
        }
        assert_eq!(super::story::org_scans(&root), 1);
    }

    /// SV-CDE-100: writes under `.git` (a fetch, an index write) do not move the org fingerprint,
    /// so they never cost another org walk.
    #[test]
    fn sv_cde_100_writes_under_git_do_not_rewalk_the_org() {
        let d = tempfile::tempdir().unwrap();
        let root = context_org_with_story(d.path());
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let names = ["loose".to_string()];
        assert_eq!(named_ids(d.path(), None, &names), [None]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(root.join(".git/index.lock"), "").unwrap();
        std::fs::write(root.join(".git/FETCH_HEAD"), "x").unwrap();
        assert_eq!(named_ids(d.path(), None, &["other".to_string()]), [None]);
        assert_eq!(super::story::org_scans(&root), 1);
    }

    /// SV-CDE-100: a story file with no `source_repo` is not in the index, and a worktree named
    /// for it still resolves, as it did when every lookup scanned the disk.
    #[test]
    fn sv_cde_100_a_story_without_source_repo_still_resolves_beside_a_built_index() {
        use crate::install::frontmatter_cache::FrontmatterCache;
        let d = tempfile::tempdir().unwrap();
        let root = context_org_with_story(d.path());
        std::fs::write(
            root.join("riverside/content/SV-CDE-70.md"),
            "---\ntype: story\nstatus: todo\nstory_id: SV-CDE-70\n---\n\n# No repo\n",
        )
        .unwrap();
        let index = std::sync::Arc::new(FrontmatterCache::build_from_scan(&root));
        assert!(index.by_story_id("SV-CDE-70").is_empty(), "the index skips a story with no source_repo");
        let ids = named_ids(d.path(), Some((index, 0)), &["cde-70".to_string()]);
        assert_eq!(ids, [Some("SV-CDE-70".to_string())]);
    }

    /// SV-CDE-100: a story added in a subfolder resolves once the index has it, though the
    /// org fingerprint (top-level folders only) never moved.
    #[test]
    fn sv_cde_100_a_story_added_in_a_subfolder_resolves_after_the_index_updates() {
        use crate::install::frontmatter_cache::FrontmatterCache;
        let d = tempfile::tempdir().unwrap();
        let root = context_org_with_story(d.path());
        std::fs::create_dir_all(root.join("riverside/content/epic")).unwrap();
        let shared = FrontmatterCache::shared(FrontmatterCache::build_from_scan(&root));
        let names = ["cde-71".to_string()];
        assert_eq!(named_ids(d.path(), Some((FrontmatterCache::snapshot(&shared), 0)), &names), [None]);
        let added = root.join("riverside/content/epic/SV-CDE-71.md");
        std::fs::write(&added, "---\ntype: story\nstatus: todo\nstory_id: SV-CDE-71\nsource_repo: acme/app\n---\n\n# Added\n").unwrap();
        FrontmatterCache::refresh_under_lock(&shared, &added, "riverside");
        let ids = named_ids(d.path(), Some((FrontmatterCache::snapshot(&shared), 1)), &names);
        assert_eq!(ids, [Some("SV-CDE-71".to_string())]);
    }

    #[test]
    fn the_innermost_of_nested_worktrees_wins() {
        let d = tempfile::tempdir().unwrap();
        let outer = story_worktree(d.path(), "OUT-1", "title: Outer\n");
        let mut inner = story_worktree(d.path(), "IN-1", "title: Inner\n");
        let inner_dir = outer.code_repo_path.join("src/deep");
        inner.code_repo_path = inner_dir.clone();
        // Either order of the records gives the same answer.
        for records in [vec![outer.clone(), inner.clone()], vec![inner.clone(), outer.clone()]] {
            let (_d, mut s) = store();
            s.ingest(&[
                event_in("a", "in-inner", &inner_dir.join("x"), 1),
                event_in("b", "in-outer", &outer.code_repo_path.join("src"), 1),
            ])
            .unwrap();
            let r = s.report_with(&Filter::default(), &mut StoryResolver::new(records, HashMap::new())).unwrap();
            assert_eq!(by_id(&r, "in-inner").story.as_ref().map(|s| s.id.as_str()), Some("IN-1"));
            assert_eq!(by_id(&r, "in-outer").story.as_ref().map(|s| s.id.as_str()), Some("OUT-1"));
        }
    }

    #[test]
    fn a_session_keeps_its_story_after_its_worktree_record_is_gone() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-5", "title: Five\nstatus: review\n");
        let wt = rec.code_repo_path.clone();
        let story_file = rec.story_source_path.clone();
        store.ingest(&[event_in("a", "recorded", &wt, 1), event_in("b", "reported", &wt.join("src"), 1)]).unwrap();
        // Linked when its usage was recorded...
        store
            .link_stories(&[("recorded".into(), wt.to_string_lossy().into_owned())], || {
                StoryResolver::new(vec![rec.clone()], HashMap::new())
            })
            .unwrap();
        // ...or on a report that finds it live.
        store.report_with(&Filter::default(), &mut StoryResolver::new(vec![rec], HashMap::new())).unwrap();
        // The worktree and its record are gone: the stored link still names the story and its file.
        std::fs::remove_dir_all(&wt).unwrap();
        let r = store.report_with(&Filter::default(), &mut StoryResolver::default()).unwrap();
        for session in ["recorded", "reported"] {
            let story = by_id(&r, session).story.clone().unwrap();
            assert_eq!((story.id.as_str(), story.title.as_deref(), story.status.as_deref()), ("SV-5", Some("Five"), Some("review")));
        }
        let filter = Filter { story: Some("SV-5".into()), ..Filter::default() };
        assert_eq!(store.report_with(&filter, &mut StoryResolver::default()).unwrap().sessions.len(), 2);
        // The story file is gone too: the stored id remains.
        std::fs::remove_file(&story_file).unwrap();
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(by_id(&r, "recorded").story, Some(UsageStory { id: "SV-5".into(), title: None, status: None, epic: None, basis: None }));
    }

    #[test]
    fn the_session_story_table_is_added_without_changing_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        {
            let store = UsageStore::open(&path).unwrap();
            store.conn.execute_batch("DROP TABLE session_story;").unwrap();
        }
        let store = UsageStore::open(&path).unwrap();
        assert!(has_column(&store.conn, "session_story", "story_path").unwrap());
        let version: i64 = store.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        // An older CodeSense refuses a store whose version is above its own: it stays at 5, so
        // the older build's check (version > 5 refuses) still opens it.
        assert_eq!(version, 5);
        assert!(version <= 5, "an older build would refuse this store");
    }

    #[test]
    fn a_story_link_is_kept_only_for_usage_recorded_on_this_mac() {
        let (d, mut store) = store();
        let rec = story_worktree(d.path(), "SV-5", "title: Five\n");
        let wt = rec.code_repo_path.clone();
        store.ingest(&[event_in("a", "local", &wt, 1)]).unwrap();
        store.ingest_for("mac-2", &[event_in("b", "remote", &wt, 1)]).unwrap();
        let r = store.report_with(&Filter::default(), &mut StoryResolver::new(vec![rec], HashMap::new())).unwrap();
        // Both are tied live...
        assert!(r.sessions.iter().all(|s| s.story.as_ref().is_some_and(|s| s.id == "SV-5")));
        // ...but only this Mac's session keeps the link once the worktree record is gone.
        let r = store.report(&Filter::default()).unwrap();
        assert_eq!(by_id(&r, "local").story.as_ref().map(|s| s.id.as_str()), Some("SV-5"));
        assert_eq!(by_id(&r, "remote").story, None);
    }
}
