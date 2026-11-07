// SPDX-License-Identifier: MIT
//! `weft::spawn` — the typed ephemeral worker spawn (SW-WV-A2).
//!
//! Any agent spawns a **typed ephemeral worker** — subclass `impl`,
//! `review`, `planning`, or `research` — into its own managed COW worktree with a
//! per-subclass profile baked into the launch: one job, one context, one exit.
//! Every worker is born, does its job, and exits. The profile is narrow: see below
//! for what it does and does not restrict.
//!
//! # Where the pieces live
//!
//! - The **types** ([`WorkerSubclass`], [`WorkerSpawnRequest`], [`WeftSpawnResult`],
//!   [`WeftError`]) and the **orchestration** ([`wv_spawn_worker`] and its testable
//!   core [`spawn_with`]) live here.
//! - The **roster writes** (`provision` insert → `dispatch` transition → delete on
//!   failure, each with its co-committed `wv_write_log` audit row) live in
//!   [`super::sqlite`] (the sanctioned single-writer file, ARCHITECTURE §1.2), and
//!   this module calls through them.
//! - The **PTY launch** goes through the SW-AO seam
//!   [`crate::commands::mcp_workspace::launch_story_session`], reached only via the
//!   [`WorkerLauncher`] trait so the orchestration (idempotency, roster
//!   state machine, error rollback) is unit-testable without a real PTY.
//!
//! # What a worker is and is not restricted by
//!
//! - **Orchestration verbs:** nothing keeps a worker from calling `wv_spawn_worker`
//!   or the other `wv_*` verbs. `InteractiveGate::admit` checks only that a verb
//!   is registered, not who calls it, so a worker can spawn another worker.
//! - **MCP servers (spawn):** [`WorkerSubclass::mcp_servers`] is empty for every
//!   subclass, so the scoped launch projects only the `codesense` bridge and strips
//!   every other team MCP server. The bridge keeps its full tool surface: nothing at
//!   spawn removes a `codesense` tool.
//! - **Tool deny-list (worktree):** each subclass writes a
//!   [`WorkerSubclass::deny_tools`] list (`Bash`/`Edit`/`Write` only) into the
//!   worktree's `.claude/settings.local.json` via `install_agent_permissions`. Only a
//!   Claude worker reads that file; a Codex worker gets no equivalent restriction.

use super::sqlite::{
    wv_delete_provision_row, wv_dispatch_worker_row, wv_find_live_worker, wv_provision_worker_row,
};
use super::store::MutateResult;
use super::store::WeftStore;
use crate::install::agent_account_store::AgentProvider;

/// The canonical MCP verb string for the worker-spawn tool. Registered in BOTH
/// [`crate::commands::interactive_gate::INTERACTIVE_VERBS`] and
/// [`super::sqlite::WV_INTERACTIVE_VERBS`] (the mandatory two-seam gate, §3.1), and
/// gated by `InteractiveGate::admit(WV_SPAWN_WORKER_VERB)` at the MCP dispatch
/// branch. That gate does not identify the caller, so a worker can call it too.
pub const WV_SPAWN_WORKER_VERB: &str = "wv_spawn_worker";

/// A typed worker subclass — the four core capability profiles (AC-2, AC-7).
///
/// Each subclass bakes a profile into the launch: the MCP server set
/// ([`Self::mcp_servers`], bridge only for every subclass) and a tool deny-list
/// ([`Self::deny_tools`]) written into the worktree's `.claude/settings.local.json`.
/// The enum can be extended with team-defined
/// subclasses later without a schema change (the roster stores the subclass as
/// text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerSubclass {
    /// The acting subclass — full tool-use, denies nothing.
    Impl,
    /// Reads and produces a verdict; must not modify files (`Bash` denied).
    Review,
    /// Read-and-produce; no write, no bash (`Bash`/`Edit`/`Write` denied).
    Planning,
    /// May run bash for reads; must not write files (`Edit`/`Write` denied).
    Research,
}

impl WorkerSubclass {
    /// The wire / storage string (`wv_roster.subclass`).
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerSubclass::Impl => "impl",
            WorkerSubclass::Review => "review",
            WorkerSubclass::Planning => "planning",
            WorkerSubclass::Research => "research",
        }
    }

    /// Parse a subclass from its wire string; `None` for an unknown value.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "impl" => Some(WorkerSubclass::Impl),
            "review" => Some(WorkerSubclass::Review),
            "planning" => Some(WorkerSubclass::Planning),
            "research" => Some(WorkerSubclass::Research),
            _ => None,
        }
    }

    /// Every subclass variant, for exhaustive checks over the subclasses. Kept next to the enum so a new variant
    /// forces a compile error here (the `match` below is exhaustive).
    pub fn all() -> &'static [WorkerSubclass] {
        &[
            WorkerSubclass::Impl,
            WorkerSubclass::Review,
            WorkerSubclass::Planning,
            WorkerSubclass::Research,
        ]
    }

    /// The per-subclass tool deny-list written into the worktree's
    /// `.claude/settings.local.json` (AC-2). Per the Weft glossary,
    /// `read-and-produce` subclasses carry no write/bash capability:
    ///
    /// - `impl` — deny nothing (acting subclass, full allow list).
    /// - `review` — deny `Bash` (reads + produces a verdict; must not modify files).
    /// - `planning` — deny `Bash`, `Edit`, `Write` (read-and-produce).
    /// - `research` — deny `Edit`, `Write` (may run bash for reads; must not write).
    pub fn deny_tools(self) -> &'static [&'static str] {
        match self {
            WorkerSubclass::Impl => &[],
            WorkerSubclass::Review => &["Bash"],
            WorkerSubclass::Planning => &["Bash", "Edit", "Write"],
            WorkerSubclass::Research => &["Edit", "Write"],
        }
    }

    /// The `.mcp.json` server ids (never MCP tool names) passed to
    /// `ResolvedLaunchProfile::mcp_allow`. The scope projection always force-includes the
    /// `codesense` bridge and keeps only listed ids that name an installed server, so
    /// an empty list means "bridge only": the worker sees the full `codesense` tool
    /// surface and no other team MCP server. It does not narrow the bridge's tools.
    pub fn mcp_servers(self) -> Option<Vec<String>> {
        Some(Vec::new())
    }
}

/// The request to spawn one typed ephemeral worker (Task 1).
#[derive(Debug, Clone)]
pub struct WorkerSpawnRequest {
    /// The story the worker works — also the `wv_roster` PK (`name`), so at most one
    /// live worker exists per story per scope (the uniqueness invariant, AC-6).
    pub story_id: String,
    /// The worker subclass — its capability profile.
    pub subclass: WorkerSubclass,
    /// The first prompt seeded into the worker (e.g. a dispatch brief). `None` lets
    /// the launch spine derive the story recipe.
    pub initial_prompt: Option<String>,
    /// The agent provider (ADK-neutral seam, AC-4). `None` runs the preferred runtime.
    pub provider: Option<AgentProvider>,
    /// The scope the worker's roster row is filed under (only `"root"` today).
    pub scope: String,
    /// The durable run key to stamp onto the roster row at dispatch (SW-WA-A5), so the
    /// watchdog / done-record path can correlate the live session to its run-store
    /// record. `None` when the caller has no run correlation for this worker.
    pub run_id: Option<String>,
    /// The session that asked for the worker, taken from the caller's verified connection and
    /// never from a request parameter. `None` for a person or for the durable dispatch bridge.
    pub launched_by: Option<String>,
}

impl WorkerSpawnRequest {
    /// The runtime this request runs — the supplied provider's CLI, or the preferred runtime
    /// when it names none (AC-4).
    pub fn resolved_runtime(&self) -> codesense_pty_core::agent_cli::AgentCli {
        self.provider.map_or(
            crate::agents::adapter::PREFERRED_RUNTIME,
            crate::agents::adapter::cli_of_provider,
        )
    }
}

/// The outcome of a [`wv_spawn_worker`] call (Task 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeftSpawnResult {
    /// A new worker was spawned; `handle` is its live session handle (the story_id,
    /// per the launch spine's opaque-handle contract).
    Spawned { handle: String },
    /// A live worker already exists for this story; no second PTY was opened (AC-6).
    /// `handle` is the existing worker's session handle when known.
    AlreadyLive { handle: String },
}

/// A typed spawn error (Task 1). Kept distinct from the store's `WvWriteError` so
/// the caller (the MCP verb) maps each variant to the right outcome.
#[derive(Debug)]
pub enum WeftError {
    /// The PTY launch failed; the provision roster row was rolled back (AC-3). The
    /// inner string is the launch-spine error.
    SpawnFailed(String),
    /// A ledger/storage error surfaced from the weft store.
    Store(String),
}

impl std::fmt::Display for WeftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeftError::SpawnFailed(e) => write!(f, "worker spawn failed: {e}"),
            WeftError::Store(e) => write!(f, "weft store error: {e}"),
        }
    }
}
impl std::error::Error for WeftError {}

impl From<super::sqlite::WvWriteError> for WeftError {
    fn from(e: super::sqlite::WvWriteError) -> Self {
        WeftError::Store(e.to_string())
    }
}

/// The PTY-launch seam, abstracted so the spawn orchestration is unit-testable
/// without a real `openpty`/fork.
///
/// The production implementation ([`RealWorkerLauncher`]) provisions the
/// per-worker `.claude/settings.local.json` deny-list and calls
/// [`crate::commands::mcp_workspace::launch_story_session`] on the blocking pool
/// (AC-5); a test double records the call and returns a synthetic handle so the
/// roster state machine and error rollback can be exercised deterministically.
#[async_trait::async_trait]
pub trait WorkerLauncher: Send + Sync {
    /// Provision the worktree capability profile and open the worker PTY, returning
    /// its live session handle on success. Errors propagate to
    /// [`WeftError::SpawnFailed`] and trigger the provision-row rollback (AC-3).
    async fn launch(&self, req: &WorkerSpawnRequest) -> Result<String, String>;
}

/// The spawn orchestration core (AC-1, AC-2, AC-3, AC-4, AC-5, AC-6), factored out
/// of [`wv_spawn_worker`] so it takes a store + launcher and is exercised end-to-end
/// in tests with a stub launcher.
///
/// Steps (ordered, each cites its AC):
/// 1. **Idempotency** — a live `wv_roster` row for `story_id` → [`WeftSpawnResult::AlreadyLive`],
///    no second PTY (AC-6).
/// 2. **Provision** — insert the roster row `status='provision'` BEFORE launch (AC-3).
/// 3. **Launch** — the injected [`WorkerLauncher`] (real: off-thread PTY, AC-5).
/// 4. **On success** — guarded `provision → dispatch` UPDATE + audit (AC-3), then
///    return [`WeftSpawnResult::Spawned`]. **On failure** — delete the provision row
///    (no audit) and propagate [`WeftError::SpawnFailed`] (AC-3).
pub async fn spawn_with<L: WorkerLauncher + ?Sized>(
    store: &WeftStore,
    launcher: &L,
    req: WorkerSpawnRequest,
) -> Result<WeftSpawnResult, WeftError> {
    // 1. Idempotency guard — a live worker for this story returns AlreadyLive with
    //    no second PTY. AC-6. The find-then-provision is two transactions, so two
    //    concurrent same-story spawns can race; the dispatch CAS below surfaces the
    //    loser as a store error. Folding the check into one IMMEDIATE tx (and killing
    //    the leaked PTY on a dispatch-CAS loss) is tracked as DW-WV-A2-1.
    if let Some(live) = wv_find_live_worker(store, &req.story_id)? {
        return Ok(WeftSpawnResult::AlreadyLive {
            handle: live.session_id.unwrap_or_else(|| live.name.clone()),
        });
    }

    // 2. Write the provision roster row BEFORE the PTY is live, so the caller
    //    can read it synchronously. Insert + audit co-commit in one IMMEDIATE tx. AC-3.
    wv_provision_worker_row(store, &req.story_id, req.subclass.as_str(), &req.scope)?;

    // 3. Off-thread PTY launch through the injected seam. The real launcher runs the
    //    blocking openpty/fork on the Tokio blocking pool (AC-5).
    match launcher.launch(&req).await {
        Ok(handle) => {
            // 4a. Transition provision → dispatch, stamping the session handle.
            //     Guarded UPDATE + changes()==1 (ONE-WRITER) + co-committed audit. AC-3.
            match wv_dispatch_worker_row(store, &req.story_id, &handle, req.run_id.as_deref())? {
                MutateResult::Updated => Ok(WeftSpawnResult::Spawned { handle }),
                // The provision row vanished under us (a concurrent delete/dispatch);
                // the PTY is live but the ledger lost the CAS. Surface as a store
                // error rather than silently returning a row-less success.
                MutateResult::NotOwner => Err(WeftError::Store(format!(
                    "roster dispatch transition lost the CAS for '{}' (row not in 'provision')",
                    req.story_id
                ))),
            }
        }
        Err(e) => {
            // 4b. Roll back the provision row (no audit entry for an aborted
            //     provision) and propagate the failure. AC-3.
            let _ = wv_delete_provision_row(store, &req.story_id);
            Err(WeftError::SpawnFailed(e))
        }
    }
}

/// Spawn a typed ephemeral worker (Task 2) — the production entry point.
///
/// Opens the caller-captured store and delegates to [`spawn_with`] with the
/// real [`RealWorkerLauncher`]. The MCP verb wraps this with the two-seam interactive
/// gate (spine §3.1).
///
/// AC-5: the PTY-opening work runs on the Tokio blocking pool inside
/// [`RealWorkerLauncher::launch`] via `commands::await_blocking`, so neither the
/// Tauri main thread nor the async executor is blocked.
pub async fn wv_spawn_worker(
    app: &tauri::AppHandle,
    store_path: std::path::PathBuf,
    store_org: String,
    req: WorkerSpawnRequest,
) -> Result<WeftSpawnResult, WeftError> {
    let store = resolve_store(store_path.clone()).await?;
    let launcher = RealWorkerLauncher {
        app: app.clone(),
        store_path,
        store_org,
    };
    let outcome = spawn_with(&store, &launcher, req).await?;
    // SW-WV-A4 (AC-5): the moment a worker is dispatched (session bound), arm its
    // silence watchdog so a crash-before-signal surfaces itself. Registered in the
    // per-team `WeftWatchdogRegistry`; the `signal_done` watchdog-cancel hook (A3
    // registry) cancels it on a real finish. A missing registry (e.g. a test harness
    // without the managed state) is a graceful skip — spawn never fails on it.
    if let WeftSpawnResult::Spawned { handle } = &outcome {
        arm_worker_silence_watchdog(app, store.path().to_path_buf(), handle);
    }
    Ok(outcome)
}

/// SW-WV-A4 (AC-5): arm + register one worker's silence watchdog at dispatch time.
///
/// The watchdog is keyed on the worker's LIVE PTY `session_id` — the same value the
/// worker's own `signal_done` reports (`$CODESENSE_TAB_SESSION_ID`) and that both the
/// A3 `roster-update` hook and A4's `watchdog-cancel` hook resolve by. The spawn
/// `handle` is the opaque launch handle (the story_id per the launch spine's contract);
/// [`crate::commands::dashboard_launch::DashboardLaunchState::resolve`] maps it to the
/// bound live session id, so cancel-on-`signal_done` and the roster `needs-attention`
/// flip both hit the same row. A handle with no bound session (should not happen for a
/// just-`Spawned` worker) skips arming rather than register an unmatchable watchdog.
///
/// The `cancel` channel starts `false`; the A3 `watchdog-cancel` hook sends `true` on
/// `signal_done`. The production TTL is [`crate::weft::WORKER_SILENCE_TTL_S`]. A
/// non-managed registry (test contexts) is a no-op skip.
fn arm_worker_silence_watchdog(
    app: &tauri::AppHandle,
    store_path: std::path::PathBuf,
    handle: &str,
) {
    use tauri::Manager;
    let Some(registry) = app.try_state::<crate::weft::WeftWatchdogRegistry>() else {
        return;
    };
    // Resolve the opaque launch handle to the worker's live PTY session id — the id
    // `signal_done` reports and the cancel/roster hooks key on.
    let Some(session_id) = app
        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .and_then(|state| state.resolve(handle))
    else {
        log::warn!(
            target: "weft",
            "silence watchdog not armed: handle {handle:?} has no bound live session"
        );
        return;
    };
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let effects = std::sync::Arc::new(crate::weft::AppHandleEffects {
        app: app.clone(),
        store_path,
    });
    let handle = crate::weft::arm_silence_watchdog(
        effects,
        session_id.clone(),
        cancel_rx,
        std::time::Duration::from_secs(crate::weft::WORKER_SILENCE_TTL_S),
    );
    registry.register(session_id, cancel_tx, handle);
}

/// Open the already captured store off the reactor. There is no ambient-team lookup.
async fn resolve_store(path: std::path::PathBuf) -> Result<WeftStore, WeftError> {
    crate::commands::await_blocking("weft.spawn.open_store", move || {
        WeftStore::open(path).map_err(|e| e.to_string())
    })
    .await
    .map_err(WeftError::Store)
}

/// The worker exit-contract doctrine every spawned Weft worker starts with: deliver the
/// packet idempotently, report the drafted handoff, then `signal_done(+doneRecord)`. It
/// is Weft's own text, so it ships with the app rather than as team content; the docs
/// repo carries a byte-equal copy as an example.
pub(crate) const WORKER_EXIT_CONTRACT: &str = include_str!("worker_exit_contract.md");

/// Stage [`WORKER_EXIT_CONTRACT`] into a spawned worker's worktree
/// (`<worktree>/.claude/skills/worker-exit-contract/SKILL.md`), so the worker is told the
/// exit order before it starts. Idempotent: it overwrites the file so a re-provision
/// always lands the current doctrine. Pure filesystem I/O — the caller runs it on the
/// blocking pool.
pub(super) fn stage_worker_exit_contract_into(
    worktree_dir: &std::path::Path,
) -> Result<(), String> {
    let skill_dir = worktree_dir
        .join(".claude")
        .join("skills")
        .join("worker-exit-contract");
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| format!("create worker-exit-contract skill dir: {e}"))?;
    std::fs::write(skill_dir.join("SKILL.md"), WORKER_EXIT_CONTRACT)
        .map_err(|e| format!("write worker-exit-contract SKILL.md: {e}"))
}

/// The agent a worker launch runs: the unattended built-in of the request's provider, prepared
/// headless against `target_team` and scoped to the subclass's MCP servers.
pub(crate) fn prepare_worker_agent(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    req: &WorkerSpawnRequest,
    target_team: Option<(String, String)>,
) -> Result<crate::agents::launch::AgentLaunch, String> {
    let runtime = req.resolved_runtime();
    let mut agent = crate::agents::launch::prepare(
        home,
        principal,
        &crate::agents::builtin::unattended_key(runtime),
        crate::agents::launch::PrepareOptions {
            target_team,
            mode_override: Some(crate::agents::adapter::LaunchMode::Headless),
            prompt: req.initial_prompt.as_deref(),
            account: None,
        },
    )?;
    agent.launch.mcp_allow = req.subclass.mcp_servers();
    Ok(agent)
}

/// The production [`WorkerLauncher`] — provisions the per-worker capability profile
/// then opens the PTY via the SW-AO launch spine.
struct RealWorkerLauncher {
    app: tauri::AppHandle,
    store_path: std::path::PathBuf,
    /// The org of the store's team: the worker's story is looked up there only.
    store_org: String,
}

#[async_trait::async_trait]
impl WorkerLauncher for RealWorkerLauncher {
    async fn launch(&self, req: &WorkerSpawnRequest) -> Result<String, String> {
        // Resolve the story's managed worktree so we can write the subclass deny-list
        // into it BEFORE the agent starts (AC-2). This mirrors `provision_headless`
        // (`dashboard_launch.rs`): merge-never-clobber, `defaultMode: "dontAsk"`,
        // `git_exclude: false` (a managed worktree already excluded the file).
        let (ctx, worktree_dir, _, team) =
            crate::commands::mcp_workspace::resolve_hydrated_story_worktree(
                &self.app,
                &req.story_id,
                Some(self.store_org.as_str()),
            )
            .await?;
        // The launched worker's MCP uses its story's captured team. Refuse a
        // different team before provisioning so its roster cannot split stores.
        let story_store = super::sqlite::resolve_weft_store_path(
            &ctx.codesense_home,
            &ctx.principal(),
            &team.org,
            &team.team,
        )?;
        if story_store != self.store_path {
            return Err("worker story must belong to the captured Weft store's team".into());
        }
        let deny: Vec<&str> = req.subclass.deny_tools().to_vec();
        let worktree_for_perms = worktree_dir.clone();
        // AC-5: the settings write is filesystem I/O — run it off the async reactor
        // on the blocking pool, together with the PTY-adjacent provisioning.
        crate::commands::await_blocking("weft.spawn_worker.provision", move || {
            crate::commands::agent_provision::install_agent_permissions(
                &worktree_for_perms,
                &[],
                &deny,
                false,
                Some("dontAsk"),
            );
            // Stage the C7 exit-contract skill so the worker is told the
            // deliver → handoff → signal_done order before it starts.
            stage_worker_exit_contract_into(&worktree_for_perms)
        })
        .await?;

        // The worker is the provider's built-in agent, headless and scoped to its
        // subclass's servers. The canonical `.mcp.json` is never mutated.
        let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
        let worker = req.clone();
        let agent = crate::commands::await_blocking("weft.spawn_worker.prepare", move || {
            prepare_worker_agent(&home, &principal, &worker, Some((team.org, team.team)))
        })
        .await?;

        // AC-5 (assertion): the PTY-opening work (openpty + ZDOTDIR write + fork/exec)
        // happens inside `launch_story_session` → the layout's `open_tab`, which the launch
        // spine already dispatches to a blocking worker thread. The controller's
        // dispatch step never blocks the Tauri main thread or the async executor.
        let outcome = crate::commands::mcp_workspace::launch_story_session_by(
            &self.app,
            &req.story_id,
            Some(self.store_org.as_str()),
            req.initial_prompt.as_deref(),
            agent,
            None,
            crate::commands::mcp_workspace::SessionView::Background,
            req.launched_by.clone(),
        )
        .await?;
        Ok(outcome.handle)
    }
}

#[cfg(test)]
mod tests;
