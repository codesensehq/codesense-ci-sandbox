// SPDX-License-Identifier: MIT
//! SW-OR-1 (AC-3) — the trusted-side **`Interactive` autonomy gate** that fronts
//! the UI-driving MCP tools (`open_workspace`, `send_to_session`).
//!
//! **Why it lives in Rust.** The dashboard's browser bridge is a separate path
//! from the MCP call path — an orchestrator drives the app over the MCP socket,
//! not the browser bridge. So the autonomy decision that mediates a UI-driving verb must
//! run in the **trusted process**, in front of the tool handlers, or the whole
//! anti-forgery posture (SE-5 charter) is bypassed.
//!
//! **Deny-first, one tier (the slice).** This story ships exactly ONE tier —
//! `Interactive` — with a fixed allowset of the two UI-driving verbs. The full
//! tier set + approve-once allowlists + budgets are SW-OR-2; this module is the
//! seam they extend, not the finished policy. A verb the tier does not admit is
//! refused `not-granted:` before any spawn/resolve work touches a PTY.
//!
//! **Typed refusal contract:**
//!  - `not-granted: <verb>` — the tier does not admit this verb (minted here).
//!  - `denied: <reason>` — a policy refusal (minted here). SW-OR-1 minted this for a
//!    `headed:true` `open_workspace`; SW-WA-F5 relaxed that (headed is the default
//!    now — see [`WorkspaceMode`]), so the remaining triggers are an unrecognized
//!    `mode` string ([`WorkspaceMode::resolve`]) and the scoped-grant policy checks.
//!  - `busy` — the shared [`SpawnCap`](super::spawn_cap::SpawnCap) is saturated.
//!    Reused verbatim from [`SpawnCapError::Busy`] so the gate and the cap agree.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::spawn_cap::SpawnCapError;

/// The UI-driving verbs the `Interactive` tier admits in this slice (SW-OR-1).
/// SW-OR-2 replaces this fixed set with the configurable tier/allowlist policy.
///
/// SW-WV-B1 registration pattern: any story adding an interactive `wv_*` MCP
/// verb MUST (a) append the verb string to BOTH this constant AND
/// `crate::weft::WV_INTERACTIVE_VERBS`, and (b) gate it with
/// `InteractiveGate::admit` in `mcp/handler.rs` at its dispatch branch. The
/// `every_wv_interactive_verb_has_both_seams` test in `weft::sqlite` cross-checks
/// the two registries so a verb can never be added to one without the other.
pub const INTERACTIVE_VERBS: &[&str] = &[
    "create_team",
    "rename_team",
    "archive_team",
    "delete_team",
    "set_team_parent",
    "set_team_starter_page",
    "rename_workspace",
    "add_source_repo",
    "attach_source_repo_path",
    "remove_source_repo",
    "create_local_organization",
    "commit_registry_edit_local",
    "save_source_repo_attachment",
    "attach_context_repository",
    "detach_context_repository",
    "test_machine_connection",
    "enroll_private_machine_account",
    "install_terminal_server",
    "prepare_machine_terminal_server_replacement",
    "restart_machine_terminal_server",
    // SW-AGR-19 PR-8: Update all over MCP, each machine as listed.
    "update_terminal_servers",
    "save_machine",
    "resolve_machine_identity",
    "set_machine_hidden",
    "set_machine_auto_connect",
    "set_machine_remote_mcp_control",
    "unshare_machine",
    "remove_machine",
    "connect_machine",
    "disconnect_machine",
    "retry_machine",
    "capture_machine_passphrase",
    "open_remote_tab",
    "prepare_remote_workspace",
    "remote_workspace_fetch",
    "update_workspace_base",
    // SW-WS-1: re-home a workspace to another machine. Interactive because
    // it mutates the authoritative `location.machineId` + spans two machines
    // + involves deliberate user intent (workspace-header affordance).
    "move_workspace",
    "start_remote_agent_task",
    "read_remote_agent_task",
    "cancel_remote_agent_task",
    "open_workspace",
    "send_to_session",
    // SW-AGR-9 (SPEC §9.5): every wake delivery into a live session, and every
    // session-timer change, passes the same tier as `send_to_session`. Gated in
    // `crate::delivery::authority`, the one path wakes and timers take.
    "wake_deliver",
    "schedule_wakeup",
    // Switches the app's active team (and the UI with it) — a UI-driving
    // operator verb, gated at its `mcp/handler.rs` dispatch branch.
    "set_active_team",
    // SW-WA-F1: open a normal CLI terminal tab in an OPEN workspace over MCP. A
    // UI-driving operator verb (not a `wv_*` verb), so it lives ONLY
    // here — deliberately absent from `crate::weft::WV_INTERACTIVE_VERBS` (the
    // `every_wv_interactive_verb_has_both_seams` cross-check is one-way: every WV
    // verb must appear here, not the reverse). Gated by `InteractiveGate::admit`
    // at its `mcp/handler.rs` dispatch branch.
    "open_terminal_tab",
    // SW-WA-F2: run a shell command in the F1-opened handle-addressed tab over MCP.
    // A raw shell-run is MORE sensitive than an agent prompt, so it carries the
    // shell-run consent decision here (v1 Interactive tier; fuller tier/allowlist
    // policy deferred to SW-OR-2). A UI-driving operator verb (not a `wv_*`
    // verb), so it lives ONLY here — absent from
    // `crate::weft::WV_INTERACTIVE_VERBS` (the cross-check is one-way).
    "run_in_terminal_tab",
    // SW-WA-F3: read a handle-addressed terminal tab's current scrollback back
    // over MCP (the F1 tab, driven by F2). Same operator-verb tier as
    // `open_terminal_tab` — deny-first, gated at its `mcp/handler.rs` dispatch
    // branch; deliberately absent from `crate::weft::WV_INTERACTIVE_VERBS`.
    "read_terminal_tab",
    // Rename a terminal tab through the same layout rename a person uses.
    "rename_terminal_tab",
    // Operator close verbs — the symmetric pair for `open_terminal_tab` and
    // `open_workspace`. Gated at their `mcp/handler.rs` dispatch branches, and
    // deliberately absent from `crate::weft::WV_INTERACTIVE_VERBS` (not `wv_*`
    // verbs; the cross-check is one-way).
    "close_terminal_tab",
    "close_workspace",
    "workspace_group_edit",
    "workspace_group_list",
    "workspace_group_create",
    "workspace_group_rename",
    "workspace_group_set_members",
    "workspace_group_add",
    "workspace_group_remove",
    "workspace_group_delete",
    "wv_spawn_worker",
    "wv_post_assertion",
    "wv_classify_action",
    "wv_write_dispatch",
    "wv_verify_done",
    // SW-WV-C5: the release + closure verbs (start-and-release +
    // two-condition close predicate D5). Both carry the two-seam gate — this entry
    // plus `InteractiveGate::admit` at each `mcp/handler.rs` dispatch branch — and
    // are registered in `crate::weft::WV_INTERACTIVE_VERBS` too.
    "wv_mark_dispatched",
    "wv_close_task",
    // SW-WV-E3: the Tier-2 approval-window verbs (open / check / close / pause-all /
    // resume-all). Each carries the two-seam gate — this entry plus
    // `InteractiveGate::admit` at its `mcp/handler.rs` dispatch branch — and is
    // registered in `crate::weft::WV_INTERACTIVE_VERBS` too.
    "wv_open_t2_window",
    "wv_check_t2_window",
    "wv_close_t2_window",
    "wv_pause_t2_windows",
    "wv_resume_t2_windows",
    // SW-WA-D4: the scope-level posture SOURCE verbs — read (the self-initiation
    // wrap's posture input) + set (fires the single-sited E3 pause/resume on an Away
    // transition). Two-seam gated — this entry plus `InteractiveGate::admit` at each
    // `mcp/handler.rs` dispatch branch — and registered in `crate::weft::WV_INTERACTIVE_VERBS` too.
    "wv_read_posture",
    "wv_set_posture",
    // SW-WV-F7: the FE dispatch bridge (`weft::dispatch_bridge::wv_dispatch_worker`)
    // gates on this at its command entry. It is a Tauri-command door, NOT a `wv_*`
    // MCP verb, so it lives ONLY here — deliberately absent from
    // `crate::weft::WV_INTERACTIVE_VERBS` (the `every_wv_interactive_verb_has_both_seams`
    // cross-check is one-way: every WV verb must appear here, not the reverse).
    "wv_dispatch_worker",
];
pub const MANAGE_AGENT_ACCOUNTS: &str = "manage_agent_accounts";

const MAX_GRANT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_GRANT_BUDGET: u32 = 20;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT: usize = 10;
const MAX_AUDIT_RECORDS: usize = 256;

#[derive(Debug, Clone)]
struct ScopedGrant {
    expires_at: Instant,
    remaining: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationAuditRecord {
    pub principal: u64,
    pub capability: String,
    pub outcome: String,
    pub at: u64,
}

#[derive(Default)]
struct AuthorizationState {
    grants: HashMap<(u64, String), ScopedGrant>,
    recent: HashMap<(u64, String), VecDeque<Instant>>,
    audit: VecDeque<AuthorizationAuditRecord>,
}

fn authorization_state() -> &'static Mutex<AuthorizationState> {
    static STATE: OnceLock<Mutex<AuthorizationState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(AuthorizationState::default()))
}

fn audit(state: &mut AuthorizationState, principal: u64, capability: &str, outcome: &str) {
    if state.audit.len() == MAX_AUDIT_RECORDS {
        state.audit.pop_front();
    }
    state.audit.push_back(AuthorizationAuditRecord {
        principal,
        capability: capability.to_string(),
        outcome: outcome.to_string(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    });
}

pub fn grant_scoped_capability(
    principal: u64,
    capability: &str,
    ttl: Duration,
    budget: u32,
) -> Result<(), GateRefusal> {
    if capability != MANAGE_AGENT_ACCOUNTS {
        return Err(GateRefusal::NotGranted(capability.to_string()));
    }
    let ttl = ttl.min(MAX_GRANT_TTL);
    let budget = budget.min(MAX_GRANT_BUDGET);
    if ttl.is_zero() || budget == 0 {
        return Err(GateRefusal::Denied(
            "grant TTL and budget must be positive".into(),
        ));
    }
    let now = Instant::now();
    let mut state = authorization_state().lock().unwrap();
    // Rate-limit grant issuance itself: without this, a visible renderer could
    // continuously re-mint a fresh grant (each refilling TTL + budget) and so
    // defeat the per-grant consume-budget cap enforced in `authorize_scoped_capability`.
    // Keyed distinctly from the authorize-side counter so the two never interfere.
    let rate_limited = {
        let recent = state
            .recent
            .entry((principal, format!("grant:{capability}")))
            .or_default();
        recent.retain(|at| now.duration_since(*at) < RATE_WINDOW);
        recent.push_back(now);
        recent.len() > RATE_LIMIT
    };
    if rate_limited {
        audit(&mut state, principal, capability, "grant-rate-limited");
        return Err(GateRefusal::Denied("rate_limited".into()));
    }
    state.grants.insert(
        (principal, capability.to_string()),
        ScopedGrant {
            expires_at: Instant::now() + ttl,
            remaining: budget,
        },
    );
    audit(&mut state, principal, capability, "granted");
    Ok(())
}

pub fn authorize_scoped_capability(principal: u64, capability: &str) -> Result<(), GateRefusal> {
    let now = Instant::now();
    let mut state = authorization_state().lock().unwrap();
    let rate_limited = {
        let recent = state
            .recent
            .entry((principal, capability.to_string()))
            .or_default();
        recent.retain(|at| now.duration_since(*at) < RATE_WINDOW);
        recent.push_back(now);
        recent.len() > RATE_LIMIT
    };
    if rate_limited {
        audit(&mut state, principal, capability, "rate-limited");
        return Err(GateRefusal::Denied("rate_limited".into()));
    }
    let key = (principal, capability.to_string());
    let Some(grant) = state.grants.get_mut(&key) else {
        audit(
            &mut state,
            principal,
            capability,
            "interactive-authorization-required",
        );
        return Err(GateRefusal::Denied(
            "interactive_authorization_required".into(),
        ));
    };
    if grant.expires_at <= now || grant.remaining == 0 {
        state.grants.remove(&key);
        audit(
            &mut state,
            principal,
            capability,
            "interactive-authorization-required",
        );
        return Err(GateRefusal::Denied(
            "interactive_authorization_required".into(),
        ));
    }
    grant.remaining -= 1;
    audit(&mut state, principal, capability, "authorized");
    Ok(())
}

#[cfg(test)]
pub fn authorization_audit() -> Vec<AuthorizationAuditRecord> {
    authorization_state()
        .lock()
        .unwrap()
        .audit
        .iter()
        .cloned()
        .collect()
}

/// Visible-webview grant front door. Callers must wire this to an explicit user
/// action; a hidden/background MCP request cannot mint its own authority.
#[tauri::command]
pub async fn grant_trusted_capability(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    capability: String,
    ttl_seconds: u64,
    budget: u32,
) -> Result<(), String> {
    if !window.is_visible().unwrap_or(false) {
        return Err("interactive_authorization_required".into());
    }
    let ctx = crate::commands::route_to_context::derive_active_context(&app).await?;
    grant_scoped_capability(
        ctx.user_id,
        &capability,
        Duration::from_secs(ttl_seconds),
        budget,
    )
    .map_err(|error| error.to_string())
}

/// A refusal from the [`InteractiveGate`], carrying the typed wire prefix a
/// consumer sees. `Display` renders the exact contract string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateRefusal {
    /// The tier does not admit this verb. Wire: `not-granted: <verb>`.
    NotGranted(String),
    /// A policy refusal with a human reason. Wire: `denied: <reason>`.
    Denied(String),
    /// The shared spawn cap is saturated. Wire: `busy`.
    Busy,
}

impl fmt::Display for GateRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateRefusal::NotGranted(verb) => write!(f, "not-granted: {verb}"),
            GateRefusal::Denied(reason) => write!(f, "denied: {reason}"),
            GateRefusal::Busy => write!(f, "busy"),
        }
    }
}

/// A saturated cap surfaces as `busy` — the ONE mapping, so the gate never
/// invents a second spelling of the cap's refusal.
impl From<SpawnCapError> for GateRefusal {
    fn from(err: SpawnCapError) -> Self {
        match err {
            SpawnCapError::Busy => GateRefusal::Busy,
        }
    }
}

/// The `Interactive` tier: deny-first admission for the UI-driving MCP verbs.
///
/// Stateless in the slice (the allowset is a constant); SW-OR-2 gives it the
/// per-verb allowlist + budget state. Both tool handlers call [`admit`] BEFORE
/// touching the registry or a PTY, so a refusal never has a side effect.
///
/// [`admit`]: InteractiveGate::admit
pub struct InteractiveGate;

impl InteractiveGate {
    /// Admit `verb` at the `Interactive` tier, or refuse
    /// [`GateRefusal::NotGranted`]. Deny-first: only the fixed
    /// [`INTERACTIVE_VERBS`] set passes.
    pub fn admit(verb: &str) -> Result<(), GateRefusal> {
        if INTERACTIVE_VERBS.contains(&verb) {
            Ok(())
        } else {
            Err(GateRefusal::NotGranted(verb.to_string()))
        }
    }

    /// Trust changes require a person at the native action boundary. MCP
    /// callers are agents even when their JSON asks to accept a new identity.
    pub fn admit_human(
        verb: &str,
        actor: crate::panes::commands::Actor,
    ) -> Result<(), GateRefusal> {
        Self::admit(verb)?;
        if actor != crate::panes::commands::Actor::Human {
            return Err(GateRefusal::Denied(format!(
                "{verb} is for humans only; report the change in Machines"
            )));
        }
        Ok(())
    }

    /// SW-WA-F5: admit an `open_workspace` at the requested [`WorkspaceMode`].
    ///
    /// SW-OR-1 shipped a blanket `denied:` for any `headed:true` MCP open — the
    /// rationale being "the trusted process cannot create/focus an FE tab." F5
    /// closes that gap: opening a story workspace over MCP should feel like the
    /// human opening a normal CodeSense CLI tab, so the operator-gesture default
    /// is **headed** (visible + focused). The requested visibility mode IS the
    /// focus-consent decision — a caller asking for a headed open is asking to be
    /// shown the tab — so all three modes admit here:
    ///  - [`WorkspaceMode::Headed`] — visible + focused (the default gesture).
    ///  - [`WorkspaceMode::VisibleUnfocused`] — a real tab that does not steal
    ///    focus (the auto-dispatch default, so many workers don't
    ///    fight for focus).
    ///  - [`WorkspaceMode::Headless`] — background (SW-OR-1's behavior); opt-in
    ///    only, never the default.
    ///
    /// Deny-first is preserved by the caller: `InteractiveGate::admit("open_workspace")`
    /// still fronts this, so an ungranted verb never reaches a mode decision, and
    /// an unknown/non-managed story id is still refused trusted-side downstream.
    pub fn admit_workspace_mode(mode: WorkspaceMode) -> Result<(), GateRefusal> {
        // Every mode is a legitimate, consented visibility choice on the MCP path
        // now — the mode is the consent. Kept as a `Result` so SW-OR-2 can layer a
        // per-caller focus-consent budget on the headed path without changing the
        // call site.
        let _ = mode;
        Ok(())
    }
}

/// SW-WA-F5: how an `open_workspace` surfaces the story session. The wire default
/// is [`Headed`](WorkspaceMode::Headed) — an operator-gesture open feels like the
/// human opening a normal CLI tab, not a background spawn. Headless is opt-in only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceMode {
    /// Visible + focused: a focused tab in the viewer. The operator-gesture default.
    #[default]
    Headed,
    /// A real, visible tab that does NOT steal focus — the
    /// auto-dispatch default so many dispatches don't fight for the viewport.
    VisibleUnfocused,
    /// Background (SW-OR-1's original behavior); opt-in only, never the default.
    Headless,
}

impl WorkspaceMode {
    /// Resolve the wire request into a mode. Precedence, chosen so the default is
    /// headed and headless is opt-in only:
    ///  - an explicit `mode` string wins (`"headed"` | `"visible-unfocused"` |
    ///    `"headless"`); an unrecognized string is refused `denied:` rather than
    ///    silently defaulting (a typo must not mask the intended visibility).
    ///  - else the legacy `headed` bool: `Some(false)` → headless (back-compat with
    ///    SW-OR-1 callers that spawned background), `Some(true)`/`None` → headed.
    pub fn resolve(mode: Option<&str>, headed: Option<bool>) -> Result<Self, GateRefusal> {
        if let Some(raw) = mode {
            return match raw.trim().to_ascii_lowercase().as_str() {
                "headed" => Ok(WorkspaceMode::Headed),
                "visible-unfocused" | "visible_unfocused" | "unfocused" => {
                    Ok(WorkspaceMode::VisibleUnfocused)
                }
                "headless" | "background" => Ok(WorkspaceMode::Headless),
                other => Err(GateRefusal::Denied(format!(
                    "unknown workspace mode '{other}' — use headed | visible-unfocused | headless"
                ))),
            };
        }
        match headed {
            Some(false) => Ok(WorkspaceMode::Headless),
            _ => Ok(WorkspaceMode::Headed),
        }
    }

    /// Whether this mode surfaces a visible tab (headed OR visible-unfocused).
    pub fn is_visible(self) -> bool {
        !matches!(self, WorkspaceMode::Headless)
    }

    /// Whether this mode should steal focus (headed only).
    pub fn is_focused(self) -> bool {
        matches!(self, WorkspaceMode::Headed)
    }

    /// The wire string echoed back in the tool result.
    pub fn as_wire(self) -> &'static str {
        match self {
            WorkspaceMode::Headed => "headed",
            WorkspaceMode::VisibleUnfocused => "visible-unfocused",
            WorkspaceMode::Headless => "headless",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_machine_restart_continues_through_interactive_admission() {
        for verb in [
            "prepare_machine_terminal_server_replacement",
            "restart_machine_terminal_server",
            // SW-AGR-19 PR-8: refused before this, so the tool never ran.
            "update_terminal_servers",
        ] {
            assert_eq!(InteractiveGate::admit(verb), Ok(()));
        }
    }

    #[test]
    fn admits_the_two_ui_driving_verbs() {
        assert!(InteractiveGate::admit("open_workspace").is_ok());
        assert!(InteractiveGate::admit("send_to_session").is_ok());
    }

    #[test]
    fn open_terminal_tab_is_an_interactive_verb() {
        // SW-WA-F1 (AC-5): the compile-time attestation that the runtime
        // `InteractiveGate::admit("open_terminal_tab")` in `mcp/handler.rs` gates a
        // registered verb — the gate only admits a verb that is in this set.
        assert!(INTERACTIVE_VERBS.contains(&"open_terminal_tab"));
        assert!(InteractiveGate::admit("open_terminal_tab").is_ok());
    }

    #[test]
    fn run_in_terminal_tab_is_an_interactive_verb() {
        // SW-WA-F2 (AC-3): the runtime `InteractiveGate::admit("run_in_terminal_tab")`
        // in `mcp/handler.rs` gates a registered verb — the gate only admits a verb in
        // this set. A raw shell-run is MORE sensitive than an agent prompt, so the
        // shell-run consent is enforced at the handler, not by prompt convention.
        assert!(INTERACTIVE_VERBS.contains(&"run_in_terminal_tab"));
        assert!(InteractiveGate::admit("run_in_terminal_tab").is_ok());
    }

    #[test]
    fn read_terminal_tab_is_an_interactive_verb() {
        // SW-WA-F3 (AC-3): the compile-time attestation that the runtime
        // `InteractiveGate::admit("read_terminal_tab")` in `mcp/handler.rs` gates a
        // registered verb — the gate only admits a verb that is in this set, so the
        // scrollback read is behind the deny-first tier.
        assert!(INTERACTIVE_VERBS.contains(&"read_terminal_tab"));
        assert!(InteractiveGate::admit("read_terminal_tab").is_ok());
    }

    #[test]
    fn close_terminal_tab_is_an_interactive_verb() {
        // Compile-time attestation that the runtime
        // `InteractiveGate::admit("close_terminal_tab")` in `mcp/handler.rs` gates
        // a registered verb — the gate only admits a verb that is in this set.
        assert!(INTERACTIVE_VERBS.contains(&"close_terminal_tab"));
        assert!(InteractiveGate::admit("close_terminal_tab").is_ok());
    }

    #[test]
    fn close_workspace_is_an_interactive_verb() {
        // Compile-time attestation that the runtime
        // `InteractiveGate::admit("close_workspace")` in `mcp/handler.rs` gates a
        // registered verb — the gate only admits a verb that is in this set.
        assert!(INTERACTIVE_VERBS.contains(&"close_workspace"));
        assert!(InteractiveGate::admit("close_workspace").is_ok());
    }

    fn literal_admit_calls(source: &str) -> Vec<(u32, String)> {
        let production = crate::test_support::source_guard::production_text(source);
        let production = production.as_str();
        let pattern = regex::Regex::new(r#"InteractiveGate::admit\(\s*"([^"]+)""#).unwrap();
        pattern
            .captures_iter(production)
            .map(|cap| {
                let line = production[..cap.get(0).unwrap().start()]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count() as u32
                    + 1;
                (line, cap[1].to_string())
            })
            .collect()
    }

    #[test]
    fn admit_scan_sees_an_unregistered_verb_after_a_test_only_import() {
        let source = r#"#[cfg(test)]
use super::test_helper;
fn production() {
    InteractiveGate::admit("open_workspace");
    InteractiveGate::admit(
        "unregistered_verb",
    );
}
#[cfg(test)]
mod tests {
    fn fixture() { InteractiveGate::admit("test_only_verb"); }
}
"#;
        let calls = literal_admit_calls(source);
        assert_eq!(
            calls,
            [
                (4, "open_workspace".into()),
                (5, "unregistered_verb".into())
            ]
        );
        let offenders: Vec<_> = calls
            .iter()
            .filter(|(_, verb)| !INTERACTIVE_VERBS.contains(&verb.as_str()))
            .collect();
        assert_eq!(offenders, [&(5, "unregistered_verb".into())]);
    }

    #[test]
    fn every_literal_admit_call_names_a_registered_verb() {
        // Source-scan guard: every non-test `InteractiveGate::admit("<literal>")`
        // under `src-tauri/src` must name a verb in `INTERACTIVE_VERBS`. A gated
        // verb cannot ship without its registration — the whole class of bug the
        // close verbs hit (gated at the handler but absent from the registry, so
        // every caller got `not-granted:`) is closed by this scan.
        //
        // `WV_INTERACTIVE_VERBS` is a strict subset (the cross-check in
        // `weft::sqlite` ensures every WV verb also appears here), so checking
        // against `INTERACTIVE_VERBS` covers both registries at once.
        use std::fs;
        use std::path::{Path, PathBuf};

        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    // Test modules often live in sibling files, which the scan
                    // skips. The guard targets production code only.
                    if name == "tests.rs" || name.ends_with("_tests.rs") {
                        continue;
                    }
                    out.push(path);
                }
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();

        let mut matches: Vec<(String, u32, String)> = Vec::new();
        let mut offenders: Vec<(String, u32, String)> = Vec::new();
        for path in &files {
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path.as_path())
                .display()
                .to_string();
            for (line, verb) in literal_admit_calls(&text) {
                matches.push((rel.clone(), line, verb.clone()));
                if !INTERACTIVE_VERBS.contains(&verb.as_str()) {
                    offenders.push((rel.clone(), line, verb));
                }
            }
        }

        // The minimum-count assertion catches gross breakage — a broken regex
        // or a wrong root path would otherwise pass silently with zero matches.
        assert!(
            matches.len() >= 40,
            "literal `InteractiveGate::admit(\"…\")` scan matched only {} call(s); expected at least 40. A broken regex or path?",
            matches.len()
        );

        // The multi-line admits in `handler.rs` (the formatter splits long
        // verb names across lines) must be found; this proves the whitespace
        // allowance in the regex works.
        let verbs: std::collections::HashSet<&str> =
            matches.iter().map(|(_, _, v)| v.as_str()).collect();
        assert!(
            verbs.contains("prepare_machine_terminal_server_replacement"),
            "scan missed the rustfmt-split `prepare_machine_terminal_server_replacement` call"
        );
        assert!(
            verbs.contains("restart_machine_terminal_server"),
            "scan missed the rustfmt-split `restart_machine_terminal_server` call"
        );

        // The specific verbs this guard is here to catch are seen by the scan
        // on this branch. If someone removes either `INTERACTIVE_VERBS` entry,
        // the file:line of the offender appears in the failure below.
        assert!(
            verbs.contains("close_terminal_tab"),
            "scan missed the `close_terminal_tab` admit call"
        );
        assert!(
            verbs.contains("close_workspace"),
            "scan missed the `close_workspace` admit call"
        );

        if !offenders.is_empty() {
            let formatted: Vec<String> = offenders
                .iter()
                .map(|(file, line, verb)| format!("  {file}:{line} → {verb:?}"))
                .collect();
            panic!(
                "{} `InteractiveGate::admit(\"<literal>\")` call(s) name a verb that is NOT in `INTERACTIVE_VERBS`. Every gated verb must be registered or the gate refuses it with `not-granted:` for every caller:\n{}",
                offenders.len(),
                formatted.join("\n")
            );
        }
    }

    #[test]
    fn wv_spawn_worker_is_an_interactive_verb() {
        // SW-WV-A2 (AC-7): the compile-time attestation half of the two-seam gate.
        // The runtime `InteractiveGate::admit("wv_spawn_worker")` in `mcp/handler.rs`
        // only gates a verb that is in this set; this proves the registration edit is
        // present so the lease check is a belt, not the sole control.
        assert!(INTERACTIVE_VERBS.contains(&"wv_spawn_worker"));
        assert!(InteractiveGate::admit("wv_spawn_worker").is_ok());
    }

    #[test]
    fn peer_lane_verbs_not_in_interactive_verbs() {
        // SW-WV-D4 AC-5 (Test 3): the three peer-lane verbs are NOT interactive
        // UI-driving verbs; nothing at spawn scopes them, and the two reads are bounded
        // only by the dispatch `peers:` check inside each verb. Adding any of
        // them here would (a) force an `InteractiveGate::admit` at the dispatch branch
        // and (b) violate the ARCHITECTURE §3.2 Tier-A rule. This constraint is
        // structural and machine-checked, not only prose.
        assert!(!INTERACTIVE_VERBS.contains(&"wv_coord_read"));
        assert!(!INTERACTIVE_VERBS.contains(&"wv_peer_read_file"));
        assert!(!INTERACTIVE_VERBS.contains(&"wv_escalate"));
    }

    #[test]
    fn refuses_an_unadmitted_verb_not_granted() {
        // Deny-first: a verb outside the tier's allowset is refused with the
        // typed `not-granted:` prefix naming the verb.
        let refusal = InteractiveGate::admit("run_skill").expect_err("not admitted");
        assert_eq!(refusal, GateRefusal::NotGranted("run_skill".to_string()));
        assert_eq!(refusal.to_string(), "not-granted: run_skill");
    }

    #[test]
    fn every_workspace_mode_is_admitted() {
        // SW-WA-F5 (AC-1/AC-5): the SW-OR-1 blanket `headed:true` refusal is
        // relaxed — the requested visibility mode is itself the focus-consent
        // decision, so all three modes admit. (Headless stays opt-in via
        // `WorkspaceMode::resolve`, not via a refusal here.)
        assert!(InteractiveGate::admit_workspace_mode(WorkspaceMode::Headed).is_ok());
        assert!(InteractiveGate::admit_workspace_mode(WorkspaceMode::VisibleUnfocused).is_ok());
        assert!(InteractiveGate::admit_workspace_mode(WorkspaceMode::Headless).is_ok());
    }

    #[test]
    fn workspace_mode_defaults_to_headed_headless_is_opt_in() {
        // SW-WA-F5 Decision: default is HEADED (visible + focused); headless is
        // opt-in only, never the default.
        assert_eq!(WorkspaceMode::default(), WorkspaceMode::Headed);
        // No mode + no headed bool → headed (the operator-gesture default).
        assert_eq!(
            WorkspaceMode::resolve(None, None).unwrap(),
            WorkspaceMode::Headed
        );
        // Legacy SW-OR-1 back-compat: an explicit `headed:false` still means
        // background (headless), an explicit `headed:true` means headed.
        assert_eq!(
            WorkspaceMode::resolve(None, Some(false)).unwrap(),
            WorkspaceMode::Headless
        );
        assert_eq!(
            WorkspaceMode::resolve(None, Some(true)).unwrap(),
            WorkspaceMode::Headed
        );
    }

    #[test]
    fn workspace_mode_string_wins_over_headed_bool() {
        // An explicit `mode` string takes precedence over the legacy `headed`
        // bool, so a caller can select visible-unfocused / headless by name.
        assert_eq!(
            WorkspaceMode::resolve(Some("headed"), Some(false)).unwrap(),
            WorkspaceMode::Headed
        );
        assert_eq!(
            WorkspaceMode::resolve(Some("visible-unfocused"), None).unwrap(),
            WorkspaceMode::VisibleUnfocused
        );
        assert_eq!(
            WorkspaceMode::resolve(Some("headless"), Some(true)).unwrap(),
            WorkspaceMode::Headless
        );
    }

    #[test]
    fn workspace_mode_unknown_string_is_denied() {
        // A typo must not silently fall back to a default visibility.
        let refusal =
            WorkspaceMode::resolve(Some("hedaed"), None).expect_err("unknown mode denied");
        assert!(matches!(refusal, GateRefusal::Denied(_)));
        assert!(refusal.to_string().starts_with("denied: "));
    }

    #[test]
    fn workspace_mode_visibility_and_focus_flags() {
        assert!(WorkspaceMode::Headed.is_visible() && WorkspaceMode::Headed.is_focused());
        assert!(WorkspaceMode::VisibleUnfocused.is_visible());
        assert!(!WorkspaceMode::VisibleUnfocused.is_focused());
        assert!(!WorkspaceMode::Headless.is_visible());
        assert!(!WorkspaceMode::Headless.is_focused());
        assert_eq!(WorkspaceMode::Headed.as_wire(), "headed");
        assert_eq!(
            WorkspaceMode::VisibleUnfocused.as_wire(),
            "visible-unfocused"
        );
        assert_eq!(WorkspaceMode::Headless.as_wire(), "headless");
    }

    #[test]
    fn spawn_cap_busy_maps_to_busy() {
        // The gate and the cap agree on the ONE spelling of saturation.
        let refusal: GateRefusal = SpawnCapError::Busy.into();
        assert_eq!(refusal, GateRefusal::Busy);
        assert_eq!(refusal.to_string(), "busy");
    }

    #[test]
    fn scoped_grant_is_budgeted_and_audited_without_sensitive_payloads() {
        *authorization_state().lock().unwrap() = AuthorizationState::default();
        grant_scoped_capability(42, MANAGE_AGENT_ACCOUNTS, Duration::from_secs(60), 1).unwrap();
        assert_eq!(
            authorize_scoped_capability(7, MANAGE_AGENT_ACCOUNTS)
                .unwrap_err()
                .to_string(),
            "denied: interactive_authorization_required"
        );
        assert!(authorize_scoped_capability(42, MANAGE_AGENT_ACCOUNTS).is_ok());
        assert_eq!(
            authorize_scoped_capability(42, MANAGE_AGENT_ACCOUNTS)
                .unwrap_err()
                .to_string(),
            "denied: interactive_authorization_required"
        );
        let audit = authorization_audit();
        assert!(audit.iter().any(|record| record.outcome == "authorized"));
        let serialized = serde_json::to_string(&audit).unwrap();
        assert!(!serialized.contains("account_id"));
        assert!(!serialized.contains("path"));
    }
}

#[cfg(test)]
mod human_action_tests {
    use super::*;
    use crate::panes::commands::Actor;
    #[test]
    fn agents_can_report_identity_changes_but_cannot_accept_them() {
        assert!(InteractiveGate::admit("resolve_machine_identity").is_ok());
        assert!(InteractiveGate::admit_human("resolve_machine_identity", Actor::Agent).is_err());
        assert!(InteractiveGate::admit_human("resolve_machine_identity", Actor::Human).is_ok());
    }
}
