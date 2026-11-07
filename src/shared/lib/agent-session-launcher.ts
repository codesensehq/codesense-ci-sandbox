// SPDX-License-Identifier: MIT
import {
  projectMcpScopeConfig,
  sessionHasLiveAgent,
  type McpScopeLaunch,
  type McpScopeOutcome,
  type ScopedExecution,
  type ResolveAgentAccountInput,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";
import {
  injectAgentLine,
  injectWhenReady,
  type InjectDeps,
  type InjectOutcome,
} from "@/shared/lib/agent-session-injection";
import {
  createTerminalTab,
  getViewerState,
  selectRoleTab,
  type NewTerminalTab,
  type TerminalRole,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { whenTerminalLayoutHydrated } from "@/shared/lib/terminal-layout";
import { logger } from "@/shared/lib/logger";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";

/**
 * SW-GS-2b — the generalized codesense-managed-agent session launcher: connect
 * an agent into a workspace session, keyed by `(teamKey, role)` and
 * parameterized by `(cwd, agentPath, role, workspaceKey, reorientCommand)` plus
 * injected `LaunchDeps` (provision/spawn/inject) — NO `"claude"` or other
 * agent-specific literals, so story/epic agent surfaces reuse it unchanged.
 *
 * SW-WA-F10: an MCP managed launch (`open_workspace` → `start_agent`) spawns
 * the agent CLI as PID-1 of its PTY (argv-direct, no shell wrapper). Ctrl+C
 * stays with the agent in-process (standard REPL: cancel the current turn);
 * agent exit ends the session. The historical "drops to zsh" behavior was
 * an artifact of running the agent as a shell child, not a user-facing
 * feature. A user-opened mixed-mode terminal (zsh with `claude` typed by
 * hand) still routes Ctrl+C through the shell — see `pty.rs`
 * `agent_in_foreground`.
 */

/** The env vars a guarded agent session must never inherit. AC-7: the session is
 *  subscription-billed; an `ANTHROPIC_API_KEY` would silently switch it to
 *  API billing. */
export const GUARDED_ENV_STRIP = ["ANTHROPIC_API_KEY"];

/** Shell-quote a single argument (single-quote wrap, escaping embedded quotes). */
function shellQuote(arg: string): string {
  return `'${arg.replace(/'/g, `'\\''`)}'`;
}

/**
 * The launch line for the agent at `agentPath` with `args`: every token shell-quoted. A
 * named agent's launch passes the args its backend prepared; the launcher's own default
 * launch (a dashboard tab's agent) omits them and runs permissively
 * (`--dangerously-skip-permissions`), which is safe because the session runs inside
 * CodeSense — visible, steerable and killable. NEVER carries `-p`/`--print`: the session
 * must be a real interactive (subscription) TUI.
 */
const PERMISSIVE_LAUNCH_FLAG = "--dangerously-skip-permissions";

export function buildAgentLaunchCommand(
  agentPath: string,
  args: readonly string[] = [PERMISSIVE_LAUNCH_FLAG],
): string {
  // Shell-quote the path AND every arg: each arg is one discrete argv token, so a
  // token with whitespace/metacharacters must stay one token (defense-in-depth for
  // a security-adjacent builder — SW-AN-10 will let users author profile `args`).
  // A bare flag like `--dangerously-skip-permissions` quotes to an identical token.
  return [agentPath, ...args].map(shellQuote).join(" ");
}

/** SW-AN-45: render an `env_dir` scope's env vars as an inline `KEY='value' ` shell prefix
 *  (each value shell-quoted, trailing space). Empty for a `config_flag` / unscoped launch.
 *  Prepended to the submitted launch line so an env-scoped agent (e.g. Codex → `CODEX_HOME`)
 *  runs with its scoped home — the shell-line analogue of the headless path's real env vars. */
/** Injected seams (real defaults below) so the orchestration unit-tests without
 *  Tauri, a store, or a PTY. */
export interface LaunchDeps {
  /** AC-1 dedup: the already-owned session for `(teamKey, role)`, if one exists
   *  — read from the terminal hosts' layout once it is known, so an agent that
   *  outlived a reload or relaunch is adopted, never duplicated. */
  findExisting: (
    teamKey: string,
    role: TerminalRole,
  ) => Promise<{ workspaceId: string; sessionId: string } | undefined>;
  /** Provision `org_root` (MCP + recipe + AC-6 allowlist). */
  provision: (orgRoot: string) => Promise<void>;
  /** Open the role-tagged owned tab in `workspaceId` — its interactive login
   *  shell spawned at `cwd` and placed by its terminal host at once — stripping
   *  `envRemove`. Resolves once placed. */
  openTab: (
    teamKey: string,
    workspaceId: string,
    tab: NewTerminalTab & { role: TerminalRole },
    shell: {
      cwd: string;
      envRemove: string[];
      account?: ResolveAgentAccountInput;
      environmentBinding?: string;
      execution?: ScopedExecution;
    },
  ) => Promise<void>;
  /** End a half-built owned tab after a FAILED launch, so a retry relaunches
   *  instead of re-binding a bare orphan. Ending the session also reaps a scoped
   *  launch's 0600 MCP config (keyed by the session), so the secret file never
   *  lingers or blocks a same-session retry. */
  closeTab: (sessionId: string) => void | Promise<void>;
  /** Inject the agent launch line, retrying until the bare `zsh` is idle (gate
   *  `"shell"`). Returns the final outcome — the caller surfaces a failure. */
  launchAgentLine: (sessionId: string, line: string) => Promise<InjectOutcome>;
  /** SW-AN-42 → SW-AN-45: project the scoped MCP config for a scoped launch and return the
   *  launch additions (agent-neutral) — `extraArgs` plus an optional opaque
   *  backend environment binding — or both empty when the resolving pack declares no scope mechanism (launch
   *  unfiltered). Keyed by the PTY `sessionId` so ending the session reaps the config. Injected
   *  so the orchestration unit-tests without Tauri. */
  projectScopeConfig: (
    sessionId: string,
    cwd: string,
    mcpAllow: string[] | null,
    account?: ResolveAgentAccountInput,
    launchArgs?: string[],
    launch?: McpScopeLaunch,
  ) => Promise<McpScopeOutcome>;
}

/** A launch the backend prepared for one session (a named agent): what to run, and the
 *  shell rules to run it with. Every field is the backend's; nothing is recomputed here. */
export interface PreparedShellLaunch {
  /** Present when the backend requires an argv root, rather than a shell child. */
  execution?: ScopedExecution;
  /** Canonical product root whose preparation already includes required wiring. */
  cwd?: string;
  /** Resolved agent executable. */
  agentPath: string;
  /** The argv after the agent path, before the scope's flags. */
  args: string[];
  /** The MCP allowlist a relaunch projects a fresh scope for. */
  mcpAllow: string[];
  /** Captured effective CodeSense tool grant; null/absent is unrestricted, [] denies all. */
  bridgeTools?: string[] | null;
  /** The scope's launch flags, already projected for this session. */
  extraArgs: string[];
  /** Env var names stripped from the shell (the runtime's guarded key). */
  envRemove: string[];
  /** The account the spawn resolves. */
  account?: ResolveAgentAccountInput;
  /** The opaque scope environment binding the spawn consumes. */
  environmentBinding?: string;
  /** The agent's own environment variables, which a relaunch sets again. */
  env?: Record<string, string>;
  /** The tab's title when the launcher names none: the prepared agent's display name. */
  title?: string;
}

export interface LaunchParams {
  teamKey: string;
  /** The session cwd (git-sync passes the single context clone root `org_root`). */
  orgRoot: string;
  /** Resolved agent executable (e.g. `claudePath`). Unused when `prepare` is given. */
  agentPath: string;
  /** The role stamped on the owned tab — the agent-agnostic binding signal. */
  role: TerminalRole;
  /** The workspace id the owned tab lives in (one owned tab per team there).
   *  Consumer-supplied — no git-sync literal here. */
  workspaceKey: string;
  /** SW-GW-6: optional display name for the owned tab (e.g. "Architecture
   *  Review"). Omitted → the prepared agent's display name, else the sequential
   *  `Terminal N` default. Purely cosmetic; does NOT affect the role dedup key or
   *  the session id. */
  title?: string;
  /** A launch the backend prepares for the new session, given its id once minted and
   *  before its shell spawns. When set, what it returns replaces `agentPath` and supplies
   *  the args, MCP allowlist, env strip and account, and no scope is projected here (the
   *  backend already projected it for the session). Without it the agent at `agentPath`
   *  launches with the launcher's default args and a per-session config. */
  prepare?: (sessionId: string) => Promise<PreparedShellLaunch>;
}

export interface LaunchResult {
  workspaceId: string;
  sessionId: string;
}

/**
 * Connect (or relaunch into) the owned agent session. Order is load-bearing:
 *  1. provision the cwd (a caller's own surface setup), then project the session's
 *     config (MCP bridge, status hooks) outside it;
 *  2. open the role-tagged owned tab in the consumer's `workspaceKey` workspace —
 *     its terminal host spawns `zsh` at the cwd with `ANTHROPIC_API_KEY` stripped
 *     (AC-7) and places the tab at once — NOT the agent as PID-1, so Ctrl+C drops
 *     to the shell (AC-2);
 *  3. guarded-inject the agent launch line (no `-p`) to start the TUI.
 */
/** In-flight launches keyed by `${teamKey}::${role}` so two overlapping calls
 *  (a double-clicked Publish, or Publish racing an auto-launch) SHARE one launch
 *  instead of both passing the check-then-act dedup and spawning two agents. An
 *  entry older than {@link IN_FLIGHT_STALE_MS} is a launch that never settled (a
 *  hung prepare or inject): it is dropped, so it can never wedge every later start
 *  of the same agent. */
const inFlightLaunches = new Map<string, { launch: Promise<LaunchResult>; startedAt: number }>();

/** How long a launch may stay in flight before a new call stops waiting on it. */
export const IN_FLIGHT_STALE_MS = 120_000;

export function connectAgentSession(
  params: LaunchParams,
  deps: LaunchDeps,
  now: () => number = Date.now,
): Promise<LaunchResult> {
  const { teamKey, role } = params;

  // AC-1 "never spawn a duplicate": (1) a concurrent in-flight launch — share its
  // promise; (2) an already-running owned tab (this run's, or one that outlived
  // a reload or relaunch) — adopt it.
  const key = `${teamKey}::${role}`;
  const inFlight = inFlightLaunches.get(key);
  if (inFlight && now() - inFlight.startedAt < IN_FLIGHT_STALE_MS) return inFlight.launch;

  const entry = {
    startedAt: now(),
    launch: (async () =>
      (await deps.findExisting(teamKey, role)) ?? connectAgentSessionInner(params, deps))().finally(
      () => {
        // A stale entry replaced by a newer launch is not this one's to clear.
        if (inFlightLaunches.get(key) === entry) inFlightLaunches.delete(key);
      },
    ),
  };
  inFlightLaunches.set(key, entry);
  return entry.launch;
}

async function connectAgentSessionInner(
  params: LaunchParams,
  deps: LaunchDeps,
): Promise<LaunchResult> {
  const { teamKey, orgRoot, agentPath, role, workspaceKey, title } = params;

  if (!params.prepare) await deps.provision(orgRoot);
  const workspaceId = workspaceKey;

  // SW-GW-6: `title` names the owned tab (e.g. a named story session's label);
  // omitted → the prepared agent's name, else the host's `Terminal N` default for
  // git-sync/notes callers.
  const minted = { ...createTerminalTab(title), role };
  let tab = minted;
  let tabOpened = false;

  // End the owned tab on ANY failure, so the next connect attempt relaunches
  // instead of `findExisting` re-binding a dead or bare-shell orphan (the
  // half-built-tab-wedges-the-session bug).
  try {
    const prepared = params.prepare ? await params.prepare(tab.sessionId) : undefined;
    if (!title && prepared?.title) tab = { ...minted, name: prepared.title };
    if (params.prepare && !prepared?.cwd) await deps.provision(orgRoot);
    const shell = {
      cwd: prepared?.cwd ?? orgRoot,
      // The prepared runtime's guarded key (Codex → `OPENAI_API_KEY`); the default launch
      // strips Claude's.
      envRemove: prepared?.envRemove ?? GUARDED_ENV_STRIP,
    };
    const accountIntent = prepared?.account;
    let extraArgs: string[] = prepared?.extraArgs ?? [];
    let environmentBinding: string | undefined = prepared?.environmentBinding;
    let execution = prepared?.execution;
    // Every launch gets its per-session config (the bridge, and Claude's status hooks)
    // from here — nothing is written into the folder. A launch with no allowlist keeps the
    // agent's own servers.
    if (!prepared) {
      const scope = await deps.projectScopeConfig(
        tab.sessionId,
        orgRoot,
        null,
        undefined,
        undefined,
        { binary: agentBasename(agentPath), defaultArgs: true },
      );
      extraArgs = scope.extraArgs;
      environmentBinding = scope.environmentBinding ?? undefined;
      execution = scope.execution;
    }
    const spec: AgentLaunchSpec = prepared
      ? {
          agentPath: prepared.agentPath,
          args: prepared.args,
          shell: {
            ...shell,
            account: accountIntent,
            mcpAllow: prepared.mcpAllow,
            bridgeTools: prepared.bridgeTools ?? null,
            agent: true,
            ...(prepared.env ? { env: prepared.env } : {}),
          },
        }
      : { agentPath, shell };

    // The backend may require the agent as the PTY root. Otherwise retain the
    // existing shell launch, with the same cwd, account and environment binding.
    await deps.openTab(teamKey, workspaceId, tab, {
      ...shell,
      account: accountIntent,
      environmentBinding,
      execution,
    });
    tabOpened = true;

    // A structured exec already started the agent. Shell launches still use the
    // existing bounded injector and surface its failure.
    const line = execution
      ? buildAgentLaunchCommand(execution.argv[0], execution.argv.slice(1))
      : agentLaunchLine(spec, extraArgs);
    const launched = execution ? { ok: true } : await deps.launchAgentLine(tab.sessionId, line);
    if (!launched.ok) {
      throw new Error(
        `Agent launch: the shell never reached its prompt to start the agent (session ${tab.sessionId}).`,
      );
    }
    const recorded = execution
      ? { ...spec, agentPath: execution.argv[0], args: execution.argv.slice(1) }
      : spec;
    recordAgentLaunch(tab.sessionId, recorded, line);
  } catch (err) {
    try {
      await deps.closeTab(tab.sessionId);
    } catch (closeError) {
      // A failed prepare or open may never have placed a tab. Only that session's
      // missing entry is already closed; other host failures must reach the caller.
      const message = closeError instanceof Error ? closeError.message : closeError;
      if (tabOpened || message !== `layout: no entry for session ${tab.sessionId}`)
        throw closeError;
    }
    throw err;
  }

  return { workspaceId, sessionId: tab.sessionId };
}

/**
 * Store-and-host-backed launch deps, parameterized by the consumer's
 * `provision` step (each agent passes its own allowlist provisioner — this is
 * the "permissionSet" seam, carried by provisioning rather than a speculative
 * param). The store/injection deps are generic. SW-GS-2d: an agent born into a
 * minimized workspace opens its tab there — its xterm never mounts (zero-flash,
 * headless).
 */
export function makeRealLaunchDeps(provision: LaunchDeps["provision"]): LaunchDeps {
  return {
    findExisting: async (teamKey, role) => {
      // Until the hosts' layout is known, "no role tab" means "not known yet".
      // A minimized or Hidden agent counts, so reconnect re-binds it instead of
      // spawning a second.
      await whenTerminalLayoutHydrated();
      const hit = selectRoleTab(getViewerState(), teamKey, role);
      return hit ? { workspaceId: hit.workspaceId, sessionId: hit.tab.sessionId } : undefined;
    },
    provision,
    openTab: (teamKey, workspaceId, tab, shell) =>
      getViewerState().addTerminalTab(teamKey, tab, workspaceId, {
        cwd: shell.cwd,
        envRemove: shell.envRemove,
        account: shell.account,
        environmentBinding: shell.environmentBinding,
        commandArgv: shell.execution?.argv,
        nativeLaunch: shell.execution?.nativeLaunch,
      }),
    closeTab: (sessionId) => getViewerState().removeTerminalTabBySession(sessionId, { kill: true }),
    launchAgentLine: launchAgentLineInShell,
    // SW-AN-42: the real Rust projection (thin wrapper over AN-41's projector).
    projectScopeConfig: (sessionId, cwd, mcpAllow, account, launchArgs, launch) =>
      projectMcpScopeConfig(sessionId, cwd, mcpAllow, account, launchArgs, launch),
  };
}

/**
 * Type an agent's launch line into its bare shell: waits until the shell is at
 * its prompt (gate "shell"), and the injector's leading space keeps it out of
 * the user's history. Shell launches and shell relaunches go through here.
 */
export function launchAgentLineInShell(
  sessionId: string,
  line: string,
  injectDeps?: InjectDeps,
): Promise<InjectOutcome> {
  return injectWhenReady(sessionId, line, { gate: "shell" }, injectDeps);
}

/**
 * How CodeSense launched a session's agent — enough to launch it again exactly
 * the same way. Kept per session (`recordAgentLaunch`); a session whose agent
 * the user typed has none.
 */
export interface AgentLaunchSpec {
  agentPath: string;
  /** The dir a launch whose shell the panel spawned projected its per-session config
   *  for; a relaunch projects a fresh one there. A launch with `shell` uses its cwd. */
  configCwd?: string;
  /** The launch args before any scope flags (omitted: the launcher's default). */
  args?: string[];
  /** Present when the launcher chose the shell's rules itself. */
  shell?: {
    cwd: string;
    /** Env var names stripped from the shell (the guarded API key). */
    envRemove: string[];
    /** The account-selection intent; Rust resolves it again at each spawn. */
    account?: ResolveAgentAccountInput;
    /** The MCP allowlist a scoped launch projects a fresh config for (`null`: none). */
    mcpAllow?: string[] | null;
    /** Captured effective bridge tool grant, kept for a freshly projected relaunch. */
    bridgeTools?: string[] | null;
    /** A named agent's launch, whose own settings file rides its args. */
    agent?: boolean;
    /** A named agent's own environment variables, set again on a relaunch. */
    env?: Record<string, string>;
  };
}

/** Mirror agent_cli::compose/operands_at: fresh options go before the operand suffix. */
function scopedLaunchArgs(args: readonly string[], extraArgs: readonly string[]): string[] {
  const separator = args.indexOf("--");
  const end = separator < 0 ? args.length : separator;
  const options = [...args.slice(0, end), ...extraArgs];
  const ordered = extraArgs.includes("--no-daemon")
    ? ["--no-daemon", ...options.filter((arg) => arg !== "--no-daemon")]
    : options;
  return [...ordered, ...args.slice(end)];
}

/** The line that launches `spec`'s agent, with its per-session config's `extraArgs`. */
export function agentLaunchLine(spec: AgentLaunchSpec, extraArgs: readonly string[]): string {
  if (spec.shell?.mcpAllow != null) {
    const ordered = scopedLaunchArgs(spec.args ?? [], extraArgs);
    return buildAgentLaunchCommand(spec.agentPath, ordered);
  }
  if (extraArgs.length > 0) {
    return buildAgentLaunchCommand(
      spec.agentPath,
      scopedLaunchArgs(spec.args ?? [PERMISSIVE_LAUNCH_FLAG], extraArgs),
    );
  }
  return spec.args === undefined
    ? buildAgentLaunchCommand(spec.agentPath)
    : buildAgentLaunchCommand(spec.agentPath, spec.args);
}

/** Remember how CodeSense launched a session's agent (and the exact line it typed). */
export function recordAgentLaunch(sessionId: string, spec: AgentLaunchSpec, line: string): void {
  useAgentStateStore
    .getState()
    .recordLaunch(sessionId, { line, agent: agentBasename(spec.agentPath), spec });
}

/** What a respawn of the tab runs with: the launch's own shell rules (env strip,
 *  account, directory), and for a relaunch, the scope to project afresh. */
export interface RespawnRequest {
  shell?: { cwd: string; envRemove: string[]; account?: ResolveAgentAccountInput };
  scope?: {
    cwd: string;
    mcpAllow: string[] | null;
    bridgeTools?: string[] | null;
    account?: ResolveAgentAccountInput;
    launchArgs?: string[];
    binary?: string;
    agent?: boolean;
    defaultArgs?: boolean;
    agentEnv?: Record<string, string>;
  };
}

/** Start a new process in the ended tab (`terminal_respawn_tab`); resolves
 *  with a scoped relaunch's fresh launch arguments. */
export type RespawnTab = (
  request: RespawnRequest,
) => Promise<{ extraArgs: string[]; removeArgs?: string[]; execution?: ScopedExecution }>;

/** The respawn for a plain Restart: a launch's own shell rules, no agent. */
export function restartRequest(spec: AgentLaunchSpec | undefined): RespawnRequest {
  const shell = spec?.shell;
  return shell
    ? { shell: { cwd: shell.cwd, envRemove: shell.envRemove, account: shell.account } }
    : {};
}

/** How a restart or relaunch went; `reason` says why it did not. */
export type RelaunchResult = { ok: true } | { ok: false; reason: string };

/** Why a launch line did not go in, in words. */
function injectFailure(outcome: Extract<InjectOutcome, { ok: false }>): string {
  return outcome.reason === "not-submitted"
    ? "the launch line was typed but never ran"
    : "the shell never reached its prompt";
}

export interface RelaunchDeps {
  launchAgentLine: LaunchDeps["launchAgentLine"];
}

export const realRelaunchDeps: RelaunchDeps = { launchAgentLine: launchAgentLineInShell };

/**
 * Launch a session's agent again the way CodeSense first launched it, through
 * the same seams: the host starts a new process in the ended tab with the
 * launch's own shell rules (env strip, account) and a freshly projected scope,
 * then the launch line goes in through the same shell-gated injector.
 */
export async function relaunchAgentSession(
  sessionId: string,
  spec: AgentLaunchSpec,
  respawn: RespawnTab,
  deps: RelaunchDeps = realRelaunchDeps,
): Promise<RelaunchResult> {
  const shell = spec.shell;
  const request: RespawnRequest = restartRequest(spec);
  const cwd = shell?.cwd ?? spec.configCwd;
  if (cwd) {
    // Every relaunch gets a fresh per-session config, scoped or not.
    request.scope = {
      cwd,
      mcpAllow: shell?.mcpAllow ?? null,
      bridgeTools: shell?.bridgeTools ?? null,
      account: shell?.account,
      binary: agentBasename(spec.agentPath),
      ...(shell?.agent ? { agent: true } : {}),
      ...(shell?.env ? { agentEnv: shell.env } : {}),
      ...(spec.args === undefined ? { defaultArgs: true } : {}),
      launchArgs: spec.args ?? [],
    };
  }
  const { extraArgs, removeArgs, execution } = await respawn(request);
  if (execution) {
    const freshSpec = { ...spec, agentPath: execution.argv[0], args: execution.argv.slice(1) };
    const line = buildAgentLaunchCommand(freshSpec.agentPath, freshSpec.args);
    recordAgentLaunch(sessionId, freshSpec, line);
    return { ok: true };
  }
  let freshSpec = spec;
  if (removeArgs !== undefined) {
    const original = spec.args ?? [];
    const kept: string[] = [];
    let removed = 0;
    for (let at = 0; at < original.length; ) {
      // Match complete overrides, including each -c value, in their original order.
      // A shared flag token alone must never remove the user's unrelated override.
      const width = removeArgs[removed] === "-c" || removeArgs[removed] === "--config" ? 2 : 1;
      if (
        removed < removeArgs.length &&
        removeArgs
          .slice(removed, removed + width)
          .every((token, offset) => original[at + offset] === token)
      ) {
        removed += width;
        at += width;
      } else {
        kept.push(original[at++]);
      }
    }
    freshSpec = { ...spec, args: scopedLaunchArgs(kept, extraArgs) };
  }
  const line = agentLaunchLine(freshSpec, removeArgs === undefined ? extraArgs : []);
  const launched = await deps.launchAgentLine(sessionId, line);
  if (!launched.ok) return { ok: false, reason: injectFailure(launched) };
  recordAgentLaunch(sessionId, freshSpec, line);
  return { ok: true };
}

/** Launch the agent again in its shell, which still runs (the agent crashed in it). */
export async function relaunchAgentInLiveShell(
  sessionId: string,
  line: string,
  deps: RelaunchDeps = realRelaunchDeps,
): Promise<RelaunchResult> {
  const launched = await deps.launchAgentLine(sessionId, line);
  return launched.ok ? { ok: true } : { ok: false, reason: injectFailure(launched) };
}

/** The process name the AC-12 liveness probe matches: the agent exe basename. */
export function agentBasename(agentPath: string): string {
  const parts = agentPath.split("/");
  return parts[parts.length - 1] || agentPath;
}

/** AC-12: true `<agent>`-process liveness in the owned tab (connected vs dropped
 *  to a bare `zsh`). What SW-GS-1's connection chip polls — never the render path. */
export function isAgentConnected(sessionId: string, agentPath: string): Promise<boolean> {
  return sessionHasLiveAgent(sessionId, agentBasename(agentPath));
}

/**
 * Poll the AC-12 liveness probe until the agent process is alive (or attempts
 * exhausted). This is the RELIABLE "the (re)launched agent has come up" signal —
 * it reads the actual process tree (sysinfo), independent of the hook-driven
 * `aiStatus` (hook-driven, so it cannot be the gate for an auto-injected line into
 * a live agent).
 */
export async function waitForAgentLive(
  sessionId: string,
  agentName: string,
  opts: {
    attempts?: number;
    sleepMs?: number;
    sleep?: (ms: number) => Promise<void>;
    isLive?: (sessionId: string, agentName: string) => Promise<boolean>;
  } = {},
): Promise<boolean> {
  const attempts = opts.attempts ?? 30;
  const sleepMs = opts.sleepMs ?? 200;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const isLive = opts.isLive ?? sessionHasLiveAgent;
  for (let i = 0; i < attempts; i++) {
    if (await isLive(sessionId, agentName)) return true;
    if (i < attempts - 1) await sleep(sleepMs);
  }
  return false;
}

export interface ReconnectDeps {
  /** AC-1: probe whether the bound session's agent process is still alive, so
   *  reconnect can RE-ATTACH instead of relaunching. */
  probeLive: (sessionId: string, agentName: string) => Promise<boolean>;
  /** Relaunch line into the bare `zsh` (gate "shell" — polls until idle). */
  relaunch: (sessionId: string, line: string) => Promise<InjectOutcome>;
  /** Wait until the relaunched agent process is alive (liveness probe). */
  waitLive: (sessionId: string, agentName: string) => Promise<boolean>;
  /** Send the re-orient line to the CONFIRMED-live agent (gate "none"). */
  sendReorient: (sessionId: string, line: string) => Promise<InjectOutcome>;
  /** A fresh per-session config for a relaunch with no recorded line. */
  projectScopeConfig: LaunchDeps["projectScopeConfig"];
}

export const realReconnectDeps: ReconnectDeps = {
  probeLive: (sessionId, agentName) => sessionHasLiveAgent(sessionId, agentName),
  relaunch: (sessionId, line) => injectWhenReady(sessionId, line, { gate: "shell" }),
  waitLive: (sessionId, agentName) => waitForAgentLive(sessionId, agentName),
  // SW-GS-30 `submit:true`: route the `/cs-status` re-orient (the recipe trigger)
  // through `submit_pty_line` — confirmed-submitted / nudged, not a raw `text\r`.
  sendReorient: (sessionId, line) =>
    injectAgentLine(sessionId, line, undefined, { gate: "none", submit: true }),
  projectScopeConfig: (sessionId, cwd, mcpAllow, account, launchArgs, launch) =>
    projectMcpScopeConfig(sessionId, cwd, mcpAllow, account, launchArgs, launch),
};

/** The launcher's default line for `agentPath`, with a fresh per-session config for `cwd`. */
async function freshLaunchLine(
  params: { sessionId: string; agentPath: string; cwd?: string },
  deps: ReconnectDeps,
): Promise<string> {
  if (!params.cwd) return buildAgentLaunchCommand(params.agentPath);
  const scope = await deps.projectScopeConfig(params.sessionId, params.cwd, null, undefined, [], {
    binary: agentBasename(params.agentPath),
    defaultArgs: true,
  });
  return agentLaunchLine({ agentPath: params.agentPath }, scope.extraArgs);
}

/** AC-1: which path reconnect took — `reattached` to a still-live agent (no
 *  relaunch) vs. relaunched a fresh context. The caller re-primes either way
 *  (AC-0), but may message the user differently. */
export interface ReconnectResult {
  reattached: boolean;
}

/**
 * SW-GS-2d (AC-1) — STATE-AWARE reconnect. SW-GS-2b relaunched unconditionally
 * through a SHELL-idle gate, which threw "shell never reached its prompt"
 * whenever the agent was still running (a Disconnect only releases the keyboard;
 * the process keeps owning the TTY, so the bare-`zsh` gate never opens). Now:
 *
 *  - **Agent alive →** RE-ATTACH: just re-run the consumer's `reorientCommand`
 *    (`/cs-status`) against the RUNNING agent (gate "none"). No relaunch, no
 *    shell gate — the reported bug can't occur for a Disconnect→Reconnect.
 *  - **Agent dead →** relaunch a FRESH context (NOT `--resume` — Claude-specific,
 *    breaks agent-agnosticism; the agent re-derives durable state), wait on the
 *    LIVENESS probe (not `aiStatus`, so it never lands in the boot gap), then
 *    re-orient. SW-WA-F10 makes "dropped to a bare shell" unreachable for an
 *    SW-WA-F10 launched session: the agent is PID-1 of the PTY, so its exit
 *    ends the session rather than returning to zsh.
 *
 * Store-free + literal-free: ownership/connection writes and the chat re-prime
 * (AC-0) are the CALLER's job (the feature adapter / `useAgentWorkspaceSession`),
 * which also runs the ping/pong handshake. Each step's failure surfaces loudly.
 */
export async function reconnectAgentSession(
  params: { sessionId: string; agentPath: string; reorientCommand: string; cwd?: string },
  deps: ReconnectDeps = realReconnectDeps,
): Promise<ReconnectResult> {
  const agentName = agentBasename(params.agentPath);

  // SW-GS-30: the re-orient routes through `submit_pty_line` (`submit:true`), which
  // reports whether the line actually submitted. This is a fire-and-forget path (no
  // reply-watch), so surface a not-submitted outcome as a diagnostic here at the
  // caller rather than swallowing it — the reliable-submit nudges make it rare, but
  // when it happens "the agent never re-derived" is exactly what we'd need to know.
  const reorient = async (): Promise<void> => {
    const outcome = await deps.sendReorient(params.sessionId, params.reorientCommand);
    if (!outcome.ok) {
      logger.warn(
        "agent-session",
        `reconnect re-orient not confirmed submitted (session ${params.sessionId}, ${outcome.reason}): ${params.reorientCommand}`,
      );
    }
  };

  // AC-1 re-attach: the agent is still running — just re-orient it. No relaunch.
  // A probe FAILURE (Tauri error, session gone) is treated as "not live" so we
  // fall through to a safe relaunch rather than aborting reconnect — matching how
  // the picker's liveness probe treats errors.
  let alive = false;
  try {
    alive = await deps.probeLive(params.sessionId, agentName);
  } catch {
    alive = false;
  }
  if (alive) {
    await reorient();
    return { reattached: true };
  }

  // AC-1 relaunch: the agent is gone — structurally NO `--resume`. The line is the one
  // CodeSense launched it with (which names the session's own config files, alive as
  // long as its shell). With none recorded (the app restarted under a shell the session
  // server kept), a fresh per-session config for its dir.
  const recorded = useAgentStateStore.getState().launchBySession[params.sessionId]?.line;
  const relaunched = await deps.relaunch(
    params.sessionId,
    recorded ?? (await freshLaunchLine(params, deps)),
  );
  if (!relaunched.ok) {
    throw new Error(
      `Agent reconnect: shell never reached its prompt to relaunch the agent (session ${params.sessionId}).`,
    );
  }
  const live = await deps.waitLive(params.sessionId, agentName);
  if (!live) {
    throw new Error(
      `Agent reconnect: the agent did not come up after relaunch (session ${params.sessionId}).`,
    );
  }
  await reorient();
  return { reattached: false };
}
