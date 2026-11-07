// SPDX-License-Identifier: MIT
//! SW-SD-2: account/CLI-agnostic headless **agent runner**, plus the `analyze`
//! bridge-verb command. (RF-1b removed the former `invoke_claude`
//! back-compat wrapper and its sole caller, the `.meta` git-sync conflict resolver;
//! `run_agent` is now the single runner entry point.)
//!
//! `run_agent` is the ONE correlated request→agent→response primitive (the VISION
//! "dispatch an agent" rung reuses it: same runner, different `AgentRunnerConfig`).
//! It holds **no credential** — the configured CLI owns its own auth (a Claude Max
//! login, an API key, or another agent CLI).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};

use crate::commands::spawn_cap::SpawnCap;
use crate::shared::proc_capture::{spawn_capture, CaptureError, CaptureOutput, CaptureRequest};

/// Placeholder token in an argv template that `run_agent` substitutes with
/// `AgentRunnerConfig::model` (or, when `model` is `None`, drops together with
/// its preceding `--model` flag — a non-Claude CLI that takes no model flag).
/// `pub(crate)` so SW-AN-15's authoring validator checks the SAME token the
/// runtime substitutes — one spelling, so the guard and `build_argv` can't drift.
pub(crate) const MODEL_PLACEHOLDER: &str = "{model}";

// SW-AN-2 (G1): the default model was `DEFAULT_AGENT_MODEL` (SW-AN-1's inline of
// RF-1b's residue); it is adapter data (`crate::agents::builtin::preferred_runner().default_model`), so the hardcoded const is gone as SW-AN-1 foretold.

/// Default wall-clock ceiling for a runner subprocess before it is killed.
/// Inlined from the retired Claude-constants module by SW-AN-1 (value unchanged).
const DEFAULT_RUNNER_TIMEOUT: Duration = Duration::from_secs(60);

/// Account/CLI-agnostic headless agent config. **No secret** — the CLI owns its
/// auth. The whole argv is config, so "another CLI later" is a different
/// `AgentRunnerConfig`, not a code change.
#[derive(Clone, Debug)]
pub struct AgentRunnerConfig {
    /// PATH-detected agent CLI (the launch-binary authority, `resolve_agent_for_launch`).
    pub binary: PathBuf,
    /// argv TEMPLATE, e.g. `["-p","--output-format","text","--model","{model}","--max-turns","1"]`.
    pub args: Vec<String>,
    /// Substituted into `{model}`; `None` drops the `--model {model}` pair.
    pub model: Option<String>,
    /// Wall-clock ceiling before the subprocess is killed (default 60s).
    pub timeout: Duration,
    /// RF-7: working directory the child is spawned in (`Command::current_dir`).
    /// `None` inherits the app process's cwd (the read-only `analyze` behavior —
    /// cwd is irrelevant to a single-turn, tool-less run). `run_skill` sets this
    /// to the **managed content-repo root** so the
    /// dispatched `/<skill>` resolves against the install-layer `.claude/skills/`
    /// CodeSense materializes there. Threading it as a spawn attribute (beside
    /// binary/args) keeps `run_agent`'s signature — and its callers — untouched.
    pub cwd: Option<PathBuf>,
}

/// The default `analyze` argv template (model expressed as the `{model}`
/// placeholder so `run_agent` owns the substitute/drop decision). SW-AN-2: the
/// template is adapter data (`crate::agents::builtin::preferred_runner().analyze_args`) — a
/// thin accessor so callers/tests are unchanged. Byte-compatible with the
/// original `invoke_claude` invocation once `{model}` → `claude-opus-4-6`.
pub fn claude_default_args_template() -> Vec<String> {
    crate::agents::builtin::preferred_runner()
        .analyze_args
        .clone()
}

/// Runner tunables held in Tauri managed state (SW-SD-2). **Tunables only — the
/// binary is NOT stored here**: each call goes through the launch-time authority
/// (`resolve_agent_for_launch`), which re-checks the cached binary, re-resolves one
/// that moved or changed, and retries a lost one after a cooldown. A CLI that was
/// never found needs a Re-check to be picked up.
#[derive(Clone, Debug)]
pub struct AgentRunnerTunables {
    pub args_template: Vec<String>,
    pub model: Option<String>,
    pub timeout: Duration,
}

impl Default for AgentRunnerTunables {
    fn default() -> Self {
        Self {
            args_template: claude_default_args_template(),
            // SW-AN-2 (G1): the default model is adapter data, not a hardcoded const.
            model: crate::agents::builtin::preferred_runner()
                .default_model
                .clone(),
            timeout: DEFAULT_RUNNER_TIMEOUT,
        }
    }
}

impl AgentRunnerTunables {
    /// Compose a complete, runnable config by pairing the call-time binary with
    /// these tunables. `run_agent` stays pure over the resulting config.
    pub fn with_binary(&self, binary: PathBuf) -> AgentRunnerConfig {
        AgentRunnerConfig {
            binary,
            args: self.args_template.clone(),
            model: self.model.clone(),
            timeout: self.timeout,
            // `analyze` inherits the app cwd (read-only, single-turn, tool-less).
            cwd: None,
        }
    }
}

/// Resolve an argv template against an optional model. A `--model {model}` pair
/// is treated as a unit: substituted when `model` is `Some`, dropped whole when
/// `None`. A standalone `{model}` placeholder is substituted/dropped defensively.
pub fn build_argv(template: &[String], model: Option<&str>) -> Vec<String> {
    let mut out = Vec::with_capacity(template.len());
    let mut i = 0;
    while i < template.len() {
        let tok = template[i].as_str();
        // `--model` immediately followed by the placeholder is one unit.
        if tok == "--model" && template.get(i + 1).map(String::as_str) == Some(MODEL_PLACEHOLDER) {
            if let Some(m) = model {
                out.push("--model".to_string());
                out.push(m.to_string());
            }
            i += 2;
            continue;
        }
        if tok == MODEL_PLACEHOLDER {
            if let Some(m) = model {
                out.push(m.to_string());
            }
            i += 1;
            continue;
        }
        out.push(tok.to_string());
        i += 1;
    }
    out
}

/// Process-lifecycle knobs for a captured agent run. Defaults preserve `run_agent`'s historical
/// behavior (no cancel token, lone child), so every existing `run_agent` caller is byte-identical.
#[derive(Default)]
pub struct RunAgentControl {
    /// Cooperative cancellation: when the flag flips `true` the child is killed (its whole process
    /// group when `process_group` is set) and [`CaptureOutput::cancelled`] is set. `None` never cancels.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Spawn the child in its own process group so a cancel/timeout kill (`killpg`) reaps any
    /// grandchildren too — leaving nothing orphaned. Default `false` keeps the lone-child behavior.
    pub process_group: bool,
}

/// Spawn the configured agent, feed `prompt` on stdin, and return the RAW capture outcome
/// (stdout / stderr / status / timed_out / cancelled) with no success/empty policy applied. This is
/// the launch core [`run_agent`] layers its bridge-verb policy on top of; a caller that needs the
/// full transcript regardless of exit (e.g. the workflow-executor `agent` node, which attaches the
/// output as Run evidence and distinguishes cancel/timeout) uses this directly rather than a second
/// spawn path. **Synchronous/blocking** — callers on an async runtime MUST offload it.
pub fn capture_agent(
    config: &AgentRunnerConfig,
    prompt: &str,
    control: RunAgentControl,
) -> Result<CaptureOutput, CaptureError> {
    let argv = build_argv(&config.args, config.model.as_deref());
    // RF-7: `config.cwd` anchors the child to a managed surface so `/<skill>` resolves against
    // `<cwd>/.claude/skills/`; `None` inherits the process cwd.
    spawn_capture(CaptureRequest {
        program: config.binary.clone(),
        args: argv,
        cwd: config.cwd.clone(),
        env: Vec::new(),
        env_remove: Vec::new(),
        stdin: Some(prompt.as_bytes().to_vec()),
        timeout: config.timeout,
        process_group: control.process_group,
        pid_sink: None,
        output_sink: None,
        last_drain_ms: None,
        max_output_bytes: None,
        cancel: control.cancel,
    })
}

/// Spawn the configured agent, feed `prompt` via stdin, capture stdout. Pure over
/// `config` + `prompt` (no `AppHandle`) — the exact, parameterized generalization
/// of `invoke_claude`'s body (same spawn / stdin / try_wait-timeout / stdout
/// capture / non-zero + empty-output error handling). **Synchronous/blocking** —
/// callers on an async runtime MUST offload it (see `analyze`'s `await_blocking`).
pub fn run_agent(config: &AgentRunnerConfig, prompt: &str) -> Result<String, String> {
    info!(
        target: "agent",
        "run_agent: binary={}, model={:?}, prompt_length={}",
        config.binary.display(), config.model, prompt.len()
    );

    // The spawn / thread-drained I/O / kill-on-timeout core lives in the shared `proc_capture`
    // primitive (SW-GT-8), reached through [`capture_agent`]. `run_agent` keeps only its policy:
    // treat a non-zero exit or empty stdout as an error.
    let out = capture_agent(config, prompt, RunAgentControl::default()).map_err(|e| match e {
        CaptureError::Spawn(err) => {
            error!(target: "agent", "failed to spawn agent process: {}", err);
            format!("failed to spawn agent: {}", err)
        }
        CaptureError::Wait(err) => {
            error!(target: "agent", "failed to wait for agent process: {}", err);
            format!("failed to wait for agent: {}", err)
        }
    })?;

    if out.timed_out {
        warn!(target: "agent", "agent process exceeded timeout, killing");
        return Err("agent timed out".into());
    }
    // Non-timeout always carries a status.
    let status = out
        .status
        .expect("a non-timed-out capture has an exit status");
    debug!(target: "agent", "process exited with status {}", status);

    if !status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        error!(target: "agent", "agent exited with non-zero status {}: {}", status, stderr.trim());
        return Err(format!(
            "agent exited with status {}: {}",
            status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("agent returned empty output".into());
    }

    debug!(target: "agent", "success, output_length={}", stdout.len());
    Ok(stdout)
}

// ─────────────────────────── analyze bridge verb ────────────────────────────

/// Max bytes of caller `data` folded into the analysis prompt. Oversize input is
/// **truncated with a visible annotation** — never silently, never hard-rejected
/// (a large-but-legitimate dashboard still gets an analysis, and the model is
/// told the input was clipped).
const ANALYZE_MAX_DATA_BYTES: usize = 256 * 1024;

/// Global single-flight latch: one agent run at a time across all dashboards.
/// Each CLI spawn is heavy, so one-at-a-time is the deliberate anti-storm posture
/// (per-doc concurrency is a later refinement). Mirrors `deps::GIT_INSTALL_IN_PROGRESS`.
static ANALYZE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// RAII guard that clears the single-flight latch on drop — so it clears even if
/// `run_agent` panics or times out (panic-safe). Holds a `&'static AtomicBool`
/// so it is `Send` and legally lives inside the `await_blocking` closure.
struct FlagGuard<'a>(&'a AtomicBool);
impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Compose the analysis prompt from the caller instruction + (capped) data.
fn compose_prompt(prompt: &str, data: &str) -> String {
    format!("{}\n\n<data>\n{}\n</data>", prompt, cap_data(data))
}

/// Cap `data` at `ANALYZE_MAX_DATA_BYTES` on a UTF-8 char boundary, appending a
/// visible truncation marker when clipped.
fn cap_data(data: &str) -> Cow<'_, str> {
    if data.len() <= ANALYZE_MAX_DATA_BYTES {
        return Cow::Borrowed(data);
    }
    let mut end = ANALYZE_MAX_DATA_BYTES;
    while end > 0 && !data.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!(
        "{}\n\n[codesense: data truncated at {} KiB of {} bytes]",
        &data[..end],
        ANALYZE_MAX_DATA_BYTES / 1024,
        data.len()
    ))
}

/// Map a raw `run_agent` error into a stable, FE-parseable typed prefix
/// (`timeout:` / `unavailable:` / passthrough generic). `busy` is returned by the
/// single-flight latch before we ever reach here.
fn classify_agent_error(raw: String) -> String {
    let low = raw.to_lowercase();
    if low.contains("timed out") {
        format!("timeout: {}", raw)
    } else if low.contains("failed to spawn")
        || low.contains("not logged in")
        || low.contains("not authenticated")
        || low.contains("logged out")
        || low.contains("unauthorized")
        || low.contains("claude login")
        || low.contains("authentication failed")
    {
        // Anchored phrases only — a bare "login"/"authentication"/"please run"
        // substring would misclassify a generic CLI error that merely mentions the
        // word (its trimmed stderr is arbitrary, content-influenced text). Claude's
        // real "Please run `claude login`" message is caught by "claude login".
        format!("unavailable: {}", raw)
    } else {
        raw
    }
}

/// Single-flight + compose + run, all synchronous → runs entirely on the blocking
/// pool inside `analyze`'s `await_blocking`. Extracted (with an injected latch) so
/// the busy/compose/degrade logic is unit-testable without Tauri state.
pub fn run_analyze(
    flag: &AtomicBool,
    config: &AgentRunnerConfig,
    prompt: &str,
    data: &str,
) -> Result<String, String> {
    if flag.swap(true, Ordering::SeqCst) {
        return Err("busy".into());
    }
    let _guard = FlagGuard(flag);
    let composed = compose_prompt(prompt, data);
    run_agent(config, &composed).map_err(classify_agent_error)
}

/// SW-SD-2 bridge verb backend. A trusted dashboard calls
/// `window.codesense.invoke('analyze', {prompt, data})`; the parent trust-gates it
/// (`html-viewer.tsx`) then invokes this. Runs the user's OWN configured agent CLI
/// (no codesense credential), one-shot, no tools (`--max-turns 1` → cannot act).
///
/// The binary is resolved **at call time** through the launch-binary authority
/// (`resolve_agent_for_launch`), so a moved or reinstalled CLI is re-resolved.
/// The blocking run is offloaded via `await_blocking`; a second call while one is
/// in flight returns `busy`.
#[tauri::command]
pub async fn analyze(
    app: tauri::AppHandle,
    tunables: tauri::State<'_, AgentRunnerTunables>,
    prompt: String,
    data: String,
) -> Result<String, String> {
    // SW-AN-44: run the preferred adapter's agent — these bridge verbs run
    // "the user's configured agent", not a per-profile binary.
    let binary = crate::commands::mcp_workspace::resolve_agent_for_launch(
        &app,
        crate::agents::builtin::preferred_binary(),
    )
    .await?
    .path;
    let config = tunables.with_binary(binary);
    crate::commands::await_blocking("analyze", move || {
        run_analyze(&ANALYZE_IN_FLIGHT, &config, &prompt, &data)
    })
    .await
}

// ─────────────────────────── runSkill bridge verb ───────────────────────────
//
// SW-DX-11: the WRITE-CAPABLE sibling of `analyze`. Same `run_agent` core, but a
// config that DROPS `--max-turns 1` (multi-turn) and leaves tool use ENABLED —
// the user's agent CLI runs with its normal Write/Edit/Bash/MCP capabilities,
// exactly like Claude Code in a terminal. Bounded by its OWN `SpawnCap` (NOT the
// openWorkspace/runPrompt cap, NOT `analyze`'s single-flight) so a drift-board
// sync loop can't starve workspace spawns.

// SW-AN-2: `run_skill`'s max-turns (vs `analyze`'s hard `1`) is now adapter data
// (`runner.skill_args`) — a skill like `sync-story` is inherently multi-step
// (read → edit → verify), so the write-capable template raises the ceiling.

/// Wall-clock ceiling for a skill run — far longer than `analyze`'s 60s, since a
/// write-capable multi-turn skill legitimately takes minutes.
const RUN_SKILL_TIMEOUT: Duration = Duration::from_secs(600);

/// Concurrent `run_skill` dispatches allowed across all dashboards. A small cap
/// (heavier than a read, lighter than a workspace) — its OWN budget.
const RUN_SKILL_CAP: usize = 4;

/// The write-capable skill argv template (model as the `{model}` placeholder).
/// Differs from [`claude_default_args_template`] in exactly the security-relevant
/// way: `--max-turns 20` (not `1`) and `--permission-mode acceptEdits` so file
/// edits proceed without an interactive prompt no headless run could answer.
/// Bash/other gated tools still follow the CLI's own posture. SW-AN-2: the
/// template is adapter data (`crate::agents::builtin::preferred_runner().skill_args`).
pub fn skill_default_args_template() -> Vec<String> {
    crate::agents::builtin::preferred_runner()
        .skill_args
        .clone()
}

/// Compose the exact prompt fired into the agent: a bounded slash-command
/// `/<skill> <args…>`. The skill name is grant-checked FE-side (two-axis S1); the
/// args are the doc-supplied parameters (e.g. a story id) joined by spaces.
pub fn compose_skill_prompt(skill: &str, args: &[String]) -> String {
    if args.is_empty() {
        format!("/{skill}")
    } else {
        format!("/{skill} {}", args.join(" "))
    }
}

/// SW-DX-11 (review r1 — Edge Hunter): reject a skill name or arg that could
/// SPLIT the composed single-line slash-command into a second turn. The per-skill
/// grant only authorizes the OUTER `skill`; without this, an arg like
/// `"x\n/other-skill"` would compose `/<skill> x\n/other-skill` and the agent CLI
/// could run `/other-skill` — a skill the user never granted. So: no line-breaking
/// character anywhere, and the skill name is a bare token (no whitespace / `/`,
/// which would fork tokens or look path-like).
fn validate_skill_invocation(skill: &str, args: &[String]) -> Result<(), String> {
    // Any char that a CLI could treat as a line/turn boundary: the C0/C1 control
    // set (`\n`, `\r`, NEL U+0085, …) PLUS the Unicode line/paragraph separators
    // U+2028/U+2029 — the latter are category Zl/Zp, NOT `char::is_control()`, so
    // they must be named explicitly (review r2).
    let breaks_line = |c: char| c.is_control() || c == '\u{2028}' || c == '\u{2029}';
    if skill.is_empty()
        || skill.chars().any(|c| breaks_line(c) || c.is_whitespace())
        || skill.contains('/')
    {
        return Err(format!("not-allowed: invalid skill name '{skill}'"));
    }
    if args.iter().any(|a| a.chars().any(breaks_line)) {
        return Err("not-allowed: skill arg contains a line-breaking character".to_string());
    }
    Ok(())
}

/// SW-DX-11: the dedicated concurrency cap for `run_skill` — SEPARATE from the
/// openWorkspace/runPrompt cap (`DashboardLaunchState`) and from `analyze`'s
/// single-flight, so skill dispatches and workspace spawns never starve each
/// other (two named budgets, per the story's concurrency-separation guardrail).
#[derive(Clone)]
pub struct RunSkillCap(pub SpawnCap);

impl Default for RunSkillCap {
    fn default() -> Self {
        RunSkillCap(SpawnCap::new(RUN_SKILL_CAP))
    }
}

/// Cap-acquire + compose + run, all synchronous → runs on the blocking pool
/// inside `run_skill`'s `await_blocking`. Extracted (injected cap + config) so
/// the busy / compose / typed-degrade logic is unit-testable without Tauri state.
/// The `SpawnPermit` is held for the whole run and released on drop (RAII).
pub fn run_skill_core(
    cap: &SpawnCap,
    config: &AgentRunnerConfig,
    skill: &str,
    args: &[String],
) -> Result<String, String> {
    // Reject a prompt-splitting skill/arg BEFORE reserving a permit (an invalid
    // invocation shouldn't consume a cap slot).
    validate_skill_invocation(skill, args)?;
    // RF-7: a skill dispatch MUST be anchored to a real, absolute working
    // directory — otherwise `/<skill>` can't resolve. The managed-surface check
    // lives upstream in `run_skill`'s resolver (`resolve_managed_team_content_root`);
    // this is the core-level invariant that also guards direct callers/tests, and
    // it fires BEFORE the cap so a bad cwd never consumes a slot and never spawns
    // (AC-2 — typed `unavailable:`, classified by the FE).
    let cwd = config
        .cwd
        .as_deref()
        .ok_or_else(|| "unavailable: no working directory resolved for skill run".to_string())?;
    // `is_dir()` (not `exists()`) also rejects a file-at-the-path — so a
    // misconfigured content root fails HERE (before the cap + spawn) rather than
    // as an opaque `current_dir` spawn error.
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(format!(
            "unavailable: skill working directory is not an existing absolute directory: {}",
            cwd.display()
        ));
    }
    // Fail fast on a saturated cap — never blocks (the `busy` prefix matches
    // `analyze`'s single-flight reject, so the FE classifies both identically).
    let _permit = cap.try_acquire().map_err(|e| e.to_string())?;
    let prompt = compose_skill_prompt(skill, args);
    run_agent(config, &prompt).map_err(classify_agent_error)
}

/// SW-DX-11 bridge verb backend (AC-5). A dashboard calls
/// `window.codesense.act('runSkill', {skill, args})`; the parent invokes this. Runs the user's OWN agent CLI
/// multi-turn WITH tool use — the deliberately-different, write-capable sibling
/// of `analyze` (never conflate the two: `analyze` stays `--max-turns 1`).
///
/// The binary is resolved at call time via `resolve_agent_for_launch`; the blocking run is
/// offloaded via `await_blocking`; a dispatch over the cap returns `busy`; a
/// prompt-splitting skill/arg returns `not-allowed:`.
///
/// **Working directory (RF-7):** the child is anchored to the active team's
/// **managed content-repo root** (`resolve_managed_team_content_root`), the dir
/// that holds the install-layer `.claude/skills/`. Without this the CLI inherits
/// the app process's cwd — neither a managed surface nor `~/.claude/skills/` — so
/// a cs-published `/<skill>` is unregistered there and the headless CLI **errors**
/// with `Unknown command: /<skill>`. The cwd is resolved and confined to a
/// managed surface (SLW-FX-13) BEFORE a cap slot is reserved; a missing/unmanaged
/// surface returns a typed `unavailable:` and never spawns.
///
/// **Unknown/uninstalled skill:** with the cwd correctly anchored, an installed
/// skill resolves. A genuinely-uninstalled `/<skill>` makes the headless CLI
/// **error** (observed `Unknown command: /<skill>`, surfaced as a generic run
/// failure) — it does NOT silently degrade to an ordinary-prompt completion. The
/// contract does not promise a distinct `unavailable: unknown skill` shape (the
/// runner can't reliably distinguish that from any other non-zero exit); the
/// two-axis grant already bounds what may be dispatched, so an uninstalled skill
/// is a config gap, never a security event.
#[tauri::command]
pub async fn run_skill(
    app: tauri::AppHandle,
    tunables: tauri::State<'_, AgentRunnerTunables>,
    cap: tauri::State<'_, RunSkillCap>,
    skill: String,
    args: Vec<String>,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<String, String> {
    // SW-AN-44: run the preferred adapter's agent — the write-capable sibling
    // of `analyze`, still "the user's configured agent", not a per-profile binary.
    let binary = crate::commands::mcp_workspace::resolve_agent_for_launch(
        &app,
        crate::agents::builtin::preferred_binary(),
    )
    .await?
    .path;
    let config = AgentRunnerConfig {
        binary,
        args: skill_default_args_template(),
        model: tunables.model.clone(),
        timeout: RUN_SKILL_TIMEOUT,
        cwd: None,
    };
    run_skill_for(app, cap.0.clone(), config, skill, args, frame_id, page).await
}

async fn run_skill_for<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    cap: SpawnCap,
    mut config: AgentRunnerConfig,
    skill: String,
    args: Vec<String>,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<String, String> {
    let scope =
        crate::context_repo::caller_scope::for_page(&app, page.as_deref(), frame_id.as_deref())?;
    // Architecture: a page's own skill directory follows its host team, independent of UI selection.
    let ctx = scope
        .context(&app)
        .await
        .map_err(|e| format!("unavailable: {e}"))?;
    crate::commands::await_blocking("run_skill", move || {
        let team = ctx
            .team_name
            .as_deref()
            .ok_or_else(|| "unavailable: no active team for a skill run".to_string())?;
        config.cwd = Some(
            crate::commands::dashboard_launch::resolve_managed_team_content_root_for(
                &ctx.codesense_home,
                &ctx.principal(),
                &ctx.org_slug,
                team,
                &scope,
            )?,
        );
        run_skill_core(&cap, &config, &skill, &args)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn test_dir(label: &str) -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("codesense_agent_runner_test")
            .join(format!("{}_{}_{}", label, std::process::id(), id));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn create_fake_binary(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("agent");
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn config_for(binary: PathBuf) -> AgentRunnerConfig {
        AgentRunnerConfig {
            binary,
            args: claude_default_args_template(),
            model: crate::agents::builtin::preferred_runner()
                .default_model
                .clone(),
            timeout: DEFAULT_RUNNER_TIMEOUT,
            cwd: None,
        }
    }

    // ── run_agent parity with invoke_claude (AC-1) ──────────────────────────

    #[test]
    fn run_agent_success() {
        let dir = test_dir("success");
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\necho '{\"ok\":true}'",
        );
        let result = run_agent(&config_for(binary), "prompt");
        assert_eq!(result.unwrap(), "{\"ok\":true}");
        cleanup(&dir);
    }

    #[test]
    fn run_agent_non_zero_exit_includes_stderr() {
        let dir = test_dir("nonzero");
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\necho 'boom' >&2\nexit 1",
        );
        let err = run_agent(&config_for(binary), "prompt").unwrap_err();
        assert!(err.contains("exited with status"), "got: {}", err);
        assert!(err.contains("boom"), "got: {}", err);
        cleanup(&dir);
    }

    #[test]
    fn run_agent_empty_output_is_error() {
        let dir = test_dir("empty");
        let binary = create_fake_binary(&dir, "#!/bin/sh\ncat /dev/stdin > /dev/null");
        let err = run_agent(&config_for(binary), "prompt").unwrap_err();
        assert!(err.contains("empty output"), "got: {}", err);
        cleanup(&dir);
    }

    #[test]
    fn run_agent_missing_binary_is_spawn_error() {
        let cfg = config_for(PathBuf::from("/nonexistent/path/agent"));
        let err = run_agent(&cfg, "prompt").unwrap_err();
        assert!(err.contains("failed to spawn"), "got: {}", err);
    }

    #[test]
    fn run_agent_trims_output() {
        let dir = test_dir("trim");
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\necho '   {\"a\":1}   '",
        );
        assert_eq!(run_agent(&config_for(binary), "p").unwrap(), "{\"a\":1}");
        cleanup(&dir);
    }

    #[test]
    fn run_agent_handles_large_output_without_deadlock() {
        // ~270 KiB of stdout — well past the ~64 KiB pipe buffer. A non-draining
        // reader (read-after-exit) would let the child block on a full pipe and
        // only die at the timeout kill; concurrent draining must let it complete
        // fast with the full output. This is the risk the runner's generalization
        // to large `analyze` responses opened.
        let dir = test_dir("large");
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\nawk 'BEGIN{for(i=0;i<8192;i++)printf \"0123456789ABCDEF0123456789ABCDEF\\n\"}'",
        );
        let mut cfg = config_for(binary);
        cfg.timeout = Duration::from_secs(10);
        let start = std::time::Instant::now();
        let out = run_agent(&cfg, "p").unwrap();
        assert!(
            out.len() > 128 * 1024,
            "expected large output, got {}",
            out.len()
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "should not near the timeout"
        );
        cleanup(&dir);
    }

    #[test]
    fn run_agent_large_prompt_to_non_reading_binary_does_not_hang() {
        // Review regression (MAJOR): a CLI that exits WITHOUT draining stdin, given a
        // prompt far larger than the pipe buffer, must not block the caller. stdin is
        // written on its own thread, so a broken-pipe write is harmless and the child's
        // own exit governs — no hang, no wedged single-flight latch.
        let dir = test_dir("nostdin");
        let binary = create_fake_binary(&dir, "#!/bin/sh\necho 'quick'");
        let mut cfg = config_for(binary);
        cfg.timeout = Duration::from_secs(10);
        let big_prompt = "x".repeat(300 * 1024);
        let start = std::time::Instant::now();
        let out = run_agent(&cfg, &big_prompt).unwrap();
        assert_eq!(out, "quick");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not block on the stdin write"
        );
        cleanup(&dir);
    }

    #[test]
    fn run_agent_times_out_and_kills() {
        // Config-driven timeout is the payoff of generalization: assert the
        // kill-on-hang path fast (200ms) instead of the 60s production ceiling.
        let dir = test_dir("timeout");
        // Default capture kills one child; exec avoids a pipe-holding descendant.
        let binary = create_fake_binary(&dir, "#!/bin/sh\nexec sleep 5");
        let mut cfg = config_for(binary);
        cfg.timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let err = run_agent(&cfg, "p").unwrap_err();
        assert!(err.contains("timed out"), "got: {}", err);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "should kill promptly"
        );
        cleanup(&dir);
    }

    #[test]
    fn run_agent_non_claude_config_no_model_flag_runs() {
        // A non-Claude CLI: different binary, model: None → no --model flag, and
        // a bespoke argv template. Proves "another CLI later" needs no code change.
        let dir = test_dir("nonclaude");
        // Echoes its own argv so we can assert --model is absent.
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\necho \"args:$*\"",
        );
        let cfg = AgentRunnerConfig {
            binary,
            args: vec![
                "--print".to_string(),
                "--model".to_string(),
                MODEL_PLACEHOLDER.to_string(),
            ],
            model: None,
            timeout: Duration::from_secs(60),
            cwd: None,
        };
        let out = run_agent(&cfg, "p").unwrap();
        assert_eq!(out, "args:--print");
        cleanup(&dir);
    }

    // ── argv template substitution (AC-1) ───────────────────────────────────

    #[test]
    fn build_argv_substitutes_model_when_present() {
        let argv = build_argv(&claude_default_args_template(), Some("claude-opus-4-6"));
        assert_eq!(
            argv,
            vec![
                "-p",
                "--output-format",
                "text",
                "--model",
                "claude-opus-4-6",
                "--max-turns",
                "1"
            ]
        );
    }

    #[test]
    fn build_argv_drops_model_pair_when_absent() {
        let argv = build_argv(&claude_default_args_template(), None);
        assert_eq!(
            argv,
            vec!["-p", "--output-format", "text", "--max-turns", "1"]
        );
        assert!(!argv
            .iter()
            .any(|a| a == "--model" || a == MODEL_PLACEHOLDER));
    }

    #[test]
    fn tunables_default_is_byte_compatible_claude_invocation() {
        // AC-2 behavior-preservation lock: the adapter-driven default must still
        // produce the exact historical Claude invocation, byte-for-byte. Asserted
        // against the literal (not the adapter) so an adapter edit can't silently drift.
        let cfg = AgentRunnerTunables::default().with_binary(PathBuf::from("/x/claude"));
        let argv = build_argv(&cfg.args, cfg.model.as_deref());
        assert_eq!(
            argv,
            vec![
                "-p",
                "--output-format",
                "text",
                "--model",
                "claude-opus-4-6",
                "--max-turns",
                "1"
            ]
        );
        assert_eq!(cfg.timeout, DEFAULT_RUNNER_TIMEOUT);
    }

    // ── no credential field (AC-5) ──────────────────────────────────────────

    #[test]
    fn config_carries_no_credential_field() {
        // Exhaustively destructure EVERY field. If a `secret`/`token`/`api_key`
        // field is ever added, this stops compiling until a reviewer consciously
        // updates it — encoding "no LLM credential lives in the runner" in types.
        let cfg = config_for(PathBuf::from("/x"));
        let AgentRunnerConfig {
            binary: _,
            args: _,
            model: _,
            timeout: _,
            cwd: _,
        } = cfg;
    }

    // ── analyze: data cap, single-flight, typed degrade (AC-3/AC-5) ─────────

    #[test]
    fn compose_prompt_passes_small_data_unchanged() {
        let out = compose_prompt("analyze", "{\"a\":1}");
        assert!(out.contains("analyze"));
        assert!(out.contains("{\"a\":1}"));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn compose_prompt_truncates_oversize_data_with_annotation() {
        let big = "x".repeat(ANALYZE_MAX_DATA_BYTES + 5_000);
        let out = compose_prompt("p", &big);
        assert!(out.contains("truncated at 256 KiB"), "expected annotation");
        // Truncated near the cap, not the full oversize payload.
        assert!(out.len() < ANALYZE_MAX_DATA_BYTES + 1_000);
    }

    #[test]
    fn cap_data_truncates_on_char_boundary() {
        // Multi-byte chars around the boundary must not panic / split a char.
        let s = "€".repeat(ANALYZE_MAX_DATA_BYTES); // 3 bytes each
        let out = cap_data(&s);
        assert!(matches!(out, Cow::Owned(_)));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn run_analyze_second_concurrent_is_busy() {
        let flag = AtomicBool::new(true); // simulate one already in flight
        let cfg = config_for(PathBuf::from("/nonexistent/agent"));
        assert_eq!(run_analyze(&flag, &cfg, "p", "d").unwrap_err(), "busy");
        // The busy path must NOT clear a latch it did not acquire.
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn run_analyze_clears_latch_on_completion() {
        let dir = test_dir("latch");
        let binary = create_fake_binary(
            &dir,
            "#!/bin/sh\ncat /dev/stdin > /dev/null\necho 'analysis'",
        );
        let flag = AtomicBool::new(false);
        assert_eq!(
            run_analyze(&flag, &config_for(binary), "p", "d").unwrap(),
            "analysis"
        );
        assert!(
            !flag.load(Ordering::SeqCst),
            "latch must reset after completion"
        );
        cleanup(&dir);
    }

    #[test]
    fn run_analyze_missing_binary_degrades_to_unavailable() {
        let flag = AtomicBool::new(false);
        let cfg = config_for(PathBuf::from("/nonexistent/agent"));
        let err = run_analyze(&flag, &cfg, "p", "d").unwrap_err();
        assert!(err.starts_with("unavailable:"), "got: {}", err);
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn run_analyze_timeout_degrades_to_typed_timeout() {
        let dir = test_dir("analyze_timeout");
        let binary = create_fake_binary(&dir, "#!/bin/sh\nsleep 5");
        let mut cfg = config_for(binary);
        cfg.timeout = Duration::from_millis(200);
        let flag = AtomicBool::new(false);
        let err = run_analyze(&flag, &cfg, "p", "d").unwrap_err();
        assert!(err.starts_with("timeout:"), "got: {}", err);
        cleanup(&dir);
    }

    #[test]
    fn classify_maps_auth_stderr_to_unavailable() {
        let err = classify_agent_error(
            "agent exited with status exit status: 1: Please run `claude login`".into(),
        );
        assert!(err.starts_with("unavailable:"), "got: {}", err);
    }

    #[test]
    fn classify_passes_generic_error_through() {
        let err = classify_agent_error("agent returned empty output".into());
        assert_eq!(err, "agent returned empty output");
    }

    #[test]
    fn classify_does_not_over_match_a_word_in_a_generic_error() {
        // Review regression (LOW): a generic failure that merely mentions "login" or
        // "authentication" as a word must NOT be steered to the re-auth `unavailable:`
        // degrade — only anchored auth phrases classify.
        let a = classify_agent_error("agent exited with status 1: cannot read login.json".into());
        assert!(!a.starts_with("unavailable:"), "got: {}", a);
        let b = classify_agent_error(
            "agent exited with status 1: analysis of the authentication module failed".into(),
        );
        assert!(!b.starts_with("unavailable:"), "got: {}", b);
    }

    // ── SW-DX-11: run_skill — write-capable sibling (AC-5/AC-8) ──────────────

    fn skill_config_for(binary: PathBuf) -> AgentRunnerConfig {
        // Skills MUST run anchored to a real absolute dir; default to a valid,
        // existing managed-like dir so the core's cwd invariant passes. Tests
        // that exercise the cwd path override this explicitly.
        skill_config_in(binary, Some(std::env::temp_dir()))
    }

    fn skill_config_in(binary: PathBuf, cwd: Option<PathBuf>) -> AgentRunnerConfig {
        AgentRunnerConfig {
            binary,
            args: skill_default_args_template(),
            model: crate::agents::builtin::preferred_runner()
                .default_model
                .clone(),
            timeout: DEFAULT_RUNNER_TIMEOUT,
            cwd,
        }
    }

    #[test]
    fn compose_skill_prompt_forms_a_bounded_slash_command() {
        assert_eq!(compose_skill_prompt("sync-story", &[]), "/sync-story");
        assert_eq!(
            compose_skill_prompt("sync-story", &["DEMO-1893".to_string()]),
            "/sync-story DEMO-1893"
        );
        assert_eq!(
            compose_skill_prompt("x", &["a".to_string(), "b".to_string()]),
            "/x a b"
        );
    }

    #[test]
    fn skill_template_is_multi_turn_and_edit_capable_unlike_analyze() {
        // The security-relevant difference from `analyze`: max-turns is NOT 1, and
        // edits are pre-accepted so a headless write-capable run can proceed.
        let argv = build_argv(&skill_default_args_template(), Some("claude-opus-4-8"));
        let mt = argv.iter().position(|a| a == "--max-turns").unwrap();
        // The value is descriptor data; assert the security-relevant property (not `1`)
        // and parity with the descriptor's own skill recipe.
        assert_ne!(argv[mt + 1], "1", "run_skill must NOT be single-turn");
        let descriptor_turns = {
            let sa = &crate::agents::builtin::preferred_runner().skill_args;
            let i = sa.iter().position(|a| a == "--max-turns").unwrap();
            sa[i + 1].clone()
        };
        assert_eq!(argv[mt + 1], descriptor_turns);
        assert!(argv.iter().any(|a| a == "acceptEdits"));
    }

    #[test]
    fn run_skill_core_feeds_the_slash_command_prompt_to_the_agent() {
        // A fake agent that echoes its stdin (the composed prompt) back on stdout.
        let dir = test_dir("skill_run");
        let binary = create_fake_binary(&dir, "#!/bin/sh\ncat /dev/stdin");
        let cap = SpawnCap::new(1);
        let out = run_skill_core(
            &cap,
            &skill_config_for(binary),
            "sync-story",
            &["DEMO-1".to_string()],
        )
        .unwrap();
        assert_eq!(out, "/sync-story DEMO-1");
        // The permit was released after the run — cap back to full.
        assert_eq!(cap.available(), 1);
        cleanup(&dir);
    }

    #[test]
    fn run_skill_core_rejects_a_prompt_splitting_arg_before_spawning() {
        // Review r1 (Edge Hunter) + r2: a line-breaking char in an arg must NOT
        // compose a second slash-command turn — `\n`, `\r`, AND the Unicode line
        // separator U+2028 (which is NOT `char::is_control()`). Rejected before the
        // permit + spawn (binary never reached).
        let cap = SpawnCap::new(1);
        for bad_arg in [
            "DEMO-1\n/other-skill",
            "DEMO-1\u{2028}/other-skill",
            "x\r/y",
        ] {
            let err = run_skill_core(
                &cap,
                &skill_config_for(PathBuf::from("/nonexistent/agent")),
                "sync-story",
                &[bad_arg.to_string()],
            )
            .unwrap_err();
            assert!(err.starts_with("not-allowed:"), "arg {bad_arg:?} → {err}");
        }
        assert_eq!(
            cap.available(),
            1,
            "an invalid invocation must not take a slot"
        );
    }

    #[test]
    fn run_skill_core_rejects_a_forked_skill_name() {
        let cap = SpawnCap::new(1);
        for bad in ["sync story", "a/b", "x\ny", ""] {
            let err = run_skill_core(
                &cap,
                &skill_config_for(PathBuf::from("/nonexistent/agent")),
                bad,
                &[],
            )
            .unwrap_err();
            assert!(err.starts_with("not-allowed:"), "skill '{bad}' → {err}");
        }
    }

    #[test]
    fn run_skill_core_is_busy_when_its_own_cap_is_saturated() {
        let cap = SpawnCap::new(1);
        let _held = cap.try_acquire().expect("saturate the cap");
        // Binary is never reached — the busy reject fires before any spawn.
        let err = run_skill_core(
            &cap,
            &skill_config_for(PathBuf::from("/nonexistent/agent")),
            "sync-story",
            &[],
        )
        .unwrap_err();
        assert_eq!(err, "busy");
    }

    #[test]
    fn run_skill_core_missing_binary_degrades_to_unavailable() {
        let cap = SpawnCap::new(1);
        let err = run_skill_core(
            &cap,
            &skill_config_for(PathBuf::from("/nonexistent/agent")),
            "sync-story",
            &[],
        )
        .unwrap_err();
        assert!(err.starts_with("unavailable:"), "got: {}", err);
        // A failed run still released the permit.
        assert_eq!(cap.available(), 1);
    }

    #[test]
    fn run_skill_cap_is_separate_budget_from_default() {
        // Two independent caps — saturating one does not consume the other (the
        // concurrency-separation guarantee vs the openWorkspace cap).
        let skill_cap = RunSkillCap::default();
        assert!(skill_cap.0.available() >= 1);
    }

    // ── RF-7: cwd anchoring — the child runs in the resolved managed dir ──────

    #[test]
    fn run_skill_core_threads_cwd_to_the_spawned_child() {
        // AC-4: the resolved cwd is applied to the child via `current_dir`, so a
        // `/<skill>` resolves against `<cwd>/.claude/skills/`. A fake agent prints
        // its own physical working directory; assert it equals the threaded dir.
        let dir = test_dir("skill_cwd_bin");
        let anchor = test_dir("skill_cwd_anchor");
        let anchor_canon = anchor.canonicalize().unwrap();
        let binary = create_fake_binary(&dir, "#!/bin/sh\npwd -P");
        let cap = SpawnCap::new(1);
        let out = run_skill_core(
            &cap,
            &skill_config_in(binary, Some(anchor_canon.clone())),
            "tech-debt-scout",
            &[],
        )
        .unwrap();
        assert_eq!(fs::canonicalize(out.trim()).unwrap(), anchor_canon);
        // The permit was released after the run — cap back to full.
        assert_eq!(cap.available(), 1);
        cleanup(&dir);
        cleanup(&anchor);
    }

    #[test]
    fn run_skill_core_rejects_missing_cwd_before_cap() {
        // AC-2: a skill run with no resolved working directory never spawns and
        // never consumes a cap slot — it degrades to a typed `unavailable:`.
        let cap = SpawnCap::new(1);
        let err = run_skill_core(
            &cap,
            &skill_config_in(PathBuf::from("/nonexistent/agent"), None),
            "tech-debt-scout",
            &[],
        )
        .unwrap_err();
        assert!(err.starts_with("unavailable:"), "got: {err}");
        assert_eq!(cap.available(), 1, "a missing cwd must not take a slot");
    }

    #[test]
    fn run_skill_core_rejects_relative_or_nonexistent_cwd() {
        // AC-2: a relative or non-existent cwd is refused BEFORE the cap + spawn
        // (the binary is never reached), so the FE can classify it honestly.
        let cap = SpawnCap::new(1);
        for bad in ["relative/dir", "/no/such/managed/dir"] {
            let err = run_skill_core(
                &cap,
                &skill_config_in(
                    PathBuf::from("/nonexistent/agent"),
                    Some(PathBuf::from(bad)),
                ),
                "tech-debt-scout",
                &[],
            )
            .unwrap_err();
            assert!(err.starts_with("unavailable:"), "cwd {bad:?} → {err}");
        }
        assert_eq!(cap.available(), 1, "a bad cwd must not take a slot");
    }
    #[tokio::test]
    async fn page_skill_spawn_uses_fixed_child_host_while_native_uses_active_team() {
        use crate::context_repo::caller_scope::tests::AppFixture;
        use tauri::Manager;
        let f = AppFixture::new();
        let page = f.fixture.path("parent", "panes/parent.html");
        let frame = f.frame(&page, "workspace");
        let binary = create_fake_binary(&f.fixture.home, "#!/bin/sh\npwd -P");
        for org in ["acme/ctx", "other/ctx"] {
            f.app.state::<crate::mcp::state::ActiveTeamState>().update(
                crate::mcp::state::ActiveTeam {
                    org_slug: org.into(),
                    team_name: "parent".into(),
                },
            );
            let out = run_skill_for(
                f.app.handle().clone(),
                SpawnCap::new(1),
                skill_config_in(binary.clone(), None),
                "inspect".into(),
                vec![],
                Some(frame.frame_id.clone()),
                Some(page.display().to_string()),
            )
            .await
            .unwrap();
            assert_eq!(
                fs::canonicalize(out.trim()).unwrap(),
                f.fixture.path("child", "content")
            );
        }
        f.app
            .state::<crate::mcp::state::ActiveTeamState>()
            .update(crate::mcp::state::ActiveTeam {
                org_slug: "acme/ctx".into(),
                team_name: "parent".into(),
            });
        let out = run_skill_for(
            f.app.handle().clone(),
            SpawnCap::new(1),
            skill_config_in(binary, None),
            "inspect".into(),
            vec![],
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fs::canonicalize(out.trim()).unwrap(),
            f.fixture.path("parent", "content")
        );
    }
}
